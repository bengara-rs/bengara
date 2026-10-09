//! 静的ファイルの配信。
//!
//! 入口は2つあり、パスで**どちらか一方**に決まります。両方を続けて探すことはしません。
//!
//! | 入口                       | 探す先                                             |
//! |----------------------------|----------------------------------------------------|
//! | `/storage/...` で始まる    | `storage/app/public/` だけ                         |
//! | それ以外                   | 埋め込んだ `public/` → 無ければディスクの `public/` |
//!
//! どちらも `GET` と `HEAD` のときだけです。`/storage/x.css` が無くても
//! `public/storage/x.css` は探しません。
//!
//! 埋め込みはリリースビルドのときだけ入ります（`bengara-build` が `PROFILE` を見ます）。
//! デバッグビルドでは常にディスクを読むので、ファイルを直せばすぐ反映されます。
//!
//! ## 2回目以降の読み込み
//!
//! どちらの経路でも `ETag` と `Cache-Control` を付けます。ブラウザが
//! `If-None-Match` を送ってきて中身が変わっていなければ、本文を返さずに 304 を返します。
//! 画面1枚に20本のアセットがあると、リロードごとの全文送信がそのまま消えます。
//!
//! ETag は**弱い ETag**（`W/"..."`）です。ディスクのファイルは「長さと更新時刻」、
//! 埋め込みは「長さと中身のハッシュ」から作ります。中身を読まずに決まるので、
//! 304 を返すときはファイルを開きません。

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use crate::http::Response;

/// `/storage/...` で配信する頭のパス。
pub(crate) const STORAGE_PREFIX: &str = "/storage/";

/// 静的ファイルに付ける `Cache-Control`。
///
/// 中身が変わったことに必ず気づけるよう、毎回確かめさせます。確かめた結果
/// 変わっていなければ 304 なので、本文は流れません。
/// 名前に中身の印が入ったファイル（`app.abc123.js` など）に長い期限を付けたいときは、
/// 前段のプロキシで上書きしてください。
const CACHE_CONTROL: &str = "public, max-age=0, must-revalidate";

/// 静的ファイルに付ける `X-Content-Type-Options`。
///
/// `storage/app/public/` はアプリが置いたファイルを配る入口です。種類の取り違えで
/// `.html` や `.svg` が同じオリジンのスクリプトとして動くのを止めます。
const NO_SNIFF: &str = "nosniff";

/// 埋め込んだ1本ぶん。
pub(crate) struct EmbeddedFile {
    body: &'static [u8],
    /// 中身から作った弱い ETag。起動時に1回だけ作ります。
    etag: String,
}

/// 埋め込んだ `public/` の一覧。鍵は `public/` からの相対パス（区切りは常に `/`）。
pub(crate) type Embedded = HashMap<&'static str, EmbeddedFile>;

/// バイナリに埋め込んだ `public/`。起動時に1回だけ入れ、以後は読むだけです。
static EMBEDDED: OnceLock<Embedded> = OnceLock::new();

/// 埋め込んだ一覧を固定する。`bengara::app!()` が生成したものを渡します。
///
/// ここで一度だけ `HashMap` にします。1本ずつ探すと、見つからないとき（404 になる
/// すべてのリクエスト）に一覧を端まで見ることになります。
/// ETag も同時に作ります。中身は変わらないので、1回作れば使い回せます。
pub(crate) fn install_embedded(files: &'static [(&'static str, &'static [u8])]) {
    // 2回目は黙って無視する（テストから複数回呼ばれても落ちないように）。
    let _ = EMBEDDED.set(
        files
            .iter()
            .map(|(key, body)| {
                (
                    *key,
                    EmbeddedFile {
                        body,
                        etag: embedded_etag(body),
                    },
                )
            })
            .collect(),
    );
}

/// 埋め込んだ一覧。入っていなければ空。
pub(crate) fn embedded() -> &'static Embedded {
    static EMPTY: OnceLock<Embedded> = OnceLock::new();
    EMBEDDED
        .get()
        .unwrap_or_else(|| EMPTY.get_or_init(Embedded::new))
}

/// バイナリに埋め込んだ `public/` から探して返す。
pub(crate) fn serve_embedded(
    files: &'static Embedded,
    request_path: &str,
    if_none_match: Option<&str>,
) -> Option<Response> {
    if files.is_empty() {
        return None;
    }
    let relative = safe_relative(request_path)?;
    let key = to_key(&relative)?;

    // そのままの鍵で探し、無ければディレクトリとして index.html を探す。
    if let Some(file) = files.get(key.as_str()) {
        return Some(embedded_response(&key, file, if_none_match));
    }
    let nested = format!("{key}/index.html");
    let file = files.get(nested.as_str())?;
    Some(embedded_response(&nested, file, if_none_match))
}

fn embedded_response(
    key: &str,
    file: &'static EmbeddedFile,
    if_none_match: Option<&str>,
) -> Response {
    if etag_matches(if_none_match, &file.etag) {
        return not_modified(&file.etag);
    }
    // `static_bytes` は `&'static [u8]` をそのまま持つので、本文をコピーしない。
    Response::static_bytes(content_type(Path::new(key)), file.body)
        .with_header("etag", file.etag.clone())
        .with_header("cache-control", CACHE_CONTROL)
        .with_header("x-content-type-options", NO_SNIFF)
}

/// `storage/app/public/` から探して返す（`/storage/...`）。
///
/// 置き場所の外に出ようとしたときは、エラーにせず `None`（＝404）にします。
/// 攻撃の試みに「何が起きたか」を教えないためです。
pub(crate) fn serve_storage(
    public_disk: &Path,
    canonical_root: Option<&Path>,
    request_path: &str,
    if_none_match: Option<&str>,
    want_body: bool,
) -> Option<Response> {
    let rest = request_path.strip_prefix(STORAGE_PREFIX)?;
    // `/storage/` だけで来たときは 404。`safe_relative("")` は `index.html` を返すので、
    // そのまま渡すと `storage/app/public/index.html` を配ってしまいます。
    // 「空なら index.html」は `public/`（`GET /`）のための規則です。
    //
    // `/storage//` のように `/` が続くと `rest` が `"/"` になります。先頭の `/` を
    // 落としてから空かどうかを見ます（落とさないと空判定をすり抜けます）。
    if rest.trim_start_matches('/').is_empty() {
        return None;
    }
    serve(public_disk, canonical_root, rest, if_none_match, want_body)
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
///
/// `canonical_root` は置き場所の**実体**です。起動時に1回だけ解決したものを渡します
/// （以前はリクエストごとに `canonicalize` を2回叩いていました）。
/// まだ無かった置き場所（`storage/` を作る前に起動したとき）は `None` で渡され、
/// そのときだけここで解決します。
///
/// `want_body` が偽のときは**ファイルを読みません**。`HEAD` で 100 MB の
/// アセットを丸ごとメモリに読んでから捨てるのを避けるためです。長さは
/// `Content-Length` で知らせます。
pub(crate) fn serve(
    public_dir: &Path,
    canonical_root: Option<&Path>,
    request_path: &str,
    if_none_match: Option<&str>,
    want_body: bool,
) -> Option<Response> {
    let relative = safe_relative(request_path)?;
    let mut path = public_dir.join(&relative);
    if path.is_dir() {
        path.push("index.html");
    }
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    // シンボリックリンクなどで置き場所の外に出ていないか確かめる。
    let canonical = path.canonicalize().ok()?;
    let inside = match canonical_root {
        Some(root) => canonical.starts_with(root),
        None => public_dir
            .canonicalize()
            .map(|root| canonical.starts_with(&root))
            .unwrap_or(false),
    };
    if !inside {
        return None;
    }

    let etag = disk_etag(&metadata);
    if etag_matches(if_none_match, &etag) {
        return Some(not_modified(&etag));
    }

    let response = Response::new(200)
        .with_header("content-type", content_type(&canonical))
        .with_header("etag", etag)
        .with_header("cache-control", CACHE_CONTROL)
        .with_header("x-content-type-options", NO_SNIFF);
    if !want_body {
        // 本文を読まないので、長さは自分で知らせる。
        return Some(response.with_header("content-length", metadata.len().to_string()));
    }
    let body = std::fs::read(&canonical).ok()?;
    Some(response.with_body(body))
}

/// 本文を返さない 304。
fn not_modified(etag: &str) -> Response {
    Response::new(304)
        .with_header("etag", etag.to_string())
        .with_header("cache-control", CACHE_CONTROL)
}

/// ディスクのファイルの弱い ETag。長さと更新時刻から作ります。
fn disk_etag(metadata: &std::fs::Metadata) -> String {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    format!("W/\"{:x}-{:x}\"", metadata.len(), modified)
}

/// 埋め込んだファイルの弱い ETag。長さと中身から作ります。
///
/// 埋め込みに更新時刻はありません。中身のハッシュ（FNV-1a）を使うので、
/// 版が変わればかならず違う値になります。起動時に1回だけ計算します。
fn embedded_etag(body: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in body {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("W/\"{:x}-{hash:x}\"", body.len())
}

/// `If-None-Match` が合うか。
///
/// `*` と、カンマ区切りの一覧に対応します。弱い ETag しか作らないので、
/// `W/` の有無は無視して比べます。
fn etag_matches(header: Option<&str>, etag: &str) -> bool {
    let Some(header) = header else {
        return false;
    };
    let header = header.trim();
    if header == "*" {
        return true;
    }
    let want = etag.trim_start_matches("W/");
    header
        .split(',')
        .any(|candidate| candidate.trim().trim_start_matches("W/") == want)
}

/// リクエストのパスを、`public/` の下から出られない相対パスに直す。
fn safe_relative(request_path: &str) -> Option<PathBuf> {
    // `+` は空白にしません。`+` が空白なのはフォームの規則で、URL のパスには当てはまりません。
    let decoded = super::percent::decode_strict(request_path.trim_start_matches('/'));
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

    const FILES: &[(&str, &[u8])] = &[
        ("index.html", b"<h1>top</h1>"),
        ("css/app.css", b"body{}"),
        ("docs/index.html", b"<h1>docs</h1>"),
    ];

    /// テスト用に、埋め込みの一覧を1つ作って借り続ける。
    fn files() -> &'static Embedded {
        static ONCE: OnceLock<Embedded> = OnceLock::new();
        ONCE.get_or_init(|| {
            FILES
                .iter()
                .map(|(key, body)| {
                    (
                        *key,
                        EmbeddedFile {
                            body,
                            etag: embedded_etag(body),
                        },
                    )
                })
                .collect()
        })
    }

    fn empty() -> &'static Embedded {
        static ONCE: OnceLock<Embedded> = OnceLock::new();
        ONCE.get_or_init(Embedded::new)
    }

    #[test]
    fn 埋め込みから配信できる() {
        let res = serve_embedded(files(), "/css/app.css", None).expect("あるはず");
        assert_eq!(res.status(), 200);
        assert_eq!(res.body(), b"body{}");
        assert_eq!(
            res.header("content-type").unwrap(),
            "text/css; charset=utf-8"
        );
        assert!(res.header("etag").is_some());
        assert_eq!(res.header("cache-control"), Some(CACHE_CONTROL));
    }

    #[test]
    fn 埋め込みのルートはindexを指す() {
        let res = serve_embedded(files(), "/", None).expect("あるはず");
        assert_eq!(res.body(), b"<h1>top</h1>");
    }

    #[test]
    fn 埋め込みのディレクトリはindexを探す() {
        let res = serve_embedded(files(), "/docs", None).expect("あるはず");
        assert_eq!(res.body(), b"<h1>docs</h1>");
        assert_eq!(
            res.header("content-type").unwrap(),
            "text/html; charset=utf-8"
        );
    }

    #[test]
    fn 埋め込みに無ければnone() {
        assert!(serve_embedded(files(), "/none.txt", None).is_none());
        assert!(serve_embedded(files(), "/../secret", None).is_none());
    }

    #[test]
    fn 埋め込みが空ならnone() {
        assert!(serve_embedded(empty(), "/css/app.css", None).is_none());
    }

    #[test]
    fn 埋め込みは同じetagなら304を返す() {
        let first = serve_embedded(files(), "/css/app.css", None).expect("あるはず");
        let etag = first.header("etag").expect("etag が付く").to_string();

        let again = serve_embedded(files(), "/css/app.css", Some(&etag)).expect("あるはず");
        assert_eq!(again.status(), 304);
        assert!(again.body().is_empty(), "304 に本文は付けない");
        assert_eq!(again.header("etag"), Some(etag.as_str()));

        // 別の値なら 200 で中身を返す。
        let other = serve_embedded(files(), "/css/app.css", Some("W/\"0-0\"")).expect("あるはず");
        assert_eq!(other.status(), 200);
    }

    #[test]
    fn etagの比べ方() {
        assert!(etag_matches(Some("*"), "W/\"1-2\""));
        assert!(etag_matches(Some("W/\"1-2\""), "W/\"1-2\""));
        // 弱い印の有無は無視する。
        assert!(etag_matches(Some("\"1-2\""), "W/\"1-2\""));
        assert!(etag_matches(Some("W/\"9-9\", W/\"1-2\""), "W/\"1-2\""));
        assert!(!etag_matches(Some("W/\"9-9\""), "W/\"1-2\""));
        assert!(!etag_matches(None, "W/\"1-2\""));
    }

    #[test]
    fn 中身が違えばetagも違う() {
        assert_ne!(embedded_etag(b"a"), embedded_etag(b"b"));
        // 長さが同じで中身が違っても分かれる。
        assert_ne!(embedded_etag(b"ab"), embedded_etag(b"ba"));
        assert_eq!(embedded_etag(b"abc"), embedded_etag(b"abc"));
    }

    #[test]
    fn storageの頭が合わなければnone() {
        let dir = std::env::temp_dir();
        assert!(serve_storage(&dir, None, "/css/app.css", None, true).is_none());
        assert!(serve_storage(&dir, None, "/storage", None, true).is_none());
    }

    #[test]
    fn storageだけで来たらnone() {
        // `/storage/` は index.html を指さない。`public/` 用の規則を持ち込まない。
        let dir = std::env::temp_dir();
        assert!(serve_storage(&dir, None, STORAGE_PREFIX, None, true).is_none());
        // `/` が続いても同じ。先頭の `/` を落としてから空かどうかを見る。
        assert!(serve_storage(&dir, None, "/storage//", None, true).is_none());
        assert!(serve_storage(&dir, None, "/storage///", None, true).is_none());
    }

    #[test]
    fn 静的ファイルにはnosniffが付く() {
        // 埋め込みから。
        let res = serve_embedded(files(), "/css/app.css", None).expect("あるはず");
        assert_eq!(res.header("x-content-type-options"), Some(NO_SNIFF));

        // ディスクから。
        let dir = temp_dir("nosniff");
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let root = dir.canonicalize().unwrap();
        let res = serve(&dir, Some(&root), "/a.txt", None, true).expect("あるはず");
        assert_eq!(res.header("x-content-type-options"), Some(NO_SNIFF));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn パスのプラスは空白にしない() {
        assert_eq!(safe_relative("/a+b.css"), Some(PathBuf::from("a+b.css")));
        // `%20` は空白に戻る（こちらはパスでも同じ）。
        assert_eq!(safe_relative("/a%20b.css"), Some(PathBuf::from("a b.css")));
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

    /// テスト用の空ディレクトリ。名前が重ならないように用途を付ける。
    fn temp_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bengara-statics-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn ディスクから配信してetagを付ける() {
        let dir = temp_dir("disk");
        std::fs::write(dir.join("a.txt"), "やきそば").unwrap();
        let root = dir.canonicalize().unwrap();

        let res = serve(&dir, Some(&root), "/a.txt", None, true).expect("あるはず");
        assert_eq!(res.status(), 200);
        assert_eq!(res.body_text(), "やきそば");
        let etag = res.header("etag").expect("etag が付く").to_string();

        // 同じ etag なら 304。本文もファイルも読まない。
        let again = serve(&dir, Some(&root), "/a.txt", Some(&etag), true).expect("あるはず");
        assert_eq!(again.status(), 304);
        assert!(again.body().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 本文が要らないときは読まずに長さだけ返す() {
        let dir = temp_dir("head");
        std::fs::write(dir.join("a.txt"), "0123456789").unwrap();
        let root = dir.canonicalize().unwrap();

        let res = serve(&dir, Some(&root), "/a.txt", None, false).expect("あるはず");
        assert_eq!(res.status(), 200);
        assert!(res.body().is_empty(), "本文は読まない");
        assert_eq!(res.header("content-length"), Some("10"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 置き場所の実体を渡さなくても外には出られない() {
        let dir = temp_dir("root");
        std::fs::write(dir.join("a.txt"), "x").unwrap();

        // 実体を渡さない場合は、その場で解決する（起動時に無かった置き場所のため）。
        assert!(serve(&dir, None, "/a.txt", None, true).is_some());
        assert!(serve(&dir, None, "/../a.txt", None, true).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 無いファイルはnone() {
        let dir = temp_dir("missing");
        let root = dir.canonicalize().unwrap();
        assert!(serve(&dir, Some(&root), "/none.txt", None, true).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
