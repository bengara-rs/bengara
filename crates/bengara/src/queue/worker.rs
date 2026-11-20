//! ジョブを取り出して動かす側（`queue:work`）。
//!
//! - **同じジョブを2つのワーカーが同時に取らない**ように、取り出しは 2 文の
//!   比較交換（select → 条件付き update）で行います。
//! - 失敗したら回数を数え、待ち時間を伸ばして戻します。上限を超えたら `failed_jobs` へ移します。
//! - 停止の合図（Ctrl+C）を受けたら、**いま処理中のジョブを終えてから**止まります。
//! - ワーカーが強制終了しても、`retry_after` 秒たてば別のワーカーが取り直します
//!   （Laravel の `retry_after` と同じ考え方）。
//! - 1 本のジョブは `timeout` 秒で打ち切ります（[`run_one`]）。ジョブごとの値は
//!   `config/queue.rs`（[`QueueConfig`](super::QueueConfig)）で決められます。
//! - 打ち切りの時間が `retry_after` より長いジョブは、予約もその分だけ延ばします
//!   （[`reserved_until`]）。動いている途中に取り直されて二重に動くことはありません。
//! - DB の一時的なエラーでは、すぐには終わりません。短い待ちを挟んで
//!   数回やり直します（[`RESERVE_RETRIES`]、[`RECORD_RETRIES`]）。

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::{Job, DEFAULT_QUEUE, FAILED_TABLE, TABLE};
use crate::database::{QueryBuilder, Row, Value, DB};
use crate::error::Result;
use crate::support::time;

/// `queue:work` に渡せる指定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    /// 処理するキューの名前。
    pub queue: String,
    /// 失敗を許す回数。
    pub tries: u32,
    /// 空のときに待つ秒数。
    pub sleep: u64,
    /// 1 本だけ処理して終わるか。
    pub once: bool,
    /// 処理中のまま放置されたジョブを取り直すまでの秒数。
    ///
    /// 打ち切りの時間（`timeout`）がこれより長いジョブは、予約をその分だけ
    /// 延ばします（[`reserved_until`]）。打ち切らない（`0`）ジョブだけは延ばせないので、
    /// この秒数を過ぎると、まだ動いていても別のワーカーが取り直します。
    pub retry_after: i64,
    /// 1 本のジョブを待つ上限（秒）。**0 は無制限です。**
    ///
    /// 超えたらジョブのタスクを打ち切って、失敗として扱います。
    /// `timeouts` に載っているジョブは、そちらを使います。
    pub timeout: u64,
    /// ジョブごとの打ち切りの時間（秒）。`config/queue.rs` の [`QueueConfig`](super::QueueConfig) から来ます。
    pub timeouts: Vec<(String, u64)>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            queue: DEFAULT_QUEUE.to_string(),
            tries: 3,
            sleep: 1,
            once: false,
            retry_after: DEFAULT_RETRY_AFTER,
            timeout: DEFAULT_TIMEOUT,
            timeouts: Vec::new(),
        }
    }
}

impl Options {
    /// そのジョブを待つ上限（秒）と、どこで決めたか。
    ///
    /// `config/queue.rs` に載っていればそれ（最初の 1 つ）、無ければ `--timeout` です。
    fn timeout_for(&self, job: &str) -> (u64, TimeoutSource) {
        match self.timeouts.iter().find(|(name, _)| name == job) {
            Some((_, secs)) => (*secs, TimeoutSource::Config),
            None => (self.timeout, TimeoutSource::Flag),
        }
    }
}

/// 打ち切りの時間をどこで決めたか。打ち切ったときの説明文に入れます。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimeoutSource {
    /// `queue:work --timeout`（既定 60 秒）。
    Flag,
    /// `config/queue.rs` の `QueueConfig::timeout`。
    Config,
}

impl TimeoutSource {
    fn label(self) -> &'static str {
        match self {
            Self::Flag => "--timeout",
            Self::Config => "config/queue.rs",
        }
    }
}

/// 打ち切りの時間が `retry_after` に足りないときに、予約へ足す余裕（秒）。
///
/// 打ち切った後の後始末（[`ABORT_GRACE`] と結果の記録）の時間です。
/// 予約が切れるのは「打ち切りの時間 + これ」と `retry_after` の遅いほうです。
const RECLAIM_MARGIN_SECS: i64 = 30;

/// 予約の目印（`reserved_at`）に書く時刻。
///
/// 取り直しの判定は `reserved_at <= now - retry_after` です（[`reclaim_cutoff`]）。
/// 打ち切りの時間が `retry_after` より長いジョブにいまの時刻を書くと、動いている
/// 途中で取り直されて二重に動きます。そこで、その分だけ**先の時刻**を書きます。
/// 判定の式は変えないので、どのワーカーも同じ表で同じように見分けられます。
///
/// | 打ち切りの時間 | 書く時刻 | 取り直されるのは |
/// |---|---|---|
/// | `retry_after - 30` 秒以下 | いま | `retry_after` 秒後 |
/// | それより長い | いま + (打ち切り + 30 - `retry_after`) | 打ち切り + 30 秒後 |
/// | 0（打ち切らない） | いま | `retry_after` 秒後（延ばせない） |
///
/// 時刻は `9999-12-31 23:59:59` で止めます（文字の比較が逆転しないように）。
pub(crate) fn reserved_until(now: i64, retry_after: i64, timeout_secs: u64) -> String {
    let timeout = i64::try_from(timeout_secs).unwrap_or(i64::MAX);
    let extra = if timeout == 0 {
        0
    } else {
        timeout
            .saturating_add(RECLAIM_MARGIN_SECS)
            .saturating_sub(retry_after.max(1))
            .max(0)
    };
    super::available_at(now, extra.unsigned_abs())
}

/// 予約を取り直すまでの既定の秒数。Laravel の `retry_after` に当たります。
pub(crate) const DEFAULT_RETRY_AFTER: i64 = 90;

/// 1 本のジョブを待つ上限の既定（秒）。Laravel の `--timeout` と同じ 60 秒です。
pub(crate) const DEFAULT_TIMEOUT: u64 = 60;

/// 秒数を「待つ上限」にする。0 は無制限（`None`）。
fn job_timeout(secs: u64) -> Option<Duration> {
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// 打ち切りの時間（`--timeout` と `config/queue.rs`）を確かめ、気になるものを警告文にする。
///
/// どれも起動は止めません。ワーカーを止めるほどの誤りではないためです。
///
/// | 見るもの | 警告する理由 |
/// |---|---|
/// | `--timeout 0`（打ち切らない） | 予約を延ばせないので、`--retry-after` を過ぎると二重に動く |
/// | 設定の `app/Jobs/` に無い名前 | 書き間違いだと、そのジョブは `--timeout` のまま動く |
/// | 設定の同じ名前が 2 回 | 使うのは最初の 1 つだけ |
/// | 設定の `0`（打ち切らない） | `--timeout 0` と同じ |
///
/// `--timeout` と `--retry-after` の大小は、もう警告しません。打ち切りの時間が
/// 長いジョブは予約を延ばすので（[`reserved_until`]）、二重には動きません。
fn timeout_warnings(options: &Options, jobs: &[Job]) -> Vec<String> {
    let mut warnings = Vec::new();
    if options.timeout == 0 {
        warnings.push(format!(
            "警告: --timeout 0 は打ち切りません。予約を延ばせないので、\
             --retry-after（{} 秒）を過ぎても動いているジョブは、\
             別のワーカーが取り直して二重に動きます",
            options.retry_after
        ));
    }
    for (index, (name, secs)) in options.timeouts.iter().enumerate() {
        if options.timeouts[..index]
            .iter()
            .any(|(earlier, _)| earlier == name)
        {
            warnings.push(format!(
                "警告: config/queue.rs に `{name}` の打ち切りが 2 回あります。最初の 1 つを使います"
            ));
            continue;
        }
        if !jobs.iter().any(|job| job.name == name) {
            warnings.push(format!(
                "警告: config/queue.rs の `{name}` というジョブは app/Jobs/ にありません\
                 （名前はファイル名です。書き間違いなら、そのジョブは --timeout で打ち切られます）"
            ));
        }
        if *secs == 0 {
            warnings.push(format!(
                "警告: config/queue.rs の `{name}` は打ち切りません（0）。\
                 予約を延ばせないので、--retry-after（{} 秒）を過ぎても動いていると、\
                 別のワーカーが同じジョブを取り直して二重に動きます",
                options.retry_after
            ));
        }
    }
    warnings
}

/// 失敗の回数から次に試すまでの待ち時間（秒）を決める。
///
/// 1 回目 10 秒、2 回目 20 秒、3 回目 40 秒…と伸ばします。
/// すぐ試し直すと、同じ理由で失敗し続けて DB を叩くだけになるためです。
///
/// `attempts` は `jobs.attempts` の実値なので、負の値も来ます
/// （アプリが `DB::table("jobs").update(..)` で書けます）。`clamp` で
/// 0〜6 に収め、ずらし幅が負になることを防ぎます。
pub(crate) fn backoff_secs(attempts: i64) -> i64 {
    let shift = attempts.clamp(0, 6);
    10 * (1_i64 << shift) / 2
}

/// 予約を取り直す境目の時刻。
///
/// これ以前に予約された（`reserved_at <= cutoff`）ものは、持ち主が死んだと見なします。
pub(crate) fn reclaim_cutoff(now: i64, retry_after: i64) -> String {
    time::format_timestamp(now - retry_after.max(1))
}

/// 取り直したあとの失敗回数。
///
/// 目印が残っていた（`reserved_at` がある）ときだけ +1 します。同じジョブを
/// 永久に回し続けず、`tries` に達したら諦められるようにするためです。
///
/// 値は `jobs.attempts` の実値（`i64`）です。丸めないので負の値も来ます。
/// あふれないよう飽和加算にします。
pub(crate) fn next_attempts(reserved_at: Option<&str>, attempts: i64) -> i64 {
    if reserved_at.is_some() {
        attempts.saturating_add(1)
    } else {
        attempts
    }
}

/// 取り直してよい行だけに絞る条件。
///
/// **`reserve` の 2 つの文が同じものを使います。** 片方だけ直すと、
/// 候補の選び方と更新の条件がずれて、同じジョブを 2 つのワーカーが取ります。
///
/// 時刻は `YYYY-MM-DD HH:MM:SS` なので、文字の大小が時刻の前後と一致します。
fn reclaimable(query: QueryBuilder, cutoff: &str) -> QueryBuilder {
    let cutoff = cutoff.to_string();
    query.where_group(move |q| {
        q.where_null("reserved_at")
            .or_where_op("reserved_at", "<=", cutoff)
    })
}

/// いま取れるジョブだけに絞る条件（`reserve` の 1 文目）。
fn reservable(query: QueryBuilder, queue: &str, now: &str, cutoff: &str) -> QueryBuilder {
    reclaimable(
        query
            .where_("queue", queue)
            .where_op("available_at", "<=", now.to_string()),
        cutoff,
    )
}

/// **自分が予約したままの行**だけに絞る条件。
///
/// `id` だけで絞ると、`retry_after` で取り直された後の行を触ってしまいます。
/// 成功したときは別のワーカーが動かしている行を消し、失敗したときは
/// 予約を外して 3 つ目のワーカーに取らせてしまいました。
fn mine(query: QueryBuilder, reserved: &Reserved) -> QueryBuilder {
    query
        .where_("id", reserved.id)
        .where_("reserved_at", reserved.reserved_at.clone())
}

/// 自分の予約ではなくなっていたことを知らせる。
///
/// `retried` は記録のやり直し（[`with_retries`] の 2 回目以降）かどうかです。
/// やり直しで 0 件なら、**前回の記録が実は通っていた**ことがあります
/// （返事だけが届かなかった）。そこで「`retry_after` より長くかかった」とは言いません。
fn warn_not_mine(id: i64, retried: bool) {
    tracing::warn!("{}", not_mine_message(id, retried));
}

/// [`warn_not_mine`] の文。
fn not_mine_message(id: i64, retried: bool) -> String {
    if retried {
        format!(
            "ジョブ #{id} の行は、やり直したときにはもう自分の予約ではありませんでした。\
             前回の記録が通っていた可能性があります（返事だけが届かなかった）。\
             別のワーカーが取り直した場合もあります"
        )
    } else {
        format!(
            "ジョブ #{id} は自分の予約ではなくなっていたので、何もしません（\
             処理に retry_after より長くかかって、別のワーカーが取り直しました。\
             --retry-after を長くしてください）"
        )
    }
}

/// 取り出した 1 件。
struct Reserved {
    id: i64,
    job: String,
    payload: String,
    /// 予約のときに書いた `attempts`。列の実値なので負の値も来ます。
    attempts: i64,
    queue: String,
    /// 予約したときに書いた `reserved_at`。
    ///
    /// `finish` と `fail` の `where` に入れて、**自分の予約のままか**を確かめます。
    reserved_at: String,
}

/// 取り出しの結果。
///
/// **「空だった」と「ほかのワーカーに取られた」を分けます。** 一緒にすると、
/// 競り合いに負けただけで `--sleep` 秒待つことになります。
enum Reservation {
    /// 1 件取れた。
    Taken(Reserved),
    /// 待っているジョブが無かった。
    Empty,
    /// ほかのワーカーが先に取った。待たずに取り直してよい。
    Lost,
}

/// 1 件取り出して、処理中の目印を付ける。
///
/// **トランザクションを使いません。2 文の比較交換（compare-and-set）です。**
///
/// 1. `select` で候補を 1 件取る。
/// 2. `update ... where id = ? and <1 文目と同じ条件> and attempts = <1 文目で見た値>`
///    を投げ、0 件なら負けたと見なす（[`Reservation::Lost`]、[`claim_row`]）。
///
/// 2 つのワーカーが同じ行を見ても、更新が通るのは 1 つだけです。
///
/// トランザクションの中で読んでから書くと、WAL では読み取りのスナップショットを
/// 持ったまま書き込みへ昇格します。これは `SQLITE_BUSY_SNAPSHOT` になり、
/// `busy_timeout` の再試行対象ではないので、ワーカーが丸ごと終了しました。
///
/// 目印（`reserved_at`）が `retry_after` 秒より古いものも取り直します。
/// ワーカーが強制終了・電源断で死ぬと目印が残ったままになるためです。
/// 取り直したときは `attempts` を +1 して、同じジョブを永久に回し続けないようにします。
///
/// **`tries` に達した行は予約せずに `failed_jobs` へ移します。** 諦める判定が
/// [`fail`] だけだったころ、ワーカープロセスごと落とすジョブ（OOM kill・
/// SIGKILL）は `fail` を通らず、`attempts` が `tries` を超えてもずっと
/// 回収され続けました。`failed_jobs` に入らないので `queue:failed` にも出ず、
/// 取り出しは `order_by("id") limit 1` なので、そのジョブ 1 件でキュー全体が
/// 止まりました。
///
/// 予約の目印に書く時刻は、ジョブの打ち切りの時間で決めます（[`reserved_until`]）。
async fn reserve(options: &Options) -> Result<Reservation> {
    let (queue, retry_after, tries) = (options.queue.as_str(), options.retry_after, options.tries);
    let now_secs = time::now_seconds();
    let now = time::format_timestamp(now_secs);
    let cutoff = reclaim_cutoff(now_secs, retry_after);

    let row = reservable(DB::table(TABLE), queue, &now, &cutoff)
        .order_by("id")
        .limit(1)
        .get()
        .await?
        .into_iter()
        .next();

    let Some(row) = row else {
        return Ok(Reservation::Empty);
    };

    let id: i64 = row.get("id")?;
    let reserved_at: Option<String> = row.get("reserved_at")?;
    let was_stale = reserved_at.is_some();
    // **丸めません。** 2 文目は列の実値と突き合わせるので、`max(0)` で丸めると
    // 負の `attempts` が入った行で `where attempts = ?` が永久に 0 件になり、
    // `Reservation::Lost` を待たずに繰り返して全力ループになりました。
    let done = row.get::<i64>("attempts")?;
    let attempts = next_attempts(reserved_at.as_deref(), done);

    // 回収しようとした行が、もう `tries` ぶん試されていたら諦める。
    // ここで止めないと、ワーカーごと落とすジョブが永久に回り続けます。
    // `Options.tries == 0` は `commands.rs` が弾くので、無制限はありません。
    //
    // 見るのは**数え直した `attempts`** です。列から読んだ `done` で見ていた
    // ころは、落ちた進行中の 1 回が数えられないまま 1 回ぶん多く動きました
    //（`--tries 3` で 4 回走り、`failed_jobs` の文面だけ「3 回」と出る）。
    if let (true, Some(reserved_at)) = (attempts >= i64::from(tries), reserved_at.as_deref()) {
        give_up_stale(&row, id, reserved_at, attempts).await?;
        // 自分は予約していないので、待たずに次の 1 件を見る。
        return Ok(Reservation::Lost);
    }

    let job: String = row.get("job")?;
    let (timeout_secs, _) = options.timeout_for(&job);
    let marker = reserved_until(now_secs, retry_after, timeout_secs);
    let affected = claim_row(queue, id, done, &now, &cutoff, attempts, &marker).await?;

    if affected == 0 {
        // ほかのワーカーが先に取った。待たずに取り直す。
        return Ok(Reservation::Lost);
    }

    if was_stale {
        tracing::warn!(
            "ジョブ #{id} は予約の期限を過ぎても処理中のままだったので取り直します（{attempts} 回目）"
        );
    }

    Ok(Reservation::Taken(Reserved {
        id,
        job,
        payload: row.get("payload")?,
        attempts,
        queue: row.get("queue")?,
        reserved_at: marker,
    }))
}

/// `reserve` の 2 文目。**1 文目で見たとおりの行だけ**を予約する。
///
/// `id` と取り直しの条件（[`reclaimable`]）だけで絞っていたころは、
/// 1 文目と 2 文目の間に別のワーカーが同じ行を取って**速く失敗**すると、
/// こちらの更新が通ってしまいました。[`fail`] は `reserved_at` を外して
/// `attempts` を増やし、`available_at` を先へ送ります。それを知らないまま
/// 上書きするので、(a) 待ち時間を無視して即再実行し、(b) `attempts` を
/// 1 文目で見た古い値に巻き戻して `--tries` より多く試していました。
///
/// 1 文目と同じ条件（[`reservable`]）に `attempts` の一致を足し、本当の
/// 比較交換にします。どれか 1 つでも変わっていれば 0 件になり、負けた側は
/// 待たずに取り直します。
/// `seen_attempts` は**列から読んだ値そのまま**です。丸めた値を渡すと、
/// 条件と実値がずれて永久に 0 件になります。
///
/// `marker` は予約の目印に書く時刻です（[`reserved_until`]）。打ち切りの時間が
/// 長いジョブでは、いまより先の時刻になります。
async fn claim_row(
    queue: &str,
    id: i64,
    seen_attempts: i64,
    now: &str,
    cutoff: &str,
    attempts: i64,
    marker: &str,
) -> Result<u64> {
    reservable(DB::table(TABLE).where_("id", id), queue, now, cutoff)
        .where_("attempts", seen_attempts)
        .update(&[
            ("reserved_at", Value::Text(marker.to_string())),
            ("attempts", Value::Int(attempts)),
        ])
        .await
}

/// 回収しようとした行を、予約せずに `failed_jobs` へ移す。
///
/// ワーカープロセスごと落とすジョブ（OOM kill・SIGKILL・`panic = "abort"`）は
/// [`fail`] を通りません。以前はこの形で `attempts` が `tries` を超えても
/// 回収され続け、`queue:failed` にも出ないまま、そのジョブ 1 件でキュー全体が
/// 止まりました。
///
/// `fail` と同じ「**先に消して、1 件だった側だけ入れる**」形です。0 件なら
/// 別のワーカーが先に動かしたので、`failed_jobs` にも入れません。
async fn give_up_stale(row: &Row, id: i64, reserved_at: &str, attempts: i64) -> Result<()> {
    let error = format!(
        "{attempts} 回試しましたが、ワーカーが応答しなくなったため諦めました\
         （処理の途中でワーカープロセスが落ちています）"
    );
    // 入れるのと消すのを 1 つのトランザクションにする。
    let tx = DB::begin().await?;
    let affected = tx
        .table(TABLE)
        .where_("id", id)
        .where_("reserved_at", reserved_at.to_string())
        .delete()
        .await?;
    if affected == 0 {
        tx.commit().await?;
        return Ok(());
    }
    tx.table(FAILED_TABLE)
        .insert(&[
            ("queue", Value::Text(row.get("queue")?)),
            ("job", Value::Text(row.get("job")?)),
            ("payload", Value::Text(row.get("payload")?)),
            ("error", Value::Text(error)),
            ("failed_at", Value::Text(crate::database::now())),
        ])
        .await?;
    tx.commit().await?;
    tracing::warn!(
        "ジョブ #{id} は {attempts} 回で諦めました（ワーカーが応答しなくなりました）。\
         queue:failed で見られます"
    );
    Ok(())
}

/// 成功したので消す。
///
/// **自分が予約したままの行だけ**を消します。`id` だけで消すと、
/// `retry_after` で取り直した別のワーカーが動かしている行を消してしまい、
/// そのワーカーの失敗が `failed_jobs` にも残りません。
///
/// `retried` は記録のやり直しかどうか（[`warn_not_mine`] の文を選ぶため）。
async fn finish(reserved: &Reserved, retried: bool) -> Result<()> {
    let affected = mine(DB::table(TABLE), reserved).delete().await?;
    if affected == 0 {
        warn_not_mine(reserved.id, retried);
    }
    Ok(())
}

/// 失敗したので戻す。上限を超えていたら諦める。
///
/// こちらも**自分が予約したままの行だけ**を触ります。`reserved_at` を
/// 戻すだけだと、まだ動いている別のワーカーがいるのに 3 つ目のワーカーが
/// 同じジョブを取れてしまいます。
async fn fail(reserved: &Reserved, tries: u32, error: &str, retried: bool) -> Result<bool> {
    let attempts = reserved.attempts.saturating_add(1);
    if attempts >= i64::from(tries) {
        give_up(reserved, error, retried).await?;
        return Ok(true);
    }

    let next = time::format_timestamp(time::now_seconds() + backoff_secs(attempts));
    let affected = mine(DB::table(TABLE), reserved)
        .update(&[
            ("attempts", Value::Int(attempts)),
            ("available_at", Value::Text(next)),
            ("reserved_at", Value::Null),
        ])
        .await?;
    if affected == 0 {
        warn_not_mine(reserved.id, retried);
    }
    Ok(false)
}

/// 諦めて `failed_jobs` へ移す（[`fail`] の上限到達と、名前の無いジョブで使う）。
///
/// 入れるのと消すのを 1 つのトランザクションにします。
/// 分けると、間でプロセスが死んだときに両方の表に残って再実行されます。
///
/// **先に消します。** 0 件なら自分の予約ではないので、
/// `failed_jobs` にも入れません（別のワーカーの結果を踏まない）。
async fn give_up(reserved: &Reserved, error: &str, retried: bool) -> Result<()> {
    let tx = DB::begin().await?;
    let affected = mine(tx.table(TABLE), reserved).delete().await?;
    if affected == 0 {
        tx.commit().await?;
        warn_not_mine(reserved.id, retried);
        return Ok(());
    }
    tx.table(FAILED_TABLE)
        .insert(&[
            ("queue", Value::Text(reserved.queue.clone())),
            ("job", Value::Text(reserved.job.clone())),
            ("payload", Value::Text(reserved.payload.clone())),
            ("error", Value::Text(error.to_string())),
            ("failed_at", Value::Text(crate::database::now())),
        ])
        .await?;
    tx.commit().await
}

/// 名前の無いジョブの説明文。
fn unknown_job_message(name: &str) -> String {
    format!("`{name}` というジョブはありません（app/Jobs/ を確かめてください）")
}

/// 打ち切ったときの説明文。どこで決めた時間かも入れます。
fn timed_out_message(limit: Duration, source: TimeoutSource) -> String {
    format!(
        "{} 秒で終わらなかったので打ち切りました（{}）",
        limit.as_secs(),
        source.label()
    )
}

/// 止まらないジョブを見つけたときの説明文（ワーカーを終えるときに使う）。
fn stuck_message(job: &str, id: i64, limit: Option<Duration>) -> String {
    format!(
        "{job} #{id} が {} 秒で終わらず、打ち切っても止まりませんでした\
         （CPU を回し続けているか、同期の入出力で止まっています）。\
         同じジョブが二重に動かないよう、ワーカーを終了します。\
         supervisor などで自動で起動し直してください",
        limit.map_or(0, |l| l.as_secs())
    )
}

/// 打ち切った後、タスクが本当に止まったかを見るまでの待ち。
///
/// `abort` は次の `.await` で効くので、待ちで固まったジョブはすぐ止まります。
/// この間に止まらなければ、CPU を回し続けているか、同期の入出力で
/// 止まっていると見なします。
const ABORT_GRACE: Duration = Duration::from_millis(500);

/// [`run_one`] の失敗。
#[derive(Debug)]
struct Failure {
    /// 失敗の理由（`failed_jobs` と画面に出す文）。
    message: String,
    /// 打ち切っても止まらなかったか。
    ///
    /// 真のときは、失敗を記録したあとワーカーを終えます（[`stuck_message`]）。
    stuck: bool,
}

impl Failure {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            stuck: false,
        }
    }
}

/// ジョブを 1 件動かす。パニックと時間切れも失敗として扱います。
///
/// `timeout` を超えたら、ジョブのタスクを打ち切って（`abort`）失敗にします。
/// 以前は上限が無く、固まったジョブ 1 本でワーカーが止まり続けました。
/// `retry_after` 後に別のワーカーが同じジョブを取り直すので、二重にも動きました。
///
/// **止められるのは「待ち」で固まったジョブだけです。** `abort` は次の
/// `.await` で効きます。`.await` を挟まずに CPU を回し続ける処理や、
/// 同期の入出力で止まっている処理は打ち切れません。
///
/// 打ち切ったあと [`ABORT_GRACE`] だけ様子を見ます。止まらなければ
/// [`Failure::stuck`] を立てて返し、呼ぶ側は失敗を記録してから**ワーカーを
/// 終えます。** 以前は止まらないまま次のジョブへ進んでいました。失敗は
/// 10 秒後に再挑戦されるので、止まらない 1 本目と取り直した 2 本目が
/// **同時に動きました。** Laravel も時間切れではワーカーのプロセスを
/// 終わらせます。プロセスが終われば、止まらないスレッドも一緒に消えます。
async fn run_one(
    jobs: &[Job],
    reserved: &Reserved,
    timeout: Option<Duration>,
    source: TimeoutSource,
) -> std::result::Result<(), Failure> {
    run_one_with(jobs, reserved, timeout, source, ABORT_GRACE).await
}

/// [`run_one`] の中身。様子を見る時間を引数で受け取ります（テストで短くするため）。
async fn run_one_with(
    jobs: &[Job],
    reserved: &Reserved,
    timeout: Option<Duration>,
    source: TimeoutSource,
    grace: Duration,
) -> std::result::Result<(), Failure> {
    let Some(job) = jobs.iter().find(|j| j.name == reserved.job) else {
        return Err(Failure::new(unknown_job_message(&reserved.job)));
    };

    let handle = job.handle;
    let payload = reserved.payload.clone();
    // パニックでワーカーを落とさない（ハンドラと同じ考え方。決定記録 #008）。
    let mut task = tokio::spawn(async move { handle(payload).await });
    let joined = match timeout {
        None => task.await,
        Some(limit) => match tokio::time::timeout(limit, &mut task).await {
            Ok(joined) => joined,
            Err(_) => {
                task.abort();
                // 止まったかを確かめる。待ちで固まったものはここで終わる。
                // 止まらないものの終わりは**待ちません**（待つとワーカーごと固まる）。
                let stopped = tokio::time::timeout(grace, &mut task).await.is_ok();
                return Err(Failure {
                    message: timed_out_message(limit, source),
                    stuck: !stopped,
                });
            }
        },
    };
    match joined {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(Failure::new(e.to_string())),
        Err(e) if e.is_panic() => Err(Failure::new("ジョブがパニックしました")),
        Err(e) => Err(Failure::new(format!("ジョブを動かせませんでした: {e}"))),
    }
}

/// 続けて競り合いに負けてよい回数。
///
/// [`Reservation::Lost`] は普通ならすぐ解消するので待ちません。ですが
/// 2 文目の条件が**永久に合わない**行が先頭にあると、同じ行を選び直して
/// 全力で回り続けます（CPU を使い切り、キューは 1 件も進みません）。
/// 続けてこの回数負けたら `--sleep` の待ちに落として歯止めにします。
const MAX_CONSECUTIVE_LOST: u32 = 10;

/// 続けて負けた回数を数え、待ちに落ちるべきかを返す。
///
/// 落ちると決めたら数えた回数を 0 に戻します。待った後は、また
/// [`MAX_CONSECUTIVE_LOST`] 回まで待たずに取り直せます。
fn back_off_after_losses(lost: &mut u32) -> bool {
    *lost += 1;
    if *lost >= MAX_CONSECUTIVE_LOST {
        *lost = 0;
        return true;
    }
    false
}

/// `--sleep` 秒待つ。ここで止まらないよう、細かく区切って合図を見る。
async fn idle_sleep(sleep: u64, stop: &AtomicBool) {
    pause(Duration::from_secs(sleep.max(1)), stop).await;
}

/// 決めた時間だけ待つ。**停止の合図が来たら、そこで切り上げます。**
async fn pause(wait: Duration, stop: &AtomicBool) {
    let step = Duration::from_millis(100);
    let mut left = wait;
    while !left.is_zero() && !stop.load(Ordering::Relaxed) {
        let slice = left.min(step);
        tokio::time::sleep(slice).await;
        left -= slice;
    }
}

/// 取り出し（`reserve`）が続けて失敗したとき、やり直す回数。
///
/// 以前は DB のエラー 1 回でワーカーが終わりました。DB の再起動や、つなぎ直しの
/// 一瞬の失敗でも止まるので、外で上げ直す仕組みが要りました（Windows では
/// 用意しにくい）。待ちは 1・2・4・8・10 秒で、合わせて 25 秒です。
/// それでも直らなければ、今までどおりエラーで終わります。
const RESERVE_RETRIES: u32 = 5;

/// 結果の記録（`finish` / `fail` / `give_up`）が失敗したとき、やり直す回数。
///
/// **取り出しより少なくします。** 待っている間も予約の時計（`retry_after`）は
/// 進むので、長く粘ると別のワーカーが取り直します。待ちは 1・2・4 秒です。
const RECORD_RETRIES: u32 = 3;

/// やり直しの待ちの単位。テストでは短くします。
const RETRY_UNIT: Duration = Duration::from_secs(1);

/// `failures` 回目の失敗の後に待つ時間。単位の 1・2・4・8 倍、上限は 10 倍。
fn retry_wait(failures: u32, unit: Duration) -> Duration {
    let factor = 1u32 << failures.saturating_sub(1).min(4);
    unit * factor.min(10)
}

/// 結果の記録を、失敗したら待ってやり直す。
///
/// `finish` / `fail` / `give_up` はどれも**自分の予約のままの行だけ**を触るので、
/// 2 回流しても害がありません（1 回目が実は通っていたら、2 回目は 0 件です）。
///
/// 停止の合図が来ていたら待ちません。残りの回数をすぐ試して、駄目なら返します。
///
/// `operation` は「やり直しかどうか」を受け取ります（1 回目は `false`）。
/// やり直しで 0 件だったときの警告を、1 回目と分けるためです。
async fn with_retries<T, F, Fut>(
    what: &str,
    stop: &AtomicBool,
    unit: Duration,
    mut operation: F,
) -> Result<T>
where
    F: FnMut(bool) -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut failures = 0u32;
    loop {
        let error = match operation(failures > 0).await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        failures += 1;
        if failures > RECORD_RETRIES {
            return Err(error);
        }
        eprintln!(
            "  {what}を記録できませんでした（やり直します {failures}/{RECORD_RETRIES}）: {error}"
        );
        pause(retry_wait(failures, unit), stop).await;
    }
}

/// `queue:work` の本体。止まるまでジョブを処理し続けます。
pub(crate) async fn run(jobs: &[Job], options: &Options, stop: Arc<AtomicBool>) -> Result<()> {
    run_with(jobs, options, stop, RETRY_UNIT).await
}

/// [`run`] の中身。やり直しの待ちの単位を引数で受け取ります（テストで短くするため）。
async fn run_with(
    jobs: &[Job],
    options: &Options,
    stop: Arc<AtomicBool>,
    retry_unit: Duration,
) -> Result<()> {
    println!(
        "キュー `{}` のジョブを処理します（Ctrl+C で終了）",
        options.queue
    );
    for warning in timeout_warnings(options, jobs) {
        eprintln!("{warning}");
    }

    let mut processed = 0u64;
    // 続けて競り合いに負けた回数。取れたら 0 に戻します。
    let mut lost = 0u32;
    // 続けて取り出しに失敗した回数。成功したら 0 に戻します。
    let mut reserve_failures = 0u32;
    loop {
        if stop.load(Ordering::Relaxed) {
            println!("\n停止の合図を受けました。{processed} 件処理しました。");
            return Ok(());
        }

        let reservation = match reserve(options).await {
            Ok(reservation) => {
                reserve_failures = 0;
                reservation
            }
            // 一時的な DB のエラーで終わらない。待ってやり直す。
            // 待ちの間に停止の合図が来たら、ループの先頭で普通に止まる。
            Err(error) => {
                reserve_failures += 1;
                if reserve_failures > RESERVE_RETRIES {
                    return Err(error);
                }
                let wait = retry_wait(reserve_failures, retry_unit);
                eprintln!(
                    "  ジョブを取り出せませんでした（{} 秒後にやり直します {reserve_failures}/{RESERVE_RETRIES}）: {error}",
                    wait.as_secs()
                );
                pause(wait, &stop).await;
                continue;
            }
        };

        let reserved = match reservation {
            Reservation::Taken(reserved) => {
                lost = 0;
                reserved
            }
            // 競り合いに負けただけなので待たない。ただし負け続けるときは、
            // 条件が永久に合わない行で全力ループになるので待ちに落ちる。
            Reservation::Lost => {
                if back_off_after_losses(&mut lost) {
                    idle_sleep(options.sleep, &stop).await;
                }
                continue;
            }
            Reservation::Empty => {
                lost = 0;
                if options.once {
                    println!("待っているジョブはありません。");
                    return Ok(());
                }
                idle_sleep(options.sleep, &stop).await;
                continue;
            }
        };

        // 名前の無いジョブは、待っても動くようにならない。再挑戦の待ち時間
        //（10 秒・20 秒…）を 3 回ぶん積むだけなので、すぐ `failed_jobs` へ移す。
        // ファイルを置き忘れたまま `queue:retry` で戻せるよう、記録は残す。
        if !jobs.iter().any(|j| j.name == reserved.job) {
            let message = unknown_job_message(&reserved.job);
            with_retries("失敗", &stop, retry_unit, |retried| {
                give_up(&reserved, &message, retried)
            })
            .await?;
            eprintln!(
                "  {} #{} 失敗（諦めました）: {message}",
                reserved.job, reserved.id
            );
            if options.once {
                return Ok(());
            }
            continue;
        }

        // 打ち切りの時間はジョブごと（`config/queue.rs`、無ければ `--timeout`）。
        // 予約の目印も同じ値で延ばしてある（`reserve`）。
        let (timeout_secs, source) = options.timeout_for(&reserved.job);
        let timeout = job_timeout(timeout_secs);
        let started = std::time::Instant::now();
        match run_one(jobs, &reserved, timeout, source).await {
            Ok(()) => {
                let finished = with_retries("成功", &stop, retry_unit, |retried| {
                    finish(&reserved, retried)
                })
                .await;
                if let Err(error) = finished {
                    // ジョブは終わっているのに、行が残った。黙って終わると
                    // 「なぜ 2 回動いたか」が分からないので、ここで言っておく。
                    eprintln!(
                        "  {} #{} は成功しましたが、キューから消せませんでした。\
                         --retry-after（{} 秒）の後にもう 1 回動きます",
                        reserved.job, reserved.id, options.retry_after
                    );
                    return Err(error);
                }
                processed += 1;
                println!(
                    "  {} #{} 成功（{} ms）",
                    reserved.job,
                    reserved.id,
                    started.elapsed().as_millis()
                );
            }
            Err(failure) => {
                let message = failure.message;
                let gave_up = with_retries("失敗", &stop, retry_unit, |retried| {
                    fail(&reserved, options.tries, &message, retried)
                })
                .await?;
                if gave_up {
                    eprintln!(
                        "  {} #{} 失敗（{} 回目で諦めました）: {message}",
                        reserved.job,
                        reserved.id,
                        reserved.attempts.saturating_add(1)
                    );
                } else {
                    eprintln!(
                        "  {} #{} 失敗（{} 回目、{} 秒後に再挑戦）: {message}",
                        reserved.job,
                        reserved.id,
                        reserved.attempts.saturating_add(1),
                        backoff_secs(reserved.attempts.saturating_add(1))
                    );
                }
                // 打ち切っても止まらなかった。動いたまま次へ進むと、再挑戦で
                // 同じジョブが二重に動くので、ワーカーごと終える（[`run_one`]）。
                if failure.stuck {
                    return Err(crate::error::Error::msg(stuck_message(
                        &reserved.job,
                        reserved.id,
                        timeout,
                    )));
                }
            }
        }

        if options.once {
            return Ok(());
        }
    }
}

/// 諦めたジョブの数。
pub(crate) async fn failed_count() -> Result<i64> {
    DB::table(FAILED_TABLE).count().await
}

/// 諦めたジョブをキューへ戻す。
pub(crate) async fn retry_failed(id: Option<i64>) -> Result<u64> {
    let query = match id {
        Some(id) => DB::table(FAILED_TABLE).where_("id", id),
        None => DB::table(FAILED_TABLE),
    };
    let rows = query.get().await?;
    if rows.is_empty() {
        return Ok(0);
    }

    let now = crate::database::now();
    let mut moved = 0;
    // 入れるのと消すのを 1 つのトランザクションにする。
    // 分けると、間でプロセスが死んだときに二重投入になります。
    let tx = DB::begin().await?;
    for row in &rows {
        let failed_id: i64 = row.get("id")?;
        // **先に消します。** 行はトランザクションの外で読んでいるので、
        // `queue:retry` を 2 つ同時に流すと同じ行を両方が見ます。先に入れると
        // `insert` は両方通り、二重投入になります（`delete` だけが 0 件になる）。
        // 消せた側だけが入れ直せば、通るのは 1 つだけです
        //（`finish` / `fail` と同じ「先に消す」考え方）。
        let affected = tx
            .table(FAILED_TABLE)
            .where_("id", failed_id)
            .delete()
            .await?;
        if affected != 1 {
            continue;
        }
        tx.table(TABLE)
            .insert(&[
                ("queue", Value::Text(row.get("queue")?)),
                ("job", Value::Text(row.get("job")?)),
                ("payload", Value::Text(row.get("payload")?)),
                ("attempts", Value::Int(0)),
                ("available_at", Value::Text(now.clone())),
                ("created_at", Value::Text(now.clone())),
            ])
            .await?;
        moved += 1;
    }
    tx.commit().await?;
    Ok(moved)
}

/// 諦めたジョブの一覧（`queue:failed` が使う）。
pub(crate) async fn failed_list() -> Result<Vec<(i64, String, String, String)>> {
    let rows = DB::table(FAILED_TABLE).order_by("id").get().await?;
    rows.iter()
        .map(|row| {
            Ok((
                row.get::<i64>("id")?,
                row.get::<String>("job")?,
                row.get::<String>("failed_at")?,
                row.get::<String>("error")?,
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 待ち時間は回数で伸びる() {
        assert_eq!(backoff_secs(1), 10);
        assert_eq!(backoff_secs(2), 20);
        assert_eq!(backoff_secs(3), 40);
        assert_eq!(backoff_secs(4), 80);
        // 伸ばしすぎないよう上限を付ける。
        assert_eq!(backoff_secs(6), 320);
        assert_eq!(backoff_secs(100), 320);
    }

    #[test]
    fn 既定の指定() {
        let options = Options::default();
        assert_eq!(options.queue, "default");
        assert_eq!(options.tries, 3);
        assert_eq!(options.sleep, 1);
        assert!(!options.once);
        assert_eq!(options.retry_after, 90);
        // Laravel の `--timeout` と同じ 60 秒。
        assert_eq!(options.timeout, 60);
        assert!(options.timeouts.is_empty());
        assert!(timeout_warnings(&options, &[]).is_empty());
    }

    fn job(name: &'static str) -> Job {
        Job {
            name,
            handle: |_| Box::pin(async { Ok(()) }),
        }
    }

    #[test]
    fn ジョブごとの打ち切りは設定が先で_無ければtimeout() {
        let options = Options {
            timeout: 60,
            timeouts: vec![("Long".into(), 1800), ("Long".into(), 5)],
            ..Options::default()
        };
        assert_eq!(
            options.timeout_for("Long"),
            (1800, TimeoutSource::Config),
            "最初の 1 つ"
        );
        assert_eq!(options.timeout_for("Other"), (60, TimeoutSource::Flag));
    }

    #[test]
    fn 設定の打ち切りで気になるものを警告する() {
        let jobs = [job("Long"), job("Forever")];
        let options = Options {
            timeouts: vec![
                ("Long".into(), 1800),
                ("Long".into(), 5),
                ("Typo".into(), 10),
                ("Forever".into(), 0),
            ],
            ..Options::default()
        };
        let warnings = timeout_warnings(&options, &jobs);
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(warnings[0].contains("2 回"), "{}", warnings[0]);
        assert!(warnings[1].contains("Typo"), "{}", warnings[1]);
        assert!(warnings[2].contains("二重"), "{}", warnings[2]);

        // `--timeout` が `--retry-after` より長くても、もう警告しない（予約を延ばすので）。
        let long = Options {
            timeout: 3600,
            retry_after: 90,
            ..Options::default()
        };
        assert!(timeout_warnings(&long, &jobs).is_empty());

        // `--timeout 0` は設定の `0` と同じく、予約を延ばせないことを知らせる。
        let forever = Options {
            timeout: 0,
            ..Options::default()
        };
        let warnings = timeout_warnings(&forever, &jobs);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("--timeout 0"), "{}", warnings[0]);
    }

    #[test]
    fn 打ち切りが長いジョブは予約の目印を先にずらす() {
        let now = 1_000_000;
        let at = |secs| time::format_timestamp(now + secs);
        // retry_after より 30 秒以上短ければ、いまの時刻のまま。
        assert_eq!(reserved_until(now, 90, 60), at(0));
        assert_eq!(reserved_until(now, 90, 1), at(0));
        // 長ければ「打ち切り + 30 秒」で取り直されるようにずらす。
        assert_eq!(reserved_until(now, 90, 61), at(1));
        assert_eq!(reserved_until(now, 90, 1800), at(1800 + 30 - 90));
        // 打ち切らない（0）ジョブは延ばせない。
        assert_eq!(reserved_until(now, 90, 0), at(0));
        // とても大きな値でも年が 5 桁にならない。
        assert_eq!(reserved_until(now, 90, u64::MAX), "9999-12-31 23:59:59");
        // retry_after が 0 以下でも 1 秒として扱う（reclaim_cutoff と同じ）。
        assert_eq!(reserved_until(now, 0, 10), at(10 + 30 - 1));
    }

    #[test]
    fn ずらした目印は打ち切りの時間が過ぎるまで取り直されない() {
        // 取り直しの判定は `reserved_at <= reclaim_cutoff(now, retry_after)`。
        let reserved_at = 1_000_000;
        let marker = reserved_until(reserved_at, 90, 1800);
        let reclaimable = |elapsed: i64| marker <= reclaim_cutoff(reserved_at + elapsed, 90);
        assert!(!reclaimable(90), "retry_after では取り直さない");
        assert!(!reclaimable(1800), "打ち切りの時間ちょうどでもまだ");
        assert!(reclaimable(1800 + 30), "打ち切り + 30 秒で取り直す");
    }

    #[test]
    fn timeoutの0は無制限() {
        assert_eq!(job_timeout(0), None);
        assert_eq!(job_timeout(60), Some(Duration::from_secs(60)));
    }

    #[test]
    fn やり直しの待ちは伸びて止まる() {
        let unit = Duration::from_secs(1);
        let waits: Vec<u64> = (1..=6).map(|n| retry_wait(n, unit).as_secs()).collect();
        assert_eq!(waits, vec![1, 2, 4, 8, 10, 10]);
        // 0 回目やとても大きい回数でもあふれない。
        assert_eq!(retry_wait(0, unit).as_secs(), 1);
        assert_eq!(retry_wait(u32::MAX, unit).as_secs(), 10);
        // 取り出しのやり直しは、合わせて 25 秒まで。
        let total: u64 = (1..=RESERVE_RETRIES)
            .map(|n| retry_wait(n, unit).as_secs())
            .sum();
        assert_eq!(total, 25);
    }

    /// テスト用の予約 1 件。
    fn reserved_of(job: &str) -> Reserved {
        Reserved {
            id: 1,
            job: job.into(),
            payload: "{}".into(),
            attempts: 0,
            queue: DEFAULT_QUEUE.into(),
            reserved_at: "2024-01-01 00:00:00".into(),
        }
    }

    #[tokio::test]
    async fn 固まったジョブは時間切れで打ち切る() {
        use std::sync::atomic::AtomicUsize;

        // 待ちの後まで進んだ回数。打ち切れていれば 0 のまま。
        static FINISHED: AtomicUsize = AtomicUsize::new(0);
        let jobs = [
            Job {
                name: "Stuck",
                handle: |_| {
                    Box::pin(async {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        FINISHED.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })
                },
            },
            Job {
                name: "Quick",
                handle: |_| Box::pin(async { Ok(()) }),
            },
        ];

        // 以前は上限が無く、ジョブが返るまでワーカーが止まり続けた。
        let started = std::time::Instant::now();
        let failure = run_one(
            &jobs,
            &reserved_of("Stuck"),
            Some(Duration::from_millis(30)),
            TimeoutSource::Flag,
        )
        .await
        .unwrap_err();
        // 待ちで固まったものは打ち切れば止まる。ワーカーは続けてよい。
        assert!(!failure.stuck, "止まったのでワーカーは続ける");
        let error = failure.message;
        assert!(error.contains("打ち切りました"), "{error}");
        assert!(error.contains("--timeout"), "{error}");
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "ジョブの終わりを待たない"
        );

        // タスクは打ち切られているので、待ちの後ろは動かない。
        tokio::time::sleep(Duration::from_millis(450)).await;
        assert_eq!(
            FINISHED.load(Ordering::SeqCst),
            0,
            "打ち切ったら続きは動かない"
        );

        // 上限の中で終わるジョブと、無制限は今までどおり。
        run_one(
            &jobs,
            &reserved_of("Quick"),
            Some(Duration::from_secs(5)),
            TimeoutSource::Flag,
        )
        .await
        .unwrap();
        run_one(&jobs, &reserved_of("Quick"), None, TimeoutSource::Flag)
            .await
            .unwrap();

        // 設定で決めた時間で打ち切ったときは、そう書く。
        let failure = run_one(
            &jobs,
            &reserved_of("Stuck"),
            Some(Duration::from_millis(30)),
            TimeoutSource::Config,
        )
        .await
        .unwrap_err();
        assert!(
            failure.message.contains("config/queue.rs"),
            "{}",
            failure.message
        );
    }

    /// `.await` を挟まずにスレッドをふさぐジョブ。`abort` では止められない。
    fn block_thread(_payload: String) -> crate::queue::JobFuture {
        Box::pin(async {
            std::thread::sleep(Duration::from_millis(600));
            Ok(())
        })
    }

    // 止まらないジョブがスレッドを 1 本ふさいでも、時計を進められるよう
    // スレッドを 2 本にする。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 打ち切っても止まらないジョブは印を付けて返す() {
        let jobs = [Job {
            name: "Blocking",
            handle: block_thread,
        }];
        let started = std::time::Instant::now();
        let failure = run_one_with(
            &jobs,
            &reserved_of("Blocking"),
            Some(Duration::from_millis(30)),
            TimeoutSource::Flag,
            Duration::from_millis(50),
        )
        .await
        .unwrap_err();
        // 以前は止まらないまま次のジョブへ進み、再挑戦で二重に動いた。
        assert!(failure.stuck, "止まらなかったと分かる");
        assert!(
            failure.message.contains("打ち切りました"),
            "{}",
            failure.message
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "ジョブの終わりは待たない"
        );

        let message = stuck_message("Blocking", 7, Some(Duration::from_secs(60)));
        assert!(message.contains("Blocking #7"), "{message}");
        assert!(message.contains("ワーカーを終了します"), "{message}");
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 止まらないジョブは失敗を記録してからワーカーを終える() {
        let _db = crate::testing::refresh_database().await;
        表を作る().await;
        let id = 待っているジョブを入れる("Blocking").await;

        let jobs = [Job {
            name: "Blocking",
            handle: |_| {
                Box::pin(async {
                    std::thread::sleep(Duration::from_millis(2500));
                    Ok(())
                })
            },
        }];
        let stop = Arc::new(AtomicBool::new(false));
        let options = Options {
            timeout: 1,
            ..Options::default()
        };
        // `once` でなくても、止まらないジョブを見たらエラーで終わる。
        let error = run(&jobs, &options, stop).await.unwrap_err().to_string();
        assert!(error.contains("ワーカーを終了します"), "{error}");

        // 失敗 1 回として記録してから終わる（今までの時間切れと同じ記録）。
        let row = DB::table(TABLE).find(id).await.unwrap().unwrap();
        assert_eq!(row.get::<i64>("attempts").unwrap(), 1);
        assert!(row.value("reserved_at").unwrap().is_null(), "予約を外す");
    }

    #[tokio::test]
    async fn 記録の失敗は数回やり直す() {
        use std::sync::atomic::AtomicU32;

        let stop = AtomicBool::new(false);
        let unit = Duration::from_millis(1);

        // 2 回失敗して 3 回目に通る。以前は 1 回目でワーカーが終わった。
        let calls = AtomicU32::new(0);
        let value = with_retries("成功", &stop, unit, |_| async {
            if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                Err(crate::error::Error::msg("一時的な失敗"))
            } else {
                Ok(7)
            }
        })
        .await
        .unwrap();
        assert_eq!(value, 7);
        assert_eq!(calls.load(Ordering::SeqCst), 3);

        // 失敗し続けるなら、決めた回数で諦めて元のエラーを返す。
        let calls = AtomicU32::new(0);
        let error = with_retries("成功", &stop, unit, |_| async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err::<(), _>(crate::error::Error::msg("直らない失敗"))
        })
        .await
        .unwrap_err();
        assert!(error.to_string().contains("直らない失敗"));
        assert_eq!(calls.load(Ordering::SeqCst), 1 + RECORD_RETRIES);
    }

    #[tokio::test]
    async fn やり直しかどうかを操作に渡す() {
        use std::sync::atomic::AtomicU32;

        let stop = AtomicBool::new(false);
        let calls = AtomicU32::new(0);
        let seen = std::sync::Mutex::new(Vec::new());
        with_retries("成功", &stop, Duration::from_millis(1), |retried| {
            seen.lock().unwrap().push(retried);
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if n < 2 {
                    Err(crate::error::Error::msg("一時的な失敗"))
                } else {
                    Ok(())
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![false, true, true]);
    }

    #[test]
    fn やり直しで自分の行が無いときは別の文にする() {
        // 1 回目の 0 件は、取り直されたとき。retry_after を案内する。
        let first = not_mine_message(7, false);
        assert!(first.contains("--retry-after"), "{first}");
        // やり直しの 0 件は、前回の記録が通っていたことがある。
        // 「retry_after より長くかかった」と言うと誤った案内になる。
        let again = not_mine_message(7, true);
        assert!(again.contains("前回の記録が通っていた可能性"), "{again}");
        assert!(!again.contains("retry_after より長く"), "{again}");
    }

    #[tokio::test]
    async fn 停止の合図が来ていたら待たない() {
        // 待ちの単位を長くしても、合図が立っていればすぐ返る。
        let stop = AtomicBool::new(true);
        let started = std::time::Instant::now();
        pause(Duration::from_secs(30), &stop).await;
        let result = with_retries("成功", &stop, Duration::from_secs(30), |_| async {
            Err::<(), _>(crate::error::Error::msg("失敗"))
        })
        .await;
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(5), "待たずに返る");

        // 合図が無ければ、決めた時間だけ待つ。
        let stop = AtomicBool::new(false);
        let started = std::time::Instant::now();
        pause(Duration::from_millis(120), &stop).await;
        assert!(started.elapsed() >= Duration::from_millis(120));
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 取り出しの失敗は数回やり直してから終わる() {
        let _db = crate::testing::refresh_database().await;
        // キューの表を作らない。取り出しは必ず DB のエラーになる。
        let stop = Arc::new(AtomicBool::new(false));
        let options = Options {
            once: true,
            ..Options::default()
        };
        let result = run_with(&[], &options, stop, Duration::from_millis(1)).await;
        assert!(result.is_err(), "直らなければ、今までどおりエラーで終わる");
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 取り出しの失敗が直れば続ける() {
        let _db = crate::testing::refresh_database().await;
        let stop = Arc::new(AtomicBool::new(false));
        let options = Options {
            once: true,
            ..Options::default()
        };

        // 最初は表が無くて失敗する。やり直しの間に表ができる。
        // 以前は 1 回目のエラーでワーカーが終わっていた。
        // 待ちの単位は 100 ミリ秒（合わせて 2.5 秒まで粘る）。
        let worker = run_with(&[], &options, stop, Duration::from_millis(100));
        let fix = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            表を作る().await;
        };
        let (result, ()) = tokio::join!(worker, fix);
        result.expect("直ったので、エラーにならずに終わる");
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 時間切れは普通の失敗と同じ流れに乗る() {
        let _db = crate::testing::refresh_database().await;
        表を作る().await;
        let id = 待っているジョブを入れる("Stuck").await;

        let jobs = [Job {
            name: "Stuck",
            handle: |_| {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    Ok(())
                })
            },
        }];
        let stop = Arc::new(AtomicBool::new(false));
        let options = Options {
            once: true,
            timeout: 1,
            ..Options::default()
        };
        let started = std::time::Instant::now();
        run(&jobs, &options, stop).await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(30), "固まらない");

        // 失敗 1 回として数え、予約を外して、次の時刻を先へ送る。
        let row = DB::table(TABLE).find(id).await.unwrap().unwrap();
        assert_eq!(row.get::<i64>("attempts").unwrap(), 1);
        assert!(row.value("reserved_at").unwrap().is_null(), "予約を外す");
        assert!(
            row.get::<String>("available_at").unwrap() > crate::database::now(),
            "待ち時間を置く"
        );
        assert_eq!(DB::table(FAILED_TABLE).count().await.unwrap(), 0);

        // 最後の 1 回が時間切れなら、理由を付けて諦める。
        DB::table(TABLE)
            .where_("id", id)
            .update(&[
                ("attempts", Value::Int(2)),
                ("available_at", Value::Text(crate::database::now())),
            ])
            .await
            .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        run(&jobs, &options, stop).await.unwrap();
        let failed = failed_list().await.unwrap();
        assert_eq!(failed.len(), 1);
        assert!(failed[0].3.contains("打ち切りました"), "{}", failed[0].3);
    }

    /// `reserve` の 1 文目を組み立てて SQL と値を見る。
    ///
    /// **Rust 側に判定を写しません。** 本番で走るのは SQL なので、写しを
    /// 持つと 2 つがずれても気づけません。投げる文そのものを押さえます。
    fn reserve_sql(now: &str, cutoff: &str) -> (String, Vec<Value>) {
        reservable(DB::table(TABLE), "default", now, cutoff)
            .order_by("id")
            .limit(1)
            .to_sql()
    }

    #[test]
    fn 取り直しの条件はsqlに出る() {
        let now = time::format_timestamp(1_700_000_000);
        let cutoff = reclaim_cutoff(1_700_000_000, 90);
        let (sql, bindings) = reserve_sql(&now, &cutoff);

        // 予約が無いもの、または境目**以前**に予約されたもの。`<` では
        // ちょうど境目のものを取り直せません。
        assert!(sql.contains("\"reserved_at\" is null"), "{sql}");
        assert!(sql.contains("or \"reserved_at\" <= ?"), "{sql}");
        assert!(!sql.contains("\"reserved_at\" < ?"), "{sql}");
        // 括弧でくくられていること（`or` が外に出ると全件が当たる）。
        assert!(sql.contains("and (\"reserved_at\" is null"), "{sql}");
        // 1 件ずつ、古いものから。
        assert!(sql.contains("order by \"id\" asc limit 1"), "{sql}");

        // 渡す値は キュー → いまの時刻 → 境目 の順。
        assert_eq!(
            bindings,
            vec![
                Value::Text("default".to_string()),
                Value::Text(now),
                Value::Text(cutoff),
            ]
        );
    }

    #[test]
    fn 更新も同じ条件で絞る() {
        // 2 文目（update）の `where` は 1 文目と同じ条件に、見た `attempts` を足す。
        let now = time::format_timestamp(1_700_000_000);
        let cutoff = reclaim_cutoff(1_700_000_000, 90);
        let (sql, bindings) =
            reservable(DB::table(TABLE).where_("id", 7), "default", &now, &cutoff)
                .where_("attempts", 2_i64)
                .to_sql();

        assert!(sql.contains("\"id\" = ?"), "{sql}");
        assert!(sql.contains("\"queue\" = ?"), "{sql}");
        assert!(sql.contains("\"available_at\" <= ?"), "{sql}");
        assert!(sql.contains("and (\"reserved_at\" is null"), "{sql}");
        assert!(sql.contains("or \"reserved_at\" <= ?"), "{sql}");
        assert!(sql.contains("and \"attempts\" = ?"), "{sql}");
        assert_eq!(
            bindings,
            vec![
                Value::Int(7),
                Value::Text("default".to_string()),
                Value::Text(now),
                Value::Text(cutoff),
                Value::Int(2),
            ]
        );
    }

    /// すぐ取れるジョブを 1 本入れて、その id を返す。
    #[cfg(feature = "sqlite")]
    async fn 待っているジョブを入れる(job: &str) -> i64 {
        let now = crate::database::now();
        DB::table(TABLE)
            .insert_get_id(&[
                ("queue", Value::Text(DEFAULT_QUEUE.to_string())),
                ("job", Value::Text(job.to_string())),
                ("payload", Value::Text("{}".to_string())),
                ("attempts", Value::Int(0)),
                ("available_at", Value::Text(now.clone())),
                ("created_at", Value::Text(now)),
            ])
            .await
            .unwrap()
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 間に別のワーカーが失敗させた行は取らない() {
        let _db = crate::testing::refresh_database().await;
        表を作る().await;
        let id = 待っているジョブを入れる("SendWelcome").await;

        // 1 文目で見た状態（attempts = 0、available_at = 今）。
        let now_secs = time::now_seconds();
        let now = time::format_timestamp(now_secs);
        let cutoff = reclaim_cutoff(now_secs, DEFAULT_RETRY_AFTER);

        // 2 文目を投げる前に、別のワーカーが同じ行を取って速く失敗した形。
        // `fail` と同じく、予約を外して回数を増やし、次の時刻を先へ送る。
        let later = time::format_timestamp(now_secs + 60);
        DB::table(TABLE)
            .where_("id", id)
            .update(&[
                ("attempts", Value::Int(1)),
                ("available_at", Value::Text(later.clone())),
                ("reserved_at", Value::Null),
            ])
            .await
            .unwrap();

        // 以前は `id` と取り直しの条件だけで更新が通り、待ち時間を無視して
        // 即再実行し、`attempts` を 0 に巻き戻していた。
        let affected = claim_row(DEFAULT_QUEUE, id, 0, &now, &cutoff, 0, &now)
            .await
            .unwrap();
        assert_eq!(affected, 0, "負けたと見なす");

        let row = DB::table(TABLE).find(id).await.unwrap().unwrap();
        assert_eq!(row.get::<i64>("attempts").unwrap(), 1, "回数を巻き戻さない");
        assert_eq!(
            row.get::<String>("available_at").unwrap(),
            later,
            "待ち時間を保つ"
        );
        assert!(row.value("reserved_at").unwrap().is_null(), "予約しない");
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 名前の無いジョブは待たずに諦める() {
        let _db = crate::testing::refresh_database().await;
        表を作る().await;
        待っているジョブを入れる("NoSuchJob").await;

        // 登録されているジョブが 1 つも無い状態で 1 本だけ処理する。
        let stop = Arc::new(AtomicBool::new(false));
        let options = Options {
            once: true,
            ..Options::default()
        };
        run(&[], &options, stop).await.unwrap();

        // 以前は 10 秒・20 秒と待って 3 回回してから諦めていた。
        assert_eq!(
            DB::table(TABLE).count().await.unwrap(),
            0,
            "キューから消える"
        );
        let failed = failed_list().await.unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].1, "NoSuchJob");
        assert!(
            failed[0].3.contains("というジョブはありません"),
            "{}",
            failed[0].3
        );
    }

    #[test]
    fn 自分の予約だけを触る条件() {
        // `finish` と `fail` は id だけで絞りません。
        let reserved = Reserved {
            id: 7,
            job: "SendWelcome".into(),
            payload: "{}".into(),
            attempts: 1,
            queue: "default".into(),
            reserved_at: "2024-01-01 00:00:00".into(),
        };
        let (sql, bindings) = mine(DB::table(TABLE), &reserved).to_sql();

        assert!(sql.contains("\"id\" = ?"), "{sql}");
        assert!(sql.contains("and \"reserved_at\" = ?"), "{sql}");
        assert_eq!(
            bindings,
            vec![
                Value::Int(7),
                Value::Text("2024-01-01 00:00:00".to_string()),
            ]
        );
    }

    #[test]
    fn 境目の時刻は予約より前になる() {
        let now = time::now_seconds();
        let cutoff = reclaim_cutoff(now, 90);
        // 100 秒前に予約されたまま（ワーカーが死んだ）＝境目より前なので取り直す。
        assert!(time::format_timestamp(now - 100) < cutoff);
        // 10 秒前に予約されたばかり（まだ処理中）＝境目より後なので取らない。
        assert!(time::format_timestamp(now - 10) > cutoff);
    }

    #[test]
    fn 取り直すと失敗回数が増える() {
        // 目印が残っていた＝前のワーカーが死んだので、1 回ぶん数える。
        assert_eq!(next_attempts(Some("2024-01-01 00:00:00"), 0), 1);
        assert_eq!(next_attempts(Some("2024-01-01 00:00:00"), 2), 3);
        // 初めて取るときは増やさない。
        assert_eq!(next_attempts(None, 0), 0);
        assert_eq!(next_attempts(None, 2), 2);
    }

    #[test]
    fn 取り直しの境目は0秒にしない() {
        let now = time::now_seconds();
        // 0 や負の値を渡しても、いま予約したものを取り直さない。
        assert!(reclaim_cutoff(now, 0) < time::format_timestamp(now));
        assert!(reclaim_cutoff(now, -5) < time::format_timestamp(now));
    }

    /// テスト用にキューの表を作る。`sqlite` が無いときは使いません。
    #[cfg(feature = "sqlite")]
    async fn 表を作る() {
        let mut schema = crate::database::Schema::new(crate::database::Driver::Sqlite);
        crate::queue::Queue::define(&mut schema);
        for sql in schema.to_sql() {
            DB::statement(sql, &[]).await.unwrap();
        }
    }

    /// 諦めたジョブを 1 本入れて、その id を返す。`sqlite` が無いときは使いません。
    #[cfg(feature = "sqlite")]
    async fn 失敗を1本入れる() -> i64 {
        let now = crate::database::now();
        DB::table(FAILED_TABLE)
            .insert_get_id(&[
                ("queue", Value::Text(DEFAULT_QUEUE.to_string())),
                ("job", Value::Text("SendWelcome".to_string())),
                ("payload", Value::Text("{}".to_string())),
                ("error", Value::Text("わざと失敗".to_string())),
                ("failed_at", Value::Text(now)),
            ])
            .await
            .unwrap()
    }

    /// 実際にデータベースをつなぐテストです。
    ///
    /// 機能フラグ `sqlite` が無いとつなげないので、そのときは飛ばします。
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 同じ失敗ジョブは二重に入らない() {
        let _db = crate::testing::refresh_database().await;
        表を作る().await;
        let id = 失敗を1本入れる().await;

        // `queue:retry` を 2 つ同時に流した形。行はトランザクションの外で
        // 読むので、両方が同じ失敗ジョブを見ます。消せた側だけが入れ直すので、
        // 入るのは 1 本だけです（前は `insert` が両方通っていました）。
        let (a, b) = tokio::join!(retry_failed(Some(id)), retry_failed(Some(id)));
        let moved = a.unwrap() + b.unwrap();
        assert_eq!(moved, 1, "入れ直したのは 1 本だけ");
        assert_eq!(DB::table(TABLE).count().await.unwrap(), 1, "二重に入らない");
        assert_eq!(DB::table(FAILED_TABLE).count().await.unwrap(), 0);

        // もう 1 回流しても増えない。
        assert_eq!(retry_failed(None).await.unwrap(), 0);
        assert_eq!(DB::table(TABLE).count().await.unwrap(), 1);
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 打ち切りが長いジョブは予約が延びて取り直されない() {
        let _db = crate::testing::refresh_database().await;
        表を作る().await;
        crate::queue::Queue::push_raw("Long", "{}").await.unwrap();
        crate::queue::Queue::push_raw("Short", "{}").await.unwrap();

        let options = Options {
            timeouts: vec![("Long".into(), 1800)],
            ..Options::default()
        };
        let Reservation::Taken(long) = reserve(&options).await.unwrap() else {
            panic!("取れる");
        };
        assert_eq!(long.job, "Long");
        let Reservation::Taken(short) = reserve(&options).await.unwrap() else {
            panic!("取れる");
        };
        assert_eq!(short.job, "Short");

        // 長いほうは先の時刻、短いほう（`--timeout` 60 秒）はいまの時刻。
        let now = time::format_timestamp(time::now_seconds());
        assert!(long.reserved_at > now, "{} > {now}", long.reserved_at);
        assert!(short.reserved_at <= now, "{} <= {now}", short.reserved_at);

        // 表の値も同じ（`finish` / `fail` は目印の一致で自分の予約か見分ける）。
        let stored: Option<String> = DB::table(TABLE)
            .where_("id", long.id)
            .value("reserved_at")
            .await
            .unwrap();
        assert_eq!(stored.as_deref(), Some(long.reserved_at.as_str()));

        // retry_after（90 秒）を過ぎたころに別のワーカーが見ても、長いほうは取り直さない。
        let later = time::now_seconds() + 120;
        let cutoff = reclaim_cutoff(later, options.retry_after);
        assert!(long.reserved_at > cutoff, "長いほうはまだ予約のまま");
        assert!(short.reserved_at <= cutoff, "短いほうは取り直しの対象");
    }

    /// 処理中の目印が残ったままのジョブを 1 本入れて、その id を返す。
    #[cfg(feature = "sqlite")]
    async fn 放置されたジョブを入れる(attempts: i64) -> i64 {
        let now = crate::database::now();
        // 目印は `retry_after` より十分古くする（＝回収の対象）。
        let old = time::format_timestamp(time::now_seconds() - 1_000);
        DB::table(TABLE)
            .insert_get_id(&[
                ("queue", Value::Text(DEFAULT_QUEUE.to_string())),
                ("job", Value::Text("SendWelcome".to_string())),
                ("payload", Value::Text("{}".to_string())),
                ("attempts", Value::Int(attempts)),
                ("available_at", Value::Text(now.clone())),
                ("reserved_at", Value::Text(old)),
                ("created_at", Value::Text(now)),
            ])
            .await
            .unwrap()
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn ワーカーごと落ちたジョブは諦める() {
        let _db = crate::testing::refresh_database().await;
        表を作る().await;
        // `tries` を超えて試したあと、ワーカーごと落ちて目印が残ったままの行。
        放置されたジョブを入れる(3).await;

        // 以前はここで予約し直し、`attempts` が `tries` を超えても回収され
        // 続けました。`order_by("id") limit 1` なので、この 1 件で
        // キュー全体が止まりました。
        let taken = reserve(&Options::default()).await.unwrap();
        assert!(matches!(taken, Reservation::Lost), "予約しない");
        assert_eq!(
            DB::table(TABLE).count().await.unwrap(),
            0,
            "キューから消える"
        );
        assert_eq!(DB::table(FAILED_TABLE).count().await.unwrap(), 1);

        // `queue:failed` に出るので、何が起きたか分かる。
        let failed = failed_list().await.unwrap();
        assert_eq!(failed.len(), 1);
        assert!(
            failed[0].3.contains("ワーカーが応答しなくなった"),
            "{}",
            failed[0].3
        );

        // 詰まりが解けて、次は空になる。
        assert!(matches!(
            reserve(&Options::default()).await.unwrap(),
            Reservation::Empty
        ));
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 落ちた1回も数えて諦める() {
        let _db = crate::testing::refresh_database().await;
        表を作る().await;
        // 2 回ぶん記録されたあと、3 回目の途中でワーカーごと落ちた行。
        // 落ちた 1 回はまだ `attempts` に数えられていない。
        放置されたジョブを入れる(2).await;

        // 列の値（2）で見ていたころは `2 >= 3` が偽で予約し直し、
        // `--tries 3` でも 4 回走っていた。数え直した値（3）で諦める。
        let taken = reserve(&Options::default()).await.unwrap();
        assert!(matches!(taken, Reservation::Lost), "予約しない");
        assert_eq!(
            DB::table(TABLE).count().await.unwrap(),
            0,
            "キューから消える"
        );

        // 文面の回数も、実際に動いた回数とそろう。
        let failed = failed_list().await.unwrap();
        assert_eq!(failed.len(), 1);
        assert!(failed[0].3.starts_with("3 回試しました"), "{}", failed[0].3);
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 回数が負の行でも予約できる() {
        let _db = crate::testing::refresh_database().await;
        表を作る().await;
        let id = 待っているジョブを入れる("SendWelcome").await;
        // アプリが `DB::table("jobs").update(..)` で負の値を入れた形。
        DB::table(TABLE)
            .where_("id", id)
            .update(&[("attempts", Value::Int(-1))])
            .await
            .unwrap();

        // 読んだ値を `max(0)` で丸めていたころは、2 文目の
        // `where attempts = 0` が永久に 0 件になり、`Lost` を待たずに
        // 繰り返して全力ループになった（キューは 1 件も進まない）。
        match reserve(&Options::default()).await.unwrap() {
            Reservation::Taken(reserved) => {
                assert_eq!(reserved.id, id);
                assert_eq!(reserved.attempts, -1, "読んだ値をそのまま持つ");
            }
            _ => panic!("予約できるはずです"),
        }
        // 回数は `attempts + 1` で進むので、いずれ `tries` に達する。
        assert_eq!(next_attempts(Some("2024-01-01 00:00:00"), -1), 0);
        // ずらし幅が負にならない。
        assert_eq!(backoff_secs(-1), 5);
    }

    #[test]
    fn 負けが続いたら待ちに落ちる() {
        // 2 文目の条件が永久に合わない行があると、`Lost` の繰り返しで
        // 全力ループになる。ある回数で `--sleep` の待ちに落として歯止めにする。
        let mut lost = 0;
        for _ in 1..MAX_CONSECUTIVE_LOST {
            assert!(!back_off_after_losses(&mut lost), "数回なら待たない");
        }
        assert!(back_off_after_losses(&mut lost), "続けて負けたら待つ");
        // 数え直すので、次はまた待たずに取り直せる。
        assert!(!back_off_after_losses(&mut lost));
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 回数が残っていれば取り直す() {
        let _db = crate::testing::refresh_database().await;
        表を作る().await;
        let id = 放置されたジョブを入れる(1).await;

        // `tries` に届いていない行は、今までどおり取り直す（`attempts` は +1）。
        match reserve(&Options::default()).await.unwrap() {
            Reservation::Taken(reserved) => {
                assert_eq!(reserved.id, id);
                assert_eq!(reserved.attempts, 2);
            }
            _ => panic!("取り直すはずです"),
        }
        assert_eq!(DB::table(FAILED_TABLE).count().await.unwrap(), 0);
        assert_eq!(DB::table(TABLE).count().await.unwrap(), 1);
    }
}
