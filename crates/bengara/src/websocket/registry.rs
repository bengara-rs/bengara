//! 接続の表。接続 ID → 送り口、チャンネル → 接続 ID の集合。
//!
//! **プロセスに 1 つです。** `Broadcast::publish` はどこからでも呼べる関数なので、
//! 表もプロセスで共有します。
//!
//! ロックは 1 つだけです（`RwLock`）。接続・切断・join・leave で書き、
//! publish・local_count で読みます。**送るのはロックを放してから**です
//! （送り口を `Arc` で写してから、表の外で積みます）。
//! 接続とチャンネルはリクエストの処理中に増減するので、起動時に固定できません
//! （CLAUDE.md のロックの表）。
//!
//! この表と送り口（[`Conn`]）は機能フラグ `websocket` が無くてもコンパイルします。
//! ソケットの読み書きだけがフラグの内側です（`io.rs`）。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use tokio::sync::{mpsc, watch};

use super::{ConnectionId, SendError};

/// 送信の列に積むもの。
#[derive(Debug)]
pub(crate) enum Outgoing {
    Text(String),
    Binary(Vec<u8>),
}

impl Outgoing {
    /// 列の上限に数えるバイト数。
    pub(crate) fn len(&self) -> usize {
        match self {
            Outgoing::Text(text) => text.len(),
            Outgoing::Binary(bytes) => bytes.len(),
        }
    }
}

/// 1 本の接続の送り口。アプリ・配信・I/O のタスクが共有します。
///
/// ロックは持ちません。送信の列と閉じる要求は tokio の列、数と状態は原子変数です。
pub(crate) struct Conn {
    id: ConnectionId,
    /// 送信の列（Text / Binary）。上限はバイト数で数える（`queued`）。
    out: mpsc::UnboundedSender<Outgoing>,
    /// 閉じる要求。送信の列とは別にして、列が詰まっていても先に届くようにする。
    ctrl: mpsc::UnboundedSender<(u16, String)>,
    /// 列に積んで、まだソケットへ書いていないバイト数。
    queued: AtomicUsize,
    /// 列の上限（バイト）。
    limit: usize,
    /// 閉じる途中か、閉じたか。立った後は送れない。
    closed: AtomicBool,
    /// 相手が送ってきた Close（番号と理由）。
    peer_close: OnceLock<(u16, String)>,
    /// I/O のタスクが終わったか。`WebSocket::close` が待つ。
    done: watch::Sender<bool>,
}

/// 接続の側（I/O のタスク）が持つ受け口。
pub(crate) struct Receivers {
    pub(crate) out: mpsc::UnboundedReceiver<Outgoing>,
    pub(crate) ctrl: mpsc::UnboundedReceiver<(u16, String)>,
}

/// 接続 ID の連番。プロセスの中で一意です。
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

impl Conn {
    /// 送り口を作る。表にはまだ載せない（[`register`]）。
    pub(crate) fn new(limit: usize) -> (Arc<Self>, Receivers) {
        let (out, out_rx) = mpsc::unbounded_channel();
        let (ctrl, ctrl_rx) = mpsc::unbounded_channel();
        let conn = Arc::new(Self {
            id: ConnectionId(NEXT_ID.fetch_add(1, Ordering::Relaxed)),
            out,
            ctrl,
            queued: AtomicUsize::new(0),
            limit,
            closed: AtomicBool::new(false),
            peer_close: OnceLock::new(),
            done: watch::channel(false).0,
        });
        (
            conn,
            Receivers {
                out: out_rx,
                ctrl: ctrl_rx,
            },
        )
    }

    pub(crate) fn id(&self) -> ConnectionId {
        self.id
    }

    /// 送信の列に積む。待たない。
    ///
    /// 上限を超えたら、その場で 1008 の Close を頼んで `QueueFull` を返します。
    pub(crate) fn push(&self, item: Outgoing) -> Result<(), SendError> {
        if self.is_closed() {
            return Err(SendError::Closed);
        }
        let len = item.len();
        let before = self.queued.fetch_add(len, Ordering::AcqRel);
        if before.saturating_add(len) > self.limit {
            self.queued.fetch_sub(len, Ordering::AcqRel);
            tracing::warn!(
                "WebSocket {} の送信の列があふれたので、1008 で閉じます",
                self.id
            );
            self.close(1008, "send queue full");
            return Err(SendError::QueueFull);
        }
        if self.out.send(item).is_err() {
            self.queued.fetch_sub(len, Ordering::AcqRel);
            return Err(SendError::Closed);
        }
        Ok(())
    }

    /// ソケットへ書き終えた分を列の数から引く。I/O のタスクが呼びます。
    pub(crate) fn written(&self, len: usize) {
        self.queued.fetch_sub(len, Ordering::AcqRel);
    }

    /// 閉じるように頼む。待たない。2 回目からは何もしない。
    ///
    /// 番号と理由は、送れる形に直してから頼みます（[`super::close_code`]）。
    pub(crate) fn close(&self, code: u16, reason: &str) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let code = super::close_code(code);
        let reason = super::close_reason(reason);
        // 受け手がいない（I/O が終わった）ときは、もう閉じている。
        let _ = self.ctrl.send((code, reason));
    }

    /// 閉じる途中か、閉じたか。
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// 閉じたことにする（I/O のタスクが終わったとき）。
    pub(crate) fn mark_closed(&self) {
        self.closed.store(true, Ordering::Release);
    }

    /// 相手の Close を覚える。1 回だけ。
    pub(crate) fn set_peer_close(&self, code: u16, reason: String) {
        let _ = self.peer_close.set((code, reason));
    }

    /// 相手の Close。
    pub(crate) fn peer_close(&self) -> Option<(u16, String)> {
        self.peer_close.get().cloned()
    }

    /// I/O のタスクが終わったことを知らせる。
    pub(crate) fn finish(&self) {
        self.mark_closed();
        self.done.send_replace(true);
    }

    /// I/O のタスクの終わりを待つための受け口。
    pub(crate) fn subscribe_done(&self) -> watch::Receiver<bool> {
        self.done.subscribe()
    }

    /// 列に積んであるバイト数（テスト用）。
    #[cfg(test)]
    pub(crate) fn queued(&self) -> usize {
        self.queued.load(Ordering::Acquire)
    }
}

/// 表の中身。
#[derive(Default)]
struct Table {
    conns: HashMap<ConnectionId, Member>,
    channels: HashMap<String, HashSet<ConnectionId>>,
}

/// 表に載っている接続 1 本。
struct Member {
    conn: Arc<Conn>,
    /// 入っているチャンネル（`channels()` で名前の順に返すため BTreeSet）。
    channels: BTreeSet<String>,
}

fn table() -> &'static RwLock<Table> {
    static TABLE: OnceLock<RwLock<Table>> = OnceLock::new();
    TABLE.get_or_init(|| RwLock::new(Table::default()))
}

fn read() -> std::sync::RwLockReadGuard<'static, Table> {
    table().read().unwrap_or_else(|e| e.into_inner())
}

fn write() -> std::sync::RwLockWriteGuard<'static, Table> {
    table().write().unwrap_or_else(|e| e.into_inner())
}

/// 接続を表に載せる。
pub(crate) fn register(conn: &Arc<Conn>) {
    write().conns.insert(
        conn.id(),
        Member {
            conn: conn.clone(),
            channels: BTreeSet::new(),
        },
    );
}

/// 接続を表から外す。入っていたチャンネルからも漏れなく外します。
pub(crate) fn unregister(id: ConnectionId) {
    let mut table = write();
    let Some(member) = table.conns.remove(&id) else {
        return;
    };
    for channel in member.channels {
        remove_from_channel(&mut table, &channel, id);
    }
}

/// チャンネルの集合から 1 本外す。空になったらチャンネルごと消す（表が育たないように）。
fn remove_from_channel(table: &mut Table, channel: &str, id: ConnectionId) {
    if let Some(ids) = table.channels.get_mut(channel) {
        ids.remove(&id);
        if ids.is_empty() {
            table.channels.remove(channel);
        }
    }
}

/// チャンネルに入る。表に無い（閉じた）接続なら何もしない。
///
/// 名前は `Broadcast::publish` と同じ規則で確かめる（空・255 バイト超は入れない）。
/// 送る側が断る名前で入ると、何も届かないチャンネルに黙って入ってしまうため。
/// `join` は戻り値を持たないので、警告を出して入らない。
pub(crate) fn join(id: ConnectionId, channel: &str) {
    if let Err(error) = crate::broadcast::check_channel(channel) {
        tracing::warn!("チャンネルに入れませんでした: {error}");
        return;
    }
    let mut table = write();
    let Some(member) = table.conns.get_mut(&id) else {
        return;
    };
    if !member.channels.insert(channel.to_string()) {
        return;
    }
    table
        .channels
        .entry(channel.to_string())
        .or_default()
        .insert(id);
}

/// チャンネルから出る。
pub(crate) fn leave(id: ConnectionId, channel: &str) {
    let mut table = write();
    let Some(member) = table.conns.get_mut(&id) else {
        return;
    };
    if !member.channels.remove(channel) {
        return;
    }
    remove_from_channel(&mut table, channel, id);
}

/// 入っているチャンネル（名前の順）。
pub(crate) fn channels(id: ConnectionId) -> Vec<String> {
    read()
        .conns
        .get(&id)
        .map(|member| member.channels.iter().cloned().collect())
        .unwrap_or_default()
}

/// チャンネルに入っている接続の送り口を写す。ロックはこの中だけで持ちます。
fn members(channel: &str) -> Vec<Arc<Conn>> {
    let table = read();
    let Some(ids) = table.channels.get(channel) else {
        return Vec::new();
    };
    ids.iter()
        .filter_map(|id| table.conns.get(id).map(|member| member.conn.clone()))
        .collect()
}

/// このプロセスで、そのチャンネルに入っている接続の数。
pub(crate) fn count(channel: &str) -> usize {
    read().channels.get(channel).map_or(0, HashSet::len)
}

/// チャンネルの全接続へ送る。送るのはロックを放してから。
///
/// 列がいっぱいの接続は、その接続だけが 1008 で閉じます（`Conn::push`）。
/// ほかの接続への配信は止めません。返すのは積めた本数です。
pub(crate) fn deliver(channel: &str, text: &str) -> usize {
    members(channel)
        .into_iter()
        .filter(|conn| conn.push(Outgoing::Text(text.to_string())).is_ok())
        .count()
}

/// チャンネルの全接続を閉じる。
pub(crate) fn close_channel(channel: &str, code: u16, reason: &str) -> usize {
    let members = members(channel);
    for conn in &members {
        conn.close(code, reason);
    }
    members.len()
}

/// 表に載っているか（テスト用）。
#[cfg(test)]
pub(crate) fn is_registered(id: ConnectionId) -> bool {
    read().conns.contains_key(&id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テストごとに違うチャンネル名を作る（表はプロセスで共有なので、並列のテストとぶつけない）。
    fn unique(name: &str) -> String {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        format!(
            "test.registry.{name}.{}",
            SEQ.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn 入っている接続だけに届く() {
        let channel = unique("deliver");
        let (a, mut a_rx) = Conn::new(1024);
        let (b, mut b_rx) = Conn::new(1024);
        register(&a);
        register(&b);
        join(a.id(), &channel);

        assert_eq!(deliver(&channel, "hello"), 1);
        assert!(matches!(a_rx.out.try_recv(), Ok(Outgoing::Text(t)) if t == "hello"));
        assert!(b_rx.out.try_recv().is_err(), "入っていない接続には届かない");
        assert_eq!(count(&channel), 1);

        unregister(a.id());
        unregister(b.id());
    }

    #[test]
    fn 外すとチャンネルにも残らない() {
        let channel = unique("cleanup");
        let other = unique("cleanup-other");
        let (a, _rx) = Conn::new(1024);
        register(&a);
        join(a.id(), &channel);
        join(a.id(), &other);
        join(a.id(), &channel); // 2 回目は何もしない
        assert_eq!(channels(a.id()), {
            let mut v = vec![channel.clone(), other.clone()];
            v.sort();
            v
        });
        assert_eq!(count(&channel), 1);

        unregister(a.id());
        assert!(!is_registered(a.id()));
        assert_eq!(count(&channel), 0);
        assert_eq!(count(&other), 0);
        assert!(
            !read().channels.contains_key(&channel),
            "空のチャンネルは表から消える"
        );
        assert!(channels(a.id()).is_empty());

        // 閉じた後の join は何もしない。
        join(a.id(), &channel);
        assert_eq!(count(&channel), 0);
    }

    #[test]
    fn leaveで出られる() {
        let channel = unique("leave");
        let (a, _rx) = Conn::new(1024);
        register(&a);
        join(a.id(), &channel);
        leave(a.id(), &channel);
        leave(a.id(), &channel); // 2 回目は何もしない
        assert_eq!(count(&channel), 0);
        assert!(channels(a.id()).is_empty());
        unregister(a.id());
    }

    #[test]
    fn 送れない名前のチャンネルには入らない() {
        // `publish` が断る名前（空・255 バイト超）に入っても、何も届かない。
        let (a, _rx) = Conn::new(1024);
        register(&a);
        join(a.id(), "");
        join(a.id(), &"a".repeat(256));
        assert!(channels(a.id()).is_empty());

        // 255 バイトちょうどは入れる。
        let longest = "a".repeat(255);
        join(a.id(), &longest);
        assert_eq!(channels(a.id()), vec![longest]);
        unregister(a.id());
    }

    #[test]
    fn 列があふれた接続だけを閉じてほかには届ける() {
        let channel = unique("overflow");
        let (small, mut small_rx) = Conn::new(8);
        let (big, mut big_rx) = Conn::new(1024);
        register(&small);
        register(&big);
        join(small.id(), &channel);
        join(big.id(), &channel);

        assert_eq!(deliver(&channel, "12345"), 2);
        // 2 通目で小さい接続はあふれる。大きいほうには届く。
        assert_eq!(deliver(&channel, "67890"), 1);
        assert!(small.is_closed());
        assert_eq!(
            small_rx.ctrl.try_recv().ok(),
            Some((1008, "send queue full".to_string()))
        );
        assert_eq!(small.queued(), 5, "あふれた分は数えない");
        assert!(matches!(big_rx.out.try_recv(), Ok(Outgoing::Text(t)) if t == "12345"));
        assert!(matches!(big_rx.out.try_recv(), Ok(Outgoing::Text(t)) if t == "67890"));
        // 閉じた後は送れない。
        assert_eq!(
            small.push(Outgoing::Text("x".into())),
            Err(SendError::Closed)
        );
        let _ = small_rx.out.try_recv();

        unregister(small.id());
        unregister(big.id());
    }

    #[test]
    fn 書き終えた分は列の数から引く() {
        let (a, _rx) = Conn::new(10);
        a.push(Outgoing::Binary(vec![0; 6])).unwrap();
        assert_eq!(a.queued(), 6);
        a.written(6);
        a.push(Outgoing::Binary(vec![0; 10])).unwrap();
        assert_eq!(a.queued(), 10);
    }

    #[test]
    fn 閉じる要求は1回だけで番号を直す() {
        let channel = unique("close");
        let (a, mut a_rx) = Conn::new(1024);
        register(&a);
        join(a.id(), &channel);
        assert_eq!(close_channel(&channel, 1006, "x"), 1);
        assert_eq!(close_channel(&channel, 4001, "y"), 1);
        assert_eq!(a_rx.ctrl.try_recv().ok(), Some((1000, "x".to_string())));
        assert!(a_rx.ctrl.try_recv().is_err(), "2 回目は頼まない");
        unregister(a.id());
    }

    #[test]
    fn 接続idは重ならない() {
        let (a, _) = Conn::new(1);
        let (b, _) = Conn::new(1);
        assert_ne!(a.id(), b.id());
    }
}
