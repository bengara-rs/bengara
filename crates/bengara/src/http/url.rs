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
    let key = crate::config_registry::app_config().signing_key()?.to_vec();

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
        &key,
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
    let key = crate::config_registry::app_config().signing_key()?.to_vec();

    let mut pairs = Vec::new();
    let mut given = None;
    for (k, v) in req.query_all() {
        if k == SIGNATURE {
            given = Some(v);
        } else {
            pairs.push((k, v));
        }
    }
    let Some(given) = given else {
        return Ok(false);
    };

    // 期限が入っていれば、まず時間を見る。
    if let Some((_, expires)) = pairs.iter().find(|(k, _)| k == EXPIRES) {
        match expires.parse::<u64>() {
            Ok(expires) if expires > now() => {}
            _ => return Ok(false),
        }
    }

    let absolute = url(req.path());
    let query = canonical_query(&pairs);
    let expected = crypto::hmac_sha256(&key, payload(&absolute, &query).as_bytes());
    let Some(given) = crypto::from_hex(&given) else {
        return Ok(false);
    };
    Ok(crypto::constant_time_eq(&expected, &given))
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
fn encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        let c = *byte;
        if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~') {
            out.push(c as char);
        } else {
            out.push('%');
            out.push_str(&crypto::to_hex(&[c]).to_uppercase());
        }
    }
    out
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
}
