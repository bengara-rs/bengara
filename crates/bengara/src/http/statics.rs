//! 静的ファイルの配信。
//!
//! 探す順番は次のとおりです。最初に見つかったものを返します。
//!
//! | 順 | 探す先                                   | いつ                     |
//! |----|------------------------------------------|--------------------------|
//! | 1  | `/storage/...` → `storage/app/public/`   | `GET` と `HEAD`          |
//! | 2  | バイナリに埋め込んだ `public/`           | リリースビルド           |
//! | 3  | ディスクの `public/`                     | 埋め込みに無いとき       |
//!
//! 埋め込みはリリースビルドのときだけ入ります（`bengara-build` が `PROFILE` を見ます）。
//! デバッグビルドでは常にディスクを読むので、ファイルを直せばすぐ反映されます。

use std::path::{Component, Path, PathBuf};

use crate::http::Response;

/// `/storage/...` で配信する頭のパス。
pub(crate) const STORAGE_PREFIX: &str = "/storage/";

/// バイナリに埋め込んだ `public/`。起動時に1回だけ入れ、以後は読むだけです。
static EMBEDDED: std::sync::OnceLock<&'static [(&'static str, &'static [u8])]> =
    std::sync::OnceLock::new();

/// 埋め込んだ一覧を固定する。`bengara::app!()` が生成したものを渡します。
pub(crate) fn install_embedded(files: &'static [(&'static str, &'static [u8])]) {
    // 2回目は黙って無視する（テストから複数回呼ばれても落ちないように）。
    let _ = EMBEDDED.set(files);
}

/// 埋め込んだ一覧。入っていなければ空。
pub(crate) fn embedded() -> &'static [(&'static str, &'static [u8])] {
    EMBEDDED.get().copied().unwrap_or(&[])
}

/// バイナリに埋め込んだ `public/` から探して返す。
///
/// 鍵は `public/` からの相対パスで、区切りは常に `/` です。
pub(crate) fn serve_embedded(
    files: &'static [(&'static str, &'static [u8])],
    request_path: &str,
) -> Option<Response> {
    if files.is_empty() {
        return None;
    }
    let relative = safe_relative(request_path)?;
    let key = to_key(&relative)?;

    // そのままの鍵で探し、無ければディレクトリとして index.html を探す。
    let found = files.iter().find(|(name, _)| *name == key).or_else(|| {
        let nested = format!("{key}/index.html");
        files.iter().find(|(name, _)| *name == nested)
    })?;

    Some(Response::bytes(
        content_type(Path::new(found.0)),
        found.1.to_vec(),
    ))
}

/// `storage/app/public/` から探して返す（`/storage/...`）。
///
/// 置き場所の外に出ようとしたときは、エラーにせず `None`（＝404）にします。
/// 攻撃の試みに「何が起きたか」を教えないためです。
pub(crate) fn serve_storage(public_disk: &Path, request_path: &str) -> Option<Response> {
    let rest = request_path.strip_prefix(STORAGE_PREFIX)?;
    serve(public_disk, rest)
}

/// 配信するときの鍵（`/` 区切り）に直す。
fn to_key(relative: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?),
            _ => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// ディレクトリの中からファイルを探して返す。無ければ `None`。
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
    fn 鍵はスラッシュ区切りになる() {
        assert_eq!(
            to_key(&PathBuf::from("css").join("app.css")).as_deref(),
            Some("css/app.css")
        );
        assert_eq!(
            to_key(&PathBuf::from("robots.txt")).as_deref(),
            Some("robots.txt")
        );
        assert_eq!(to_key(&PathBuf::new()), None);
    }

    const EMBEDDED: &[(&str, &[u8])] = &[
        ("index.html", b"<h1>top</h1>"),
        ("css/app.css", b"body{}"),
        ("docs/index.html", b"<h1>docs</h1>"),
    ];

    #[test]
    fn 埋め込みから配信できる() {
        let res = serve_embedded(EMBEDDED, "/css/app.css").expect("あるはず");
        assert_eq!(res.status(), 200);
        assert_eq!(res.body(), b"body{}");
        assert_eq!(
            res.header("content-type").unwrap(),
            "text/css; charset=utf-8"
        );
    }

    #[test]
    fn 埋め込みのルートはindexを指す() {
        let res = serve_embedded(EMBEDDED, "/").expect("あるはず");
        assert_eq!(res.body(), b"<h1>top</h1>");
    }

    #[test]
    fn 埋め込みのディレクトリはindexを探す() {
        let res = serve_embedded(EMBEDDED, "/docs").expect("あるはず");
        assert_eq!(res.body(), b"<h1>docs</h1>");
    }

    #[test]
    fn 埋め込みに無ければnone() {
        assert!(serve_embedded(EMBEDDED, "/none.txt").is_none());
        assert!(serve_embedded(EMBEDDED, "/../secret").is_none());
    }

    #[test]
    fn 埋め込みが空ならnone() {
        assert!(serve_embedded(&[], "/css/app.css").is_none());
    }

    #[test]
    fn storageの頭が合わなければnone() {
        let dir = std::env::temp_dir();
        assert!(serve_storage(&dir, "/css/app.css").is_none());
        assert!(serve_storage(&dir, "/storage").is_none());
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
