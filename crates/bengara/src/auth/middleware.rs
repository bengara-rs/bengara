//! ログインの有無で止めるミドルウェア。
//!
//! | 型 | 止める相手 | Laravel |
//! |---|---|---|
//! | `Authenticate` | ログインしていない人 | `auth` |
//! | `RedirectIfAuthenticated` | ログイン中の人 | `guest` |
//!
//! ```ignore
//! // bootstrap/app.rs
//! .alias("auth", Authenticate::new())
//! .alias("guest", RedirectIfAuthenticated::new())
//! ```
//!
//! ```ignore
//! Route::get("/profile", UserController::show).middleware("auth");
//! Route::post("/login", AuthController::login).middleware("guest");
//! ```

use crate::error::Error;
use crate::http::handler::BoxFuture;
use crate::http::middleware::{Middleware, Next};
use crate::http::Request;

/// 戻り先をセッションに入れておく場所。
///
/// 読む側は `Session::intended()` です。
pub(crate) const INTENDED_KEY: &str = "bengara_auth_intended";

/// 自分のサイトの中を指すパスか。
///
/// ログイン後の戻り先として覚えてよいかを決めます。`//evil.example` は
/// 「プロトコル相対 URL」で、転送先に使うと**外のサイト**へ飛ばせます。
/// `Location: //evil.example` はブラウザが `https://evil.example` と読むためです。
///
/// `/` で始まることだけを見ていたころは `/\evil.example` を通していました。
/// ブラウザはパスの中の `\` を `/` に直すので、これは `//evil.example` と同じ意味です。
/// タブや改行も URL から取り除かれるため、`/<タブ>/evil.example` が同じ形になります。
/// どちらも断ります。
pub(crate) fn is_internal_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    if path.contains('\\') || path.bytes().any(|b| b.is_ascii_control()) {
        return false;
    }
    !rest.starts_with('/')
}

/// 戻り先として覚えてよいメソッドか。
///
/// ログイン後の転送は `GET` です。`GET` で開けるパスだけを覚えます
/// （`HEAD` は `GET` のルートで受けるので、同じ扱いにします）。
fn remembers_intended(method: &str) -> bool {
    matches!(method, "GET" | "HEAD")
}

/// ログイン必須にするミドルウェア。
///
/// | 状況 | 返すもの |
/// |---|---|
/// | JSON を期待している | 401 |
/// | 転送先を指定していない | 401 |
/// | 転送先を指定している | 302 |
/// | セッションが無い（`StartSession` の登録し忘れ） | 500（直し方を添えたエラー） |
///
/// # ログイン後に元の画面へ返す
///
/// 転送するとき（上の表の 302 の行）だけ、**もとのパスをセッションに覚えます。**
/// ログインを処理するハンドラで `req.session().intended()` を呼ぶと取り出せます。
/// 覚えるのは自分のサイトの中を指すパスだけです。
///
/// ```ignore
/// // ログインできたあとで
/// let back = req.session().intended().unwrap_or_else(|| "/".to_string());
/// redirect().to(back)
/// ```
///
/// 401 を返すだけの経路（JSON を期待しているとき、転送先を決めていないとき）では
/// **覚えません。** 使う相手がいないのに、401 のたびにセッションを書くことになります。
///
/// **覚えるのは `GET` と `HEAD` のときだけです**（Laravel も `GET` のときだけ覚えます）。
/// ログイン後の転送は `GET` で飛ぶので、`POST` 専用のパスを覚えても 405 になります。
pub struct Authenticate {
    redirect_to: Option<String>,
}

impl Authenticate {
    /// 401 を返す形で作る。
    pub fn new() -> Self {
        Self { redirect_to: None }
    }

    /// 画面からの場合に転送する先を決める。
    ///
    /// JSON を期待しているリクエストには、転送ではなく 401 を返します。
    pub fn redirect_to(path: impl Into<String>) -> Self {
        Self {
            redirect_to: Some(path.into()),
        }
    }
}

impl Default for Authenticate {
    fn default() -> Self {
        Self::new()
    }
}

impl Middleware for Authenticate {
    fn handle(&self, req: Request, next: Next) -> BoxFuture {
        // セッションが無いと、ログインしていても理由なく 401 になる。登録し忘れを知らせる。
        if req.try_session().is_none() {
            return Box::pin(async {
                Err(Error::msg(
                    "ログインの確認にはセッションが要ります。bootstrap/app.rs で \
                     StartSession を Authenticate（auth）より前に登録してください",
                ))
            });
        }
        if req.auth().check() {
            return next.run(req);
        }

        let options = next.render_options();
        let redirect_to = self.redirect_to.clone();
        Box::pin(async move {
            // 転送するかどうかを先に決める。転送しないなら戻り先も覚えない。
            let will_redirect = redirect_to.is_some() && !options.wants_json;

            // 戻り先を覚えておく（ログイン後に元の画面へ返せるように）。
            // **自分のサイトの中だけ**を覚えます。
            // 401 を返すだけの経路で覚えても使い道が無く、401 のたびに
            // セッションの書き込みが増えるだけなので、転送するときに限ります。
            //
            // **`GET` と `HEAD` のときだけ覚えます。** ログイン後は `GET` で飛ばすので、
            // `POST` 専用の口のパスを覚えると、ログインできた直後に 405 になります。
            if will_redirect && remembers_intended(req.method()) {
                if let Some(session) = req.try_session() {
                    let intended = req.full_path();
                    if is_internal_path(&intended) {
                        session.put(INTENDED_KEY, intended);
                    }
                }
            }

            match redirect_to {
                // JSON が欲しいなら、転送ではなく 401 を返す。
                Some(path) if !options.wants_json => crate::http::redirect().to(path),
                _ => {
                    let error = Error::http(401, "ログインしてください");
                    tracing::debug!("{error}");
                    Ok(crate::http::response::error_response(&error, options))
                }
            }
        })
    }
}

/// ログイン中の人を止めるミドルウェア。Laravel の `guest` に当たります。
///
/// ログイン・登録・パスワード再設定の口に掛けます。
///
/// | 状況 | 返すもの |
/// |---|---|
/// | ログインしていない | そのまま通す |
/// | JSON を期待している | 403 |
/// | 転送先を指定していない | 403 |
/// | 転送先を指定している | 302 |
///
/// **回数の制限と組み合わせるときに要ります。** `Throttle` はログイン中の人を
/// `user:{id}` ごとに数えます。ログイン中でも登録の口を叩けると、登録し直すたびに
/// 新しい枠を得て、制限を回せてしまいます。`guest` で止めるか、
/// `Throttle::by_ip` で接続元ごとに数えてください。
///
/// Laravel は転送だけを返しますが、ここでは転送先を決めないと 403 を返します。
/// 行き先（ログイン後の画面）はアプリごとに違い、決め打ちにできないためです。
pub struct RedirectIfAuthenticated {
    redirect_to: Option<String>,
}

impl RedirectIfAuthenticated {
    /// 403 を返す形で作る。
    pub fn new() -> Self {
        Self { redirect_to: None }
    }

    /// 画面からの場合に転送する先を決める。
    ///
    /// JSON を期待しているリクエストには、転送ではなく 403 を返します。
    pub fn redirect_to(path: impl Into<String>) -> Self {
        Self {
            redirect_to: Some(path.into()),
        }
    }
}

impl Default for RedirectIfAuthenticated {
    fn default() -> Self {
        Self::new()
    }
}

impl Middleware for RedirectIfAuthenticated {
    fn handle(&self, req: Request, next: Next) -> BoxFuture {
        if req.auth().guest() {
            return next.run(req);
        }

        let options = next.render_options();
        let redirect_to = self.redirect_to.clone();
        Box::pin(async move {
            match redirect_to {
                // JSON が欲しいなら、転送ではなく 403 を返す。
                Some(path) if !options.wants_json => crate::http::redirect().to(path),
                _ => {
                    let error = Error::http(403, "ログイン中は使えません");
                    tracing::debug!("{error}");
                    Ok(crate::http::response::error_response(&error, options))
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// リクエストを 1 本通し、状態を返す。
    async fn guest_status(mw: RedirectIfAuthenticated, logged_in: bool, wants_json: bool) -> u16 {
        use crate::http::handler::Erased;
        use std::sync::Arc;

        let mut req = Request::new("POST", "/login");
        req.set_session(crate::session::Session::empty());
        if logged_in {
            req.auth().login_using_id(5).expect("ログインできる");
        }

        let stack: Arc<[Arc<dyn Middleware>]> = Arc::from(Vec::new());
        let handler = Arc::new(Erased::<_, ()>::new(|| async {
            Ok(crate::http::Response::text("ok"))
        }));
        let options = crate::http::response::RenderOptions::new(false, wants_json);
        mw.handle(req, Next::new(stack, handler, options))
            .await
            .expect("応答が返る")
            .status()
    }

    #[tokio::test]
    async fn ログイン中の人を止める() {
        // ログインしていなければ通す。
        assert_eq!(
            guest_status(RedirectIfAuthenticated::new(), false, false).await,
            200
        );
        assert_eq!(
            guest_status(RedirectIfAuthenticated::redirect_to("/home"), false, true).await,
            200
        );

        // ログイン中なら止める。
        assert_eq!(
            guest_status(RedirectIfAuthenticated::new(), true, false).await,
            403
        );
        assert_eq!(
            guest_status(RedirectIfAuthenticated::redirect_to("/home"), true, false).await,
            302
        );
        // JSON が欲しい相手には、転送先があっても 403。
        assert_eq!(
            guest_status(RedirectIfAuthenticated::redirect_to("/home"), true, true).await,
            403
        );

        assert!(RedirectIfAuthenticated::default().redirect_to.is_none());
    }

    #[tokio::test]
    async fn セッションが無ければ登録し忘れを知らせる() {
        use crate::http::handler::Erased;
        use std::sync::Arc;

        // StartSession を付け忘れると、理由の分からない 401 になっていた。
        let req = Request::new("GET", "/profile");
        let stack: Arc<[Arc<dyn Middleware>]> = Arc::from(Vec::new());
        let handler = Arc::new(Erased::<_, ()>::new(|| async {
            Ok(crate::http::Response::text("ok"))
        }));
        let options = crate::http::response::RenderOptions::new(false, false);
        let error = Authenticate::new()
            .handle(req, Next::new(stack, handler, options))
            .await
            .unwrap_err();
        assert_eq!(error.status(), 500);
        assert!(error.to_string().contains("StartSession"), "{error}");
    }

    #[test]
    fn 転送先は任意() {
        assert!(Authenticate::new().redirect_to.is_none());
        assert_eq!(
            Authenticate::redirect_to("/login").redirect_to.as_deref(),
            Some("/login")
        );
        assert!(Authenticate::default().redirect_to.is_none());
    }

    /// 未ログインのリクエストを 1 本通し、状態と、覚えた戻り先を返す。
    async fn stopped(mw: Authenticate, method: &str) -> (u16, Option<String>) {
        use crate::http::handler::Erased;
        use std::sync::Arc;

        let session = crate::session::Session::empty();
        let mut req = Request::new(method, "/profile").with_query("page=2");
        req.set_session(session.clone());

        let stack: Arc<[Arc<dyn Middleware>]> = Arc::from(Vec::new());
        let handler = Arc::new(Erased::<_, ()>::new(|| async {
            Ok(crate::http::Response::text("ok"))
        }));
        let options = crate::http::response::RenderOptions::new(false, false);
        let response = mw
            .handle(req, Next::new(stack, handler, options))
            .await
            .expect("応答が返る");
        (response.status(), session.get(INTENDED_KEY))
    }

    #[tokio::test]
    async fn 戻り先はgetとheadのときだけ覚える() {
        // ログイン後は GET で飛ばす。POST 専用の口を覚えると 405 になる。
        for method in ["GET", "HEAD"] {
            let (status, intended) = stopped(Authenticate::redirect_to("/login"), method).await;
            assert_eq!(status, 302, "{method}");
            assert_eq!(intended.as_deref(), Some("/profile?page=2"), "{method}");
        }
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            let (status, intended) = stopped(Authenticate::redirect_to("/login"), method).await;
            assert_eq!(status, 302, "{method}: 転送はする");
            assert_eq!(intended, None, "{method}: 戻り先は覚えない");
        }

        // 401 を返すだけの経路では、GET でも覚えない（今までどおり）。
        let (status, intended) = stopped(Authenticate::new(), "GET").await;
        assert_eq!(status, 401);
        assert_eq!(intended, None);

        assert!(remembers_intended("GET"));
        assert!(remembers_intended("HEAD"));
        // メソッドは大文字小文字を区別する（`get` はどのルートにも当たらない）。
        assert!(!remembers_intended("get"));
        assert!(!remembers_intended("OPTIONS"));
    }

    #[test]
    fn 戻り先は自分のサイトの中だけ覚える() {
        assert!(is_internal_path("/"));
        assert!(is_internal_path("/profile"));
        assert!(is_internal_path("/profile?page=2"));

        // プロトコル相対 URL は外のサイトを指せる。
        assert!(!is_internal_path("//evil.example"));
        assert!(!is_internal_path("//evil.example/login"));
        // `\` はブラウザが `/` に直すので、これも外のサイトを指せる。
        assert!(!is_internal_path("/\\evil.example"));
        assert!(!is_internal_path("/\\evil.example/login"));
        assert!(!is_internal_path("/\\/evil.example"));
        assert!(
            !is_internal_path("/path/\\evil.example"),
            "途中の `\\` も断る"
        );
        // タブや改行は URL から取り除かれるので、`//` の形になる。
        assert!(!is_internal_path("/\t/evil.example"));
        assert!(!is_internal_path("/\n/evil.example"));
        assert!(!is_internal_path("/\r/evil.example"));
        // 絶対 URL も断る。
        assert!(!is_internal_path("https://evil.example"));
        // `/` で始まらないものも断る。
        assert!(!is_internal_path(""));
        assert!(!is_internal_path("profile"));

        assert_eq!(INTENDED_KEY, "bengara_auth_intended");
    }
}
