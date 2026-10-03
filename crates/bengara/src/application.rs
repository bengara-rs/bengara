//! アプリの組み立てと、1リクエストの処理。
//!
//! `bootstrap/app.rs` が返す `Application` が、起動時に固定される本体です。

use std::path::PathBuf;
use std::sync::Arc;

use crate::config_registry::app_config;
use crate::error::Error;
use crate::http::response::error_response;
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
    public_dir: PathBuf,
    debug: bool,
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
        let response = match self.inner.routes.find(req.method(), req.path()) {
            Matched::Found { def, params } => {
                req.set_params(params);
                req.set_route_name(def.name.clone());
                let future = def.handler.call(req);
                // ハンドラのパニックを1リクエストに閉じ込める。
                match tokio::spawn(future).await {
                    Ok(Ok(response)) => response,
                    Ok(Err(error)) => {
                        self.log_error(&error);
                        error_response(&error, self.inner.debug)
                    }
                    Err(join_error) => {
                        let error = Error::msg(panic_message(&join_error));
                        tracing::error!("ハンドラがパニックしました: {error}");
                        error_response(&error, self.inner.debug)
                    }
                }
            }
            Matched::MethodNotAllowed { mut allowed } => {
                allowed.sort_unstable();
                let error = Error::Http {
                    status: 405,
                    message: String::new(),
                };
                error_response(&error, self.inner.debug).with_header("allow", allowed.join(", "))
            }
            Matched::NotFound => self.serve_static_or_404(&req).await,
        };
        if is_head {
            response.with_body(Vec::new())
        } else {
            response
        }
    }

    async fn serve_static_or_404(&self, req: &Request) -> Response {
        if matches!(req.method(), "GET" | "HEAD") {
            let dir = self.inner.public_dir.clone();
            let path = req.path().to_string();
            let found =
                tokio::task::spawn_blocking(move || crate::http::statics::serve(&dir, &path))
                    .await
                    .unwrap_or(None);
            if let Some(response) = found {
                return response;
            }
        }
        error_response(
            &Error::Http {
                status: 404,
                message: String::new(),
            },
            self.inner.debug,
        )
    }

    fn log_error(&self, error: &Error) {
        if error.status() >= 500 {
            tracing::error!("{error}");
        } else {
            tracing::debug!("{error}");
        }
    }
}

fn panic_message(join_error: &tokio::task::JoinError) -> String {
    if join_error.is_cancelled() {
        return "処理が打ち切られました".to_string();
    }
    "ハンドラの中でパニックが起きました".to_string()
}

/// `Application` の組み立て。
#[derive(Default)]
pub struct ApplicationBuilder {
    routing: Routing,
}

impl ApplicationBuilder {
    /// ルートを決める。
    pub fn with_routing(mut self, configure: impl FnOnce(Routing) -> Routing) -> Self {
        self.routing = configure(std::mem::take(&mut self.routing));
        self
    }

    /// 組み立てを終えて `Application` にする。
    ///
    /// # パニック
    ///
    /// 同じメソッドとパスのルートが2本あるときは、ここでパニックします。
    /// 起動時に気づけるようにするためです。
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
        Application {
            inner: Arc::new(Inner {
                routes: Routes::build(defs),
                public_dir: paths::app_path("public"),
                debug: app_config().debug,
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
