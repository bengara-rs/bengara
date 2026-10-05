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
            if let Some(session) = req.try_session() {
                session.put("bengara_auth_intended", req.full_path());
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
}
