//! アプリの組み立てと、1リクエストの処理。
//!
//! `bootstrap/app.rs` が返す `Application` が、起動時に固定される本体です。

use std::path::Path;
use std::sync::Arc;

use crate::config_registry::app_config;
use crate::error::Error;
use crate::http::handler::{Erased, ErasedHandler};
use crate::http::middleware::{Middleware, Middlewares, Next};
use crate::http::response::{error_response, RenderOptions};
use crate::http::routing::{Matched, RouteDef, Routes};
use crate::http::{Request, Response};
use crate::paths;

/// 組み立てが終わったアプリ。
///
/// 中身は読むだけなので、クローンは安く、スレッド間で共有できます。
#[derive(Clone)]
pub struct Application {
    inner: Arc<Inner>,
}

struct Inner {
    routes: Routes,
    /// 共通ミドルウェアだけの並び。404 や静的ファイルにも通すので、ルートの外に持ちます。
    global: Arc<[Arc<dyn Middleware>]>,
    /// `public/` の場所。`Arc` なので、裏のスレッドへ渡すときに確保が起きません。
    public_dir: Arc<Path>,
    /// `public/` の**実体**。シンボリックリンクの確認に使います。
    ///
    /// 起動時に1回だけ解決します。以前はリクエストごとに `canonicalize` を叩いていました。
    /// まだ無いときは `None` で、そのときだけ配信側が解決します。
    public_canonical: Option<Arc<Path>>,
    /// `/storage/...` で配信する置き場所（`storage/app/public/`）。
    storage_public_dir: Arc<Path>,
    /// その実体。`storage/` を作る前に起動したときは `None` になります。
    storage_public_canonical: Option<Arc<Path>>,
    debug: bool,
    exceptions: Option<Arc<Exceptions>>,
}

/// エラーからレスポンスを作る関数。
type ExceptionHandler = Box<dyn Fn(&Error) -> Option<Response> + Send + Sync>;

/// エラーの見せ方の差し替え。
///
/// `with_exceptions` で登録した関数を順に試し、最初に `Some` を返したものを使います。
/// どれも返さなければ、bengara の既定の画面になります。
#[derive(Default)]
pub struct Exceptions {
    handlers: Vec<ExceptionHandler>,
}

impl Exceptions {
    /// エラーからレスポンスを作る関数を足す。
    ///
    /// ```ignore
    /// .with_exceptions(|e| e.render(|error| {
    ///     (error.status() == 404).then(|| Response::text("見つかりません").with_status(404))
    /// }))
    /// ```
    pub fn render(
        mut self,
        handler: impl Fn(&Error) -> Option<Response> + Send + Sync + 'static,
    ) -> Self {
        self.handlers.push(Box::new(handler));
        self
    }

    /// 登録した関数を順に試す。
    pub(crate) fn apply(&self, error: &Error) -> Option<Response> {
        self.handlers.iter().find_map(|h| h(error))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }
}

impl Application {
    /// 組み立てを始める。
    ///
    /// ```ignore
    /// Application::configure()
    ///     .with_routing(|r| r.web(crate::routes::web::routes).health("/up"))
    ///     .create()
    /// ```
    pub fn configure() -> ApplicationBuilder {
        ApplicationBuilder::default()
    }

    /// 名前付きルートを、どこからでも引けるようにする（起動時に1回）。
    pub(crate) fn install_names(&self) {
        crate::http::routing::install_names(self.inner.routes.names().clone());
    }

    /// 登録されているルートの一覧（`route:list` の表示用）。
    pub(crate) fn routes(&self) -> &[RouteDef] {
        self.inner.routes.all()
    }

    /// 1本のリクエストを処理する。ソケットを使わないので、テストからも呼べます。
    pub(crate) async fn handle(&self, mut req: Request) -> Response {
        let is_head = req.method() == "HEAD";
        let options = RenderOptions::new(self.inner.debug, wants_json(&req))
            .with_exceptions(self.inner.exceptions.clone());

        // 共通ミドルウェアは、ルートに当たらなかったときも通します（Laravel と同じ）。
        // そのため、ここでは並びを1本だけ回し、分岐は最内側のハンドラに寄せます。
        let inner = self.innermost(&mut req, &options);
        let future = Next::new(self.inner.global.clone(), inner, options.clone()).run(req);

        // ミドルウェアとハンドラのパニックを1リクエストに閉じ込める。
        // 並びの一番外で包むので、共通ミドルウェアの中で起きたものも 500 になります。
        let response = match tokio::spawn(future).await {
            // `Next::run` は必ず `Ok` を返します（エラーはその中でレスポンスに変わります）。
            // ここの `Err` には来ませんが、型を合わせるために同じ関数を通します。
            Ok(result) => crate::http::response::render(result, options),
            Err(join_error) => {
                let error = Error::msg(panic_message(join_error, options.debug));
                tracing::error!("ハンドラがパニックしました: {error}");
                error_response(&error, options)
            }
        };

        if is_head {
            // 本文を空にする**前**に、元の長さを Content-Length にしておく。
            // HEAD は「GET と同じヘッダーで、本文だけが無いもの」です。
            // 204 と 304 は長さを付けてはいけないので、そのときだけ付けません。
            //
            // すでに長さが付いているときは、そのまま残します。静的ファイルの配信は
            // HEAD のときファイルを読まずに長さだけを入れるので、ここで数え直すと
            // 0 で上書きしてしまいます。
            let response = if matches!(response.status(), 204 | 304)
                || response.header("content-length").is_some()
            {
                response
            } else {
                let length = response.body().len();
                response.with_header("content-length", length.to_string())
            };
            response.with_body(Vec::new())
        } else {
            response
        }
    }

    /// 最内側のハンドラを決める（ルート本体・405・静的ファイル・404）。
    ///
    /// パス引数とルート名は、ミドルウェアからも見えるように**ここで**載せます。
    fn innermost(&self, req: &mut Request, options: &RenderOptions) -> Arc<dyn ErasedHandler> {
        match self.inner.routes.find(req.method(), req.path()) {
            Matched::Found { def, params } => {
                let handler = def.handler.clone();
                let stack = def.stack.clone();
                let name = def.name.clone();
                let options = options.clone();
                req.set_params(params);
                req.set_route_name(name);
                req.mark_matched();
                Arc::new(Erased::new(move |req: Request| {
                    let handler = handler.clone();
                    let stack = stack.clone();
                    let options = options.clone();
                    async move {
                        // ルートごとの並びは、共通の並びの内側で回す。
                        match stack {
                            Some(stack) => Next::new(stack, handler, options).run(req).await,
                            None => handler.call(req).await,
                        }
                    }
                }))
            }
            Matched::MethodNotAllowed { mut allowed } => {
                // HEAD は GET で受けると決めているので、GET があれば HEAD も並べる。
                if allowed.contains(&"GET") && !allowed.contains(&"HEAD") {
                    allowed.push("HEAD");
                }
                allowed.sort_unstable();
                let allow = allowed.join(", ");
                let options = options.clone();
                Arc::new(Erased::new(move |_req: Request| {
                    let allow = allow.clone();
                    let options = options.clone();
                    async move {
                        let error = Error::Http {
                            status: 405,
                            message: String::new(),
                        };
                        Ok(error_response(&error, options).with_header("allow", allow))
                    }
                }))
            }
            Matched::NotFound => {
                let app = self.clone();
                let options = options.clone();
                Arc::new(Erased::new(move |req: Request| {
                    let app = app.clone();
                    let options = options.clone();
                    async move { Ok(app.serve_static_or_404(&req, options).await) }
                }))
            }
        }
    }

    async fn serve_static_or_404(
        &self,
        req: &Request,
        options: crate::http::response::RenderOptions,
    ) -> Response {
        if matches!(req.method(), "GET" | "HEAD") {
            let path = req.path().to_string();
            let if_none_match = req.header("if-none-match").map(str::to_string);
            // HEAD は本文を返さないので、ファイルを読まずに長さだけ知らせる。
            let want_body = req.method() != "HEAD";

            // 1. `/storage/...` は storage/app/public/ から。
            if path.starts_with(crate::http::statics::STORAGE_PREFIX) {
                let dir = self.inner.storage_public_dir.clone();
                let root = self.inner.storage_public_canonical.clone();
                let found = tokio::task::spawn_blocking(move || {
                    crate::http::statics::serve_storage(
                        &dir,
                        root.as_deref(),
                        &path,
                        if_none_match.as_deref(),
                        want_body,
                    )
                })
                .await
                .unwrap_or(None);
                if let Some(response) = found {
                    return response;
                }
            } else {
                // 2. バイナリに埋め込んだ public/（リリースビルドのとき）。
                //    メモリから読むだけなので、裏のスレッドに回す必要はない。
                if let Some(response) = crate::http::statics::serve_embedded(
                    crate::http::statics::embedded(),
                    &path,
                    if_none_match.as_deref(),
                ) {
                    return response;
                }
                // 3. ディスクの public/。
                let dir = self.inner.public_dir.clone();
                let root = self.inner.public_canonical.clone();
                let found = tokio::task::spawn_blocking(move || {
                    crate::http::statics::serve(
                        &dir,
                        root.as_deref(),
                        &path,
                        if_none_match.as_deref(),
                        want_body,
                    )
                })
                .await
                .unwrap_or(None);
                if let Some(response) = found {
                    return response;
                }
            }
        }
        error_response(
            &Error::Http {
                status: 404,
                message: String::new(),
            },
            options,
        )
    }
}

/// クライアントが JSON を欲しがっているか。
///
/// `Accept` に `application/json` があるか、`X-Requested-With: XMLHttpRequest` が付いているか、
/// 自分が JSON を送ってきたか、のどれかで判断します。Laravel の `expectsJson()` と同じ考え方です。
fn wants_json(req: &Request) -> bool {
    wants_json_with(|name| req.header(name))
}

/// クライアントが JSON を欲しがっているか（ヘッダーを引く関数から判断する）。
///
/// `Request` を組み立てる**前**（本文が大きすぎる・読めないとき）からも同じ判断を
/// 使いたいので、ヘッダーの引き方だけを受け取る形にしてあります。
/// `header` に渡す名前は小文字です。
pub(crate) fn wants_json_with<'a>(header: impl Fn(&str) -> Option<&'a str>) -> bool {
    if let Some(accept) = header("accept") {
        let accept = accept.to_ascii_lowercase();
        if accept.contains("application/json") || accept.contains("+json") {
            return true;
        }
        // ブラウザは text/html を先に書く。それが無く */* だけなら API 呼び出しとみなす。
        if !accept.contains("text/html") && accept.contains("*/*") {
            return true;
        }
    }
    if header("x-requested-with").is_some_and(|v| v.eq_ignore_ascii_case("XMLHttpRequest")) {
        return true;
    }
    // 自分が JSON を送ってきたか。判定は `Request` と同じ関数を使う（食い違いを作らない）。
    header("content-type").is_some_and(crate::http::request::json_content_type)
}

/// パニックしたハンドラの、見せる文を決める。
///
/// `panic!("...")` に書かれた文は `debug` のときだけ添えます。
/// 本番で出すと、内部の事情をそのまま外へ見せてしまいます。
fn panic_message(join_error: tokio::task::JoinError, debug: bool) -> String {
    if join_error.is_cancelled() {
        return "処理が打ち切られました".to_string();
    }
    let base = "ハンドラの中でパニックが起きました";
    if !debug || !join_error.is_panic() {
        return base.to_string();
    }
    let payload = join_error.into_panic();
    let detail = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned());
    match detail {
        Some(detail) => format!("{base}: {detail}"),
        None => base.to_string(),
    }
}

/// `Application` の組み立て。
#[derive(Default)]
pub struct ApplicationBuilder {
    routing: Routing,
    middleware: Middlewares,
    exceptions: Exceptions,
    events: crate::events::Events,
}

impl ApplicationBuilder {
    /// ミドルウェアを登録する。
    ///
    /// ```ignore
    /// .with_middleware(|m| m.append(AddPoweredBy::handle).alias("admin", EnsureAdmin::handle))
    /// ```
    pub fn with_exceptions(mut self, configure: impl FnOnce(Exceptions) -> Exceptions) -> Self {
        self.exceptions = configure(std::mem::take(&mut self.exceptions));
        self
    }

    /// ミドルウェアを登録する。
    pub fn with_middleware(mut self, configure: impl FnOnce(Middlewares) -> Middlewares) -> Self {
        self.middleware = configure(std::mem::take(&mut self.middleware));
        self
    }

    /// ルートを決める。
    pub fn with_routing(mut self, configure: impl FnOnce(Routing) -> Routing) -> Self {
        self.routing = configure(std::mem::take(&mut self.routing));
        self
    }

    /// 出来事（イベント）に対して、聞く側を登録する。
    ///
    /// ```ignore
    /// .with_events(|e| {
    ///     e.listen("user.registered", SendWelcome::handle)
    ///         .listen("user.registered", NotifyAdmin::handle)
    /// })
    /// ```
    ///
    /// 起動時に固定されます。実行中には増えません。
    pub fn with_events(
        mut self,
        configure: impl FnOnce(crate::events::Events) -> crate::events::Events,
    ) -> Self {
        self.events = configure(std::mem::take(&mut self.events));
        self
    }

    /// 組み立てを終えて `Application` にする。
    ///
    /// # パニック
    ///
    /// 次のときに、ここでパニックします。起動時に気づけるようにするためです。
    ///
    /// - 同じメソッドとパスのルートが2本ある
    /// - ルート名が重なっている
    /// - 登録していないミドルウェアの名前が使われている
    pub fn create(self) -> Application {
        let mut defs = self.routing.defs;
        if let Some(path) = self.routing.health {
            let already = defs.iter().any(|d| d.path == path && d.method == "GET");
            if already {
                tracing::warn!("{path} は routes/*.rs にもあるため、health の登録を飛ばします");
            } else {
                defs.extend(crate::http::routing::collect(|| {
                    crate::http::Route::get(&path, health);
                }));
            }
        }
        // 名前を実体に置き換え、ルートごとに1本の並びにして固定する。
        // ここで済ませておけば、リクエスト処理中は読むだけで済む。
        // 共通のものは別に1本持つので、ルートの並びには入れない（二重に通さない）。
        //
        // **`with_middleware` を一度も書いていないときも必ず通します。**
        // 以前は「1つも登録されていなければ組み立てを省く」としていましたが、
        // 未登録の名前に気づく仕組み（`aliased_stack` のパニック）もそこにあるため、
        // `.middleware("admin")` が無言で素通しになっていました。
        for def in &mut defs {
            if def.middleware.is_empty() {
                continue;
            }
            let stack = self.middleware.aliased_stack(&def.middleware);
            if !stack.is_empty() {
                def.stack = Some(stack);
            }
        }
        let global = self.middleware.global_stack();

        // 聞く側を固定する。以後は読むだけ。
        crate::events::install(self.events);

        // 置き場所と、その実体を起動時に1回だけ決める。以後は読むだけ。
        let public_dir: Arc<Path> = Arc::from(paths::app_path("public"));
        let public_canonical = public_dir.canonicalize().ok().map(Arc::from);
        let storage_public_dir: Arc<Path> = Arc::from(crate::storage::Storage::public_root());
        let storage_public_canonical = storage_public_dir.canonicalize().ok().map(Arc::from);

        Application {
            inner: Arc::new(Inner {
                routes: Routes::build(defs),
                global,
                public_dir,
                public_canonical,
                storage_public_dir,
                storage_public_canonical,
                debug: app_config().debug,
                exceptions: (!self.exceptions.is_empty()).then(|| Arc::new(self.exceptions)),
            }),
        }
    }
}

/// `health("/up")` が返す内容。
async fn health() -> crate::error::Result<Response> {
    let config = app_config();
    Response::json(&serde_json::json!({
        "status": "ok",
        "name": config.name,
        "env": config.env,
    }))
}

/// ルートの決め方。
#[derive(Default)]
pub struct Routing {
    defs: Vec<RouteDef>,
    health: Option<String>,
}

impl Routing {
    /// `routes/web.rs` のルート定義を取り込む。
    ///
    /// ```ignore
    /// r.web(crate::routes::web::routes)
    /// ```
    pub fn web(mut self, define: impl FnOnce()) -> Self {
        self.defs.extend(crate::http::routing::collect(define));
        self
    }

    /// 生存確認用のルートを足す（Laravel の `/up` に当たります）。
    pub fn health(mut self, path: &str) -> Self {
        self.health = Some(crate::http::routing::normalize(path));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result as AppResult;
    use crate::http::{Next, Route};

    async fn ok() -> AppResult<Response> {
        Ok(Response::text("ok"))
    }

    /// 共通ミドルウェアの見本（Laravel の AddPoweredBy に当たる形）。
    async fn powered_by(req: Request, next: Next) -> AppResult<Response> {
        let response = next.run(req).await?;
        Ok(response
            .with_header("x-powered-by", "bengara")
            .with_added_header("x-mark", "global"))
    }

    /// ルートに付けるミドルウェアの見本。
    async fn marker(req: Request, next: Next) -> AppResult<Response> {
        let response = next.run(req).await?;
        Ok(response.with_added_header("x-mark", "route"))
    }

    fn app() -> Application {
        Application::configure()
            .with_middleware(|m| m.append(powered_by).alias("marked", marker))
            .with_routing(|r| {
                r.web(|| {
                    Route::get("/", ok);
                    Route::post("/posts", ok);
                    Route::get("/admin", ok).middleware("marked");
                })
            })
            .create()
    }

    fn marks(response: &Response) -> Vec<&str> {
        response
            .headers()
            .iter()
            .filter(|(k, _)| k == "x-mark")
            .map(|(_, v)| v.as_str())
            .collect()
    }

    #[tokio::test]
    async fn 共通ミドルウェアは404にも通る() {
        let response = app().handle(Request::new("GET", "/missing")).await;
        assert_eq!(response.status(), 404);
        assert_eq!(response.header("x-powered-by"), Some("bengara"));
    }

    #[tokio::test]
    async fn 共通ミドルウェアは405にも通る() {
        let response = app().handle(Request::new("DELETE", "/")).await;
        assert_eq!(response.status(), 405);
        assert_eq!(response.header("x-powered-by"), Some("bengara"));
        // HEAD は GET で受けるので、GET があれば HEAD も並べる。
        assert_eq!(response.header("allow"), Some("GET, HEAD"));
    }

    #[tokio::test]
    async fn 共通が外側でルートのものが内側() {
        let response = app().handle(Request::new("GET", "/admin")).await;
        assert_eq!(response.status(), 200);
        // 戻りは内側から順に書き足されるので、ルートのものが先に並ぶ。
        assert_eq!(marks(&response), vec!["route", "global"]);
    }

    #[tokio::test]
    async fn 共通は二重に通らない() {
        let response = app().handle(Request::new("GET", "/")).await;
        assert_eq!(response.status(), 200);
        assert_eq!(marks(&response), vec!["global"]);
    }

    #[test]
    #[should_panic(expected = "ミドルウェア `missing` は登録されていません")]
    fn with_middlewareを書かなくても未登録の名前で止まる() {
        // 以前は「1つも登録されていなければ検査を省く」形だったので、
        // init が作る bootstrap/app.rs（with_middleware なし）のままだと
        // `.middleware("missing")` が無言で素通しになっていた。
        Application::configure()
            .with_routing(|r| {
                r.web(|| {
                    Route::get("/", ok).middleware("missing");
                })
            })
            .create();
    }

    #[tokio::test]
    async fn ミドルウェアを使わないルートはそのまま動く() {
        // 検査を必ず通す形にしても、名前を付けていないルートには何も掛からない。
        let app = Application::configure()
            .with_routing(|r| {
                r.web(|| {
                    Route::get("/", ok);
                })
            })
            .create();
        let response = app.handle(Request::new("GET", "/")).await;
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn headは本文を外してもcontent_lengthを残す() {
        let response = app().handle(Request::new("HEAD", "/")).await;
        assert_eq!(response.status(), 200);
        assert!(response.body().is_empty(), "本文は返さない");
        assert_eq!(
            response.header("content-length"),
            Some("2"),
            "`ok` の 2 バイト"
        );
    }
}
