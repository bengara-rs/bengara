//! Cookie の読み書き。
//!
//! 受け取るときは `Cookie` ヘッダーを分解し、返すときは `Set-Cookie` を1本ずつ足します。

use std::borrow::Cow;
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
        let path = sanitize_attribute("Path", &self.path);
        let _ = write!(
            out,
            "; Path={}",
            super::percent::escape_unsafe_in_url(&path)
        );
        if let Some(domain) = self.domain.as_deref().and_then(ascii_domain) {
            let _ = write!(out, "; Domain={}", sanitize_attribute("Domain", domain));
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

/// `Path` / `Domain` から、ヘッダーを壊す文字を落とす。
///
/// 落とすのは**制御文字の全部**（`char::is_control`。水平タブも含みます）と、
/// 属性の区切りの `;` です。名前と値は `%xx` に逃がしますが、この2つは逃がすと
/// 意味が変わるので落とします。
///
/// 制御文字が残ると、応答を組み立てるところ（`response::header_value_ok`）が
/// **その `Set-Cookie` を 1 本まるごと落とします**（ほかのヘッダーと応答は残ります）。
/// 以前ここで落としていたのは `\r` `\n` `\0` だけだったので、`\x01` や `\x7f` が
/// 1 文字入るだけで、文字ではなく Cookie そのものが消えていました。
/// 落とす範囲を向こうの検査より広くそろえてあるので、属性が原因で Cookie が
/// 消えることはありません。
///
/// 黙って落とすので、落としたときだけ1行だけ残します。
fn sanitize_attribute<'a>(attribute: &str, value: &'a str) -> Cow<'a, str> {
    let bad = |c: char| c.is_control() || c == ';';
    if !value.contains(bad) {
        return Cow::Borrowed(value);
    }
    tracing::warn!("Cookie の {attribute} に使えない文字があったので落としました");
    Cow::Owned(value.replace(bad, ""))
}

/// `Domain` が ASCII だけか。ASCII 以外が入っていれば `None`（属性を付けない）。
///
/// 日本語のドメインは Punycode（`xn--` の形）に直して書く決まりです。直す処理は
/// 持っていないので、付けずに落とします。付けないと、Cookie は**いまのホストだけ**に
/// 送られます（範囲が狭まる側なので、他のサイトに漏れることはありません）。
/// 生のまま付けると、ブラウザは Cookie そのものを捨てます。
fn ascii_domain(domain: &str) -> Option<&str> {
    if domain.is_ascii() {
        return Some(domain);
    }
    tracing::warn!(
        "Cookie の Domain に ASCII 以外の文字があったので、Domain を付けません         （日本語のドメインは xn-- の形で書いてください）"
    );
    None
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
/// 規則は `http/percent.rs` に置いてあります。
fn encode(input: &str) -> String {
    super::percent::encode(input, super::percent::cookie_safe)
}

/// `%xx` を元に戻す。壊れた並びはそのまま残します。
fn decode(input: &str) -> String {
    super::percent::decode_strict(input)
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
    fn pathとdomainの制御文字は全部落とす() {
        // 以前落としていたのは `\r` `\n` `\0` だけ。ほかの制御文字が残ると、
        // 応答を組み立てるところが `Set-Cookie` を 1 本まるごと落としていた。
        let header = Cookie::new("a", "b")
            .with_path("/x\u{1}y\u{7f}z\tw\u{1b}\u{85}")
            .with_domain("exa\u{8}mple.com\u{0}")
            .to_header_value();
        assert!(header.contains("; Path=/xyzw;"), "{header:?}");
        assert!(header.contains("; Domain=example.com;"), "{header:?}");
        // 残った値に制御文字が 1 つも無い（＝ヘッダーとして組み立てられる）。
        assert!(!header.chars().any(char::is_control), "{header:?}");
        // 普通の値は借りたまま返す（確保しない）。
        assert!(matches!(
            sanitize_attribute("Path", "/admin"),
            Cow::Borrowed("/admin")
        ));
    }

    #[test]
    fn pathとdomainの危ない文字は落とす() {
        // 改行や `;` が残ると、属性を足されたり `Set-Cookie` が落ちたりする。
        let header = Cookie::new("a", "b")
            .with_path("/x\r\nSet-Cookie: evil=1")
            .with_domain("example.com\n; Secure")
            .to_header_value();
        assert!(!header.contains('\r'));
        assert!(!header.contains('\n'));
        assert!(header.contains("; Path=/xSet-Cookie:%20evil=1"));
        assert!(header.contains("; Domain=example.com Secure"));
    }

    #[test]
    fn pathとdomainのascii以外でcookieが消えない() {
        // 以前は生のまま入れていたので、本番（axum の `HeaderValue`）で
        // `Set-Cookie` が 1 本まるごと消えていた。テストでは通っていた。
        let header = Cookie::new("a", "b")
            .with_path("/記事 1/%E6%97%A5")
            .with_domain("例え.jp")
            .to_header_value();
        assert!(header.is_ascii(), "{header:?}");
        // パスは送られてくる形（%XX）にそろえる。符号化済みの所は二重にしない。
        assert!(
            header.contains("; Path=/%E8%A8%98%E4%BA%8B%201/%E6%97%A5;"),
            "{header:?}"
        );
        // 日本語のドメインは付けない（いまのホストだけに送られる）。
        assert!(!header.contains("Domain="), "{header:?}");
        // ASCII のドメインはそのまま。
        let header = Cookie::new("a", "b")
            .with_domain("xn--r8jz45g.jp")
            .to_header_value();
        assert!(header.contains("; Domain=xn--r8jz45g.jp;"), "{header:?}");
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
