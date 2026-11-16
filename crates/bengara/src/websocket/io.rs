//! 切り替えた後のソケットの読み書き。機能フラグ `websocket` の内側だけです。
//!
//! 1 接続に **I/O のタスク 1 本**と、**アプリのクロージャのタスク 1 本**です。
//! I/O のタスクが `select!` で次のものを同時に待ちます。
//!
//! | 待つもの | すること |
//! |---|---|
//! | アプリのタスクの終わり | 1000 で閉じる（パニックなら 1011 と `error!`） |
//! | 閉じる要求（`close`・列あふれ・`Broadcast::disconnect`） | Close を送る |
//! | 停止の合図 | 1001 で閉じる |
//! | ソケットから読む | アプリの `recv` に渡す（列は 16 件。いっぱいなら読まない） |
//! | 送信の列 | ソケットへ書く |
//! | Ping の時計・無通信の時計 | Ping を送る・1000 で閉じる |
//!
//! アプリが `recv` を呼んでいない間も、Ping には Pong を返し、相手の Close にも応じます。

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinError, JoinHandle};
use tokio::time::{Instant, Interval};
use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Error as WsError, Message as WsMessage};
use tokio_tungstenite::WebSocketStream;

use super::registry::{self, Conn, Outgoing, Receivers};
use super::{Callback, Message, Pending, Seat, Settings, WebSocket, CLOSE_WAIT};
use crate::error::{Error, Result};

/// 受けたメッセージをアプリの `recv` に渡す列の長さ。
const INBOUND_QUEUE: usize = 16;

/// 読み込みの入れ物の大きさ。tungstenite の既定（128 KiB）は先に確保されるので、
/// 1 万本つなぐと 1 GiB を超えます。足りなければ伸びるので小さくしておきます。
const READ_BUFFER: usize = 8 * 1024;

/// 切り替えを待ってから、接続を動かすタスクを立てる。
pub(super) fn launch(pending: Pending) {
    tokio::spawn(async move {
        let Pending {
            on_upgrade,
            seat,
            settings,
            callback,
        } = pending;
        let upgraded = match on_upgrade.await {
            Ok(upgraded) => upgraded,
            Err(e) => {
                // 席は `seat` を捨てたときに返る。
                tracing::debug!("WebSocket への切り替えに失敗しました: {e}");
                return;
            }
        };
        let stream = WebSocketStream::from_raw_socket(
            hyper_util::rt::TokioIo::new(upgraded),
            Role::Server,
            Some(config(&settings)),
        )
        .await;
        serve(stream, seat, settings, callback).await;
    });
}

/// tungstenite の設定。メッセージとフレームの上限を同じにする。
pub(super) fn config(settings: &Settings) -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(READ_BUFFER)
        .max_message_size(Some(settings.max_message_size))
        .max_frame_size(Some(settings.max_message_size))
}

/// 1 本の接続を動かす。閉じるまで返りません。
pub(super) async fn serve<S>(
    stream: WebSocketStream<S>,
    seat: Seat,
    settings: Settings,
    callback: Callback,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (conn, receivers) = Conn::new(settings.send_queue_limit);
    let id = conn.id();
    registry::register(&conn);
    let (inbound, inbound_rx) = mpsc::channel(INBOUND_QUEUE);
    let started = std::time::Instant::now();
    tracing::debug!("WebSocket {id} がつながりました");

    let app = tokio::spawn(callback(WebSocket::new(conn.clone(), inbound_rx)));
    let io = Io {
        stream,
        conn: conn.clone(),
        receivers,
        inbound: Some(inbound),
        settings,
        shutdown: seat.server().subscribe_shutdown(),
        shutdown_seen: false,
        closing: None,
        code: None,
    };
    let (code, app) = io.run(app).await;

    // 後始末。表から外してから「終わった」を知らせる（`close` が返った時点で表に残らない）。
    registry::unregister(id);
    conn.finish();
    drop(seat);
    tracing::debug!(
        "WebSocket {id} が閉じました（{:.1} 秒、番号 {}）",
        started.elapsed().as_secs_f32(),
        code.map_or_else(|| "なし".to_string(), |code| code.to_string())
    );

    // ソケットは閉じたが、アプリのタスクがまだ動いている。`recv` は None を返すので
    // ふつうはすぐ終わる。パニックを見落とさないように最後まで待つ。
    if let Some(app) = app {
        if let Err(e) = app.await {
            report_panic(id, e);
        }
    }
}

/// アプリのタスクのパニックをログに出す。
fn report_panic(id: super::ConnectionId, error: JoinError) {
    if !error.is_panic() {
        return;
    }
    let payload = error.into_panic();
    let detail = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    tracing::error!("WebSocket {id} の処理がパニックしました: {detail}");
}

/// I/O のタスクの状態。
struct Io<S> {
    stream: WebSocketStream<S>,
    conn: std::sync::Arc<Conn>,
    receivers: Receivers,
    /// アプリの `recv` へ渡す口。閉じるときに捨てる（`recv` が None を返す）。
    inbound: Option<mpsc::Sender<Result<Message>>>,
    settings: Settings,
    shutdown: watch::Receiver<bool>,
    shutdown_seen: bool,
    /// こちらから Close を送った後、相手の Close を待つ期限。
    closing: Option<Instant>,
    /// 閉じた番号（ログ用）。
    code: Option<u16>,
}

/// 1 回の `select!` で起きたこと。
enum Step {
    App(std::result::Result<(), JoinError>),
    Close(u16, String),
    Shutdown,
    Read(
        Option<mpsc::OwnedPermit<Result<Message>>>,
        Option<std::result::Result<WsMessage, WsError>>,
    ),
    Write(Outgoing),
    Ping,
    Idle,
    CloseTimeout,
}

/// 書いている途中で割り込んだもの。
enum Interrupt {
    Close(u16, String),
    Shutdown,
}

/// ループを抜けるか続けるか。
enum Flow {
    Continue,
    Break,
}

impl<S> Io<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// 閉じるまで回す。閉じた番号と、まだ終わっていないアプリのタスクを返す。
    async fn run(mut self, app: JoinHandle<()>) -> (Option<u16>, Option<JoinHandle<()>>) {
        let mut app = Some(app);
        let mut ping = self.settings.ping_interval.map(|every| {
            let mut interval = tokio::time::interval_at(Instant::now() + every, every);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            interval
        });
        let mut idle_at = self.idle_deadline();

        loop {
            let step = {
                let discard = self.closing.is_some();
                let read = read_next(&mut self.stream, self.inbound.clone(), discard);
                tokio::select! {
                    biased;
                    result = join(&mut app) => Step::App(result),
                    Some((code, reason)) = self.receivers.ctrl.recv() => Step::Close(code, reason),
                    _ = self.shutdown.wait_for(|stop| *stop), if !self.shutdown_seen => Step::Shutdown,
                    (permit, frame) = read => Step::Read(permit, frame),
                    Some(item) = self.receivers.out.recv(), if !discard => Step::Write(item),
                    _ = tick(&mut ping), if !discard && ping.is_some() => Step::Ping,
                    _ = sleep_until(idle_at), if !discard && idle_at.is_some() => Step::Idle,
                    _ = sleep_until(self.closing), if discard => Step::CloseTimeout,
                }
            };

            let flow = match step {
                Step::App(result) => {
                    app = None;
                    // アプリが返る前に積んだものは、閉じる前に送り出す。
                    let code = match result {
                        Ok(()) => 1000,
                        Err(e) if e.is_panic() => {
                            report_panic(self.conn.id(), e);
                            1011
                        }
                        Err(_) => 1000,
                    };
                    match self.drain_queue().await {
                        Flow::Continue => self.start_close(code, "").await,
                        Flow::Break => Flow::Break,
                    }
                }
                Step::Close(code, reason) => {
                    // `close` の前に積んだものは送り出す。列があふれた（1008）ときは送らない。
                    let flow = if code == 1008 {
                        Flow::Continue
                    } else {
                        self.drain_queue().await
                    };
                    match flow {
                        Flow::Continue => self.start_close(code, &reason).await,
                        Flow::Break => Flow::Break,
                    }
                }
                Step::Shutdown => {
                    self.shutdown_seen = true;
                    self.start_close(1001, "server shutting down").await
                }
                Step::Read(permit, frame) => {
                    if matches!(frame, Some(Ok(_))) {
                        idle_at = self.idle_deadline();
                    }
                    self.on_read(permit, frame).await
                }
                Step::Write(item) => {
                    let len = item.len();
                    let flow = self.write(outgoing_message(item)).await;
                    self.conn.written(len);
                    flow
                }
                Step::Ping => self.write(WsMessage::Ping(Default::default())).await,
                Step::Idle => self.start_close(1000, "idle timeout").await,
                Step::CloseTimeout => Flow::Break,
            };
            if matches!(flow, Flow::Break) {
                break;
            }
        }
        (self.code, app)
    }

    fn idle_deadline(&self) -> Option<Instant> {
        self.settings
            .idle_timeout
            .map(|after| Instant::now() + after)
    }

    /// 1 通書く。相手が読まずに詰まっても、閉じる要求と停止の合図には応じる。
    async fn write(&mut self, message: WsMessage) -> Flow {
        let interrupt = tokio::select! {
            biased;
            result = self.stream.send(message) => {
                return match result {
                    Ok(()) => Flow::Continue,
                    Err(_) => Flow::Break,
                };
            }
            Some((code, reason)) = self.receivers.ctrl.recv() => Interrupt::Close(code, reason),
            _ = self.shutdown.wait_for(|stop| *stop), if !self.shutdown_seen => Interrupt::Shutdown,
        };
        match interrupt {
            Interrupt::Close(code, reason) => self.start_close(code, &reason).await,
            Interrupt::Shutdown => {
                self.shutdown_seen = true;
                self.start_close(1001, "server shutting down").await
            }
        }
    }

    /// 送信の列に残っているものを全部書く。Close を送った後は何もしない。
    async fn drain_queue(&mut self) -> Flow {
        while self.closing.is_none() {
            let Ok(item) = self.receivers.out.try_recv() else {
                break;
            };
            let len = item.len();
            let flow = self.write(outgoing_message(item)).await;
            self.conn.written(len);
            if matches!(flow, Flow::Break) {
                return Flow::Break;
            }
        }
        Flow::Continue
    }

    /// こちらから Close を送り、相手の Close を待つ状態にする。もう送っていれば何もしない。
    async fn start_close(&mut self, code: u16, reason: &str) -> Flow {
        if self.closing.is_some() {
            return Flow::Continue;
        }
        self.conn.mark_closed();
        let code = super::close_code(code);
        let frame = CloseFrame {
            code: code.into(),
            reason: super::close_reason(reason).into(),
        };
        self.code = Some(code);
        self.closing = Some(Instant::now() + CLOSE_WAIT);
        match tokio::time::timeout(CLOSE_WAIT, self.stream.send(WsMessage::Close(Some(frame))))
            .await
        {
            Ok(Ok(())) => Flow::Continue,
            // 送れない（相手が読まない・もう切れている）。待たずに切る。
            _ => Flow::Break,
        }
    }

    /// 読んだものを扱う。
    async fn on_read(
        &mut self,
        permit: Option<mpsc::OwnedPermit<Result<Message>>>,
        frame: Option<std::result::Result<WsMessage, WsError>>,
    ) -> Flow {
        match frame {
            Some(Ok(WsMessage::Text(text))) => {
                if let Some(permit) = permit {
                    permit.send(Ok(Message::Text(text.as_str().to_owned())));
                }
                Flow::Continue
            }
            Some(Ok(WsMessage::Binary(bytes))) => {
                if let Some(permit) = permit {
                    permit.send(Ok(Message::Binary(bytes.to_vec())));
                }
                Flow::Continue
            }
            Some(Ok(WsMessage::Ping(_))) => {
                // tungstenite が Pong を積んでいる。アプリが読んでいなくても返す。
                let _ = tokio::time::timeout(CLOSE_WAIT, self.stream.flush()).await;
                Flow::Continue
            }
            Some(Ok(WsMessage::Close(frame))) => {
                let peer = frame.map(|frame| (u16::from(frame.code), frame.reason.to_string()));
                if let Some((code, reason)) = &peer {
                    self.conn.set_peer_close(*code, reason.clone());
                }
                self.conn.mark_closed();
                if self.closing.is_none() {
                    // 相手から閉じた。tungstenite が積んだ Close の返事を送り出す。
                    self.code = Some(peer.as_ref().map_or(1005, |(code, _)| *code));
                    let _ = tokio::time::timeout(CLOSE_WAIT, self.stream.flush()).await;
                }
                Flow::Break
            }
            Some(Ok(_)) => Flow::Continue,
            Some(Err(error)) => self.on_error(permit, error).await,
            None => Flow::Break,
        }
    }

    /// 読み込みのエラー。番号を決めて閉じ、アプリには `Err` を 1 回渡す。
    async fn on_error(
        &mut self,
        permit: Option<mpsc::OwnedPermit<Result<Message>>>,
        error: WsError,
    ) -> Flow {
        let (code, reason, message) = match &error {
            WsError::Capacity(_) => (
                1009,
                "message too big",
                format!(
                    "受け取ったメッセージが大きすぎます（上限 {} バイト）",
                    self.settings.max_message_size
                ),
            ),
            WsError::Utf8(_) => (
                1007,
                "invalid utf-8",
                "受け取った文字が UTF-8 ではありません".to_string(),
            ),
            WsError::Protocol(_) => (
                1002,
                "protocol error",
                format!("WebSocket の決まりに合わないフレームを受け取りました: {error}"),
            ),
            // 切れた・読めない。もう送れないので、そのまま終わる。
            _ => return Flow::Break,
        };
        if let Some(permit) = permit {
            permit.send(Err(Error::msg(message)));
        }
        // アプリにはこの後 None を返す。
        self.inbound = None;
        match self.start_close(code, reason).await {
            Flow::Continue => Flow::Continue,
            Flow::Break => Flow::Break,
        }
    }
}

/// アプリの `recv` の列に空きができてから、ソケットを 1 通読む。
///
/// 列がいっぱいの間は読みません（TCP の押し戻しで相手を待たせる）。
/// 閉じる途中（`discard`）や、アプリが受け口を捨てたときは、読んで捨てます。
async fn read_next<S>(
    stream: &mut WebSocketStream<S>,
    inbound: Option<mpsc::Sender<Result<Message>>>,
    discard: bool,
) -> (
    Option<mpsc::OwnedPermit<Result<Message>>>,
    Option<std::result::Result<WsMessage, WsError>>,
)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let permit = match inbound {
        Some(inbound) if !discard => inbound.reserve_owned().await.ok(),
        _ => None,
    };
    (permit, stream.next().await)
}

/// 送信の列のものを tungstenite の形にする。
fn outgoing_message(item: Outgoing) -> WsMessage {
    match item {
        Outgoing::Text(text) => WsMessage::Text(text.into()),
        Outgoing::Binary(bytes) => WsMessage::Binary(bytes.into()),
    }
}

/// アプリのタスクの終わりを待つ。終わった後は永久に待つ。
async fn join(app: &mut Option<JoinHandle<()>>) -> std::result::Result<(), JoinError> {
    match app {
        Some(handle) => handle.await,
        None => std::future::pending().await,
    }
}

/// Ping の時計。無ければ永久に待つ。
async fn tick(ping: &mut Option<Interval>) {
    match ping {
        Some(interval) => {
            interval.tick().await;
        }
        None => std::future::pending().await,
    }
}

/// 期限まで待つ。無ければ永久に待つ。
async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// 待つ長さの上限（テストで使う）。
#[cfg(test)]
const TEST_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(test)]
mod tests {
    use super::super::{registry, ServerState};
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::DuplexStream;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

    type Client = WebSocketStream<DuplexStream>;

    /// ソケットの代わりに duplex で 1 本つなぐ。HTTP を通さずに I/O のタスクを確かめる。
    async fn pair_with<F, Fut>(
        settings: Settings,
        buffer: usize,
        f: F,
    ) -> (Client, Arc<ServerState>, JoinHandle<()>)
    where
        F: FnOnce(WebSocket) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let (server_side, client_side) = tokio::io::duplex(buffer);
        let state = ServerState::new(10);
        let seat = state.try_seat().expect("席");
        let server =
            WebSocketStream::from_raw_socket(server_side, Role::Server, Some(config(&settings)))
                .await;
        let client = WebSocketStream::from_raw_socket(client_side, Role::Client, None).await;
        let callback: Callback = Box::new(move |socket| Box::pin(f(socket)));
        let task = tokio::spawn(serve(server, seat, settings, callback));
        (client, state, task)
    }

    async fn pair<F, Fut>(f: F) -> (Client, Arc<ServerState>, JoinHandle<()>)
    where
        F: FnOnce(WebSocket) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        pair_with(Settings::default(), 256 * 1024, f).await
    }

    /// 送り返すだけのアプリ。
    async fn echo(mut socket: WebSocket) {
        while let Some(Ok(message)) = socket.recv().await {
            let _ = match message {
                Message::Text(text) => socket.send_text(text),
                Message::Binary(bytes) => socket.send_binary(bytes),
            };
        }
    }

    /// 次のデータか Close を読む（Ping / Pong は飛ばす）。
    async fn next(client: &mut Client) -> Option<WsMessage> {
        loop {
            let message = tokio::time::timeout(TEST_WAIT, client.next())
                .await
                .expect("時間内に届く")?
                .ok()?;
            if !matches!(message, WsMessage::Ping(_) | WsMessage::Pong(_)) {
                return Some(message);
            }
        }
    }

    /// Close を受け取るまで読み、その番号を返す。
    async fn close_code_of(client: &mut Client) -> Option<u16> {
        loop {
            match next(client).await? {
                WsMessage::Close(frame) => {
                    // もう一度読むと、tungstenite のクライアントが Close の返事を送り出す。
                    let _ = tokio::time::timeout(TEST_WAIT, client.next()).await;
                    return frame.map(|f| u16::from(f.code));
                }
                _ => continue,
            }
        }
    }

    async fn finished(task: JoinHandle<()>) {
        tokio::time::timeout(TEST_WAIT, task)
            .await
            .expect("接続のタスクが終わる")
            .expect("パニックしない");
    }

    #[tokio::test]
    async fn 文字とバイト列を送り返す() {
        let (mut client, state, task) = pair(echo).await;
        assert_eq!(state.active(), 1);
        client.send(WsMessage::text("こんにちは")).await.unwrap();
        assert_eq!(next(&mut client).await, Some(WsMessage::text("こんにちは")));
        client.send(WsMessage::binary(vec![1, 2, 3])).await.unwrap();
        assert_eq!(
            next(&mut client).await,
            Some(WsMessage::binary(vec![1, 2, 3]))
        );

        // Ping には Pong が返る（アプリには見せない）。
        client.send(WsMessage::Ping(vec![9].into())).await.unwrap();
        let pong = tokio::time::timeout(TEST_WAIT, client.next())
            .await
            .unwrap();
        assert!(matches!(pong, Some(Ok(WsMessage::Pong(p))) if p.as_ref() == [9]));

        client.close(None).await.unwrap();
        finished(task).await;
        assert_eq!(state.active(), 0, "席は返る");
    }

    #[tokio::test]
    async fn アプリがrecvを呼ばなくてもpingに応じる() {
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let (mut client, _state, task) = pair(|_socket| async move {
            let _ = wait.await;
        })
        .await;
        client.send(WsMessage::Ping(vec![1].into())).await.unwrap();
        let pong = tokio::time::timeout(TEST_WAIT, client.next())
            .await
            .unwrap();
        assert!(matches!(pong, Some(Ok(WsMessage::Pong(_)))));
        let _ = release.send(());
        assert_eq!(close_code_of(&mut client).await, Some(1000));
        finished(task).await;
    }

    #[tokio::test]
    async fn アプリが返ったら1000で閉じる() {
        let (mut client, _state, task) = pair(|socket| async move {
            socket.send_text("bye").unwrap();
        })
        .await;
        assert_eq!(next(&mut client).await, Some(WsMessage::text("bye")));
        assert_eq!(close_code_of(&mut client).await, Some(1000));
        finished(task).await;
    }

    #[tokio::test]
    async fn パニックは1011で閉じてほかの接続は生きている() {
        let (mut other, _s, other_task) = pair(echo).await;
        let (mut client, _state, task) = pair(|_socket| async move {
            panic!("わざと");
        })
        .await;
        assert_eq!(close_code_of(&mut client).await, Some(1011));
        finished(task).await;

        other.send(WsMessage::text("alive")).await.unwrap();
        assert_eq!(next(&mut other).await, Some(WsMessage::text("alive")));
        other.close(None).await.unwrap();
        finished(other_task).await;
    }

    #[tokio::test]
    async fn 大きすぎるメッセージは1009で閉じてアプリにerrを渡す() {
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        let settings = Settings {
            max_message_size: 16,
            ..Settings::default()
        };
        let (mut client, _state, task) = pair_with(settings, 64 * 1024, |mut socket| async move {
            let first = socket.recv().await;
            let then = socket.recv().await;
            let _ = seen_tx.send((matches!(first, Some(Err(_))), then.is_none()));
        })
        .await;
        client.send(WsMessage::text("x".repeat(100))).await.unwrap();
        assert_eq!(close_code_of(&mut client).await, Some(1009));
        let (got_err, then_none) = seen_rx.await.unwrap();
        assert!(got_err, "Err が 1 回来る");
        assert!(then_none, "その後は None");
        finished(task).await;
    }

    #[tokio::test]
    async fn 読まない相手は列があふれて1008で閉じる() {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let settings = Settings {
            send_queue_limit: 4 * 1024,
            ..Settings::default()
        };
        // duplex の入れ物を小さくして、相手が読まないと書けないようにする。
        let (mut client, _state, task) = pair_with(settings, 512, |socket| async move {
            let chunk = "x".repeat(1024);
            let mut result = Ok(());
            for _ in 0..1000 {
                result = socket.send_text(chunk.clone());
                if result.is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
            let _ = result_tx.send(result);
            // 閉じた後は送れない。
            assert_eq!(
                socket.send_text("late"),
                Err(super::super::SendError::Closed)
            );
        })
        .await;
        assert_eq!(
            result_rx.await.unwrap(),
            Err(super::super::SendError::QueueFull)
        );
        assert_eq!(close_code_of(&mut client).await, Some(1008));
        finished(task).await;
    }

    #[tokio::test]
    async fn 無通信なら1000で閉じる() {
        let settings = Settings {
            idle_timeout: Some(Duration::from_millis(100)),
            ..Settings::default()
        };
        let (mut client, _state, task) = pair_with(settings, 64 * 1024, echo).await;
        let started = std::time::Instant::now();
        let frame = loop {
            match next(&mut client).await {
                Some(WsMessage::Close(frame)) => break frame,
                Some(_) => continue,
                None => panic!("Close が来る"),
            }
        };
        assert!(started.elapsed() >= Duration::from_millis(100));
        let frame = frame.expect("番号つき");
        assert_eq!(frame.code, CloseCode::Normal);
        assert_eq!(frame.reason.as_str(), "idle timeout");
        let _ = client.next().await;
        finished(task).await;
    }

    #[tokio::test]
    async fn pingの間隔を決めるとサーバーがpingを送る() {
        let settings = Settings {
            ping_interval: Some(Duration::from_millis(50)),
            ..Settings::default()
        };
        let (mut client, _state, task) = pair_with(settings, 64 * 1024, echo).await;
        let message = tokio::time::timeout(TEST_WAIT, client.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(message, WsMessage::Ping(_)), "{message:?}");
        client.close(None).await.unwrap();
        finished(task).await;
    }

    #[tokio::test]
    async fn 相手のcloseでrecvがnoneになり中身を読める() {
        let (frame_tx, frame_rx) = tokio::sync::oneshot::channel();
        let (mut client, _state, task) = pair(|mut socket| async move {
            while socket.recv().await.is_some() {}
            let _ = frame_tx.send(socket.close_frame());
        })
        .await;
        client
            .close(Some(CloseFrame {
                code: CloseCode::from(4001),
                reason: "bye".into(),
            }))
            .await
            .unwrap();
        assert_eq!(frame_rx.await.unwrap(), Some((4001, "bye".to_string())));
        finished(task).await;
    }

    #[tokio::test]
    async fn closeは番号を直して送り相手の返事を待つ() {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let (mut client, _state, task) = pair(|mut socket| async move {
            socket.close(1005, "x").await; // 送れない番号は 1000 に直す
            let _ = done_tx.send(socket.send_text("late"));
        })
        .await;
        assert_eq!(close_code_of(&mut client).await, Some(1000));
        // 相手（tungstenite のクライアント）は読んだ時点で Close を返している。
        let _ = client.next().await;
        assert_eq!(done_rx.await.unwrap(), Err(super::super::SendError::Closed));
        finished(task).await;

        let (mut client, _state, task) = pair(|mut socket| async move {
            // close の前に積んだものは、Close より先に届く。
            socket.send_text("last").unwrap();
            socket.close(4000, "app").await;
        })
        .await;
        assert_eq!(next(&mut client).await, Some(WsMessage::text("last")));
        assert_eq!(close_code_of(&mut client).await, Some(4000));
        let _ = client.next().await;
        finished(task).await;
    }

    #[tokio::test]
    async fn 停止の合図で1001を送る() {
        let (mut client, state, task) = pair(echo).await;
        state.begin_shutdown();
        assert_eq!(close_code_of(&mut client).await, Some(1001));
        let _ = client.next().await;
        finished(task).await;
        assert!(state.wait_idle(Duration::from_millis(10)).await);
    }

    #[tokio::test]
    async fn 閉じた後はチャンネルの表に残らない() {
        let channel = format!("test.io.cleanup.{}", std::process::id());
        let (id_tx, id_rx) = tokio::sync::oneshot::channel();
        let joined = channel.clone();
        let (mut client, _state, task) = pair(move |mut socket| async move {
            socket.join(&joined);
            socket.join("test.io.cleanup.second");
            let _ = id_tx.send(socket.id());
            while socket.recv().await.is_some() {}
        })
        .await;
        let id = id_rx.await.unwrap();
        assert!(registry::is_registered(id));
        assert!(registry::count(&channel) >= 1);

        // 配信が届く。
        registry::deliver(&channel, "to-channel");
        assert_eq!(next(&mut client).await, Some(WsMessage::text("to-channel")));

        client.close(None).await.unwrap();
        finished(task).await;
        assert!(!registry::is_registered(id));
        assert!(registry::channels(id).is_empty());
        assert_eq!(registry::count(&channel), 0);
    }

    #[tokio::test]
    async fn チャンネルを閉じると指定の番号で閉じる() {
        let channel = format!("test.io.disconnect.{}", std::process::id());
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let joined = channel.clone();
        let (mut client, _state, task) = pair(move |mut socket| async move {
            socket.join(&joined);
            let _ = ready_tx.send(());
            while socket.recv().await.is_some() {}
        })
        .await;
        ready_rx.await.unwrap();
        assert_eq!(
            registry::close_channel(&channel, 4001, "session revoked"),
            1
        );
        let frame = loop {
            if let Some(WsMessage::Close(frame)) = next(&mut client).await {
                break frame.expect("番号つき");
            }
        };
        assert_eq!(u16::from(frame.code), 4001);
        assert_eq!(frame.reason.as_str(), "session revoked");
        let _ = client.next().await;
        finished(task).await;
    }

    #[tokio::test]
    async fn 別のタスクからsenderで送れる() {
        let (mut client, _state, task) = pair(|mut socket| async move {
            let sender = socket.sender();
            tokio::spawn(async move {
                sender.send_text("from-task").unwrap();
            });
            while socket.recv().await.is_some() {}
        })
        .await;
        assert_eq!(next(&mut client).await, Some(WsMessage::text("from-task")));
        client.close(None).await.unwrap();
        finished(task).await;
    }
}
