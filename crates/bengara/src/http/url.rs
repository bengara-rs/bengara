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
    let key = crate::config_registry::app_config().derived_key(SIGNING_LABEL)?;

    // クエリは `Request` が1回だけ解析したものを借りる（呼ぶたびに解析し直さない）。
    let mut pairs = Vec::new();
    let mut given = None;
    for (k, v) in req.query_pairs() {
        if k == SIGNATURE {
            given = Some(v.clone());
        } else {
            pairs.push((k.clone(), v.clone()));
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

/// 署名付き URL にできるパスか確かめる。
///
/// 確かめる側は、符号化されたままの `req.path()` を使います。組み立てる側がここで
/// path を符号化すると、すでに符号化された path（`route_with` の結果）を二重に
/// 符号化してしまい、どちらが生の値かを見分けられません。
/// そこで黙って直さず、**署名が必ず合わなくなる文字**が入っていたらエラーにします。
/// 気づかないまま「いつも 403 になる」より、作ったところで止まるほうが分かります。
fn check_signable_path(path: &str) -> Result<()> {
    let bad = path
        .chars()
        .find(|c| *c == '?' || *c == '#' || !c.is_ascii());
    match bad {
        Some(bad) => Err(Error::msg(format!(
            "署名付き URL のパスに `{bad}` は使えません。\
             パーセントエンコードしてから渡してください（確かめる側は符号化されたパスを見ます）"
        ))),
        None => Ok(()),
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

    #[test]
    fn 署名できないパスは作る時点で断る() {
        // 符号化された形ならそのまま通る（確かめる側と同じ文字列になる）。
        assert!(check_signable_path("/unsubscribe").is_ok());
        assert!(check_signable_path("/hello/%E3%81%82").is_ok());
        assert!(check_signable_path("/a+b").is_ok());

        // 確かめる側の `req.path()` と必ず食い違うものは断る。
        for bad in ["/hello/あ", "/search?q=1", "/page#top"] {
            let error = check_signable_path(bad).unwrap_err();
            assert!(
                error.to_string().contains("使えません"),
                "{bad} は断るはず: {error}"
            );
        }
    }
}
