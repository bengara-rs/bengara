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

    /// 本文を JSON として読む。
    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.body).map_err(Error::Json)
    }
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
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
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
