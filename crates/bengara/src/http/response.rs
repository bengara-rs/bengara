//! レスポンス。

use serde::Serialize;

use crate::error::{reason_phrase, Error, Result};

/// クライアントに返す内容。
#[derive(Debug, Clone)]
pub struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    /// ステータスだけを決めた空のレスポンス。
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// `text/plain` を返す。
    pub fn text(body: impl Into<String>) -> Self {
        Self::new(200)
            .with_header("content-type", "text/plain; charset=utf-8")
            .with_body(body.into().into_bytes())
    }

    /// `text/html` を返す。
    pub fn html(body: impl Into<String>) -> Self {
        Self::new(200)
            .with_header("content-type", "text/html; charset=utf-8")
            .with_body(body.into().into_bytes())
    }

    /// `application/json` を返す。
    pub fn json<T: Serialize>(value: &T) -> Result<Self> {
        let body = serde_json::to_vec(value)?;
        Ok(Self::new(200)
            .with_header("content-type", "application/json")
            .with_body(body))
    }

    /// 本文のない 204 を返す。
    pub fn no_content() -> Self {
        Self::new(204)
    }

    /// 任意のバイト列を返す。
    pub fn bytes(content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self::new(200)
            .with_header("content-type", content_type)
            .with_body(body)
    }

    /// ステータスを変える。
    pub fn with_status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }

    /// ヘッダーを足す。
    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_ascii_lowercase(), value.into()));
        self
    }

    /// 本文を差し替える。
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    /// ステータス。
    pub fn status(&self) -> u16 {
        self.status
    }

    /// ヘッダーの値（最初に見つかったもの）。
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }

    /// ヘッダーの一覧。
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// 本文。
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// 本文を文字列として見る（UTF-8 でない部分は置き換える）。
    pub fn body_text(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }
}

/// `text/plain` のレスポンスを作る。
pub fn text(body: impl Into<String>) -> Result<Response> {
    Ok(Response::text(body))
}

/// `text/html` のレスポンスを作る。
pub fn html(body: impl Into<String>) -> Result<Response> {
    Ok(Response::html(body))
}

/// JSON のレスポンスを作る。
pub fn json<T: Serialize>(value: &T) -> Result<Response> {
    Response::json(value)
}

/// リダイレクトの組み立て。
///
/// ```ignore
/// redirect().to("/login")
/// redirect().route("home")
/// ```
#[derive(Debug, Clone, Copy)]
pub struct Redirect {
    status: u16,
}

/// リダイレクトを作り始める。既定は 302。
pub fn redirect() -> Redirect {
    Redirect { status: 302 }
}

impl Redirect {
    /// 301（恒久的な移動）にする。
    pub fn permanent(mut self) -> Self {
        self.status = 301;
        self
    }

    /// ステータスを自分で決める。
    pub fn with_status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }

    /// パスへリダイレクトする。
    pub fn to(self, path: impl Into<String>) -> Result<Response> {
        Ok(Response::new(self.status).with_header("location", path.into()))
    }

    /// 名前付きルートへリダイレクトする。
    pub fn route(self, name: &str) -> Result<Response> {
        self.to(super::routing::route(name)?)
    }

    /// 名前付きルートへ、パス引数を埋めてリダイレクトする。
    pub fn route_with(self, name: &str, params: &[(&str, &str)]) -> Result<Response> {
        self.to(super::routing::route_with(name, params)?)
    }
}

/// そのステータスで処理を打ち切る。
///
/// ```ignore
/// if !allowed { return abort(403); }
/// ```
pub fn abort<T>(status: u16) -> Result<T> {
    Err(Error::Http {
        status,
        message: String::new(),
    })
}

/// メッセージ付きで処理を打ち切る。
pub fn abort_with<T>(status: u16, message: impl Into<String>) -> Result<T> {
    Err(Error::Http {
        status,
        message: message.into(),
    })
}

/// エラーをレスポンスに変える。
///
/// `debug` が真なら、画面に詳しい内容を出します。
pub(crate) fn error_response(error: &Error, debug: bool) -> Response {
    let status = error.status();
    let title = format!("{status} {}", reason_phrase(status));
    let detail = if debug {
        let mut detail = error.to_string();
        let mut source = std::error::Error::source(error);
        while let Some(e) = source {
            detail.push_str(&format!("\n  原因: {e}"));
            source = e.source();
        }
        detail
    } else {
        error.public_message()
    };
    Response::html(error_page(&title, &detail, debug)).with_status(status)
}

fn error_page(title: &str, detail: &str, debug: bool) -> String {
    let detail = escape_html(detail);
    let hint = if debug {
        "<p class=\"hint\">この詳細は APP_DEBUG=true のときだけ表示されます。</p>"
    } else {
        ""
    };
    format!(
        "<!doctype html>\n\
         <html lang=\"ja\"><head><meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{title}</title>\n\
         <style>\n\
         :root {{ color-scheme: light dark; }}\n\
         body {{ font-family: system-ui, sans-serif; margin: 0; display: grid; place-items: center; min-height: 100vh; }}\n\
         main {{ max-width: 44rem; padding: 2rem; }}\n\
         h1 {{ font-size: 1.5rem; margin: 0 0 1rem; }}\n\
         pre {{ background: rgba(127,127,127,.12); padding: 1rem; border-radius: .5rem; white-space: pre-wrap; word-break: break-word; }}\n\
         .hint {{ opacity: .6; font-size: .85rem; }}\n\
         </style></head>\n\
         <body><main><h1>{title}</h1><pre>{detail}</pre>{hint}</main></body></html>\n"
    )
}

/// HTML に埋め込める形にする。
///
/// 利用者から来た文字列をそのまま HTML に入れると、意図しないタグを書き込まれます
/// （クロスサイトスクリプティング）。画面に出す前にこれを通してください。
///
/// ```ignore
/// html(format!("<h1>{} さん、こんにちは</h1>", escape_html(name)))
/// ```
pub fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn テキストのレスポンス() {
        let res = Response::text("hello");
        assert_eq!(res.status(), 200);
        assert_eq!(
            res.header("Content-Type"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(res.body_text(), "hello");
    }

    #[test]
    fn jsonのレスポンス() {
        let res = Response::json(&serde_json::json!({"a": 1})).unwrap();
        assert_eq!(res.header("content-type"), Some("application/json"));
        assert_eq!(res.body_text(), "{\"a\":1}");
    }

    #[test]
    fn リダイレクト() {
        let res = redirect().to("/login").unwrap();
        assert_eq!(res.status(), 302);
        assert_eq!(res.header("location"), Some("/login"));
        let res = redirect().permanent().to("/new").unwrap();
        assert_eq!(res.status(), 301);
    }

    #[test]
    fn abortはエラーになる() {
        let err = abort::<Response>(403).unwrap_err();
        assert_eq!(err.status(), 403);
    }

    #[test]
    fn htmlエスケープ() {
        assert_eq!(
            escape_html("<a href=\"x\">&</a>"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&lt;/a&gt;"
        );
    }
}
