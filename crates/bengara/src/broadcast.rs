//! 配信（Broadcast）。WebSocket の接続をチャンネルに入れ、どこからでも送ります。
//!
//! ```ignore
//! use bengara::broadcast::Broadcast;
//!
//! // HTTP のハンドラから。トランザクションのコミットの後で呼ぶ。
//! Broadcast::publish("conversation.123", payload_json).await?;
//!
//! // チャンネルの全接続を閉じる（別のプロセスからも効く）。
//! Broadcast::disconnect("session.987", 4001, "session revoked").await?;
//! ```
//!
//! **送り方は 2 つです。** `BROADCAST_CONNECTION`（Laravel 11 の名前）で選びます。
//! 空なら `BROADCAST_DRIVER` を見ます。どちらも空なら `memory` です。
//!
//! | 送り方 | 届く範囲 | 使う場面 |
//! |---|---|---|
//! | `memory`（既定。別名 `array`） | 同じプロセスの中だけ | サーバー 1 つで、別のプロセスから送らないとき |
//! | `database` | すべてのプロセス（表 `broadcasts` を中継に使う） | 定期処理・キュー・コマンドから送るとき、サーバーが複数のとき |
//!
//! 知らない名前は `memory` に落とさず、送るときにエラーにします（メールと同じ）。
//!
//! **`publish` はトランザクションに参加しません。** 巻き戻った操作を配信しないように、
//! 呼ぶ側がコミットの後に呼んでください。
//!
//! 機能フラグ `websocket` が無くても呼べます。`memory` なら届く相手がいないだけで、
//! `database` なら表に行を書きます（サーバーのプロセスが中継します）。

// 中継（relay）はサーバーが `websocket` のフラグ付きで立てる。フラグが無いと使われない。
#![cfg_attr(not(feature = "websocket"), allow(dead_code))]

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;

use crate::database::{Schema, Value, DB};
use crate::error::{Error, Result};
use crate::websocket::registry;

/// 中継に使う表の名前。
pub const TABLE: &str = "broadcasts";

/// チャンネルの名前の上限（表の列の長さ）。
const MAX_CHANNEL_BYTES: usize = 255;

/// 中継が 1 回に読む行の数。
const BATCH: usize = 500;

/// 中継が「ここまでは届けた」と決めてよい古さ。
///
/// これより新しい行は、id の若い行が後からコミットされることがあるので、
/// 何度でも読み直します（届けた id は覚えておき、二度は届けない）。
const SETTLE_AFTER_SECS: i64 = 5;

/// 古い行を掃除する間隔。
const SWEEP_EVERY: Duration = Duration::from_secs(60);

/// 中継の DB の失敗を、続けて警告しない間隔。
const WARN_EVERY: Duration = Duration::from_secs(60);

/// 配信の設定。`config/broadcasting.rs` などが返します。
///
/// ```ignore
/// pub fn config() -> BroadcastConfig {
///     BroadcastConfig { driver: env("BROADCAST_CONNECTION", "database"), ..Default::default() }
/// }
/// ```
///
/// `config/` に無ければ環境変数から作ります。
#[derive(Debug, Clone)]
pub struct BroadcastConfig {
    /// 送り方（`memory` / `database`）。`array` は `memory` の別名です。
    ///
    /// 大文字小文字と前後の空白は見ません。知らない名前は `memory` に落とさず、
    /// 送るときにエラーにします。
    pub driver: String,
    /// 中継が表を読む間隔（ミリ秒）。10 より小さい値は 10 にします。
    pub poll_ms: u64,
    /// 中継の行を残す長さ（秒）。5 より小さい値は 5 にします。
    pub retention_seconds: u64,
}

impl Default for BroadcastConfig {
    /// 環境変数から組み立てます。
    ///
    /// | 環境変数 | 既定値 |
    /// |---|---|
    /// | `BROADCAST_CONNECTION` | 空なら `BROADCAST_DRIVER` |
    /// | `BROADCAST_DRIVER` | `memory` |
    /// | `BROADCAST_POLL_MS` | 250 |
    /// | `BROADCAST_RETENTION_SECONDS` | 60 |
    fn default() -> Self {
        Self {
            driver: driver_from(
                std::env::var("BROADCAST_CONNECTION").ok().as_deref(),
                std::env::var("BROADCAST_DRIVER").ok().as_deref(),
            ),
            poll_ms: crate::env::<u64>("BROADCAST_POLL_MS", 250u64),
            retention_seconds: crate::env::<u64>("BROADCAST_RETENTION_SECONDS", 60u64),
        }
    }
}

/// 送り方の名前を決める。`BROADCAST_CONNECTION` が先、空なら `BROADCAST_DRIVER`、
/// どちらも空なら `memory`。前後の空白を落として小文字にします。
fn driver_from(connection: Option<&str>, driver: Option<&str>) -> String {
    [connection, driver]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|name| !name.is_empty())
        .unwrap_or("memory")
        .to_ascii_lowercase()
}

/// いまの設定。
fn config() -> &'static BroadcastConfig {
    static FALLBACK: OnceLock<BroadcastConfig> = OnceLock::new();
    crate::try_config::<BroadcastConfig>()
        .unwrap_or_else(|| FALLBACK.get_or_init(BroadcastConfig::default))
}

/// 中継が表を読む間隔。
fn poll_interval(config: &BroadcastConfig) -> Duration {
    Duration::from_millis(config.poll_ms.max(10))
}

/// 中継の行を残す長さ（秒）。
fn retention_secs(config: &BroadcastConfig) -> i64 {
    i64::try_from(config.retention_seconds.max(5)).unwrap_or(i64::MAX)
}

/// 送り方。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Driver {
    Memory,
    Database,
    /// 知らない名前。送るときにエラーにする。
    Unknown(String),
}

impl Driver {
    fn parse(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "memory" | "array" | "" => Driver::Memory,
            "database" => Driver::Database,
            other => Driver::Unknown(other.to_string()),
        }
    }
}

/// 知らない送り方の説明文。
fn unknown_driver_message(name: &str) -> String {
    format!("BROADCAST_CONNECTION の `{name}` は知りません。使えるのは memory と database です")
}

/// いまの送り方。最初に使うときに 1 回だけ決めます（知らない名前なら警告も 1 回）。
fn driver() -> &'static Driver {
    static DRIVER: OnceLock<Driver> = OnceLock::new();
    DRIVER.get_or_init(|| {
        let driver = Driver::parse(&config().driver);
        if let Driver::Unknown(name) = &driver {
            tracing::warn!("{}", unknown_driver_message(name));
        }
        driver
    })
}

/// このプロセスがサーバー（`serve`）か。
static SERVER: AtomicBool = AtomicBool::new(false);

/// 「このプロセスはサーバー」と覚える。`serve` が呼びます。
pub(crate) fn mark_server() {
    SERVER.store(true, Ordering::Release);
}

fn is_server() -> bool {
    SERVER.load(Ordering::Acquire)
}

/// 書いたプロセスの印（起動のたびに作る UUID）。中継が自分の行を見分けるのに使う。
fn origin() -> &'static str {
    static ORIGIN: OnceLock<String> = OnceLock::new();
    ORIGIN.get_or_init(new_origin)
}

/// UUID（版 4）の形の文字列を作る。36 文字。
fn new_origin() -> String {
    let mut bytes = crate::support::crypto::random_bytes::<16>();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = crate::support::crypto::to_hex(&bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

// ---------------------------------------------------------------------------
// テスト用の記録（fake）
// ---------------------------------------------------------------------------

/// `fake()` の間か。
static FAKE: AtomicBool = AtomicBool::new(false);

/// `fake()` の間に publish されたもの（チャンネル、中身）。
static RECORDED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

fn recorded() -> std::sync::MutexGuard<'static, Vec<(String, String)>> {
    RECORDED.lock().unwrap_or_else(|e| e.into_inner())
}

/// fake を終えて記録を空にする。`testing::exclusive()` の札を手放したときに呼ばれます。
pub(crate) fn reset_fake() {
    if FAKE.swap(false, Ordering::AcqRel) {
        recorded().clear();
    }
}

// ---------------------------------------------------------------------------
// 送る
// ---------------------------------------------------------------------------

/// 送るもの。
enum Kind<'a> {
    Publish(&'a str),
    Disconnect { code: u16, reason: &'a str },
}

/// チャンネルの名前を確かめる。送る側（`publish`）と入る側（`join`）で同じ規則を使う。
pub(crate) fn check_channel(channel: &str) -> Result<()> {
    if channel.is_empty() {
        return Err(Error::msg("配信のチャンネルの名前がありません"));
    }
    if channel.len() > MAX_CHANNEL_BYTES {
        return Err(Error::msg(format!(
            "配信のチャンネルの名前は {MAX_CHANNEL_BYTES} バイトまでです"
        )));
    }
    Ok(())
}

/// 送り方を決めて送る。`Broadcast::publish` / `disconnect` の本体。
///
/// 送り方を引数で受け取るのは、選び方を 1 回しか決めないため（テストから確かめられない）。
async fn send(driver: &Driver, channel: &str, kind: Kind<'_>) -> Result<()> {
    check_channel(channel)?;
    match driver {
        Driver::Unknown(name) => Err(Error::msg(unknown_driver_message(name))),
        Driver::Memory => {
            if !is_server() {
                warn_not_server();
            }
            deliver_local(channel, &kind);
            Ok(())
        }
        Driver::Database => {
            insert_row(channel, &kind, origin()).await?;
            // 自分の接続にはすぐ届ける（中継を待たない）。中継は自分の行を飛ばす。
            deliver_local(channel, &kind);
            Ok(())
        }
    }
}

/// サーバーでないプロセスから memory で送ったときの警告。1 プロセスに 1 回だけ。
fn warn_not_server() {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::AcqRel) {
        tracing::warn!(
            "このプロセスはサーバーではないので、memory の配信はどの接続にも届きません。\
             別のプロセスから送るときは BROADCAST_CONNECTION=database にしてください"
        );
    }
}

/// このプロセスの接続に届ける。
fn deliver_local(channel: &str, kind: &Kind<'_>) {
    match kind {
        Kind::Publish(text) => {
            registry::deliver(channel, text);
        }
        Kind::Disconnect { code, reason } => {
            registry::close_channel(channel, *code, reason);
        }
    }
}

/// 中継の表に 1 行入れる。
async fn insert_row(channel: &str, kind: &Kind<'_>, origin: &str) -> Result<()> {
    let (kind, payload, code, reason) = match kind {
        Kind::Publish(text) => (
            "publish",
            Value::Text(text.to_string()),
            Value::Null,
            Value::Null,
        ),
        Kind::Disconnect { code, reason } => (
            "disconnect",
            Value::Null,
            Value::Int(i64::from(*code)),
            Value::Text(reason.to_string()),
        ),
    };
    DB::table(TABLE)
        .insert(&[
            ("kind", Value::Text(kind.to_string())),
            ("channel", Value::Text(channel.to_string())),
            ("payload", payload),
            ("close_code", code),
            ("reason", reason),
            ("origin", Value::Text(origin.to_string())),
            ("created_at", Value::Text(crate::database::now())),
        ])
        .await?;
    Ok(())
}

/// 配信の入口。
pub struct Broadcast;

impl Broadcast {
    /// チャンネルに入っている全接続へ送る。どのプロセスから呼んでもかまいません。
    ///
    /// 全員に同じ文字列を送ります。列がいっぱいの接続はその接続だけを 1008 で閉じ、
    /// ほかへの配信は止めません。同じプロセスから同じチャンネルへ送ったものは、送った順に届きます。
    ///
    /// **トランザクションに参加しません。** コミットの後に呼んでください。
    pub async fn publish(channel: &str, text: impl Into<String>) -> Result<()> {
        let text = text.into();
        if FAKE.load(Ordering::Acquire) {
            check_channel(channel)?;
            recorded().push((channel.to_string(), text));
            return Ok(());
        }
        send(driver(), channel, Kind::Publish(&text)).await
    }

    /// 値を JSON にして送る。
    pub async fn publish_json<T: Serialize>(channel: &str, value: &T) -> Result<()> {
        let text = serde_json::to_string(value)?;
        Self::publish(channel, text).await
    }

    /// チャンネルに入っている全接続を閉じる。どのプロセスから呼んでもかまいません。
    ///
    /// 送れない番号（1004〜1006、1015、1016〜2999、5000 以上、1000 未満）は 1000 に直します。
    /// 理由は 123 バイトを超えたら文字の境目で切ります。
    pub async fn disconnect(channel: &str, code: u16, reason: &str) -> Result<()> {
        if FAKE.load(Ordering::Acquire) {
            return check_channel(channel);
        }
        let code = crate::websocket::close_code(code);
        let reason = crate::websocket::close_reason(reason);
        send(
            driver(),
            channel,
            Kind::Disconnect {
                code,
                reason: &reason,
            },
        )
        .await
    }

    /// **このプロセスの中で**、そのチャンネルに入っている接続の数。
    ///
    /// サーバーが複数あるときは、ほかのプロセスの接続は数えません。
    pub fn local_count(channel: &str) -> usize {
        registry::count(channel)
    }

    /// 中継の表（`broadcasts`）を作るマイグレーションの中身。
    ///
    /// ```ignore
    /// pub fn up(schema: &mut Schema) {
    ///     Broadcast::define(schema);
    /// }
    /// ```
    pub fn define(schema: &mut Schema) {
        schema.create_if_not_exists(TABLE, |t| {
            t.id();
            t.string_with("kind", 16);
            t.string_with("channel", 255);
            t.text("payload").nullable();
            t.integer("close_code").nullable();
            t.string_with("reason", 123).nullable();
            t.string_with("origin", 36);
            t.date_time("created_at");
            t.index(&["created_at"]);
        });
    }

    /// 以後の `publish` を記録だけして送らない（テスト用）。記録は空から始めます。
    ///
    /// **記録はプロセスで共有です。** 使うテストは `testing::exclusive()` の札を取ってください
    /// （`Mail::sent()` と同じ）。札を手放すと fake は終わり、記録も空になります。
    pub fn fake() {
        recorded().clear();
        FAKE.store(true, Ordering::Release);
    }

    /// fake の間に、そのチャンネルへ publish された中身（送った順）。
    pub fn published(channel: &str) -> Vec<String> {
        recorded()
            .iter()
            .filter(|(c, _)| c == channel)
            .map(|(_, text)| text.clone())
            .collect()
    }

    /// そのチャンネルへ、`contains` を含む中身が publish されたか確かめる。無ければパニック。
    #[track_caller]
    pub fn assert_published(channel: &str, contains: &str) {
        let published = Self::published(channel);
        assert!(
            published.iter().any(|text| text.contains(contains)),
            "チャンネル `{channel}` に `{contains}` を含む配信がありません（届いたもの: {published:?}）"
        );
    }

    /// 何も publish されていないことを確かめる。あればパニック。
    #[track_caller]
    pub fn assert_nothing_published() {
        let all = recorded().clone();
        assert!(all.is_empty(), "配信がありました: {all:?}");
    }
}

// ---------------------------------------------------------------------------
// 中継（database）
// ---------------------------------------------------------------------------

/// サーバーの起動時に呼ぶ。送り方が database のときだけ、中継のタスクを立てます。
#[cfg(feature = "websocket")]
pub(crate) fn start_relay() {
    if *driver() != Driver::Database {
        return;
    }
    let config = config();
    let poll = poll_interval(config);
    let mut relay = Relay::new(origin().to_string(), retention_secs(config));
    tokio::spawn(async move {
        loop {
            let more = relay.step().await;
            if !more {
                tokio::time::sleep(poll).await;
            }
        }
    });
}

/// 中継の状態。1 プロセスに 1 本のタスクだけが持ちます（接続ごとに DB を読まない）。
///
/// id の順に読むだけだと、複数のプロセスが同時に書いたときに、**id の若い行が後から
/// コミットされて取りこぼします**（PostgreSQL のシーケンス、MySQL の auto_increment）。
/// そこで次のように読みます。
///
/// - 起動時: いまの最大の id を `settled`（ここまでは届けない）とする
/// - 毎回: `id > settled` の行を id の順に読む。届けた id は `delivered` に入れ、二度届けない
/// - `settled` は「`created_at` が 5 秒より古い行の最大の id」まで進め、`delivered` からそれ以下を捨てる
/// - 500 行届けたら、待たずに続けて読む
struct Relay {
    origin: String,
    retention_secs: i64,
    /// ここまでの id は読まない。起動して最初に読めるまでは `None`。
    settled: Option<i64>,
    /// `settled` より大きい id のうち、届けたもの。
    delivered: BTreeSet<i64>,
    last_sweep: Option<std::time::Instant>,
    last_warn: Option<std::time::Instant>,
}

impl Relay {
    fn new(origin: String, retention_secs: i64) -> Self {
        Self {
            origin,
            retention_secs,
            settled: None,
            delivered: BTreeSet::new(),
            last_sweep: None,
            last_warn: None,
        }
    }

    /// 1 回分進める。真なら待たずに続けて読む。
    ///
    /// DB の失敗は警告してやり直します。**中継のタスクは止めません。**
    /// 警告は 1 分に 1 回までにします（表が無いまま起動すると毎回失敗するため）。
    async fn step(&mut self) -> bool {
        match self.poll().await {
            Ok(more) => {
                if self.last_warn.take().is_some() {
                    tracing::info!("配信の中継が表を読めるようになりました");
                }
                more
            }
            Err(e) => {
                let quiet = self.last_warn.is_some_and(|at| at.elapsed() < WARN_EVERY);
                if !quiet {
                    tracing::warn!("配信の中継が表 {TABLE} を読めません（やり直します）: {e}");
                    self.last_warn = Some(std::time::Instant::now());
                }
                false
            }
        }
    }

    /// 表を 1 回読み、届けて、`settled` を進め、ときどき掃除する。
    async fn poll(&mut self) -> Result<bool> {
        let Some(settled) = self.settled else {
            let max = DB::table(TABLE).max::<i64>("id").await?.unwrap_or(0);
            self.settled = Some(max);
            return Ok(false);
        };

        // 届けていない id を、最大 500 個まで集める。
        let mut fresh = Vec::new();
        // 集めきれずに止めたとき、`settled` を進めてよい上限。
        let mut frontier: Option<i64> = None;
        let mut cursor = settled;
        'scan: loop {
            let ids: Vec<i64> = DB::table(TABLE)
                .where_op("id", ">", cursor)
                .order_by("id")
                .limit(BATCH as u64)
                .pluck("id")
                .await?;
            for id in &ids {
                if self.delivered.contains(id) {
                    continue;
                }
                if fresh.len() == BATCH {
                    frontier = Some(id - 1);
                    break 'scan;
                }
                fresh.push(*id);
            }
            match ids.last() {
                Some(last) if ids.len() == BATCH => cursor = *last,
                _ => break,
            }
        }

        for chunk in fresh.chunks(BATCH) {
            let rows = DB::table(TABLE)
                .where_in("id", chunk)
                .order_by("id")
                .get()
                .await?;
            for row in rows {
                let id: i64 = row.get("id")?;
                self.delivered.insert(id);
                let origin: String = row.get("origin")?;
                if origin == self.origin {
                    // 自分の行。送ったときに届け済み。
                    continue;
                }
                let channel: String = row.get("channel")?;
                let kind: String = row.get("kind")?;
                match kind.as_str() {
                    "publish" => {
                        let payload: Option<String> = row.get("payload")?;
                        registry::deliver(&channel, payload.as_deref().unwrap_or(""));
                    }
                    "disconnect" => {
                        let code: Option<i64> = row.get("close_code")?;
                        let reason: Option<String> = row.get("reason")?;
                        let code = code.and_then(|c| u16::try_from(c).ok()).unwrap_or(1000);
                        registry::close_channel(&channel, code, reason.as_deref().unwrap_or(""));
                    }
                    other => {
                        tracing::warn!(
                            "配信の表に知らない種類 `{other}` の行があります（飛ばします）"
                        );
                    }
                }
            }
        }

        // 5 秒より古い行までは、もう若い id が後から来ないと見なして進める。
        let cutoff = crate::support::time::format_timestamp(
            crate::support::time::now_seconds() - SETTLE_AFTER_SECS,
        );
        let old: Option<i64> = DB::table(TABLE)
            .where_op("id", ">", settled)
            .where_op("created_at", "<", cutoff)
            .max("id")
            .await?;
        if let Some(old) = old {
            let next = frontier.map_or(old, |frontier| old.min(frontier));
            if next > settled {
                self.settled = Some(next);
                self.delivered = self.delivered.split_off(&(next + 1));
            }
        }

        self.sweep().await?;
        Ok(frontier.is_some())
    }

    /// 1 分に 1 回、残す長さより古い行を消す。
    async fn sweep(&mut self) -> Result<()> {
        if self.last_sweep.is_some_and(|at| at.elapsed() < SWEEP_EVERY) {
            return Ok(());
        }
        let cutoff = crate::support::time::format_timestamp(
            crate::support::time::now_seconds().saturating_sub(self.retention_secs),
        );
        DB::table(TABLE)
            .where_op("created_at", "<", cutoff)
            .delete()
            .await?;
        self.last_sweep = Some(std::time::Instant::now());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Driver as DbDriver;
    use crate::websocket::registry::{Conn, Outgoing, Receivers};
    use std::sync::Arc;

    /// テストごとに違うチャンネル名（表はプロセスで共有）。
    fn unique(name: &str) -> String {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        format!(
            "test.broadcast.{name}.{}",
            SEQ.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// 表に載った接続を 1 本作り、チャンネルに入れる。
    fn member(channel: &str) -> (Arc<Conn>, Receivers) {
        let (conn, rx) = Conn::new(1024 * 1024);
        registry::register(&conn);
        registry::join(conn.id(), channel);
        (conn, rx)
    }

    fn texts(rx: &mut Receivers) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(item) = rx.out.try_recv() {
            if let Outgoing::Text(text) = item {
                out.push(text.to_string());
            }
        }
        out
    }

    #[test]
    fn 送り方の名前の選び方() {
        assert_eq!(driver_from(None, None), "memory");
        assert_eq!(driver_from(Some("database"), Some("memory")), "database");
        assert_eq!(driver_from(Some("  "), Some(" Database ")), "database");
        assert_eq!(driver_from(Some(""), None), "memory");
        assert_eq!(Driver::parse("array"), Driver::Memory);
        assert_eq!(Driver::parse("MEMORY"), Driver::Memory);
        assert_eq!(Driver::parse(" database "), Driver::Database);
        assert_eq!(Driver::parse("redis"), Driver::Unknown("redis".into()));
    }

    #[test]
    fn 設定の下限() {
        let config = BroadcastConfig {
            driver: "memory".into(),
            poll_ms: 1,
            retention_seconds: 1,
        };
        assert_eq!(poll_interval(&config), Duration::from_millis(10));
        assert_eq!(retention_secs(&config), 5);
        let defaults = BroadcastConfig {
            driver: "memory".into(),
            poll_ms: 250,
            retention_seconds: 60,
        };
        assert_eq!(poll_interval(&defaults), Duration::from_millis(250));
        assert_eq!(retention_secs(&defaults), 60);
    }

    #[test]
    fn 書いたプロセスの印はuuidの形() {
        let origin = new_origin();
        assert_eq!(origin.len(), 36);
        assert_eq!(origin.as_bytes()[14], b'4', "版 4");
        assert_eq!(origin.matches('-').count(), 4);
        assert_ne!(new_origin(), origin);
        assert_eq!(super::origin(), super::origin(), "プロセスの中では同じ");
    }

    #[tokio::test]
    async fn 知らない送り方はエラーにする() {
        let error = send(&Driver::Unknown("redis".into()), "a", Kind::Publish("x"))
            .await
            .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("`redis` は知りません"), "{text}");
        assert!(text.contains("memory と database"), "{text}");
    }

    #[tokio::test]
    async fn チャンネルの名前を確かめる() {
        assert!(send(&Driver::Memory, "", Kind::Publish("x")).await.is_err());
        let long = "a".repeat(256);
        assert!(send(&Driver::Memory, &long, Kind::Publish("x"))
            .await
            .is_err());
        assert!(send(&Driver::Memory, &"a".repeat(255), Kind::Publish("x"))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn memoryは入っている接続だけに届ける() {
        let channel = unique("memory");
        let (inside, mut inside_rx) = member(&channel);
        let (outside, mut outside_rx) = Conn::new(1024);
        registry::register(&outside);

        send(&Driver::Memory, &channel, Kind::Publish("one"))
            .await
            .unwrap();
        send(&Driver::Memory, &channel, Kind::Publish("two"))
            .await
            .unwrap();
        assert_eq!(texts(&mut inside_rx), vec!["one", "two"], "送った順に届く");
        assert!(texts(&mut outside_rx).is_empty());
        assert_eq!(Broadcast::local_count(&channel), 1);

        send(
            &Driver::Memory,
            &channel,
            Kind::Disconnect {
                code: 4001,
                reason: "revoked",
            },
        )
        .await
        .unwrap();
        assert_eq!(
            inside_rx.ctrl.try_recv().ok(),
            Some((4001, "revoked".to_string()))
        );

        registry::unregister(inside.id());
        registry::unregister(outside.id());
        assert_eq!(Broadcast::local_count(&channel), 0);
    }

    #[tokio::test]
    async fn publishとdisconnectは送り方に従う() {
        // 既定の送り方（環境変数が無ければ memory）。fake と取り合わないよう札を取る。
        let _lock = crate::testing::exclusive().await;
        let channel = unique("public");
        let (conn, mut rx) = member(&channel);
        if *driver() == Driver::Memory {
            Broadcast::publish(&channel, "hello").await.unwrap();
            Broadcast::publish_json(&channel, &serde_json::json!({ "op": 1 }))
                .await
                .unwrap();
            assert_eq!(
                texts(&mut rx),
                vec!["hello".to_string(), r#"{"op":1}"#.to_string()]
            );
            Broadcast::disconnect(&channel, 1006, &"あ".repeat(60))
                .await
                .unwrap();
            let (code, reason) = rx.ctrl.try_recv().unwrap();
            assert_eq!(code, 1000, "送れない番号は直す");
            assert!(reason.len() <= 123);
        }
        registry::unregister(conn.id());
    }

    #[tokio::test]
    async fn fakeは記録だけして送らない() {
        let channel = unique("fake");
        let (conn, mut rx) = member(&channel);
        {
            let _lock = crate::testing::exclusive().await;
            Broadcast::fake();
            Broadcast::assert_nothing_published();
            Broadcast::publish(&channel, "a").await.unwrap();
            Broadcast::publish_json(&channel, &serde_json::json!({ "op": "x" }))
                .await
                .unwrap();
            Broadcast::publish("other", "b").await.unwrap();
            Broadcast::disconnect(&channel, 4001, "bye").await.unwrap();

            assert_eq!(
                Broadcast::published(&channel),
                vec!["a".to_string(), r#"{"op":"x"}"#.to_string()]
            );
            Broadcast::assert_published(&channel, "\"op\"");
            assert!(texts(&mut rx).is_empty(), "送らない");
            assert!(rx.ctrl.try_recv().is_err(), "閉じない");
            assert!(!conn.is_closed());

            let missing = std::panic::catch_unwind(|| Broadcast::assert_published(&channel, "zzz"));
            assert!(missing.is_err());
            let some = std::panic::catch_unwind(Broadcast::assert_nothing_published);
            assert!(some.is_err());

            // もう一度 fake にすると記録は空から。
            Broadcast::fake();
            Broadcast::assert_nothing_published();
            Broadcast::publish(&channel, "c").await.unwrap();
        }
        // 札を手放すと fake は終わり、記録も空。
        assert!(!FAKE.load(Ordering::Acquire));
        assert!(Broadcast::published(&channel).is_empty());
        registry::unregister(conn.id());
    }

    #[test]
    fn 表の定義が作れる() {
        let mut schema = Schema::new(DbDriver::Sqlite);
        Broadcast::define(&mut schema);
        let sql = schema.to_sql();
        assert!(sql[0].contains("\"broadcasts\""), "{}", sql[0]);
        assert!(sql[0].contains("\"payload\" text null"), "{}", sql[0]);
        assert!(sql[0].contains("\"close_code\" integer null"), "{}", sql[0]);
        assert!(sql
            .iter()
            .any(|s| s.contains("broadcasts_created_at_index")));
        assert_eq!(TABLE, "broadcasts");
    }

    #[cfg(feature = "sqlite")]
    mod relay {
        use super::*;

        async fn 表を作る() {
            let mut schema = Schema::new(DbDriver::Sqlite);
            Broadcast::define(&mut schema);
            for sql in schema.to_sql() {
                DB::statement(sql, &[]).await.unwrap();
            }
        }

        /// ほかのプロセスが書いた扱いの行を入れる。`id` を決めると、遅れてコミットされた行の代わりになる。
        async fn other(id: Option<i64>, channel: &str, payload: &str, created_at: &str) {
            let mut values = vec![
                ("kind", Value::Text("publish".into())),
                ("channel", Value::Text(channel.into())),
                ("payload", Value::Text(payload.into())),
                (
                    "origin",
                    Value::Text("00000000-0000-4000-8000-000000000000".into()),
                ),
                ("created_at", Value::Text(created_at.into())),
            ];
            if let Some(id) = id {
                values.push(("id", Value::Int(id)));
            }
            DB::table(TABLE).insert(&values).await.unwrap();
        }

        fn ago(secs: i64) -> String {
            crate::support::time::format_timestamp(crate::support::time::now_seconds() - secs)
        }

        #[tokio::test]
        async fn ほかのプロセスの行が届き自分の行は二重に届かない() {
            let _db = crate::testing::refresh_database().await;
            表を作る().await;
            let channel = unique("relay");
            let (conn, mut rx) = member(&channel);

            // 起動前の行は届けない。
            other(None, &channel, "before", &crate::database::now()).await;
            let mut relay = Relay::new(origin().to_string(), 60);
            assert!(!relay.step().await);
            assert_eq!(relay.settled, Some(1));

            other(None, &channel, "from-cron", &crate::database::now()).await;
            // 自分の行（database の送り方）。すぐ届き、中継では飛ばす。
            send(&Driver::Database, &channel, Kind::Publish("mine"))
                .await
                .unwrap();
            assert_eq!(texts(&mut rx), vec!["mine"]);
            relay.step().await;
            assert_eq!(texts(&mut rx), vec!["from-cron"]);
            // 何度読んでも二度は届かない。
            relay.step().await;
            assert!(texts(&mut rx).is_empty());

            // disconnect の行も届く。
            DB::table(TABLE)
                .insert(&[
                    ("kind", Value::Text("disconnect".into())),
                    ("channel", Value::Text(channel.clone())),
                    ("close_code", Value::Int(4001)),
                    ("reason", Value::Text("revoked".into())),
                    ("origin", Value::Text("other".into())),
                    ("created_at", Value::Text(crate::database::now())),
                ])
                .await
                .unwrap();
            relay.step().await;
            assert_eq!(rx.ctrl.try_recv().ok(), Some((4001, "revoked".to_string())));
            registry::unregister(conn.id());
        }

        #[tokio::test]
        async fn 遅れてコミットされた若いidも届く() {
            let _db = crate::testing::refresh_database().await;
            表を作る().await;
            let channel = unique("late");
            let (conn, mut rx) = member(&channel);
            let mut relay = Relay::new(origin().to_string(), 60);
            relay.step().await;
            assert_eq!(relay.settled, Some(0));

            other(Some(10), &channel, "ten", &crate::database::now()).await;
            relay.step().await;
            assert_eq!(texts(&mut rx), vec!["ten"]);

            // id 5 が後からコミットされた。新しい行なので settled は進んでいない。
            other(Some(5), &channel, "five", &crate::database::now()).await;
            relay.step().await;
            assert_eq!(texts(&mut rx), vec!["five"]);
            assert_eq!(relay.settled, Some(0));
            assert_eq!(relay.delivered.len(), 2);

            // 古くなったら settled を進め、覚えた id を捨てる。
            DB::table(TABLE)
                .update(&[("created_at", Value::Text(ago(30)))])
                .await
                .unwrap();
            relay.step().await;
            assert_eq!(relay.settled, Some(10));
            assert!(relay.delivered.is_empty());
            assert!(texts(&mut rx).is_empty());
            registry::unregister(conn.id());
        }

        #[tokio::test]
        async fn 五百行を超えたら待たずに続けて読む() {
            let _db = crate::testing::refresh_database().await;
            表を作る().await;
            let channel = unique("batch");
            let (conn, mut rx) = member(&channel);
            let mut relay = Relay::new(origin().to_string(), 60);
            relay.step().await;
            let now = crate::database::now();
            let rows: Vec<Vec<(&str, Value)>> = (0..(BATCH + 20))
                .map(|i| {
                    vec![
                        ("kind", Value::Text("publish".into())),
                        ("channel", Value::Text(channel.clone())),
                        ("payload", Value::Text(i.to_string())),
                        ("origin", Value::Text("other".into())),
                        ("created_at", Value::Text(now.clone())),
                    ]
                })
                .collect();
            for chunk in rows.chunks(100) {
                DB::table(TABLE).insert_many(chunk).await.unwrap();
            }
            assert!(relay.step().await, "残りがあるので続けて読む");
            assert_eq!(texts(&mut rx).len(), BATCH);
            assert!(!relay.step().await);
            let rest = texts(&mut rx);
            assert_eq!(rest.len(), 20);
            assert_eq!(rest.first().map(String::as_str), Some("500"), "順番どおり");
            registry::unregister(conn.id());
        }

        #[tokio::test]
        async fn 古い行は掃除する() {
            let _db = crate::testing::refresh_database().await;
            表を作る().await;
            other(None, "c", "old", &ago(120)).await;
            other(None, "c", "new", &crate::database::now()).await;
            let mut relay = Relay::new(origin().to_string(), 60);
            relay.step().await; // settled を決める
            relay.step().await; // 掃除する
            assert_eq!(DB::table(TABLE).count().await.unwrap(), 1);
        }

        #[tokio::test]
        async fn 表が無くても中継は止まらない() {
            let _db = crate::testing::refresh_database().await;
            let mut relay = Relay::new(origin().to_string(), 60);
            assert!(!relay.step().await);
            assert!(relay.last_warn.is_some(), "警告した");
            assert!(relay.settled.is_none());
            表を作る().await;
            relay.step().await;
            assert_eq!(relay.settled, Some(0));
            assert!(relay.last_warn.is_none());
        }
    }
}
