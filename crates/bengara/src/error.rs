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
    /// 入力の検査に落ちた。422 になります。
    ///
    /// `Box` に入れているのは、`Error` 全体が大きくなるのを防ぐためです。
    Validation(Box<crate::validation::ValidationErrors>),
    /// ほかのエラーをそのまま包む。500 になります。
    ///
    /// `Message(String)` と違い、**元のエラーを残します**。`source()` から
    /// 原因の連鎖をたどれます。
    Other(Box<dyn std::error::Error + Send + Sync>),
}

impl Error {
    /// メッセージからエラーを作る。
    pub fn msg(message: impl Into<String>) -> Self {
        Error::Message(message.into())
    }

    /// ほかのエラーを包む。原因の連鎖を残したいときに使います。
    ///
    /// ```ignore
    /// let parsed: u32 = text.parse().map_err(Error::other)?;
    /// ```
    pub fn other(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Error::Other(Box::new(error))
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
            Error::Validation(_) => 422,
            _ => 500,
        }
    }

    /// 利用者に見せて差し支えない短いメッセージ。
    pub fn public_message(&self) -> String {
        match self {
            Error::Http { message, .. } if !message.is_empty() => message.clone(),
            Error::Validation(e) => e.to_string(),
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
            Error::Validation(e) => write!(f, "入力の検査に落ちました: {e}"),
            Error::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Json(e) => Some(e),
            // 包んだエラーを返して、原因の連鎖をつなぐ。
            Error::Other(e) => Some(&**e),
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
///
/// 基本は `http` クレートの一覧（IANA の登録名）を使います。以前は手で並べた
/// 一覧だけだったので、413 などが「413 Unknown」になっていました。
///
/// 次のものだけは上書きします。
///
/// | ステータス | 説明文 | 理由 |
/// |---|---|---|
/// | 413 | `Content Too Large` | RFC 9110 の名前。Symfony も同じ |
/// | 419 | `Page Expired` | Laravel 独自（CSRF トークンの期限切れ） |
/// | 422 | `Unprocessable Content` | RFC 9110 の名前。Symfony も同じ |
///
/// どちらの一覧にも無いもの（範囲外や未登録の番号）は `Unknown` です。
pub(crate) fn reason_phrase(status: u16) -> &'static str {
    match status {
        413 => "Content Too Large",
        419 => "Page Expired",
        422 => "Unprocessable Content",
        _ => axum::http::StatusCode::from_u16(status)
            .ok()
            .and_then(|code| code.canonical_reason())
            .unwrap_or("Unknown"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn よく使うステータスには説明文がある() {
        // `abort(409)` などが「HTTP 409 Unknown」にならないことを確かめる。
        for status in [
            100, 101, 200, 201, 202, 204, 206, 301, 302, 303, 304, 307, 308, 400, 401, 402, 403,
            404, 405, 406, 407, 408, 409, 410, 411, 412, 413, 414, 415, 416, 417, 418, 419, 421,
            422, 423, 424, 426, 428, 429, 431, 451, 500, 501, 502, 503, 504, 505, 507, 508, 511,
        ] {
            assert_ne!(reason_phrase(status), "Unknown", "{status}");
        }
        assert_eq!(reason_phrase(409), "Conflict");
        assert_eq!(reason_phrase(504), "Gateway Timeout");
        // 以前は手で並べた一覧に無く「413 Unknown」になっていた。
        assert_eq!(reason_phrase(413), "Content Too Large");
        assert_eq!(reason_phrase(414), "URI Too Long");
        assert_eq!(reason_phrase(431), "Request Header Fields Too Large");
        // フレームワーク独自のものと、RFC 9110 の名前で上書きしたもの。
        assert_eq!(reason_phrase(419), "Page Expired");
        assert_eq!(reason_phrase(422), "Unprocessable Content");
        // 知らないものは Unknown のまま。
        assert_eq!(reason_phrase(599), "Unknown");
        assert_eq!(reason_phrase(1000), "Unknown");
    }

    #[test]
    fn メッセージの無いhttpエラーは説明文を出す() {
        let error = Error::http(409, "");
        assert_eq!(error.status(), 409);
        assert_eq!(error.public_message(), "Conflict");
        assert_eq!(error.to_string(), "HTTP 409 Conflict");
    }

    #[test]
    fn 包んだエラーは原因をたどれる() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "ファイルがありません");
        let error = Error::other(io);

        assert_eq!(error.status(), 500);
        assert!(error.to_string().contains("ファイルがありません"));
        // Message(String) と違い、元のエラーが残る。
        let source = error.source().expect("原因がある");
        assert!(source.to_string().contains("ファイルがありません"));
    }

    #[test]
    fn 文字列のエラーには原因がない() {
        assert!(Error::msg("ただの文").source().is_none());
    }

    #[test]
    fn 入出力とjsonの原因もたどれる() {
        let io = Error::Io(std::io::Error::other("壊れた"));
        assert!(io.source().is_some());

        let json = Error::Json(serde_json::from_str::<i32>("あ").unwrap_err());
        assert!(json.source().is_some());
    }
}
