//! Cookie の読み書き。
//!
//! 受け取るときは `Cookie` ヘッダーを分解し、返すときは `Set-Cookie` を1本ずつ足します。

use std::fmt::Write as _;

/// 返す Cookie 1本。
///
/// ```ignore
/// let cookie = Cookie::new("theme", "dark").with_path("/").with_max_age(3600);
/// Ok(response.with_cookie(&cookie))
/// ```
#[derive(Debug, Clone)]
pub struct Cookie {
    name: String,
    value: String,
    path: String,
    domain: Option<String>,
    max_age: Option<i64>,
    secure: bool,
    http_only: bool,
    same_site: SameSite,
}

/// 他のサイトから送られてきたときに Cookie を付けるか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameSite {
    /// 他のサイトからの移動では送らない。いちばん厳しい。
    Strict,
    /// 普通のリンクでの移動なら送る。既定。
    Lax,
    /// いつでも送る。`Secure` が必須になります。
    None,
}

impl SameSite {
    fn as_str(self) -> &'static str {
        match self {
            SameSite::Strict => "Strict",
            SameSite::Lax => "Lax",
            SameSite::None => "None",
        }
    }
}

impl Cookie {
    /// 名前と値から作る。既定は `Path=/`、`HttpOnly`、`SameSite=Lax`。
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            path: "/".to_string(),
            domain: None,
            max_age: None,
            secure: false,
            http_only: true,
            same_site: SameSite::Lax,
        }
    }

    /// 消すための Cookie を作る（空の値と `Max-Age=0`）。
    pub fn removal(name: impl Into<String>) -> Self {
        Self::new(name, "").with_max_age(0)
    }

    /// 送る範囲のパス。既定は `/`。
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    /// 送る範囲のドメイン。
    pub fn with_domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }

    /// 生きている秒数。`0` ならすぐ消えます。
    pub fn with_max_age(mut self, seconds: i64) -> Self {
        self.max_age = Some(seconds);
        self
    }

    /// HTTPS のときだけ送る。
    pub fn with_secure(mut self, secure: bool) -> Self {
        self.secure = secure;
        self
    }

    /// JavaScript から読めなくする。既定は `true`。
    pub fn with_http_only(mut self, http_only: bool) -> Self {
        self.http_only = http_only;
        self
    }

    /// 他のサイトから来たときの扱い。既定は `Lax`。
    pub fn with_same_site(mut self, same_site: SameSite) -> Self {
        self.same_site = same_site;
        self
    }

    /// 名前。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 値。
    pub fn value(&self) -> &str {
        &self.value
    }

    /// `Set-Cookie` ヘッダーの中身にする。
    pub fn to_header_value(&self) -> String {
        let mut out = format!("{}={}", encode(&self.name), encode(&self.value));
        let _ = write!(out, "; Path={}", self.path);
        if let Some(domain) = &self.domain {
            let _ = write!(out, "; Domain={domain}");
        }
        if let Some(max_age) = self.max_age {
            let _ = write!(out, "; Max-Age={max_age}");
            // Max-Age を見ない古いブラウザのために Expires も添える。
            if max_age <= 0 {
                out.push_str("; Expires=Thu, 01 Jan 1970 00:00:00 GMT");
            }
        }
        // SameSite=None は Secure が無いとブラウザに捨てられる。
        if self.secure || self.same_site == SameSite::None {
            out.push_str("; Secure");
        }
        if self.http_only {
            out.push_str("; HttpOnly");
        }
        let _ = write!(out, "; SameSite={}", self.same_site.as_str());
        out
    }
}

/// `Cookie` ヘッダーを名前と値に分ける。
pub(crate) fn parse_cookie_header(raw: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in raw.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (name, value) = match part.split_once('=') {
            Some((n, v)) => (n.trim(), v.trim()),
            // 値のない Cookie は捨てる。名前だけでは使い道がない。
            None => continue,
        };
        if name.is_empty() {
            continue;
        }
        out.push((decode(name), decode(value)));
    }
    out
}

/// Cookie に入れられない文字を `%xx` にする。
///
/// 区切りに使う文字（`;` `,` 空白 `=`）と、制御文字・非 ASCII を逃がします。
fn encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        let c = *byte;
        let safe = c.is_ascii_alphanumeric()
            || matches!(
                c,
                b'-' | b'_'
                    | b'.'
                    | b'~'
                    | b'!'
                    | b'#'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b'/'
                    | b':'
                    | b'<'
                    | b'>'
                    | b'?'
                    | b'@'
                    | b'['
                    | b']'
                    | b'^'
                    | b'`'
                    | b'{'
                    | b'|'
                    | b'}'
            );
        if safe {
            out.push(c as char);
        } else {
            out.push('%');
            out.push_str(&crate::support::crypto::to_hex(&[c]).to_uppercase());
        }
    }
    out
}

/// `%xx` を元に戻す。壊れた並びはそのまま残します。
fn decode(input: &str) -> String {
    super::request::percent_decode_strict(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 既定の属性が付く() {
        let header = Cookie::new("name", "value").to_header_value();
        assert!(header.starts_with("name=value"));
        assert!(header.contains("; Path=/"));
        assert!(header.contains("; HttpOnly"));
        assert!(header.contains("; SameSite=Lax"));
        assert!(!header.contains("Secure"));
    }

    #[test]
    fn 属性を変えられる() {
        let header = Cookie::new("a", "b")
            .with_path("/admin")
            .with_domain("example.com")
            .with_max_age(60)
            .with_secure(true)
            .with_http_only(false)
            .with_same_site(SameSite::Strict)
            .to_header_value();
        assert!(header.contains("; Path=/admin"));
        assert!(header.contains("; Domain=example.com"));
        assert!(header.contains("; Max-Age=60"));
        assert!(header.contains("; Secure"));
        assert!(!header.contains("HttpOnly"));
        assert!(header.contains("; SameSite=Strict"));
    }

    #[test]
    fn same_site_noneにはsecureが必ず付く() {
        let header = Cookie::new("a", "b")
            .with_same_site(SameSite::None)
            .to_header_value();
        assert!(header.contains("; Secure"), "None には Secure が要る");
    }

    #[test]
    fn 消すcookieはmax_ageが0になる() {
        let header = Cookie::removal("session").to_header_value();
        assert!(header.starts_with("session="));
        assert!(header.contains("; Max-Age=0"));
        assert!(header.contains("Expires=Thu, 01 Jan 1970"));
    }

    #[test]
    fn 区切りに使う文字は逃がす() {
        let header = Cookie::new("a", "x; y=z").to_header_value();
        // 値の中の ; と空白と = が生で出ないこと。
        let value = header.split_once("; Path").unwrap().0;
        assert_eq!(value, "a=x%3B%20y%3Dz");
    }

    #[test]
    fn 日本語も往復する() {
        let header = Cookie::new("name", "あ").to_header_value();
        let value = header.split_once("; Path").unwrap().0;
        let parsed = parse_cookie_header(value);
        assert_eq!(parsed, vec![("name".to_string(), "あ".to_string())]);
    }

    #[test]
    fn ヘッダーを分解できる() {
        let parsed = parse_cookie_header(" a=1; b=2 ;c=hello%20world ");
        assert_eq!(
            parsed,
            vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string()),
                ("c".to_string(), "hello world".to_string()),
            ]
        );
    }

    #[test]
    fn 壊れたヘッダーでも落ちない() {
        assert!(parse_cookie_header("").is_empty());
        assert!(parse_cookie_header(";;;").is_empty());
        assert!(parse_cookie_header("novalue").is_empty());
        assert!(parse_cookie_header("=novalue").is_empty());
        // 値が空なのは正しい形。
        assert_eq!(
            parse_cookie_header("a="),
            vec![("a".to_string(), String::new())]
        );
    }
}
