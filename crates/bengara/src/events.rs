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
//! 中身を型に読んで受け取るときは [`Events::listen_json`] と [`Json`] を使います。
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
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::{Arc, OnceLock, RwLock};

pub use crate::support::payload::{FromPayload, Json};

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

/// [`Events::listen_json`] が登録する聞く側。中身を `Json<T>` に読んでから呼びます。
///
/// `PhantomData<fn() -> T>` にしているのは、`T` を持たないので `T` が
/// `Sync` でなくても `Listener`（`Send + Sync`）になれるようにするためです。
struct JsonListener<T, F> {
    listener: F,
    payload: PhantomData<fn() -> T>,
}

impl<T, F, Fut> Listener for JsonListener<T, F>
where
    T: serde::de::DeserializeOwned + Send + 'static,
    F: Fn(Json<T>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    fn call(&self, payload: String) -> ListenerFuture {
        match Json::<T>::from_payload(payload) {
            Ok(payload) => Box::pin((self.listener)(payload)),
            Err(e) => Box::pin(std::future::ready(Err(e))),
        }
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

    /// 出来事の名前に、**中身を型に読んで受け取る**聞く側を足す。
    ///
    /// ```ignore
    /// // app/Listeners/SendWelcome.rs
    /// pub async fn handle(Json(payload): Json<Registered>) -> Result<()> { .. }
    ///
    /// // bootstrap/app.rs
    /// .with_events(|e| e.listen_json("user.registered", SendWelcome::handle))
    /// ```
    ///
    /// 型は聞く側の引数から推論します。中身を読めなければ、聞く側の失敗になります
    /// （`dispatch` はそこで止まり、`try_dispatch` はログに出して次を呼びます）。
    /// 呼ばれる順番は [`listen`](Self::listen) と同じく登録した順です。
    pub fn listen_json<T, F, Fut>(self, event: &str, listener: F) -> Self
    where
        T: serde::de::DeserializeOwned + Send + 'static,
        F: Fn(Json<T>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        self.listen(
            event,
            JsonListener {
                listener,
                payload: PhantomData,
            },
        )
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

/// 聞く側の失敗。出来事の名前を添えて、**元のエラーを原因として持ちます。**
///
/// `Error::other` に包んで返します。`source()` で元のエラーまでたどれます。
#[derive(Debug)]
struct ListenerError {
    event: String,
    source: Error,
}

impl std::fmt::Display for ListenerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "イベント `{}` の処理に失敗しました: {}",
            self.event, self.source
        )
    }
}

impl std::error::Error for ListenerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// 聞く側が返したエラーを、`dispatch` が返す形にする。
///
/// 以前は全部 `Error::msg` に包み直していたので、**元のエラーの種類が消えました。**
/// 聞く側が `abort(404)` や検査のエラー（422）を返しても、呼んだ側には 500 に
/// 見えていました。
///
/// | 元のエラー | 返すもの |
/// |---|---|
/// | ステータスを持つもの（`Error::Http`・`Error::Validation`） | **そのまま**返す |
/// | それ以外（500 になるもの） | 出来事の名前を添えて包む。原因は `source()` に残す |
///
/// ステータスを持つエラーをそのまま返すのは Laravel と同じです。Laravel の
/// 同期のリスナーが投げた例外は、包まれずに `event()` の呼び元まで届きます。
/// 包むと `status()` が 500 になるので、文を足さずに通します
/// （どの出来事だったかは `debug` のログに残します）。
fn listener_failed(event: &str, error: Error) -> Error {
    match error {
        Error::Http { .. } | Error::Validation(_) => {
            tracing::debug!(
                "イベント `{event}` の聞く側がステータス {} のエラーを返しました: {error}",
                error.status()
            );
            error
        }
        other => Error::other(ListenerError {
            event: event.to_string(),
            source: other,
        }),
    }
}

/// 登録順に全部呼び、**1つ失敗したらそこで止める**（[`Event::dispatch_raw`] の本体）。
///
/// 返すエラーの形は [`listener_failed`] を見てください。
///
/// 入れ物を引数で受け取ります。プロセス共通の表（`install` が入れるもの）を
/// 通さないので、テストが並んで走っても互いの登録を踏みません。
pub(crate) async fn dispatch_in(events: &Events, event: &str, payload: &str) -> Result<()> {
    for listener in events.get(event) {
        call_isolated(listener.as_ref(), payload)
            .await
            .map_err(|e| listener_failed(event, e))?;
    }
    Ok(())
}

/// 登録順に全部呼び、**失敗しても次を呼ぶ**（[`Event::try_dispatch`] の本体）。
///
/// **パニックも失敗の 1 つとして扱います。** 以前はそのまま呼び出し元まで
/// 巻き戻り、残りの聞く側を飛ばしたうえ、本体の処理（DB の確定など）が
/// 終わった後のリクエストが 500 になりました。
pub(crate) async fn try_dispatch_in(events: &Events, event: &str, payload: &str) -> Result<()> {
    for listener in events.get(event) {
        if let Err(e) = call_isolated(listener.as_ref(), payload).await {
            tracing::error!("イベント `{event}` の処理に失敗しました: {e}");
        }
    }
    Ok(())
}

/// 聞く側を 1 つ、別のタスクで呼ぶ。**パニックは `Err` にします。**
///
/// `tokio::spawn` で包むのは、キューの `run_one` とスケジューラの
/// `run_task` と同じ考え方です（決定記録 #008）。包まないと、聞く側の
/// パニックが `dispatch` を呼んだ側まで巻き戻ります。
///
/// 包むと失われるものを 2 つ補います。
///
/// | 失われるもの | 補い方 |
/// |---|---|
/// | `Lang::with` の言語（タスクごとの値） | いまの言語を取り、新しいタスクでも `Lang::with` で包む |
/// | 呼び出し元の取り消し | 待っている側が捨てられたら、タスクも止める（[`AbortOnDrop`]） |
async fn call_isolated(listener: &dyn Listener, payload: &str) -> Result<()> {
    let future = listener.call(payload.to_string());
    let task = match crate::lang::Lang::request_locale() {
        Some(locale) => tokio::spawn(crate::lang::Lang::with(locale, future)),
        None => tokio::spawn(future),
    };
    let mut task = AbortOnDrop(task);
    match (&mut task.0).await {
        Ok(result) => result,
        Err(e) if e.is_panic() => Err(Error::msg(format!(
            "聞く側がパニックしました: {}",
            panic_message(e.into_panic())
        ))),
        Err(e) => Err(Error::msg(format!("聞く側を動かせませんでした: {e}"))),
    }
}

/// 待っている側が捨てられたら、タスクを止める。
///
/// 包む前は、リクエストが取り消されると聞く側の処理も一緒に止まりました。
/// `tokio::spawn` だけだと、呼び出し元が居なくなっても聞く側が走り続けます。
/// 前と同じ止まり方にそろえます。
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// パニックの中身を文字にする。文字でなければ決まった文にします。
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_string();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "（中身は文字ではありません）".to_string()
}

/// 出来事を知らせる入口。
pub struct Event;

impl Event {
    /// 知らせる。登録順に全部呼び、**1つ失敗したらそこで止めます。**
    ///
    /// 聞く側が 1 件も無ければ、何もせず成功します。表はプロセス共通で、
    /// 呼ぶたびに `RwLock` の読み取りと `Arc` の複製を通ります
    /// （モジュールの説明を見てください）。
    ///
    /// **聞く側のエラーの種類を保ちます。** ステータスを持つエラー
    /// （`abort(404)` や検査の 422）はそのまま返します。それ以外は出来事の
    /// 名前を添えて包み、元のエラーを `source()` に残します（500 になります）。
    ///
    /// 聞く側がパニックしたら、そこで止めて 500 のエラーを返します
    /// （呼び出し元まで巻き戻しません）。
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
    /// 聞く側のパニックも失敗として扱い、ログに出して次を呼びます。
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
    async fn 聞く側のエラーは原因としてたどれる() {
        use std::error::Error as _;

        async fn io_fail(_payload: String) -> Result<()> {
            Err(Error::Io(std::io::Error::other("ディスクが一杯")))
        }
        let events = Events::new().listen("a", io_fail);

        let error = dispatch_in(&events, "a", "{}").await.unwrap_err();
        // 以前は `Error::msg` に包み直していたので、原因が残らなかった。
        assert!(matches!(error, Error::Other(_)), "{error:?}");
        assert_eq!(error.status(), 500);
        let text = error.to_string();
        assert!(text.contains("イベント `a` の処理に失敗しました"), "{text}");
        assert!(text.contains("ディスクが一杯"), "{text}");

        // 原因をたどると、元の `Error::Io` と、その中の入出力エラーまで届く。
        let wrapped = error.source().expect("包んだ失敗がある");
        let original = wrapped.source().expect("元のエラーがある");
        assert!(
            matches!(original.downcast_ref::<Error>(), Some(Error::Io(_))),
            "{original:?}"
        );
        assert!(original.source().is_some(), "入出力エラーまでたどれる");
    }

    #[tokio::test]
    async fn 聞く側のステータスは保つ() {
        async fn not_found(_payload: String) -> Result<()> {
            Err(Error::http(404, "記事がありません"))
        }
        async fn invalid(_payload: String) -> Result<()> {
            let mut errors = crate::validation::ValidationErrors::default();
            errors.add("title", "title は必ず入力してください。");
            Err(Error::Validation(Box::new(errors)))
        }
        let events = Events::new()
            .listen("404", not_found)
            .listen("422", invalid);

        // 以前はどちらも 500 になっていた（Laravel は投げた例外がそのまま届く）。
        let error = dispatch_in(&events, "404", "{}").await.unwrap_err();
        assert_eq!(error.status(), 404);
        assert_eq!(error.public_message(), "記事がありません");

        let error = dispatch_in(&events, "422", "{}").await.unwrap_err();
        assert_eq!(error.status(), 422);
        match error {
            Error::Validation(errors) => assert_eq!(errors.get("title").len(), 1),
            other => panic!("検査のエラーのまま返るはずです: {other:?}"),
        }
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

    async fn boom(_payload: String) -> Result<()> {
        panic!("わざと落ちる")
    }

    #[tokio::test]
    async fn try_dispatchはパニックしても次を呼ぶ() {
        let log = Log::new();
        let events = Events::new()
            .listen("mixed", log.recorder("1"))
            .listen("mixed", boom)
            .listen("mixed", log.recorder("3"));

        // 以前はパニックが呼び出し元まで巻き戻り、3 件目が呼ばれなかった。
        try_dispatch_in(&events, "mixed", "{}").await.unwrap();
        assert_eq!(
            log.taken(),
            vec!["1:{}".to_string(), "3:{}".to_string()],
            "パニックの後も呼ぶ"
        );
    }

    #[tokio::test]
    async fn dispatchはパニックを500のエラーにする() {
        let log = Log::new();
        let events = Events::new()
            .listen("mixed", boom)
            .listen("mixed", log.recorder("2"));

        let error = dispatch_in(&events, "mixed", "{}").await.unwrap_err();
        assert_eq!(error.status(), 500);
        let text = error.to_string();
        assert!(text.contains("パニックしました"), "{text}");
        assert!(text.contains("わざと落ちる"), "{text}");
        assert!(log.taken().is_empty(), "パニックした所で止まる");
    }

    #[tokio::test]
    async fn 聞く側にもいまの言語が届く() {
        async fn check_locale(_payload: String) -> Result<()> {
            assert_eq!(crate::lang::Lang::current(), "en-test");
            Ok(())
        }
        let events = Events::new().listen("a", check_locale);
        // 別のタスクで呼んでも、`Lang::with` の言語は引き継ぐ。
        crate::lang::Lang::with("en-test", dispatch_in(&events, "a", "{}"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn 待つ側が捨てられたら聞く側も止まる() {
        static FINISHED: AtomicUsize = AtomicUsize::new(0);
        async fn slow(_payload: String) -> Result<()> {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            FINISHED.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        let events = Events::new().listen("a", slow);
        let waited = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            dispatch_in(&events, "a", "{}"),
        )
        .await;
        assert!(waited.is_err(), "途中で捨てる");
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert_eq!(FINISHED.load(Ordering::SeqCst), 0, "続きは動かない");
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
    async fn 聞く側は型でも受け取れる() {
        // プロセス共通の表を書くので札を取る（`install` の説明を見てください）。
        let _lock = crate::testing::exclusive().await;
        const KEY: &str = "events.listen_json";

        #[derive(serde::Serialize, serde::Deserialize)]
        struct Registered {
            id: i64,
        }

        static SEEN: AtomicUsize = AtomicUsize::new(0);
        async fn handle(Json(payload): Json<Registered>) -> Result<()> {
            SEEN.fetch_add(payload.id as usize, Ordering::SeqCst);
            Ok(())
        }

        let log = Log::new();
        install(
            Events::new()
                .listen_json(KEY, handle)
                .listen(KEY, log.recorder("next")),
        );

        Event::dispatch(KEY, &Registered { id: 5 }).await.unwrap();
        assert_eq!(SEEN.load(Ordering::SeqCst), 5, "型に読んで届く");
        assert_eq!(log.taken(), vec![r#"next:{"id":5}"#.to_string()]);

        // 形の違う中身は聞く側の失敗。`dispatch` はそこで止まる。
        let error = Event::dispatch(KEY, &"文字列")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("ジョブ・聞く側の中身を"), "{error}");
        assert!(log.taken().is_empty(), "失敗した所で止まる");

        // `try_dispatch` はログに出して、次の聞く側を呼ぶ。
        Event::try_dispatch(KEY, &"文字列").await.unwrap();
        assert_eq!(log.taken(), vec![r#"next:"文字列""#.to_string()]);
        assert_eq!(SEEN.load(Ordering::SeqCst), 5, "壊れた中身では呼ばれない");
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
