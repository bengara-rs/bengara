//! ログインしていなければ止めるミドルウェア。
//!
//! ```ignore
//! // bootstrap/app.rs
//! .alias("auth", Authenticate::new())
//! ```
//!
//! ```ignore
//! Route::get("/profile", UserController::show).middleware("auth");
//! ```

use crate::error::Error;
use crate::http::handler::BoxFuture;
use crate::http::middleware::{Middleware, Next};
use crate::http::Request;

/// 戻り先をセッションに入れておく場所。
const INTENDED_KEY: &str = "bengara_auth_intended";

/// 自分のサイトの中を指すパスか。
///
/// ログイン後の戻り先として覚えてよいかを決めます。`//evil.example` は
/// 「プロトコル相対 URL」で、転送先に使うと**外のサイト**へ飛ばせます。
/// `Location: //evil.example` はブラウザが `https://evil.example` と読むためです。
///
/// いまは覚えるだけで誰も読んでいませんが、読む側（Laravel の `intended()`）を
/// あとで足すときに、入れるところで止めておくほうが見落としません。
fn is_internal_path(path: &str) -> bool {
    path.starts_with('/') && !path.starts_with("//")
}

/// ログイン必須にするミドルウェア。
///
/// | 状況 | 返すもの |
/// |---|---|
/// | JSON を期待している | 401 |
/// | 転送先を指定していない | 401 |
/// | 転送先を指定している | 302 |
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
        if req.auth().check() {
            return next.run(req);
        }

        let options = next.render_options();
        let redirect_to = self.redirect_to.clone();
        Box::pin(async move {
            // 戻り先を覚えておく（ログイン後に元の画面へ返せるように）。
            // **自分のサイトの中だけ**を覚えます。
            if let Some(session) = req.try_session() {
                let intended = req.full_path();
                if is_internal_path(&intended) {
                    session.put(INTENDED_KEY, intended);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 転送先は任意() {
        assert!(Authenticate::new().redirect_to.is_none());
        assert_eq!(
            Authenticate::redirect_to("/login").redirect_to.as_deref(),
            Some("/login")
        );
        assert!(Authenticate::default().redirect_to.is_none());
    }

    #[test]
    fn 戻り先は自分のサイトの中だけ覚える() {
        assert!(is_internal_path("/"));
        assert!(is_internal_path("/profile"));
        assert!(is_internal_path("/profile?page=2"));

        // プロトコル相対 URL は外のサイトを指せる。
        assert!(!is_internal_path("//evil.example"));
        assert!(!is_internal_path("//evil.example/login"));
        // 絶対 URL も断る。
        assert!(!is_internal_path("https://evil.example"));
        // `/` で始まらないものも断る。
        assert!(!is_internal_path(""));
        assert!(!is_internal_path("profile"));

        assert_eq!(INTENDED_KEY, "bengara_auth_intended");
    }
}
