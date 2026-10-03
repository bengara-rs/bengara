//! `public/` の静的ファイル配信。
//!
//! Phase 1 ではディスクから読みます。リリースビルドでバイナリへ埋め込む仕組みは
//! これから作ります（`bengara/docs/backlog.md`）。

use std::path::{Component, Path, PathBuf};

use crate::http::Response;

/// `public/` の中からファイルを探して返す。無ければ `None`。
pub(crate) fn serve(public_dir: &Path, request_path: &str) -> Option<Response> {
    let relative = safe_relative(request_path)?;
    let mut path = public_dir.join(&relative);
    if path.is_dir() {
        path.push("index.html");
    }
    if !path.is_file() {
        return None;
    }
    // シンボリックリンクなどで public/ の外に出ていないか確かめる。
    let canonical_root = public_dir.canonicalize().ok()?;
    let canonical = path.canonicalize().ok()?;
    if !canonical.starts_with(&canonical_root) {
        return None;
    }
    let body = std::fs::read(&canonical).ok()?;
    Some(Response::bytes(content_type(&canonical), body))
}

/// リクエストのパスを、`public/` の下から出られない相対パスに直す。
fn safe_relative(request_path: &str) -> Option<PathBuf> {
    let decoded = super::request::percent_decode(request_path.trim_start_matches('/'));
    if decoded.contains('\0') {
        return None;
    }
    // `/` は `public/index.html` を指す。
    let decoded = if decoded.is_empty() {
        "index.html".to_string()
    } else {
        decoded
    };
    let mut out = PathBuf::new();
    for component in Path::new(&decoded).components() {
        match component {
            Component::Normal(part) => out.push(part),
            // `..`、`/`、`C:` などは受け付けない。
            Component::CurDir => {}
            _ => return None,
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// 拡張子から Content-Type を決める。分からなければバイナリとして扱う。
fn content_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "map" => "application/json",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "pdf" => "application/pdf",
        "wasm" => "application/wasm",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 親ディレクトリへは出られない() {
        assert_eq!(safe_relative("/../secret"), None);
        assert_eq!(safe_relative("/a/../../b"), None);
    }

    #[test]
    fn ルートはindexを指す() {
        assert_eq!(safe_relative("/"), Some(PathBuf::from("index.html")));
    }

    #[test]
    fn 普通のパスは通る() {
        assert_eq!(
            safe_relative("/css/app.css"),
            Some(PathBuf::from("css").join("app.css"))
        );
        assert_eq!(
            safe_relative("/robots.txt"),
            Some(PathBuf::from("robots.txt"))
        );
    }

    #[test]
    fn 拡張子から種類を決める() {
        assert_eq!(content_type(Path::new("a.css")), "text/css; charset=utf-8");
        assert_eq!(content_type(Path::new("a.PNG")), "image/png");
        assert_eq!(
            content_type(Path::new("a.unknown")),
            "application/octet-stream"
        );
    }
}
