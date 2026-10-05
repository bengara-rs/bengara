//! リクエスト。

use std::net::{IpAddr, SocketAddr};
use std::sync::OnceLock;

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
    /// ルートに当たったか。
    ///
    /// 共通のミドルウェアは 404 や 405 にも掛かるので、
    /// 「守る対象のルートがあるのか」を見分けたいミドルウェア（CSRF の確認など）が使います。
    matched: bool,
    session: Option<crate::session::Session>,
    /// つないできた相手のアドレス。前段のプロキシがいればそのアドレスです。
    remote_addr: Option<SocketAddr>,
    /// 解析した結果の置き場所。1リクエストに1回だけ解析します。
    ///
    /// `OnceLock` なので、入れ替えはできても書き換えはできません。
    /// `Clone` は中身ごと複製します（解析済みなら、複製も解析済みのままです）。
    form_cache: OnceLock<Vec<(String, String)>>,
    input_cache: OnceLock<Vec<(String, String)>>,
    cookie_cache: OnceLock<Vec<(String, String)>>,
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
            matched: false,
            session: None,
            remote_addr: None,
            form_cache: OnceLock::new(),
            input_cache: OnceLock::new(),
            cookie_cache: OnceLock::new(),
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

    /// つないできた相手のアドレスを載せる。`server.rs` が入れます。
    pub(crate) fn with_remote_addr(mut self, addr: SocketAddr) -> Self {
        self.remote_addr = Some(addr);
        self
    }

    pub(crate) fn set_params(&mut self, params: Vec<(String, String)>) {
        self.params = params;
    }

    pub(crate) fn set_route_name(&mut self, name: Option<String>) {
        self.route_name = name;
    }

    /// ルートに当たったことを覚える。`Application` が照合の直後に呼びます。
    pub(crate) fn set_matched(&mut self, matched: bool) {
        self.matched = matched;
    }

    /// ルートに当たったか。
    ///
    /// 共通のミドルウェアは 404 と 405 にも掛かります。
    /// 「ルートがあるときだけ確かめたい」ミドルウェアは、これを見てください。
    pub fn route_matched(&self) -> bool {
        self.matched
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

    /// ログインの状態を扱う入口。
    ///
    /// ```ignore
    /// let user = req.auth().user_or_fail::<User>().await?;
    /// ```
    pub fn auth(&self) -> crate::auth::Auth<'_> {
        crate::auth::Auth::new(self.session.as_ref())
    }

    /// パス引数の値でモデルを1件読む。見つからなければ 404。
    ///
    /// ```ignore
    /// // Route::get("/posts/{post}", ...)
    /// let post = req.model::<Post>("post").await?;
    /// ```
    ///
    /// Laravel のルートモデルバインディング（引数に型を書くと勝手に読む仕組み）は
    /// ありません。**この1行で同じことをします**（決定記録 #051）。
    pub async fn model<T: crate::database::Model>(&self, param: &str) -> Result<T> {
        let value = self.param(param).ok_or_else(|| {
            Error::msg(format!(
                "パス引数 `{{{param}}}` がありません。ルートの書き方を確かめてください"
            ))
        })?;
        T::query()
            .where_(T::PRIMARY_KEY, value)
            .first()
            .await?
            .ok_or_else(|| Error::http(404, format!("{} が見つかりません", T::TABLE)))
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

    /// ページ番号（`?page=2`）。無い・読めない・0 のときは 1 になります。
    ///
    /// ページ分けに渡します。Laravel は暗黙にこの値を見ますが、bengara では
    /// `paginate(15, req.page())` のように自分で渡します（決定記録 #039）。
    pub fn page(&self) -> u64 {
        self.query("page")
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .unwrap_or(1)
            .max(1)
    }

    /// つないできた相手の IP アドレス。Laravel の `$request->ip()` に当たります。
    ///
    /// **返すのは実際につないできた相手だけです。** `X-Forwarded-For` は見ません。
    /// 前段にプロキシがいるときは、そのプロキシのアドレスになります。
    /// 転送元を知りたいときは、`TRUSTED_PROXIES` を見る `Throttle` と同じ考え方で、
    /// 自分で `header("x-forwarded-for")` を確かめてください。
    ///
    /// `#[bengara::test]` から送ったときは `127.0.0.1` です（ソケットは開きません）。
    /// ソケットも使わず、アドレスも入れずに作ったときだけ `None` になります。
    pub fn ip(&self) -> Option<IpAddr> {
        self.remote_addr.map(|addr| addr.ip())
    }

    /// ヘッダーの値（名前の大文字小文字は区別しません）。
    ///
    /// 同じ名前が何本も来たときは、最初の1本だけを返します。
    /// 全部を見る必要がある `Cookie` は `cookies()` を使ってください。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
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
        self.cookie_pairs()
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }

    /// Cookie の一覧。
    ///
    /// `Cookie` ヘッダーは HTTP/2 で何本かに分かれて来ることがあるので、全部を見ます。
    pub fn cookies(&self) -> Vec<(String, String)> {
        self.cookie_pairs().to_vec()
    }

    /// Cookie を解析した結果。1リクエストに1回だけ解析します。
    fn cookie_pairs(&self) -> &[(String, String)] {
        self.cookie_cache.get_or_init(|| {
            let mut out = Vec::new();
            for (name, value) in &self.headers {
                if name.eq_ignore_ascii_case("cookie") {
                    out.extend(super::cookie::parse_cookie_header(value));
                }
            }
            out
        })
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
        self.form_pairs()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    /// フォームの一覧。本文がフォームでなければ空。
    pub fn form_all(&self) -> Vec<(String, String)> {
        self.form_pairs().to_vec()
    }

    /// フォームを解析した結果。1リクエストに1回だけ解析します。
    fn form_pairs(&self) -> &[(String, String)] {
        self.form_cache.get_or_init(|| {
            if self.is_form() {
                parse_query(&self.body_text())
            } else {
                Vec::new()
            }
        })
    }

    /// クエリと本文をまとめた入力の一覧。
    ///
    /// 優先する順は **本文 → クエリ** です。同じ名前なら本文が勝ちます。
    /// 本文は、フォームなら組として、JSON なら最上位の値だけを読みます
    /// （入れ子は `req.json::<T>()` で読んでください）。
    pub fn input_all(&self) -> Vec<(String, String)> {
        self.input_pairs().to_vec()
    }

    /// クエリと本文をまとめた結果。1リクエストに1回だけ解析します。
    ///
    /// CSRF の検査が `_token` を見て、ハンドラが `validate()` を呼ぶので、
    /// 1本のリクエストで何度も聞かれます。毎回解析すると本文の分だけ無駄になります。
    pub(crate) fn input_pairs(&self) -> &[(String, String)] {
        self.input_cache.get_or_init(|| {
            let mut out = Vec::new();
            if self.is_form() {
                out.extend_from_slice(self.form_pairs());
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
        })
    }

    /// クエリと本文から、名前で1つ取り出す。
    ///
    /// 探す順は **本文 → クエリ** です。
    pub fn input(&self, key: &str) -> Option<String> {
        self.input_pairs()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
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
        let input = crate::validation::Input::new(self.input_pairs().to_vec());
        match crate::validation::validate(&input, rules) {
            Ok(ok) => Ok(ok),
            Err(e) => {
                // 覚え直させる入力は、ここだけで作る。`redact` を通さない道を作らない。
                if let Some(session) = self.try_session() {
                    session.flash_input(&redact(self.input_pairs()));
                }
                Err(e)
            }
        }
    }
}

/// セッションに残してはいけない名前。
///
/// 名前のどこかにこの語が入っていれば落とします（`new_password_confirmation` も落ちます）。
const SENSITIVE: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "pass",
    "secret",
    "token",
    "key",
    "api_key",
    "apikey",
    "private_key",
    "credential",
    "cvv",
    "card",
    "ssn",
    "otp",
    "pin",
];

/// 覚えておいてはいけない入力を落とす。
///
/// パスワードの類いは、入力し直しを省く値ではありません。セッションのファイルに
/// 平文で書かれてしまうので、ここで落とします。
fn redact(pairs: &[(String, String)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .filter(|(k, _)| !is_sensitive(k))
        .cloned()
        .collect()
}

/// 残してはいけない名前か。
fn is_sensitive(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SENSITIVE.iter().any(|word| lower.contains(word))
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
    fn ページ番号は1から始まる() {
        assert_eq!(Request::new("get", "/").page(), 1);
        assert_eq!(Request::new("get", "/").with_query("page=3").page(), 3);
        assert_eq!(Request::new("get", "/").with_query("page=0").page(), 1);
        assert_eq!(Request::new("get", "/").with_query("page=x").page(), 1);
    }

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

    #[test]
    fn cookieヘッダーが2本来ても全部読む() {
        let req = Request::new("get", "/").with_headers(vec![
            ("cookie".into(), "a=1".into()),
            ("Cookie".into(), "b=2".into()),
        ]);
        assert_eq!(
            req.cookies(),
            vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string()),
            ]
        );
        assert_eq!(req.cookie("b").as_deref(), Some("2"));
    }

    #[test]
    fn 接続元のアドレスを返す() {
        let req = Request::new("get", "/");
        assert_eq!(req.ip(), None, "ソケットを使わなければ分からない");

        let addr: SocketAddr = "203.0.113.9:4321".parse().unwrap();
        let req = Request::new("get", "/").with_remote_addr(addr);
        assert_eq!(
            req.ip().map(|ip| ip.to_string()).as_deref(),
            Some("203.0.113.9")
        );
    }

    #[test]
    fn 入力は1回だけ解析する() {
        let req = Request::new("post", "/")
            .with_headers(vec![(
                "content-type".into(),
                "application/x-www-form-urlencoded".into(),
            )])
            .with_body(b"a=1&b=2".to_vec())
            .with_query("c=3");
        // 何度呼んでも同じ結果。2回目は覚えたものを返す。
        let first = req.input_pairs().as_ptr();
        assert_eq!(req.input("a").as_deref(), Some("1"));
        assert_eq!(req.input("c").as_deref(), Some("3"));
        assert_eq!(req.input_all().len(), 3);
        assert_eq!(req.input_pairs().as_ptr(), first, "解析し直していない");
        assert_eq!(req.form("b").as_deref(), Some("2"));
        assert!(
            req.form_all().iter().all(|(k, _)| k != "c"),
            "クエリは入らない"
        );
    }

    #[test]
    fn 覚えてはいけない入力を落とす() {
        let banned = [
            "password",
            "password_confirmation",
            "passwd",
            "pwd",
            "pass",
            "secret",
            "token",
            "_token",
            "api_key",
            "apiKey",
            "private_key",
            "credential",
            "credentials",
            "cvv",
            "card",
            "card_number",
            "ssn",
            "otp",
            "pin",
            "key",
        ];
        for name in banned {
            assert!(is_sensitive(name), "{name} は残してはいけない");
        }
        for name in ["title", "email", "age", "body"] {
            assert!(!is_sensitive(name), "{name} は残してよい");
        }

        let pairs = vec![
            ("title".to_string(), "のこる".to_string()),
            ("password".to_string(), "きえる".to_string()),
            ("api_key".to_string(), "きえる".to_string()),
        ];
        let kept = redact(&pairs);
        assert_eq!(kept, vec![("title".to_string(), "のこる".to_string())]);
    }
}
