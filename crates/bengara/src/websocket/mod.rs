//! WebSocket。普通のルートのハンドラの中で、要求を WebSocket に切り替えます。
//!
//! ```ignore
//! use bengara::websocket::{Message, WebSocket};
//!
//! // routes/web.rs
//! Route::get("/gateway", GatewayController::connect);
//!
//! pub async fn connect(req: Request) -> Result<Response> {
//!     // 切り替える前に、普通の HTTP として確かめる（Err なら普通のエラー応答）。
//!     let ticket = req.query("ticket").ok_or_else(|| Error::http(401, "チケットがありません"))?;
//!
//!     req.websocket()?.on_upgrade(move |mut socket: WebSocket| async move {
//!         socket.join("room.1");
//!         while let Some(Ok(message)) = socket.recv().await {
//!             if let Message::Text(text) = message {
//!                 let _ = socket.send_text(text);
//!             }
//!         }
//!     })
//! }
//! ```
//!
//! 機能フラグ `websocket` が要ります（既定では入りません）。
//! フラグが無くてもこのモジュールはコンパイルされ、`req.websocket()` が
//! 直し方を書いたエラーを返します。
//!
//! | 既定 | 値 |
//! |---|---|
//! | 受け取るメッセージの上限 | 64 KiB（超えたら 1009 で閉じる） |
//! | 送信の列の上限 | 1 MiB（超えたら 1008 で閉じる） |
//! | サーバーからの Ping | 送らない |
//! | 無通信で閉じる | 閉じない |
//! | 同時接続の上限 | 10,000（`WEBSOCKET_MAX_CONNECTIONS`） |

// フラグが無いと、ソケットを扱う所（io.rs）から呼ばれる関数が使われずに残る。
#![cfg_attr(not(feature = "websocket"), allow(dead_code))]

pub(crate) mod registry;

#[cfg(feature = "websocket")]
mod io;

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{watch, Notify};

use crate::error::{Error, Result};
use crate::http::{Request, Response};

use registry::{Conn, Outgoing};

/// 受け取るメッセージの上限の既定（64 KiB）。
const DEFAULT_MAX_MESSAGE_SIZE: usize = 64 * 1024;

/// 送信の列の上限の既定（1 MiB）。
const DEFAULT_SEND_QUEUE_LIMIT: usize = 1024 * 1024;

/// 同時接続の上限の既定。
const DEFAULT_MAX_CONNECTIONS: usize = 10_000;

/// 相手の Close を待つ長さ（`close` と、bengara が閉じるとき）。
pub(crate) const CLOSE_WAIT: Duration = Duration::from_secs(5);

/// 1 通の書き込みを待つ長さ。相手が読まずに TCP の窓が 0 のままなら、これで切る。
/// 要求の本文を読む上限（`BODY_READ_TIMEOUT`）と同じ 30 秒です。
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// 受信の溜まりの上限を、受け取る 1 通の上限の何通分にするか。
const INBOUND_MESSAGES: usize = 16;

/// 停止のとき、接続が閉じるのを待つ長さの上限。
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Close の理由の上限（RFC 6455。制御フレームは 125 バイトで、番号に 2 バイト使う）。
const MAX_REASON_BYTES: usize = 123;

/// 版が 13 でないときの文。`error_response` がこれを見て `Sec-WebSocket-Version` を付けます。
pub(crate) const VERSION_MESSAGE: &str = "WebSocket の版は 13 だけを受け付けます";

/// 機能フラグが無いときの案内。「足してください」と言う（mail と同じ）。
#[cfg(not(feature = "websocket"))]
const FEATURE_HINT: &str = "WebSocket を使うには Cargo.toml の bengara の features に \
    \"websocket\" を足してください（例: `features = [\"sqlite\", \"websocket\"]`）";

/// 受け取ったメッセージ。Ping / Pong / Close はここに出ません。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// 文字。
    Text(String),
    /// バイト列。
    Binary(Vec<u8>),
}

/// 送れなかった理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// 接続はもう閉じている（閉じる途中も含む）。
    Closed,
    /// 送信の列があふれた。**この時点で接続を 1008 で閉じています。**
    QueueFull,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Closed => f.write_str("WebSocket の接続はもう閉じています"),
            SendError::QueueFull => f.write_str(
                "WebSocket の送信の列があふれました（相手が読んでいません）。接続は 1008 で閉じました",
            ),
        }
    }
}

impl std::error::Error for SendError {}

impl From<SendError> for Error {
    fn from(error: SendError) -> Self {
        Error::msg(error.to_string())
    }
}

/// 接続の ID。プロセスの中で一意です。ログと照合に使います。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnectionId(pub(crate) u64);

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 切り替えの設定。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Settings {
    pub(crate) max_message_size: usize,
    pub(crate) send_queue_limit: usize,
    pub(crate) ping_interval: Option<Duration>,
    pub(crate) idle_timeout: Option<Duration>,
    /// 1 通の書き込みを待つ長さの上限。利用者は変えられない（テストで縮める）。
    pub(crate) write_timeout: Duration,
}

impl Settings {
    /// アプリがまだ `recv` で読んでいない受信の量の上限（バイト）。
    /// 受け取る 1 通の上限の 16 通分です（既定 64 KiB × 16 = 1 MiB）。
    pub(crate) fn inbound_limit(&self) -> usize {
        self.max_message_size.saturating_mul(INBOUND_MESSAGES)
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
            send_queue_limit: DEFAULT_SEND_QUEUE_LIMIT,
            ping_interval: None,
            idle_timeout: None,
            write_timeout: WRITE_TIMEOUT,
        }
    }
}

// ---------------------------------------------------------------------------
// Request と Response に載せる非公開の欄
// ---------------------------------------------------------------------------

/// 1 回だけ取り出せる置き場所。`Request` と `Response` の非公開の欄に使います。
///
/// どちらも `Clone` なので、複製しても**中身は 1 つ**です（`Arc` で共有）。
/// 空のとき（ふつうのリクエスト）は `Arc` を作らないので、確保が増えません。
/// 錠はリクエスト 1 本の中だけのもので、ほかのリクエストと取り合いません。
pub(crate) struct Slot<T>(Option<Arc<Mutex<Option<T>>>>);

impl<T> Slot<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(Some(Arc::new(Mutex::new(Some(value)))))
    }

    /// 取り出す。2 回目からは `None`。
    pub(crate) fn take(&self) -> Option<T> {
        self.0
            .as_ref()
            .and_then(|slot| slot.lock().unwrap_or_else(|e| e.into_inner()).take())
    }

    /// 取り出さずに中身を見る。
    pub(crate) fn with<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        let slot = self.0.as_ref()?;
        let guard = slot.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref().map(f)
    }

    /// 中身があるか（テスト用）。
    #[cfg(test)]
    pub(crate) fn is_filled(&self) -> bool {
        self.with(|_| ()).is_some()
    }
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<T> Clone for Slot<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> std::fmt::Debug for Slot<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 中身は出さない（錠も取らない）。
        f.write_str(if self.0.is_some() { "Slot(..)" } else { "Slot" })
    }
}

/// `server.rs` がリクエストに載せるもの。切り替えの口と、サーバーの状態。
///
/// `TestClient` から来たリクエストには載りません（ソケットが無いため）。
pub(crate) struct Handoff {
    #[cfg(feature = "websocket")]
    pub(crate) on_upgrade: Option<hyper::upgrade::OnUpgrade>,
    pub(crate) server: Arc<ServerState>,
}

/// アプリのクロージャを包んだもの。
type Callback = Box<dyn FnOnce(WebSocket) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

/// `on_upgrade` が応答に載せるもの。`server.rs` が 101 を返した後に取り出します。
pub(crate) struct Pending {
    #[cfg(feature = "websocket")]
    on_upgrade: hyper::upgrade::OnUpgrade,
    seat: Seat,
    settings: Settings,
    callback: Callback,
}

// ---------------------------------------------------------------------------
// サーバーの状態（停止・同時接続の上限）
// ---------------------------------------------------------------------------

/// 1 つのサーバー（`serve`）の WebSocket の状態。
///
/// 切り替えた後の接続は axum の停止の待ちに入らないので、bengara が自分で数えて待ちます。
pub(crate) struct ServerState {
    /// 同時接続の上限。
    limit: usize,
    /// いま開いている接続の数（席の数）。
    active: AtomicUsize,
    /// 席が 0 になったときに起こす。
    idle: Notify,
    /// 停止の合図。立つと、全接続が 1001 で閉じる。
    shutdown: watch::Sender<bool>,
}

impl ServerState {
    pub(crate) fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            active: AtomicUsize::new(0),
            idle: Notify::new(),
            shutdown: watch::channel(false).0,
        })
    }

    /// `WEBSOCKET_MAX_CONNECTIONS`（既定 10,000）を上限にして作る。0 は既定と同じ。
    pub(crate) fn from_env() -> Arc<Self> {
        let limit = crate::env::<usize>("WEBSOCKET_MAX_CONNECTIONS", DEFAULT_MAX_CONNECTIONS);
        Self::new(if limit == 0 {
            DEFAULT_MAX_CONNECTIONS
        } else {
            limit
        })
    }

    pub(crate) fn is_shutting_down(&self) -> bool {
        *self.shutdown.borrow()
    }

    pub(crate) fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }

    fn is_full(&self) -> bool {
        self.active() >= self.limit
    }

    /// 席を 1 つ取る。上限なら `None`。
    fn try_seat(self: &Arc<Self>) -> Option<Seat> {
        let mut current = self.active.load(Ordering::Acquire);
        loop {
            if current >= self.limit {
                return None;
            }
            match self.active.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Seat(self.clone())),
                Err(actual) => current = actual,
            }
        }
    }

    /// 停止の合図を立てる。新しい切り替えは 503 になり、全接続に 1001 を送ります。
    pub(crate) fn begin_shutdown(&self) {
        self.shutdown.send_replace(true);
    }

    fn subscribe_shutdown(&self) -> watch::Receiver<bool> {
        self.shutdown.subscribe()
    }

    /// 席が 0 になるまで待つ。`limit` を過ぎたら偽。
    pub(crate) async fn wait_idle(&self, limit: Duration) -> bool {
        let wait = async {
            loop {
                let notified = self.idle.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.active() == 0 {
                    return;
                }
                notified.await;
            }
        };
        tokio::time::timeout(limit, wait).await.is_ok()
    }
}

/// 同時接続の席。手放すと返ります（切り替えに失敗したときも、接続が終わったときも）。
pub(crate) struct Seat(Arc<ServerState>);

impl Seat {
    fn server(&self) -> &ServerState {
        &self.0
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        if self.0.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

/// 停止のときに呼ぶ。全接続に 1001 を送り、閉じるのを待つ。
///
/// 待つのは `APP_SHUTDOWN_TIMEOUT` の残りと 5 秒の短いほうです。
/// `serve` は HTTP の待ちと同時に呼ぶので、残りは上限そのものです。
/// 上限なし（`APP_SHUTDOWN_TIMEOUT=0`）のときは 5 秒です。
pub(crate) async fn shutdown(state: &ServerState, timeout: Option<Duration>) {
    state.begin_shutdown();
    let open = state.active();
    if open == 0 {
        return;
    }
    let limit = timeout.map_or(SHUTDOWN_GRACE, |t| t.min(SHUTDOWN_GRACE));
    tracing::info!(
        "WebSocket の接続 {open} 本に 1001 を送り、閉じるのを最大 {} 秒待ちます",
        limit.as_secs_f32()
    );
    if !state.wait_idle(limit).await {
        tracing::warn!(
            "WebSocket の接続が {} 本閉じなかったので、そのまま切ります",
            state.active()
        );
    }
}

// ---------------------------------------------------------------------------
// 切り替えの確かめ
// ---------------------------------------------------------------------------

/// ヘッダーの値をカンマで区切り、`token` があるか（大文字小文字は見ない）。
fn has_token<'a>(mut values: impl Iterator<Item = &'a str>, token: &str) -> bool {
    values.any(|value| {
        value
            .split(',')
            .any(|part| part.trim().eq_ignore_ascii_case(token))
    })
}

/// 切り替えの要求か（`Upgrade: websocket` と `Connection: upgrade` があるか）。
pub(crate) fn wants_websocket(req: &Request) -> bool {
    has_token(req.header_all("upgrade"), "websocket")
        && has_token(req.header_all("connection"), "upgrade")
}

/// `Sec-WebSocket-Key` が base64 で 16 バイトか。
///
/// 元に戻さずに形だけを見ます。16 バイトの base64 は 24 文字で、末尾が `==`、
/// その手前の 1 文字は下 4 ビットが 0（`A` `Q` `g` `w` のどれか）です。
fn valid_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    if bytes.len() != 24 || !key.ends_with("==") {
        return false;
    }
    let body_ok = bytes[..22]
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/');
    body_ok && matches!(bytes[21], b'A' | b'Q' | b'g' | b'w')
}

/// 送れる Close の番号に直す。送れない番号は 1000 にして警告します。
///
/// 送れるのは 1000〜1003、1007〜1014、3000〜4999 です（RFC 6455）。
/// 1004〜1006 と 1015 は送ってはいけない番号、1016〜2999 は予約、
/// 5000 以上と 1000 未満は範囲の外です。
pub(crate) fn close_code(code: u16) -> u16 {
    if matches!(code, 1000..=1003 | 1007..=1014 | 3000..=4999) {
        return code;
    }
    tracing::warn!("WebSocket の閉じる番号 {code} は送れないので、1000 にします");
    1000
}

/// Close の理由を 123 バイトまでに切る。文字の途中では切りません。
pub(crate) fn close_reason(reason: &str) -> String {
    if reason.len() <= MAX_REASON_BYTES {
        return reason.to_string();
    }
    let mut end = MAX_REASON_BYTES;
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason[..end].to_string()
}

/// `req.websocket()` が返す、切り替えの組み立て。
///
/// `on_upgrade` で 101 の応答を作ります。クロージャは切り替えの後に新しいタスクで動きます。
pub struct WebSocketUpgrade {
    key: String,
    /// 相手が申し出たサブプロトコル（`Sec-WebSocket-Protocol`）。届いた順。
    offered: Vec<String>,
    protocol: Option<String>,
    settings: Settings,
    handoff: Slot<Handoff>,
}

impl std::fmt::Debug for WebSocketUpgrade {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSocketUpgrade")
            .field("protocol", &self.protocol)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

/// 503 の文。
fn unavailable(reason: &str) -> Error {
    Error::http(503, reason)
}

impl WebSocketUpgrade {
    /// リクエストを確かめて作る（`Request::websocket` の中身）。
    pub(crate) fn from_request(req: &Request) -> Result<Self> {
        #[cfg(not(feature = "websocket"))]
        {
            let _ = req;
            Err(Error::msg(FEATURE_HINT))
        }
        #[cfg(feature = "websocket")]
        {
            Self::check(req)
        }
    }

    /// 切り替えの要求を確かめる。
    ///
    /// | 確かめること | 違ったとき |
    /// |---|---|
    /// | メソッドが GET | 400 |
    /// | `Upgrade` に `websocket` | 400 |
    /// | `Connection` に `upgrade` | 400 |
    /// | `Sec-WebSocket-Version` が 13 | 426 ＋ `Sec-WebSocket-Version: 13` |
    /// | `Sec-WebSocket-Key` が base64 で 16 バイト | 400 |
    /// | サーバーが停止の途中でない | 503 |
    /// | 同時接続の上限を超えない | 503 |
    pub(crate) fn check(req: &Request) -> Result<Self> {
        if req.method() != "GET" {
            return Err(Error::http(400, "WebSocket の切り替えは GET だけです"));
        }
        if !has_token(req.header_all("upgrade"), "websocket") {
            return Err(Error::http(400, "Upgrade: websocket がありません"));
        }
        if !has_token(req.header_all("connection"), "upgrade") {
            return Err(Error::http(400, "Connection: upgrade がありません"));
        }
        if req.header("sec-websocket-version").map(str::trim) != Some("13") {
            return Err(Error::http(426, VERSION_MESSAGE));
        }
        let key = req.header("sec-websocket-key").map(str::trim).unwrap_or("");
        if !valid_key(key) {
            return Err(Error::http(400, "Sec-WebSocket-Key が正しくありません"));
        }
        let handoff = req.upgrade_slot().clone();
        let busy = handoff.with(|h| (h.server.is_shutting_down(), h.server.is_full()));
        if let Some((stopping, full)) = busy {
            if stopping {
                return Err(unavailable("サーバーは停止の途中です"));
            }
            if full {
                return Err(unavailable("WebSocket の同時接続の上限に達しています"));
            }
        }
        let offered = req
            .header_all("sec-websocket-protocol")
            .flat_map(|value| value.split(','))
            .map(|part| part.trim().to_string())
            .filter(|part| !part.is_empty())
            .collect();
        Ok(Self {
            key: key.to_string(),
            offered,
            protocol: None,
            settings: Settings::default(),
            handoff,
        })
    }

    /// 受け取るメッセージの上限（バイト）。既定 64 KiB。超えたら 1009 で閉じます。
    pub fn max_message_size(mut self, bytes: usize) -> Self {
        self.settings.max_message_size = bytes.max(1);
        self
    }

    /// 送信の列の上限（バイト）。既定 1 MiB。超えたら 1008 で閉じます（遅い相手を切る）。
    pub fn send_queue_limit(mut self, bytes: usize) -> Self {
        self.settings.send_queue_limit = bytes.max(1);
        self
    }

    /// サーバーから Ping を送る間隔。既定は送りません。
    pub fn ping_interval(mut self, every: Duration) -> Self {
        self.settings.ping_interval = (!every.is_zero()).then_some(every);
        self
    }

    /// 何も受け取らないまま過ぎたら 1000 で閉じる長さ。既定は閉じません。
    pub fn idle_timeout(mut self, after: Duration) -> Self {
        self.settings.idle_timeout = (!after.is_zero()).then_some(after);
        self
    }

    /// 受け付けるサブプロトコル（`Sec-WebSocket-Protocol`）。
    ///
    /// 相手が申し出た順に見て、ここに並べたものと最初に一致したものを選びます。
    /// 一致しなければ選びません（ヘッダーを返しません）。
    pub fn protocols(mut self, offered: &[&str]) -> Self {
        self.protocol = self
            .offered
            .iter()
            .find(|wanted| offered.iter().any(|ours| ours == wanted))
            .cloned();
        self
    }

    /// 選ばれたサブプロトコル。`on_upgrade` の前に読めます。
    pub fn protocol(&self) -> Option<&str> {
        self.protocol.as_deref()
    }

    /// 101 の応答を返す。`f` は切り替えの後に新しいタスクで動きます。
    ///
    /// `f` が返ったら、まだ開いていれば 1000 で閉じます。
    /// `f` がパニックしたら、その接続だけを 1011 で閉じます（サーバーは止まりません）。
    ///
    /// `#[bengara::test]` の `TestClient` から送ったとき（ソケットが無いとき）は、
    /// ヘッダーを確かめた 101 を返すだけで、`f` は呼びません。
    pub fn on_upgrade<F, Fut>(self, f: F) -> Result<Response>
    where
        F: FnOnce(WebSocket) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        #[cfg(not(feature = "websocket"))]
        {
            let _ = f;
            Err(Error::msg(FEATURE_HINT))
        }
        #[cfg(feature = "websocket")]
        {
            let accept =
                tokio_tungstenite::tungstenite::handshake::derive_accept_key(self.key.as_bytes());
            let mut response = Response::new(101)
                .with_header("upgrade", "websocket")
                .with_header("connection", "upgrade")
                .with_header("sec-websocket-accept", accept);
            if let Some(protocol) = &self.protocol {
                response = response.with_header("sec-websocket-protocol", protocol.clone());
            }

            // ソケットが無い（TestClient）。確かめた 101 だけを返す。
            let Some(handoff) = self.handoff.take() else {
                return Ok(response);
            };
            if handoff.server.is_shutting_down() {
                return Err(unavailable("サーバーは停止の途中です"));
            }
            let seat = handoff
                .server
                .try_seat()
                .ok_or_else(|| unavailable("WebSocket の同時接続の上限に達しています"))?;
            // 切り替えの口が無い（hyper が切り替えを受け付けない要求）。
            // 席は返し、101 だけを返す。`server.rs` が 500 に直してログに残します。
            let Some(on_upgrade) = handoff.on_upgrade else {
                return Ok(response);
            };
            let callback: Callback = Box::new(move |socket| Box::pin(f(socket)));
            Ok(response.with_upgrade(Pending {
                on_upgrade,
                seat,
                settings: self.settings,
                callback,
            }))
        }
    }
}

/// `server.rs` が、応答を返す直前に呼ぶ。
///
/// | 応答 | 欄 | すること |
/// |---|---|---|
/// | 101 | ある | 切り替えを待つタスクを立てる |
/// | 101 | 無い | 500 にして `error!`（ミドルウェアが応答を作り直して落とした） |
/// | 101 以外 | ある | 切り替えない。席を返す |
pub(crate) fn after_response(response: Response, wants_json: bool) -> Response {
    let pending = response.take_upgrade();
    if response.status() != 101 {
        // 欄があれば、ここで捨てて席を返す。
        drop(pending);
        return response;
    }
    match pending {
        #[cfg(feature = "websocket")]
        Some(pending) => {
            io::launch(pending);
            response
        }
        #[cfg(not(feature = "websocket"))]
        Some(pending) => {
            drop(pending);
            response
        }
        None => {
            tracing::error!(
                "101 の応答に WebSocket の切り替えが載っていません。\
                 ミドルウェアが応答を作り直していないか確かめてください（500 にします）"
            );
            crate::http::response::error_response(
                &Error::msg("WebSocket の切り替えに失敗しました"),
                crate::http::response::RenderOptions::new(false, wants_json),
            )
        }
    }
}

// ---------------------------------------------------------------------------
// 接続
// ---------------------------------------------------------------------------

/// 切り替えた後の接続。`on_upgrade` のクロージャが受け取ります。
///
/// 閉じたら、入っていたチャンネルからは自動で外れます。
pub struct WebSocket {
    conn: Arc<Conn>,
    inbound: registry::InboundReceiver,
}

impl std::fmt::Debug for WebSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSocket")
            .field("id", &self.conn.id())
            .finish_non_exhaustive()
    }
}

impl WebSocket {
    pub(crate) fn new(conn: Arc<Conn>, inbound: registry::InboundReceiver) -> Self {
        Self { conn, inbound }
    }

    /// 接続の ID。プロセスの中で一意です。
    pub fn id(&self) -> ConnectionId {
        self.conn.id()
    }

    /// 次のメッセージを待つ。`None` は閉じたことを表します。
    ///
    /// Ping / Pong / Close はここに出ません（Ping には自動で Pong を返します）。
    /// 相手の Close の中身は、`None` の後に [`close_frame`](Self::close_frame) で読めます。
    /// 大きすぎるメッセージなどで閉じたときは、`None` の前に `Err` が 1 回来ます。
    pub async fn recv(&mut self) -> Option<Result<Message>> {
        self.inbound.recv().await
    }

    /// 文字を送る。待ちません（送信の列に積むだけ）。
    pub fn send_text(&self, text: impl Into<String>) -> Result<(), SendError> {
        self.conn.push(Outgoing::text(text.into()))
    }

    /// バイト列を送る。待ちません。
    pub fn send_binary(&self, bytes: impl Into<Vec<u8>>) -> Result<(), SendError> {
        self.conn.push(Outgoing::Binary(bytes.into()))
    }

    /// 別のタスクから送るための口。複製できます。
    pub fn sender(&self) -> WebSocketSender {
        WebSocketSender {
            conn: self.conn.clone(),
        }
    }

    /// Close を送り、相手の Close を待つ（最大 5 秒）。
    ///
    /// 送れない番号（1004〜1006、1015、1016〜2999、5000 以上、1000 未満）は 1000 に直します。
    /// 理由は 123 バイトを超えたら文字の境目で切ります。
    pub async fn close(&mut self, code: u16, reason: &str) {
        let mut done = self.conn.subscribe_done();
        self.conn.close(code, reason);
        // I/O のタスクが 5 秒で切るので、少しだけ長く待つ。
        let _ = tokio::time::timeout(
            CLOSE_WAIT + Duration::from_secs(1),
            done.wait_for(|done| *done),
        )
        .await;
    }

    /// 相手が送ってきた Close（番号と理由）。`recv` が `None` を返した後に読みます。
    pub fn close_frame(&self) -> Option<(u16, String)> {
        self.conn.peer_close()
    }

    /// 配信のチャンネルに入る。
    pub fn join(&self, channel: &str) {
        registry::join(self.conn.id(), channel);
    }

    /// 配信のチャンネルから出る。
    pub fn leave(&self, channel: &str) {
        registry::leave(self.conn.id(), channel);
    }

    /// 入っているチャンネル（名前の順）。
    pub fn channels(&self) -> Vec<String> {
        registry::channels(self.conn.id())
    }
}

/// 別のタスクから送るための口。`WebSocket::sender` で作ります。
#[derive(Clone)]
pub struct WebSocketSender {
    conn: Arc<Conn>,
}

impl std::fmt::Debug for WebSocketSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSocketSender")
            .field("id", &self.conn.id())
            .finish_non_exhaustive()
    }
}

impl WebSocketSender {
    /// 文字を送る。待ちません。
    pub fn send_text(&self, text: impl Into<String>) -> Result<(), SendError> {
        self.conn.push(Outgoing::text(text.into()))
    }

    /// バイト列を送る。待ちません。
    pub fn send_binary(&self, bytes: impl Into<Vec<u8>>) -> Result<(), SendError> {
        self.conn.push(Outgoing::Binary(bytes.into()))
    }

    /// 閉じるように頼む。待ちません。
    pub fn close(&self, code: u16, reason: &str) {
        self.conn.close(code, reason);
    }

    /// 閉じたか（閉じる途中も含む）。
    pub fn is_closed(&self) -> bool {
        self.conn.is_closed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

    fn upgrade_request() -> Request {
        Request::new("GET", "/ws").with_headers(vec![
            ("upgrade".into(), "websocket".into()),
            ("connection".into(), "keep-alive, Upgrade".into()),
            ("sec-websocket-version".into(), "13".into()),
            ("sec-websocket-key".into(), KEY.into()),
        ])
    }

    fn without(req: Request, name: &str) -> Request {
        let headers = req
            .headers()
            .iter()
            .filter(|(k, _)| k != name)
            .cloned()
            .collect();
        Request::new(req.method(), req.path()).with_headers(headers)
    }

    fn with(req: Request, name: &str, value: &str) -> Request {
        let mut headers: Vec<(String, String)> = req
            .headers()
            .iter()
            .filter(|(k, _)| k != name)
            .cloned()
            .collect();
        headers.push((name.into(), value.into()));
        Request::new(req.method(), req.path()).with_headers(headers)
    }

    #[test]
    fn 切り替えの要求か見分ける() {
        assert!(upgrade_request().wants_websocket());
        assert!(!without(upgrade_request(), "upgrade").wants_websocket());
        assert!(!without(upgrade_request(), "connection").wants_websocket());
        assert!(with(upgrade_request(), "upgrade", "WebSocket").wants_websocket());
        assert!(!with(upgrade_request(), "connection", "keep-alive").wants_websocket());
        assert!(!Request::new("GET", "/").wants_websocket());
    }

    #[test]
    fn 鍵の形を確かめる() {
        assert!(valid_key(KEY));
        assert!(valid_key("AAAAAAAAAAAAAAAAAAAAAA=="));
        assert!(!valid_key(""));
        assert!(!valid_key("dGhlIHNhbXBsZSBub25jZQ"), "== が無い");
        assert!(
            !valid_key("dGhlIHNhbXBsZSBub25jZR=="),
            "下位ビットが 0 でない"
        );
        assert!(
            !valid_key("dGhlIHNhbXBsZSBub25j*Q=="),
            "base64 の文字でない"
        );
        assert!(!valid_key("dGhlIHNhbXBsZSBub25jZQ==="));
    }

    #[test]
    fn 閉じる番号を直す() {
        for ok in [
            1000, 1001, 1003, 1007, 1008, 1009, 1011, 1014, 3000, 4001, 4999,
        ] {
            assert_eq!(close_code(ok), ok);
        }
        for bad in [0, 999, 1004, 1005, 1006, 1015, 1016, 2999, 5000, u16::MAX] {
            assert_eq!(close_code(bad), 1000, "{bad}");
        }
    }

    #[test]
    fn 理由は文字の境目で切る() {
        assert_eq!(close_reason("short"), "short");
        let long = "あ".repeat(50); // 150 バイト
        let cut = close_reason(&long);
        assert!(cut.len() <= MAX_REASON_BYTES);
        assert_eq!(cut, "あ".repeat(41), "123 バイトに入る 41 文字");
        let ascii = "a".repeat(200);
        assert_eq!(close_reason(&ascii).len(), 123);
    }

    #[test]
    fn 送れない理由の表示とエラーへの変換() {
        assert!(SendError::Closed.to_string().contains("閉じて"));
        assert!(SendError::QueueFull.to_string().contains("1008"));
        let error: Error = SendError::QueueFull.into();
        assert_eq!(error.status(), 500);
        fn takes_std_error(_: &dyn std::error::Error) {}
        takes_std_error(&SendError::Closed);
    }

    #[test]
    fn 接続idは数字で表示する() {
        assert_eq!(ConnectionId(42).to_string(), "42");
    }

    #[test]
    fn 置き場所は1回だけ取り出せる() {
        let slot = Slot::new(5);
        let copy = slot.clone();
        assert!(copy.is_filled());
        assert_eq!(slot.with(|v| *v), Some(5));
        assert_eq!(copy.take(), Some(5));
        assert_eq!(slot.take(), None, "複製と中身を共有する");
        assert!(!slot.is_filled());
        let empty: Slot<u8> = Slot::default();
        assert_eq!(empty.take(), None);
        assert_eq!(format!("{empty:?}"), "Slot");
    }

    #[tokio::test]
    async fn 席は上限まで取れて手放すと返る() {
        let state = ServerState::new(2);
        let a = state.try_seat().expect("1 つ目");
        let b = state.try_seat().expect("2 つ目");
        assert!(state.try_seat().is_none(), "上限");
        assert!(state.is_full());
        drop(a);
        assert_eq!(state.active(), 1);
        assert!(!state.wait_idle(Duration::from_millis(20)).await);
        let waiting = {
            let state = state.clone();
            tokio::spawn(async move { state.wait_idle(Duration::from_secs(5)).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(b);
        assert!(waiting.await.unwrap(), "0 になったら起きる");
        assert!(state.wait_idle(Duration::from_millis(1)).await);
    }

    #[tokio::test]
    async fn 接続が無ければ停止はすぐ終わる() {
        let state = ServerState::new(10);
        let started = std::time::Instant::now();
        shutdown(&state, Some(Duration::from_secs(30))).await;
        assert!(state.is_shutting_down());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn 停止は5秒より短い上限なら上限まで待つ() {
        let state = ServerState::new(10);
        let _seat = state.try_seat().unwrap();
        let started = std::time::Instant::now();
        shutdown(&state, Some(Duration::from_millis(50))).await;
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(50));
        assert!(took < Duration::from_secs(2), "{took:?}");
    }

    #[tokio::test]
    async fn 送り口は閉じた後に送れない() {
        let (conn, mut rx) = Conn::new(1024);
        let socket = WebSocket::new(conn.clone(), registry::inbound(1).1);
        let sender = socket.sender();
        socket.send_text("a").unwrap();
        sender.send_binary(vec![1, 2]).unwrap();
        assert!(matches!(rx.out.try_recv(), Ok(Outgoing::Text(t)) if t.as_str() == "a"));
        assert!(matches!(rx.out.try_recv(), Ok(Outgoing::Binary(b)) if b == vec![1, 2]));
        assert!(!sender.is_closed());
        sender.close(4000, "bye");
        assert!(sender.is_closed());
        assert_eq!(rx.ctrl.try_recv().ok(), Some((4000, "bye".to_string())));
        assert_eq!(socket.send_text("b"), Err(SendError::Closed));
        assert!(format!("{socket:?}").contains(&socket.id().to_string()));
    }

    #[cfg(not(feature = "websocket"))]
    #[test]
    fn フラグが無ければ足すように言う() {
        let error = upgrade_request().websocket().unwrap_err();
        assert_eq!(error.status(), 500);
        let text = error.to_string();
        assert!(text.contains("\"websocket\" を足してください"), "{text}");
    }

    #[cfg(feature = "websocket")]
    mod handshake {
        use super::*;
        use crate::http::Route;
        use crate::testing::TestClient;

        async fn gateway(req: Request) -> Result<Response> {
            req.websocket()?
                .protocols(&["chat.v2", "chat.v1"])
                .on_upgrade(|_socket| async {
                    panic!("TestClient ではクロージャを呼ばない");
                })
        }

        fn client() -> TestClient {
            TestClient::new(
                crate::application::Application::configure()
                    .with_routing(|r| {
                        r.web(|| {
                            Route::get("/ws", gateway);
                        })
                    })
                    .create(),
            )
        }

        const HEADERS: [(&str, &str); 4] = [
            ("upgrade", "websocket"),
            ("connection", "Upgrade"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", KEY),
        ];

        /// `name` を `value` に置き換えた（空なら外した）ヘッダー。
        fn headers_with(
            name: &'static str,
            value: &'static str,
        ) -> Vec<(&'static str, &'static str)> {
            let mut out: Vec<(&str, &str)> = HEADERS
                .iter()
                .filter(|(k, _)| *k != name)
                .copied()
                .collect();
            if !value.is_empty() {
                out.push((name, value));
            }
            out
        }

        #[tokio::test]
        async fn rfcの例の鍵で101を返す() {
            let response = client().send("GET", "/ws", Vec::new(), &HEADERS).await;
            response
                .assert_status(101)
                .assert_header("sec-websocket-accept", "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
                .assert_header("upgrade", "websocket")
                .assert_header("connection", "upgrade");
        }

        #[tokio::test]
        async fn サブプロトコルは相手の順で最初に一致したもの() {
            let headers = headers_with("sec-websocket-protocol", "chat.v1, chat.v2");
            let response = client().send("GET", "/ws", Vec::new(), &headers).await;
            response
                .assert_status(101)
                .assert_header("sec-websocket-protocol", "chat.v1");

            let headers = headers_with("sec-websocket-protocol", "other");
            let response = client().send("GET", "/ws", Vec::new(), &headers).await;
            assert_eq!(response.status(), 101);
            assert_eq!(response.header("sec-websocket-protocol"), None);
        }

        #[tokio::test]
        async fn 切り替えの要求でなければ400() {
            let client = client();
            for (name, value) in [
                ("upgrade", ""),
                ("upgrade", "h2c"),
                ("connection", ""),
                ("connection", "keep-alive"),
                ("sec-websocket-key", ""),
                ("sec-websocket-key", "short"),
            ] {
                let headers = headers_with(name, value);
                let response = client.send("GET", "/ws", Vec::new(), &headers).await;
                assert_eq!(response.status(), 400, "{name}: {value}");
            }
            // GET 以外は 400（ルートが GET だけなので、ここでは直接確かめる）。
            let req = with(upgrade_request(), "x", "y");
            let post = Request::new("POST", "/ws").with_headers(req.headers().to_vec());
            assert_eq!(post.websocket().unwrap_err().status(), 400);
        }

        #[tokio::test]
        async fn 版が13でなければ426と版のヘッダー() {
            for version in ["8", ""] {
                let headers = headers_with("sec-websocket-version", version);
                let response = client().send("GET", "/ws", Vec::new(), &headers).await;
                response
                    .assert_status(426)
                    .assert_header("sec-websocket-version", "13");
            }
        }

        #[test]
        fn 停止の途中と上限は503() {
            let state = ServerState::new(1);
            let handoff = || Handoff {
                on_upgrade: None,
                server: state.clone(),
            };
            // 上限。
            let seat = state.try_seat().unwrap();
            let req = upgrade_request().with_upgrade(handoff());
            assert_eq!(req.websocket().unwrap_err().status(), 503);
            drop(seat);
            assert!(upgrade_request()
                .with_upgrade(handoff())
                .websocket()
                .is_ok());
            // 停止の途中。
            state.begin_shutdown();
            let req = upgrade_request().with_upgrade(handoff());
            assert_eq!(req.websocket().unwrap_err().status(), 503);
        }

        #[test]
        fn 切り替えの口が無ければ101のまま席を返す() {
            let state = ServerState::new(1);
            let req = upgrade_request().with_upgrade(Handoff {
                on_upgrade: None,
                server: state.clone(),
            });
            let response = req.websocket().unwrap().on_upgrade(|_| async {}).unwrap();
            assert_eq!(response.status(), 101);
            assert_eq!(state.active(), 0, "席は返す");
            // server.rs はこれを 500 に直す。
            let fixed = after_response(response, false);
            assert_eq!(fixed.status(), 500);
        }

        #[test]
        fn 設定の既定と変更() {
            let upgrade = upgrade_request().websocket().unwrap();
            assert_eq!(upgrade.settings.max_message_size, 64 * 1024);
            assert_eq!(upgrade.settings.send_queue_limit, 1024 * 1024);
            assert_eq!(upgrade.settings.ping_interval, None);
            assert_eq!(upgrade.settings.idle_timeout, None);
            let upgrade = upgrade
                .max_message_size(10)
                .send_queue_limit(20)
                .ping_interval(Duration::from_secs(3))
                .idle_timeout(Duration::from_secs(4));
            assert_eq!(upgrade.settings.max_message_size, 10);
            assert_eq!(upgrade.settings.send_queue_limit, 20);
            assert_eq!(upgrade.settings.ping_interval, Some(Duration::from_secs(3)));
            assert_eq!(upgrade.settings.idle_timeout, Some(Duration::from_secs(4)));
            assert_eq!(upgrade.protocol(), None);
            assert!(format!("{upgrade:?}").contains("WebSocketUpgrade"));
        }
    }
}
