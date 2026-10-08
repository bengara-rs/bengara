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
//! **起動時に登録して固定します。** 実行中に増えません。
//! 聞く側の形はジョブと同じ `async fn handle(payload: String) -> Result<()>` です。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

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

static EVENTS: OnceLock<Events> = OnceLock::new();

/// 起動時に1回だけ呼ぶ。2回目以降は何もしない。
pub(crate) fn install(events: Events) {
    let _ = EVENTS.set(events);
}

fn events() -> &'static Events {
    static EMPTY: OnceLock<Events> = OnceLock::new();
    EVENTS
        .get()
        .unwrap_or_else(|| EMPTY.get_or_init(Events::new))
}

/// 登録順に全部呼び、**1つ失敗したらそこで止める**（[`Event::dispatch_raw`] の本体）。
///
/// 入れ物を引数で受け取ります。`install` は 1 回しか効かないので、
/// `events()` 越しだとこの分岐をテストから確かめられません。
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
    pub async fn dispatch<T: serde::Serialize>(event: &str, payload: &T) -> Result<()> {
        let text = serde_json::to_string(payload)?;
        Self::dispatch_raw(event, &text).await
    }

    /// 文字列をそのまま渡して知らせる。
    pub async fn dispatch_raw(event: &str, payload: &str) -> Result<()> {
        dispatch_in(events(), event, payload).await
    }

    /// 知らせる。**失敗しても次を呼びます。** エラーはログに出します。
    ///
    /// 「ついでの処理」（通知など）で、本体を止めたくないときに使います。
    pub async fn try_dispatch<T: serde::Serialize>(event: &str, payload: &T) -> Result<()> {
        let text = serde_json::to_string(payload)?;
        try_dispatch_in(events(), event, &text).await
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
        // `install` は1回しか効かないので、本体（`dispatch_in`）を直接叩く。
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
