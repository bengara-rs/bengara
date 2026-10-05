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
//! 聞く側の形はジョブと同じ `async fn handle(payload: &str) -> Result<()>` です。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use crate::error::{Error, Result};

/// 聞く側が返す非同期の結果。
pub type ListenerFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

/// 聞く側の関数。`async fn handle(payload: &str) -> Result<()>` が入ります。
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
        for listener in events().get(event) {
            listener
                .call(payload.to_string())
                .await
                .map_err(|e| Error::msg(format!("イベント `{event}` の処理に失敗しました: {e}")))?;
        }
        Ok(())
    }

    /// 知らせる。**失敗しても次を呼びます。** エラーはログに出します。
    ///
    /// 「ついでの処理」（通知など）で、本体を止めたくないときに使います。
    pub async fn try_dispatch<T: serde::Serialize>(event: &str, payload: &T) -> Result<()> {
        let text = serde_json::to_string(payload)?;
        for listener in events().get(event) {
            if let Err(e) = listener.call(text.clone()).await {
                tracing::error!("イベント `{event}` の処理に失敗しました: {e}");
            }
        }
        Ok(())
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

    static CALLS: AtomicUsize = AtomicUsize::new(0);

    async fn count_up(_payload: String) -> Result<()> {
        CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn fail(_payload: String) -> Result<()> {
        Err(Error::msg("わざと失敗する"))
    }

    #[test]
    fn 登録した件数が数えられる() {
        let events = Events::new()
            .listen("a", count_up)
            .listen("a", count_up)
            .listen("b", count_up);
        assert_eq!(events.count("a"), 2);
        assert_eq!(events.count("b"), 1);
        assert_eq!(events.count("none"), 0);

        let mut names = events.names();
        names.sort();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn デバッグ表示に件数が出る() {
        let events = Events::new().listen("a", count_up).listen("a", count_up);
        assert!(format!("{events:?}").contains("a(2)"));
    }

    #[tokio::test]
    async fn 登録順に呼ばれ失敗で止まる() {
        // install は1回しか効かないので、ここでは入れ物を直接使う。
        let events = Events::new()
            .listen("ok", count_up)
            .listen("ok", count_up)
            .listen("mixed", count_up)
            .listen("mixed", fail)
            .listen("mixed", count_up);

        CALLS.store(0, Ordering::SeqCst);
        for listener in events.get("ok") {
            listener.call("{}".to_string()).await.unwrap();
        }
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);

        // 2 件目で失敗するので、3 件目は呼ばれない。
        CALLS.store(0, Ordering::SeqCst);
        let mut failed = false;
        for listener in events.get("mixed") {
            if listener.call("{}".to_string()).await.is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed);
        assert_eq!(CALLS.load(Ordering::SeqCst), 1, "失敗した時点で止まる");
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
