//! リクエスト。

use serde::de::DeserializeOwned;

use crate::error::{Error, Result};

/// 受け取ったリクエスト。
///
/// 起動後は読むだけなので、ハンドラには所有した `Request` を渡します。
#[derive(Debug, Clone)]
pub struct Request {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    params: Vec<(String, String)>,
    body: Vec<u8>,
    route_name: Option<String>,
    session: Option<crate::session::Session>,
}

impl Request {
    /// 中身を指定して作る（テストと内部用）。
    pub(crate) fn new(method: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            method: method.into().to_ascii_uppercase(),
            path: path.into(),
            query: String::new(),
            headers: Vec::new(),
            params: Vec::new(),
            body: Vec::new(),
            route_name: None,
            session: None,
        }
    }

    pub(crate) fn with_query(mut self, query: impl Into<String>) -> Self {
        self.query = query.into();
        self
    }

    pub(crate) fn with_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.headers = headers;
        self
    }

    pub(crate) fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    pub(crate) fn set_params(&mut self, params: Vec<(String, String)>) {
        self.params = params;
    }

    pub(crate) fn set_route_name(&mut self, name: Option<String>) {
        self.route_name = name;
    }

    pub(crate) fn set_session(&mut self, session: crate::session::Session) {
        self.session = Some(session);
    }

    /// セッション。
    ///
    /// # パニック
    ///
    /// `StartSession` を登録していないとパニックします。
    /// 登録し忘れに実行時すぐ気づけるようにするためです。
    pub fn session(&self) -> &crate::session::Session {
        self.session.as_ref().expect(
            "セッションが用意されていません。bootstrap/app.rs の with_middleware で \n             StartSession を登録してください",
        )
    }

    /// セッションがあれば返す。無くてもよい場面で使います。
    pub fn try_session(&self) -> Option<&crate::session::Session> {
        self.session.as_ref()
    }

    /// CSRF のトークン。フォームの `_token` に入れて返してもらいます。
    ///
    /// # パニック
    ///
    /// セッションが無いとパニックします。
    pub fn csrf_token(&self) -> String {
        crate::session::csrf_token(self.session())
    }

    /// HTTP メソッド（大文字）。
    pub fn method(&self) -> &str {
        &self.method
    }

    /// パス（クエリを含まない）。
    pub fn path(&self) -> &str {
        &self.path
    }

    /// クエリ文字列（`?` を含まない）。
    pub fn query_string(&self) -> &str {
        &self.query
    }

    /// パスとクエリをつないだもの。
    pub fn full_path(&self) -> String {
        if self.query.is_empty() {
            self.path.clone()
        } else {
            format!("{}?{}", self.path, self.query)
        }
    }

    /// 当たったルートの名前。
    pub fn route_name(&self) -> Option<&str> {
        self.route_name.as_deref()
    }

    /// パス引数（`/posts/{post}` の `post`）。
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// パス引数を型に変換して取り出す。
    pub fn param_as<T: std::str::FromStr>(&self, name: &str) -> Result<T> {
        let raw = self
            .param(name)
            .ok_or_else(|| Error::http(404, format!("パス引数 {name} がありません")))?;
        raw.parse::<T>().map_err(|_| {
            Error::http(
                404,
                format!("パス引数 {name} の値 `{raw}` を変換できません"),
            )
        })
    }

    /// パス引数の一覧。
    pub fn params(&self) -> &[(String, String)] {
        &self.params
    }

    /// クエリの値（最初に見つかったもの）。`%xx` と `+` を元に戻します。
    pub fn query(&self, key: &str) -> Option<String> {
        for (k, v) in parse_query(&self.query) {
            if k == key {
                return Some(v);
            }
        }
        None
    }

    /// クエリの一覧。
    pub fn query_all(&self) -> Vec<(String, String)> {
        parse_query(&self.query)
    }

    /// ヘッダーの値（名前の大文字小文字は区別しません）。
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }

    /// ヘッダーの一覧。
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// 本文。
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// 本文を文字列として見る。
    pub fn body_text(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }

    /// Cookie の値（名前そのまま）。
    ///
    /// 署名つきのセッション Cookie を読むときは `req.session()` を使ってください。
    pub fn cookie(&self, name: &str) -> Option<String> {
        self.cookies()
            .into_iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v)
    }

    /// Cookie の一覧。
    pub fn cookies(&self) -> Vec<(String, String)> {
        match self.header("cookie") {
            Some(raw) => super::cookie::parse_cookie_header(raw),
            None => Vec::new(),
        }
    }

    /// 本文を JSON として読む。
    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.body).map_err(Error::Json)
    }

    /// 本文がフォーム（`application/x-www-form-urlencoded`）か。
    fn is_form(&self) -> bool {
        self.header("content-type")
            .map(|ct| {
                ct.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .eq_ignore_ascii_case("application/x-www-form-urlencoded")
            })
            .unwrap_or(false)
    }

    /// 本文が JSON か。
    fn is_json_body(&self) -> bool {
        self.header("content-type")
            .map(|ct| {
                let kind = ct.split(';').next().unwrap_or("").trim();
                kind.eq_ignore_ascii_case("application/json") || kind.ends_with("+json")
            })
            .unwrap_or(false)
    }

    /// フォームの値（最初に見つかったもの）。
    pub fn form(&self, key: &str) -> Option<String> {
        self.form_all()
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    /// フォームの一覧。本文がフォームでなければ空。
    pub fn form_all(&self) -> Vec<(String, String)> {
        if self.is_form() {
            parse_query(&self.body_text())
        } else {
            Vec::new()
        }
    }

    /// クエリと本文をまとめた入力の一覧。
    ///
    /// 優先する順は **本文 → クエリ** です。同じ名前なら本文が勝ちます。
    /// 本文は、フォームなら組として、JSON なら最上位の値だけを読みます
    /// （入れ子は `req.json::<T>()` で読んでください）。
    pub fn input_all(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if self.is_form() {
            out.extend(self.form_all());
        } else if self.is_json_body() {
            if let Ok(serde_json::Value::Object(map)) =
                serde_json::from_slice::<serde_json::Value>(&self.body)
            {
                for (k, v) in map {
                    // 文字列はそのまま、数値や真偽値は見たままの文字にする。
                    let text = match v {
                        serde_json::Value::String(s) => s,
                        serde_json::Value::Null => String::new(),
                        other => other.to_string(),
                    };
                    out.push((k, text));
                }
            }
        }
        out.extend(self.query_all());
        out
    }

    /// クエリと本文から、名前で1つ取り出す。
    ///
    /// 探す順は **本文 → クエリ** です。
    pub fn input(&self, key: &str) -> Option<String> {
        self.input_all()
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    /// 入力を検査する。
    ///
    /// ```ignore
    /// let input = req.validate(&[
    ///     ("title", "required|max:255"),
    ///     ("email", "required|email"),
    /// ])?;
    /// ```
    ///
    /// 落ちると `Error::Validation` になり、422 と理由の一覧が返ります。
    /// 戻り値には**検査した項目だけ**が入ります。
    /// 落ちたとき、セッションがあれば入力を覚えておきます（`session.old("title")` で読めます）。
    pub fn validate(&self, rules: &[(&str, &str)]) -> Result<crate::validation::Validated> {
        let pairs = self.input_all();
        let input = crate::validation::Input::new(pairs.clone());
        match crate::validation::validate(&input, rules) {
            Ok(ok) => Ok(ok),
            Err(e) => {
                if let Some(session) = self.try_session() {
                    session.flash_input(&redact(pairs));
                }
                Err(e)
            }
        }
    }
}

/// 覚えておいてはいけない入力を落とす。
///
/// パスワードの類いは、入力し直しを省く値ではありません。セッションに残しません。
fn redact(pairs: Vec<(String, String)>) -> Vec<(String, String)> {
    pairs
        .into_iter()
        .filter(|(k, _)| {
            let k = k.to_ascii_lowercase();
            !(k.contains("password") || k.contains("secret") || k.contains("token"))
        })
        .collect()
}

/// `a=1&b=2` を組に分ける。値は `%xx` と `+` を元に戻します。
pub(crate) fn parse_query(query: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        out.push((percent_decode(k), percent_decode(v)));
    }
    out
}

/// `%xx` と `+` を元に戻す。壊れた並びはそのまま残します。
pub(crate) fn percent_decode(input: &str) -> String {
    decode_percent(input, true)
}

/// `%xx` だけを元に戻す。`+` は空白にしません。
///
/// Cookie の値はフォームの書き方（`+` が空白）ではないので、こちらを使います。
pub(crate) fn percent_decode_strict(input: &str) -> String {
    decode_percent(input, false)
}

fn decode_percent(input: &str, plus_is_space: bool) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' if plus_is_space => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h << 4 | l);
                    i += 3;
                }
                _ => {
                    out.push(bytes[i]);
                    i += 1;
                }
            },
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn クエリを分解できる() {
        let req = Request::new("get", "/search").with_query("q=hello+world&page=2");
        assert_eq!(req.query("q").as_deref(), Some("hello world"));
        assert_eq!(req.query("page").as_deref(), Some("2"));
        assert_eq!(req.query("none"), None);
    }

    #[test]
    fn パーセントエンコードを戻せる() {
        assert_eq!(percent_decode("%E3%81%82"), "あ");
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%4"), "%4");
    }

    #[test]
    fn メソッドは大文字になる() {
        assert_eq!(Request::new("post", "/").method(), "POST");
    }

    #[test]
    fn ヘッダーは大文字小文字を区別しない() {
        let req = Request::new("get", "/")
            .with_headers(vec![("content-type".into(), "application/json".into())]);
        assert_eq!(req.header("Content-Type"), Some("application/json"));
    }

    #[test]
    fn パス引数を型変換できる() {
        let mut req = Request::new("get", "/posts/12");
        req.set_params(vec![("post".into(), "12".into())]);
        assert_eq!(req.param_as::<i64>("post").unwrap(), 12);
        assert!(req.param_as::<i64>("missing").is_err());
    }

    #[test]
    fn 本文をjsonで読める() {
        let req = Request::new("post", "/").with_body(b"{\"a\":1}".to_vec());
        let v: serde_json::Value = req.json().unwrap();
        assert_eq!(v["a"], 1);
    }
}
