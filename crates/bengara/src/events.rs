//! イベント。出来事に後から処理を足せるようにします。
//!
//! ```ignore
//! // bootstrap/app.rs
//! .with_events(|e| e.listen("user.registered", SendWelcome::handle))
//!
//! // どこからでも
//! Event::dispatch("user.registered", &payload).await?;
//! ```
//!
//! **起動時に登録します。** 実行中に増えません。
//! 聞く側の形はジョブと同じ `async fn handle(payload: String) -> Result<()>` です。
//!
//! ## 組み立て直すと置き換わる
//!
//! 表はプロセスに 1 つです。`Application` を組み立てるたびに**丸ごと
//! 置き換わり**、前の登録は消えます。本番では起動時に 1 回なので問題に
//! なりませんが、テストでは効きます。`cargo test` はテストを同じプロセスの
//! 別スレッドで並べるので、`with_events` の中身が違うアプリを 2 件の
//! テストが組むと、後から組んだほうの表で `Event::dispatch` が動きます。
//! 聞く側が居なくても `dispatch` は成功するので、**黙って 0 件**になり、
//! 実行順で結果が変わります。
//!
//! `with_events` を既定と違う形で組み、かつ `Event::dispatch` 系を使う
//! テストは `testing::exclusive()` の札を取ってください。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock, RwLock};

use crate::error::{Error, Result};

/// 聞く側が返す非同期の結果。
pub type ListenerFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

/// 聞く側の関数。`async fn handle(payload: String) -> Result<()>` が入ります。
///
/// 受け取るのは `String` です。`Arc<str>` を渡す形も試しましたが、
/// 聞く側が `String` を取るかぎり中で作り直すことになり、**確保の回数は同じ**でした。
/// 公開しているトレイトの形を変えるだけの得が無いので、`String` のままにしています。
pub trait Listener: Send + Sync + 'static {
    /// 出来事を受け取る。
    fn call(&self, payload: String) -> ListenerFuture;
}

impl<F, Fut> Listener for F
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    fn call(&self, payload: String) -> ListenerFuture {
        Box::pin(self(payload))
    }
}

/// 登録の入れ物。`with_events` のクロージャが受け取ります。
#[derive(Default)]
pub struct Events {
    listeners: HashMap<String, Vec<Arc<dyn Listener>>>,
}

impl Events {
    /// 空の入れ物を作る。
    pub fn new() -> Self {
        Self::default()
    }

    /// 出来事の名前に、聞く側を足す。
    ///
    /// 同じ名前に何回でも足せます。**登録した順**に呼ばれます。
    pub fn listen<L: Listener>(mut self, event: &str, listener: L) -> Self {
        self.listeners
            .entry(event.to_string())
            .or_default()
            .push(Arc::new(listener));
        self
    }

    /// 登録されている出来事の名前（並び順は不定）。
    pub fn names(&self) -> Vec<&str> {
        self.listeners.keys().map(String::as_str).collect()
    }

    /// その出来事に何件登録されているか。
    pub fn count(&self, event: &str) -> usize {
        self.listeners.get(event).map_or(0, Vec::len)
    }

    /// 1 件も登録されていないか。
    pub fn is_empty(&self) -> bool {
        self.listeners.is_empty()
    }

    fn get(&self, event: &str) -> &[Arc<dyn Listener>] {
        self.listeners.get(event).map_or(&[], Vec::as_slice)
    }
}

impl std::fmt::Debug for Events {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut names: Vec<_> = self
            .listeners
            .iter()
            .map(|(name, list)| format!("{name}({})", list.len()))
            .collect();
        names.sort();
        f.debug_struct("Events").field("listeners", &names).finish()
    }
}

/// 登録された聞く側の表。
///
/// 以前は `OnceLock` でした。2 つ目以降の `Application` を黙って無視するので、
/// テストで別の聞く側を持つアプリを組んでも効きませんでした。ルート名の表
/// （決定記録 #136）と同じく、組み立てるたびに入れ替えられる形にします。
///
/// 読む側は `RwLock` の読み取り 1 回と `Arc` の複製だけです。起動後は書きません。
/// ホットパスで錠を取るのは、ルート名の表と同じ割り切りです（決定記録 #136）。
static EVENTS: RwLock<Option<Arc<Events>>> = RwLock::new(None);

/// `Application` を組み立てるたびに呼ぶ。
///
/// **聞く側が 1 件でもあれば、表を丸ごと置き換えます。** 前の登録は消えます。
/// 本番では起動時に 1 回なので問題になりませんが、テストでは同じプロセスの
/// 別スレッドで並ぶので、後から組んだアプリの表が勝ちます。`with_events` を
/// 既定と違う形で組み、かつ `Event::dispatch` 系を使うテストは、
/// `testing::exclusive()` の札を取ってください。
///
/// **空の登録では置き換えません。** `Application::configure()` は
/// `with_events` を書かなくても組み立てられるので、置き換えてしまうと
/// 「イベントを使わないテスト」が「イベントを使うテスト」の表を黙って
/// 消してしまいます（`dispatch` は聞く側 0 件でも成功するので、消えたことに
/// 気づけません）。本番では `Application` が 1 つだけなので、残しても
/// 結果は同じです（表が無ければ空として扱います）。
pub(crate) fn install(events: Events) {
    if events.is_empty() {
        tracing::debug!("聞く側が 1 件も無いので、いまの表はそのまま残します");
        return;
    }
    *EVENTS.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(events));
}

/// いまの表を取る。
///
/// `RwLock` の読み取りと `Arc` の複製を**毎回**通ります。登録が無ければ、
/// 共有の空の表を返します（`dispatch` は成功扱いです）。
fn events() -> Arc<Events> {
    static EMPTY: OnceLock<Arc<Events>> = OnceLock::new();
    EVENTS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| EMPTY.get_or_init(|| Arc::new(Events::new())).clone())
}

/// 登録順に全部呼び、**1つ失敗したらそこで止める**（[`Event::dispatch_raw`] の本体）。
///
/// 入れ物を引数で受け取ります。プロセス共通の表（`install` が入れるもの）を
/// 通さないので、テストが並んで走っても互いの登録を踏みません。
pub(crate) async fn dispatch_in(events: &Events, event: &str, payload: &str) -> Result<()> {
    for listener in events.get(event) {
        listener
            .call(payload.to_string())
            .await
            .map_err(|e| Error::msg(format!("イベント `{event}` の処理に失敗しました: {e}")))?;
    }
    Ok(())
}

/// 登録順に全部呼び、**失敗しても次を呼ぶ**（[`Event::try_dispatch`] の本体）。
pub(crate) async fn try_dispatch_in(events: &Events, event: &str, payload: &str) -> Result<()> {
    for listener in events.get(event) {
        if let Err(e) = listener.call(payload.to_string()).await {
            tracing::error!("イベント `{event}` の処理に失敗しました: {e}");
        }
    }
    Ok(())
}

/// 出来事を知らせる入口。
pub struct Event;

impl Event {
    /// 知らせる。登録順に全部呼び、**1つ失敗したらそこで止めます。**
    ///
    /// 聞く側が 1 件も無ければ、何もせず成功します。表はプロセス共通で、
    /// 呼ぶたびに `RwLock` の読み取りと `Arc` の複製を通ります
    /// （モジュールの説明を見てください）。
    pub async fn dispatch<T: serde::Serialize>(event: &str, payload: &T) -> Result<()> {
        let text = serde_json::to_string(payload)?;
        Self::dispatch_raw(event, &text).await
    }

    /// 文字列をそのまま渡して知らせる。
    pub async fn dispatch_raw(event: &str, payload: &str) -> Result<()> {
        dispatch_in(&events(), event, payload).await
    }

    /// 知らせる。**失敗しても次を呼びます。** エラーはログに出します。
    ///
    /// 「ついでの処理」（通知など）で、本体を止めたくないときに使います。
    pub async fn try_dispatch<T: serde::Serialize>(event: &str, payload: &T) -> Result<()> {
        let text = serde_json::to_string(payload)?;
        try_dispatch_in(&events(), event, &text).await
    }

    /// 聞いている処理があるか。
    pub fn has_listeners(event: &str) -> bool {
        !events().get(event).is_empty()
    }

    /// その出来事に登録されている件数。
    pub fn listener_count(event: &str) -> usize {
        events().count(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// 数えるだけの聞く側。`async fn` をそのまま渡せることの確認にも使います。
    async fn noop(_payload: String) -> Result<()> {
        Ok(())
    }

    async fn fail(_payload: String) -> Result<()> {
        Err(Error::msg("わざと失敗する"))
    }

    /// テストごとに持つ記録。
    ///
    /// **静的な変数を共有しないでください。** `#[tokio::test]` は 1 つのプロセスで
    /// 並んで走るので、共有すると別のテストの呼び出しを数えてしまいます。
    #[derive(Clone, Default)]
    struct Log(Arc<Mutex<Vec<String>>>);

    impl Log {
        fn new() -> Self {
            Self::default()
        }

        /// 呼ばれた順に名前を足していく聞く側を作る。
        fn recorder(&self, name: &'static str) -> impl Listener {
            let order = self.0.clone();
            move |payload: String| {
                let order = order.clone();
                async move {
                    order.lock().unwrap().push(format!("{name}:{payload}"));
                    Ok(())
                }
            }
        }

        /// 記録を取り出して空にする。
        fn taken(&self) -> Vec<String> {
            std::mem::take(&mut *self.0.lock().unwrap())
        }
    }

    #[test]
    fn 登録した件数が数えられる() {
        let events = Events::new()
            .listen("a", noop)
            .listen("a", noop)
            .listen("b", noop);
        assert_eq!(events.count("a"), 2);
        assert_eq!(events.count("b"), 1);
        assert_eq!(events.count("none"), 0);

        let mut names = events.names();
        names.sort();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn デバッグ表示に件数が出る() {
        let events = Events::new().listen("a", noop).listen("a", noop);
        assert!(format!("{events:?}").contains("a(2)"));
    }

    #[tokio::test]
    async fn 登録順に呼ばれ失敗で止まる() {
        // プロセス共通の表を通さず、本体（`dispatch_in`）を直接叩く。
        // `install` は表を置き換えるので、並んで走るテストと踏み合う。
        let log = Log::new();
        let events = Events::new()
            .listen("ok", log.recorder("1"))
            .listen("ok", log.recorder("2"))
            .listen("mixed", log.recorder("1"))
            .listen("mixed", fail)
            .listen("mixed", log.recorder("3"));

        dispatch_in(&events, "ok", "{}").await.unwrap();
        assert_eq!(log.taken(), vec!["1:{}".to_string(), "2:{}".to_string()]);

        // 2 件目で失敗するので、3 件目は呼ばれない。
        let error = dispatch_in(&events, "mixed", "payload")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("イベント `mixed` の処理に失敗しました"),
            "{error}"
        );
        assert_eq!(
            log.taken(),
            vec!["1:payload".to_string()],
            "失敗した時点で止まる"
        );
    }

    #[tokio::test]
    async fn try_dispatchは失敗しても止まらない() {
        let log = Log::new();
        let events = Events::new()
            .listen("mixed", log.recorder("1"))
            .listen("mixed", fail)
            .listen("mixed", log.recorder("3"));

        try_dispatch_in(&events, "mixed", "{}").await.unwrap();
        assert_eq!(
            log.taken(),
            vec!["1:{}".to_string(), "3:{}".to_string()],
            "失敗の後も呼ぶ"
        );
    }

    #[tokio::test]
    async fn 登録が無い出来事は何も呼ばない() {
        let log = Log::new();
        let events = Events::new().listen("a", log.recorder("a"));
        dispatch_in(&events, "none", "{}").await.unwrap();
        try_dispatch_in(&events, "none", "{}").await.unwrap();
        assert!(log.taken().is_empty());
    }

    #[tokio::test]
    async fn 聞く側はstringで書ける() {
        // 公開 API の形。`app/Listeners/` はこの形で書きます。
        // `async fn` は値を捕まえられないので、このテスト専用の数え手を使います。
        static CALLS: AtomicUsize = AtomicUsize::new(0);

        async fn handle(payload: String) -> Result<()> {
            assert_eq!(payload, "本文");
            CALLS.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        let events = Events::new().listen("a", handle).listen("a", handle);

        dispatch_in(&events, "a", "本文").await.unwrap();
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
    }

    /// `install` が表を**置き換える**ことを押さえる。
    ///
    /// プロセス共通の表を書くので、札（`exclusive()`）を取って直列にします。
    /// **ほかに `install` を通るテストも札を取る必要があります。**
    /// `Application` を組むだけで `install` が走るので、札を取らないテストと
    /// 同時に走ると、下の `has_listeners` が実行順で変わります。
    #[tokio::test]
    async fn 組み立て直すと登録が置き換わる() {
        let _lock = crate::testing::exclusive().await;
        // ほかのテストと鍵がぶつからないよう、このテストだけの名前を使う。
        const FIRST: &str = "events.install.first";
        const SECOND: &str = "events.install.second";

        install(Events::new().listen(FIRST, noop));
        assert!(Event::has_listeners(FIRST));

        // 以前は `OnceLock` で、2 つ目以降の `Application` を黙って無視していた。
        install(Events::new().listen(SECOND, noop).listen(SECOND, noop));
        assert!(!Event::has_listeners(FIRST), "前の登録は消える");
        assert_eq!(Event::listener_count(SECOND), 2);
        Event::dispatch_raw(SECOND, "{}").await.unwrap();
    }

    #[tokio::test]
    async fn 空の登録では置き換えない() {
        let _lock = crate::testing::exclusive().await;
        const KEY: &str = "events.install.keep";

        install(Events::new().listen(KEY, noop));
        assert!(Event::has_listeners(KEY));

        // `with_events` を書かないアプリを組んでも、ここの登録は消えない。
        // 消すと「イベントを使わないテスト」が「使うテスト」の表を黙って
        // 壊します（`dispatch` は 0 件でも成功するので気づけません）。
        install(Events::new());
        assert!(Event::has_listeners(KEY), "空の登録では置き換えない");
    }

    #[tokio::test]
    async fn 登録が無いときは何も起きない() {
        assert!(!Event::has_listeners("登録していない出来事"));
        assert_eq!(Event::listener_count("登録していない出来事"), 0);
        Event::dispatch_raw("登録していない出来事", "{}")
            .await
            .unwrap();
        Event::dispatch("登録していない出来事", &42).await.unwrap();
    }
}
