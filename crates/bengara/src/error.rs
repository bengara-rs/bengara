//! エラー型。
//!
//! ハンドラは `Result<Response>` を返します。`Err` になったときは、
//! HTTP のステータスを持つエラーならそのステータス、そうでなければ 500 を返します。

use std::fmt;

/// bengara の `Result`。
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// bengara のエラー。
#[derive(Debug)]
pub enum Error {
    /// HTTP のステータスを指定して止める（`abort(403)` など）。
    Http { status: u16, message: String },
    /// 文字列のメッセージ。500 になります。
    Message(String),
    /// 入出力のエラー。500 になります。
    Io(std::io::Error),
    /// JSON の変換エラー。500 になります。
    Json(serde_json::Error),
}

impl Error {
    /// メッセージからエラーを作る。
    pub fn msg(message: impl Into<String>) -> Self {
        Error::Message(message.into())
    }

    /// HTTP のステータスを指定したエラーを作る。
    pub fn http(status: u16, message: impl Into<String>) -> Self {
        Error::Http {
            status,
            message: message.into(),
        }
    }

    /// クライアントに返すステータス。
    pub fn status(&self) -> u16 {
        match self {
            Error::Http { status, .. } => *status,
            _ => 500,
        }
    }

    /// 利用者に見せて差し支えない短いメッセージ。
    pub fn public_message(&self) -> String {
        match self {
            Error::Http { message, .. } if !message.is_empty() => message.clone(),
            _ => reason_phrase(self.status()).to_string(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Http { status, message } if message.is_empty() => {
                write!(f, "HTTP {status} {}", reason_phrase(*status))
            }
            Error::Http { status, message } => write!(f, "HTTP {status}: {message}"),
            Error::Message(m) => write!(f, "{m}"),
            Error::Io(e) => write!(f, "入出力エラー: {e}"),
            Error::Json(e) => write!(f, "JSON エラー: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::Message(s)
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error::Message(s.to_string())
    }
}

/// ステータスコードの説明文。
pub(crate) fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        419 => "Page Expired",
        422 => "Unprocessable Content",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}
