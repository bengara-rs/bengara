//! ミドルウェア（リクエストの前後に挟む処理）。
//!
//! 利用者が書くのは、次の形の関数ひとつです。トレイトを手で実装する必要はありません。
//!
//! ```ignore
//! // app/Http/Middleware/EnsureAdmin.rs
//! pub struct EnsureAdmin;
//!
//! impl EnsureAdmin {
//!     pub async fn handle(req: Request, next: Next) -> Result<Response> {
//!         if req.header("x-admin").is_none() {
//!             return abort(403);
//!         }
//!         next.run(req).await
//!     }
//! }
//! ```
//!
//! 登録は `bootstrap/app.rs` で行います。
//!
//! ```ignore
//! .with_middleware(|m| m.append(AddPoweredBy::handle).alias("admin", EnsureAdmin::handle))
//! ```

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use crate::error::Result;
use crate::http::handler::{BoxFuture, ErasedHandler};
use crate::http::{Request, Response};

/// リクエストの前後に挟む処理。
///
/// `async fn(Request, Next) -> Result<Response>` には自動で実装されるので、
/// 普通はこのトレイトを手で書くことはありません。
/// 設定を持たせたいときだけ、自分の型に実装してください。
pub trait Middleware: Send + Sync + 'static {
    /// リクエストを受け取り、`next.run(req)` で先へ進めるか、ここで打ち切る。
    fn handle(&self, req: Request, next: Next) -> BoxFuture;
}

impl<F, Fut> Middleware for F
where
    F: Fn(Request, Next) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Response>> + Send + 'static,
{
    fn handle(&self, req: Request, next: Next) -> BoxFuture {
        Box::pin(self(req, next))
    }
}

/// 残りの処理。`next.run(req).await` で先へ進みます。
///
/// 呼ばなければ、そこで打ち切ってレスポンスを返せます。
pub struct Next {
    stack: Arc<[Arc<dyn Middleware>]>,
    index: usize,
    handler: Arc<dyn ErasedHandler>,
    options: crate::http::response::RenderOptions,
}

impl Next {
    pub(crate) fn new(
        stack: Arc<[Arc<dyn Middleware>]>,
        handler: Arc<dyn ErasedHandler>,
        options: crate::http::response::RenderOptions,
    ) -> Self {
        Self {
            stack,
            index: 0,
            handler,
            options,
        }
    }

    /// エラーをどう見せるかの指定（ミドルウェアが自分でレスポンスを組むとき用）。
    pub(crate) fn render_options(&self) -> crate::http::response::RenderOptions {
        self.options.clone()
    }

    /// 次のミドルウェア、または最後のハンドラを呼ぶ。
    ///
    /// **返るのはいつも `Ok` です。** 内側で起きたエラーは、ここでレスポンスに変えてから返します。
    /// そうしないと、レスポンスに手を入れるミドルウェアが、エラーのときだけ素通りされてしまいます。
    /// 戻り値が `Result` なのは、ミドルウェア自身が `?` と `abort()` を使えるようにするためです。
    pub fn run(self, req: Request) -> BoxFuture {
        let options = self.options.clone();
        let inner = match self.stack.get(self.index).cloned() {
            Some(middleware) => {
                let next = Self {
                    stack: self.stack,
                    index: self.index + 1,
                    handler: self.handler,
                    options: options.clone(),
                };
                middleware.handle(req, next)
            }
            None => self.handler.call(req),
        };
        Box::pin(async move { Ok(crate::http::response::render(inner.await, options)) })
    }
}

/// ミドルウェアの登録。`with_middleware` に渡ってきます。
#[derive(Default)]
pub struct Middlewares {
    global: Vec<Arc<dyn Middleware>>,
    aliases: HashMap<String, Arc<dyn Middleware>>,
}

impl Middlewares {
    /// 全ルートの**後ろ**に足す。Laravel の `$middleware->append()` に当たります。
    pub fn append(mut self, middleware: impl Middleware) -> Self {
        self.global.push(Arc::new(middleware));
        self
    }

    /// 全ルートの**前**に足す。Laravel の `$middleware->prepend()` に当たります。
    pub fn prepend(mut self, middleware: impl Middleware) -> Self {
        self.global.insert(0, Arc::new(middleware));
        self
    }

    /// 名前を付ける。ルート側から `.middleware("admin")` で呼べるようになります。
    ///
    /// # パニック
    ///
    /// 同じ名前を2回登録すると、起動時にパニックします。
    pub fn alias(mut self, name: &str, middleware: impl Middleware) -> Self {
        if self.aliases.contains_key(name) {
            panic!(
                "ミドルウェアの名前 `{name}` が2回登録されています。bootstrap/app.rs の alias を確かめてください"
            );
        }
        self.aliases.insert(name.to_string(), Arc::new(middleware));
        self
    }

    /// 名前の一覧を、登録した順ではなく並べ替えて返す（エラーメッセージ用）。
    fn known(&self) -> String {
        let mut names: Vec<&str> = self.aliases.keys().map(String::as_str).collect();
        names.sort_unstable();
        if names.is_empty() {
            "（まだ1つも登録されていません）".to_string()
        } else {
            names.join(", ")
        }
    }

    /// ルート1本分の並びを組み立てる。共通のものが先、ルートのものが後。
    ///
    /// # パニック
    ///
    /// 登録していない名前が使われていると、起動時にパニックします。
    pub(crate) fn stack_for(&self, names: &[String]) -> Arc<[Arc<dyn Middleware>]> {
        let mut stack: Vec<Arc<dyn Middleware>> = self.global.clone();
        for name in names {
            let Some(found) = self.aliases.get(name) else {
                panic!(
                    "ミドルウェア `{name}` は登録されていません。\
                     bootstrap/app.rs の with_middleware で alias(\"{name}\", ...) を足してください。\
                     いま登録されているのは: {}",
                    self.known()
                );
            };
            stack.push(found.clone());
        }
        stack.into()
    }

    /// 共通のミドルウェアが1つも無く、名前も無いか（組み立てを省けるか）。
    pub(crate) fn is_empty(&self) -> bool {
        self.global.is_empty() && self.aliases.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::http::handler::Erased;
    use std::sync::Mutex;

    /// 通った順を記録する置き場。テストごとに1つ作るので、並列に走っても混ざりません。
    type Log = Arc<Mutex<Vec<&'static str>>>;

    fn log() -> Log {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn taken(log: &Log) -> Vec<&'static str> {
        log.lock().unwrap().clone()
    }

    /// 「入る」「出る」を記録して、ヘッダーを足すだけのミドルウェアを作る。
    fn recorder(log: &Log, label: &'static str) -> impl Middleware {
        let log = log.clone();
        move |req: Request, next: Next| {
            let log = log.clone();
            async move {
                log.lock().unwrap().push(label);
                let response = next.run(req).await?;
                log.lock().unwrap().push("out");
                Ok(response.with_header("x-seen", label))
            }
        }
    }

    /// `next` を呼ばずに打ち切るミドルウェアを作る。
    fn stopper(log: &Log) -> impl Middleware {
        let log = log.clone();
        move |_req: Request, _next: Next| {
            let log = log.clone();
            async move {
                log.lock().unwrap().push("stop");
                Err(Error::http(403, "だめ"))
            }
        }
    }

    fn handler_for(log: &Log) -> Arc<dyn ErasedHandler> {
        let log = log.clone();
        Arc::new(Erased::new(move || {
            let log = log.clone();
            async move {
                log.lock().unwrap().push("handler");
                Ok(Response::text("ok"))
            }
        }))
    }

    async fn run(stack: Arc<[Arc<dyn Middleware>]>, log: &Log) -> Response {
        Next::new(
            stack,
            handler_for(log),
            crate::http::response::RenderOptions::new(false, false),
        )
        .run(Request::new("GET", "/"))
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn 書いた順に入り逆順に戻る() {
        let log = log();
        let mw = Middlewares::default()
            .append(recorder(&log, "first"))
            .append(recorder(&log, "second"));
        let response = run(mw.stack_for(&[]), &log).await;
        assert_eq!(
            taken(&log),
            vec!["first", "second", "handler", "out", "out"]
        );
        // 外側が最後に書くので、first が残る（with_header は置き換え）。
        assert_eq!(response.header("x-seen"), Some("first"));
    }

    #[tokio::test]
    async fn nextを呼ばなければ打ち切れる() {
        let log = log();
        let mw = Middlewares::default()
            .append(recorder(&log, "first"))
            .append(stopper(&log));
        let response = run(mw.stack_for(&[]), &log).await;
        // 打ち切られるので handler は通らない。外側の first は戻りを処理できる。
        assert_eq!(taken(&log), vec!["first", "stop", "out"]);
        assert_eq!(response.status(), 403);
        assert_eq!(response.header("x-seen"), Some("first"));
    }

    #[tokio::test]
    async fn 共通が先でルートのものが後() {
        let log = log();
        let mw = Middlewares::default()
            .append(recorder(&log, "global"))
            .alias("named", recorder(&log, "named"));
        run(mw.stack_for(&["named".to_string()]), &log).await;
        assert_eq!(
            taken(&log),
            vec!["global", "named", "handler", "out", "out"]
        );
    }

    #[tokio::test]
    async fn prependは前に入る() {
        let log = log();
        let mw = Middlewares::default()
            .append(recorder(&log, "later"))
            .prepend(recorder(&log, "earlier"));
        run(mw.stack_for(&[]), &log).await;
        assert_eq!(
            taken(&log),
            vec!["earlier", "later", "handler", "out", "out"]
        );
    }

    #[tokio::test]
    async fn 同じ名前を2回付けると2回通る() {
        let log = log();
        let mw = Middlewares::default().alias("named", recorder(&log, "named"));
        let names = vec!["named".to_string(), "named".to_string()];
        run(mw.stack_for(&names), &log).await;
        assert_eq!(taken(&log), vec!["named", "named", "handler", "out", "out"]);
    }

    #[test]
    fn 何も登録しなければ空() {
        let mw = Middlewares::default();
        assert!(mw.is_empty());
        assert!(mw.stack_for(&[]).is_empty());
    }

    #[test]
    #[should_panic(expected = "ミドルウェア `missing` は登録されていません")]
    fn 知らない名前はパニックする() {
        let log = log();
        let mw = Middlewares::default().alias("admin", recorder(&log, "admin"));
        let _ = mw.stack_for(&["missing".to_string()]);
    }

    #[test]
    #[should_panic(expected = "ミドルウェアの名前 `admin` が2回登録されています")]
    fn 名前の重複はパニックする() {
        let log = log();
        let _ = Middlewares::default()
            .alias("admin", recorder(&log, "a"))
            .alias("admin", recorder(&log, "b"));
    }
}
