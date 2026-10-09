//! URL の組み立てと、署名付き URL。
//!
//! ```ignore
//! url("/posts")                                  // http://localhost:8000/posts
//! signed_url("/unsubscribe", &[("user", "12")])  // 改ざんすると弾ける URL
//! ```

use crate::error::{Error, Result};
use crate::support::crypto;

/// 署名を入れるクエリの名前。
const SIGNATURE: &str = "signature";
/// 期限を入れるクエリの名前。
const EXPIRES: &str = "expires";

/// 署名の文字数。HMAC-SHA256 の 32 バイトを16進にすると 64 文字で固定です。
const SIGNATURE_CHARS: usize = 64;

/// 署名に使う鍵の用途名。
///
/// `APP_KEY` をそのまま使わず、用途ごとに別の鍵を作ります。
/// どれか1つの署名を手がかりにされても、ほかの用途へ広がらないようにするためです。
const SIGNING_LABEL: &str = "bengara:signed-url";

/// パスから、アプリの URL をつないだ絶対 URL を作る。
///
/// 元になるのは `APP_URL` です。
///
/// ```ignore
/// url("/posts")    // http://localhost:8000/posts
/// url("posts")     // 同じ（先頭の / は足されます）
/// ```
pub fn url(path: &str) -> String {
    let base = crate::config_registry::app_config()
        .url
        .trim_end_matches('/');
    if path.is_empty() {
        return base.to_string();
    }
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_string();
    }
    let path = path.trim_start_matches('/');
    format!("{base}/{path}")
}

/// 名前付きルートの絶対 URL。
pub fn route_url(name: &str) -> Result<String> {
    Ok(url(&super::routing::route(name)?))
}

/// 名前付きルートの絶対 URL（パス引数つき）。
pub fn route_url_with(name: &str, params: &[(&str, &str)]) -> Result<String> {
    Ok(url(&super::routing::route_with(name, params)?))
}

/// 改ざんを見つけられる URL を作る。
///
/// 退会用のリンクのように、**ログインしていない人に渡すリンク**で使います。
/// クエリを1文字でも書き換えると `has_valid_signature` が false になります。
///
/// ```ignore
/// let link = signed_url("/unsubscribe", &[("user", "12")])?;
/// ```
pub fn signed_url(path: &str, params: &[(&str, &str)]) -> Result<String> {
    build_signed(path, params, None)
}

/// 期限付きの署名付き URL。`expires` を過ぎると無効になります。
pub fn temporary_signed_url(
    path: &str,
    params: &[(&str, &str)],
    valid_for_secs: u64,
) -> Result<String> {
    let expires = now() + valid_for_secs;
    build_signed(path, params, Some(expires))
}

fn build_signed(path: &str, params: &[(&str, &str)], expires: Option<u64>) -> Result<String> {
    let key = crate::config_registry::app_config().derived_key(SIGNING_LABEL)?;
    build_signed_with(&key, path, params, expires)
}

/// 鍵を受け取って組み立てる。設定を触らないので、試験からも呼べます。
fn build_signed_with(
    key: &[u8],
    path: &str,
    params: &[(&str, &str)],
    expires: Option<u64>,
) -> Result<String> {
    // 空の path はルートとして扱う。ブラウザが開くと `req.path()` は `/` になるので、
    // ここで `/` にそろえないと末尾の 1 文字差で必ず署名が合わなくなる。
    let path = if path.is_empty() { "/" } else { path };
    check_signable_path(path)?;

    let mut pairs: Vec<(String, String)> = params
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    if pairs.iter().any(|(k, _)| k == SIGNATURE || k == EXPIRES) {
        return Err(Error::msg(
            "`signature` と `expires` は署名付き URL が使う名前なので、自分では指定できません",
        ));
    }
    if let Some(expires) = expires {
        pairs.push((EXPIRES.to_string(), expires.to_string()));
    }

    let absolute = url(path);
    let query = canonical_query(&pairs);
    let signature = crypto::to_hex(&crypto::hmac_sha256(
        key,
        payload(&absolute, &query).as_bytes(),
    ));

    let mut out = absolute;
    out.push('?');
    if !query.is_empty() {
        out.push_str(&query);
        out.push('&');
    }
    out.push_str(SIGNATURE);
    out.push('=');
    out.push_str(&signature);
    Ok(out)
}

/// 署名付き URL が正しいか確かめる。
///
/// ```ignore
/// pub async fn unsubscribe(req: Request) -> Result<Response> {
///     if !has_valid_signature(&req)? {
///         return abort(403);
///     }
///     // ...
/// }
/// ```
pub fn has_valid_signature(req: &super::Request) -> Result<bool> {
    let key = crate::config_registry::app_config().derived_key(SIGNING_LABEL)?;

    // クエリは `Request` が1回だけ解析したものを借りる（呼ぶたびに解析し直さない）。
    let mut pairs = Vec::new();
    let mut given = Vec::new();
    for (k, v) in req.query_pairs() {
        if k == SIGNATURE {
            given.push(v.clone());
        } else {
            pairs.push((k.clone(), v.clone()));
        }
    }
    Ok(verify_signature(&key, req.path(), &pairs, &given))
}

/// 鍵を受け取って照合する。設定を触らないので、試験からも呼べます。
///
/// `given` は、クエリに入っていた `signature` の値を**全部**並べたものです。
fn verify_signature(key: &[u8], path: &str, pairs: &[(String, String)], given: &[String]) -> bool {
    // `signature` は 1 つだけ。2 つ以上を後ろ勝ちにすると、前の値を無視したまま
    // 通ってしまう（`?signature=ごみ&signature=本物` が通る）。
    let [given] = given else {
        return false;
    };
    // 長さは 16 進に解く**前**に見る。32 バイトの 16 進は 64 文字で固定なので、
    // 1 MB の `signature` を送られても確保しない。
    if given.len() != SIGNATURE_CHARS {
        return false;
    }

    // 期限が入っていれば、まず時間を見る。
    if let Some((_, expires)) = pairs.iter().find(|(k, _)| k == EXPIRES) {
        match expires.parse::<u64>() {
            Ok(expires) if expires > now() => {}
            _ => return false,
        }
    }

    let absolute = url(path);
    let query = canonical_query(pairs);
    let expected = crypto::hmac_sha256(key, payload(&absolute, &query).as_bytes());
    let Some(given) = crypto::from_hex(given) else {
        return false;
    };
    crypto::constant_time_eq(&expected, &given)
}

/// 署名付き URL にできるパスか確かめる。
///
/// 確かめる側は、符号化されたままの `req.path()` を使います。組み立てる側がここで
/// path を符号化すると、すでに符号化された path（`route_with` の結果）を二重に
/// 符号化してしまい、どちらが生の値かを見分けられません。
/// そこで黙って直さず、**署名が必ず合わなくなる文字**が入っていたらエラーにします。
/// 気づかないまま「いつも 403 になる」より、作ったところで止まるほうが分かります。
///
/// 断る文字を数えるのではなく、**通す文字を数えます**（`percent::path_literal`）。
/// 数え落とすと「作れるのに必ず 403」になるので、分からない文字は断る側に寄せます。
fn check_signable_path(path: &str) -> Result<()> {
    let bad = path
        .chars()
        .find(|c| !c.is_ascii() || !super::percent::path_literal(*c as u8))
        .map(escape);
    match bad {
        Some(bad) => Err(Error::msg(format!(
            "署名付き URL のパスに `{bad}` は使えません。\
             パーセントエンコードしてから渡してください（確かめる側は符号化されたパスを見ます）"
        ))),
        None => Ok(()),
    }
}

/// エラー文に出すための見た目。空白と制御文字はそのまま出さず `U+xxxx` にします。
fn escape(c: char) -> String {
    if c.is_control() || c == ' ' {
        format!("U+{:04X}", c as u32)
    } else {
        c.to_string()
    }
}

/// 署名の対象にする文字列。
fn payload(absolute: &str, query: &str) -> String {
    format!("{absolute}?{query}")
}

/// 並び順で署名が変わらないように、名前の順にそろえてから文字列にする。
fn canonical_query(pairs: &[(String, String)]) -> String {
    let mut sorted: Vec<&(String, String)> = pairs.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    sorted
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// クエリに入れられない文字を `%xx` にする。
///
/// 規則は `http/percent.rs` に置いてあり、ルートの組み立て（`routing.rs`）と
/// **同じ関数**を使います。片方だけ直すと署名が合わなくなるためです。
fn encode(input: &str) -> String {
    super::percent::encode(input, super::percent::unreserved)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 並び順が変わっても同じ署名になる() {
        let a = canonical_query(&[
            ("b".to_string(), "2".to_string()),
            ("a".to_string(), "1".to_string()),
        ]);
        let b = canonical_query(&[
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
        ]);
        assert_eq!(a, b);
        assert_eq!(a, "a=1&b=2");
    }

    #[test]
    fn クエリの記号を逃がす() {
        let q = canonical_query(&[("k".to_string(), "a b&c=d".to_string())]);
        assert_eq!(q, "k=a%20b%26c%3Dd");
    }

    #[test]
    fn 日本語も逃がす() {
        assert_eq!(encode("あ"), "%E3%81%82");
    }

    #[test]
    fn 空のクエリでも形が崩れない() {
        assert_eq!(canonical_query(&[]), "");
    }

    /// 試験用の鍵。設定を入れずに済むように、組み立てと照合の両方へ手で渡します。
    const KEY: &[u8] = b"test-key-for-signed-url";

    /// 署名付き URL を作り、照合する側と同じ形（path とクエリ）に分けて確かめる。
    fn roundtrip(path: &str) -> bool {
        let link = build_signed_with(KEY, path, &[("user", "12")], None).unwrap();
        // ブラウザが送る形にする。`?` の前が path、後ろがクエリ。
        let (absolute, query) = link.split_once('?').unwrap();
        let base = crate::config_registry::app_config()
            .url
            .trim_end_matches('/');
        let request_path = absolute.strip_prefix(base).unwrap();
        // 空の path でも、ブラウザは `/` を送る。
        let request_path = if request_path.is_empty() {
            "/"
        } else {
            request_path
        };

        let mut pairs = Vec::new();
        let mut given = Vec::new();
        for pair in query.split('&') {
            let (k, v) = pair.split_once('=').unwrap();
            if k == SIGNATURE {
                given.push(v.to_string());
            } else {
                pairs.push((k.to_string(), v.to_string()));
            }
        }
        verify_signature(KEY, request_path, &pairs, &given)
    }

    #[test]
    fn 空のパスでも署名が通る() {
        // `signed_url("")` の署名対象は、`/` を付けた形にそろえる。
        assert!(roundtrip(""), "空のパスで必ず 403 になってはいけない");
        assert!(roundtrip("/"));
        assert!(roundtrip("/unsubscribe"));
    }

    #[test]
    fn 署名が2つあると偽になる() {
        let sig = "a".repeat(SIGNATURE_CHARS);
        let given = vec![sig.clone(), sig];
        assert!(!verify_signature(KEY, "/x", &[], &given));
    }

    #[test]
    fn 長すぎる署名は解かずに偽にする() {
        // 1 MB の 16 進を送られても、確保する前に長さで断る。
        let given = vec!["ab".repeat(512 * 1024)];
        assert!(!verify_signature(KEY, "/x", &[], &given));
        // 短すぎるものも同じ。
        assert!(!verify_signature(KEY, "/x", &[], &["ab".to_string()]));
    }

    #[test]
    fn 署名できないパスは作る時点で断る() {
        // 符号化された形ならそのまま通る（確かめる側と同じ文字列になる）。
        assert!(check_signable_path("/unsubscribe").is_ok());
        assert!(check_signable_path("/hello/%E3%81%82").is_ok());
        assert!(check_signable_path("/a+b").is_ok());

        // 確かめる側の `req.path()` と必ず食い違うものは断る。
        for bad in [
            "/hello/あ",
            "/search?q=1",
            "/page#top",
            "/a b",
            "/a<b",
            "/a>b",
            "/a|b",
            "/a\\b",
            "/a^b",
            "/a`b",
            "/a{b}",
            "/a\"b",
            "/a\tb",
            "/a\nb",
            "/a\u{7f}b",
        ] {
            let error = check_signable_path(bad).unwrap_err();
            assert!(
                error.to_string().contains("使えません"),
                "{bad} は断るはず: {error}"
            );
        }
    }
}
