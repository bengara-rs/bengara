//! リクエスト。

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, OnceLock};

use serde::de::DeserializeOwned;

use crate::error::{Error, Result};

/// 受け取ったリクエスト。
///
/// 起動後は読むだけなので、ハンドラには所有した `Request` を渡します。
///
/// `Debug` は手で書いてあります。秘密を出さないためです（下の `impl Debug`）。
#[derive(Clone)]
pub struct Request {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    params: Vec<(String, String)>,
    body: Vec<u8>,
    /// 当たったルートの名前。
    ///
    /// `Arc<str>` なのは、1リクエストごとに `String` を作らないためです
    /// （ルートの一覧は起動時に固定され、名前はそこから借りるだけです）。
    route_name: Option<Arc<str>>,
    /// 当たったルートの登録パス（`/posts/{id}` の形）。当たらなければ `None`。
    ///
    /// 回数の制限（`Throttle`）が、名前の無いルートの鍵にします。
    /// 届いたパス（`path`）は相手が形を変えられます（末尾の `/`、パス引数の値）。
    /// 登録パスは起動時に固定したものなので、相手には変えられません。
    route_path: Option<Arc<str>>,
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
    query_cache: OnceLock<Vec<(String, String)>>,
    /// WebSocket への切り替えの口（`server.rs` が載せる）。取り出せるのは 1 回だけ。
    ///
    /// ふつうのリクエストでは空で、確保もしません。
    upgrade: crate::websocket::Slot<crate::websocket::Handoff>,
    /// まだ読んでいない本文（`server.rs` とテストのクライアントが載せる）。
    ///
    /// 照合の後に [`receive_body`](Self::receive_body) が取り出し、全部読むか、
    /// `stream` へ移します。
    pending: crate::websocket::Slot<crate::http::body::PendingBody>,
    /// 少しずつ読む本文（`.stream_body()` のルートだけ）。取り出せるのは 1 回だけ。
    stream: crate::websocket::Slot<crate::http::body::BodyStream>,
    /// `.stream_body()` のルートに当たったか。`body_stream()` のエラー文を分けるために持ちます。
    streaming: bool,
}

/// ログに出してよい形だけを出す。
///
/// 以前は `derive(Debug)` だったので、`{req:?}` で Cookie・`Authorization`・
/// 本文（パスワードを含むフォーム）がそのままログに出ました。
///
/// | 出すもの | 形 |
/// |---|---|
/// | メソッド | そのまま |
/// | パスとクエリ | アクセスログと同じく伏せる（`server::log_target`） |
/// | ヘッダー | 名前は全部。秘密になる値は `***`（[`RedactedHeaders`]） |
/// | 本文 | 長さだけ |
///
/// セッションの中身・解析済みの入力・パス引数の値は出しません。
impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let target = crate::server::log_target(self);
        let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("path", &path)
            .field("query", &query)
            .field("headers", &RedactedHeaders(&self.headers))
            .field("body_len", &self.body.len())
            .field("route_name", &self.route_name.as_deref())
            .finish_non_exhaustive()
    }
}

/// ヘッダーの一覧を、秘密になる値を伏せて出す（`Request` と `Response` の `Debug`）。
///
/// 伏せるのは次の値です。名前は全部そのまま出します。
///
/// - `cookie`・`set-cookie`・`authorization`・`proxy-authorization`
/// - 名前が伏せる語に当たるもの（`x-csrf-token`・`x-api-key` など。[`is_sensitive`]）
pub(crate) struct RedactedHeaders<'a>(pub(crate) &'a [(String, String)]);

impl std::fmt::Debug for RedactedHeaders<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|(name, value)| {
                let hidden = [
                    "cookie",
                    "set-cookie",
                    "authorization",
                    "proxy-authorization",
                ]
                .iter()
                .any(|secret| name.eq_ignore_ascii_case(secret))
                    || is_sensitive(name);
                (name.as_str(), if hidden { "***" } else { value.as_str() })
            }))
            .finish()
    }
}

impl Request {
    /// 中身を指定して作る（テストと内部用）。
    ///
    /// メソッドは**受け取ったまま**持ちます。HTTP のメソッドは大文字小文字を
    /// 区別するので、ここで大文字にそろえてはいけません。そろえていたころは
    /// `get /x HTTP/1.1` が `GET` として照合され、ハンドラまで通っていました
    /// （本来は 405）。いまは `get` がどのメソッドにも当たらず 405 になります。
    pub(crate) fn new(method: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            path: path.into(),
            query: String::new(),
            headers: Vec::new(),
            params: Vec::new(),
            body: Vec::new(),
            route_name: None,
            route_path: None,
            matched: false,
            session: None,
            remote_addr: None,
            form_cache: OnceLock::new(),
            input_cache: OnceLock::new(),
            cookie_cache: OnceLock::new(),
            query_cache: OnceLock::new(),
            upgrade: crate::websocket::Slot::default(),
            pending: crate::websocket::Slot::default(),
            stream: crate::websocket::Slot::default(),
            streaming: false,
        }
    }

    /// まだ読んでいない本文を載せる。`server.rs` とテストのクライアントが入れます。
    pub(crate) fn with_pending_body(mut self, body: crate::http::body::PendingBody) -> Self {
        self.pending = crate::websocket::Slot::new(body);
        self
    }

    /// ルートの決まりに従って本文を受け取る。`Application` が照合の直後に 1 回呼びます。
    ///
    /// | ルート | すること |
    /// |---|---|
    /// | ふつう | 全部読んで `body` に入れる（上限・30 秒） |
    /// | `.stream_body()` | 読まずに `body_stream()` で取り出せるようにする |
    ///
    /// 長さの申告（`Content-Length`）が上限を超えていれば、どちらも読まずに 413 です。
    /// エラー（413・400・408）はミドルウェアを通さずに返します。
    pub(crate) async fn receive_body(
        &mut self,
        policy: crate::http::body::BodyPolicy,
    ) -> Result<()> {
        use crate::http::body::{read_all, too_large, BodyStream, PendingBody};

        let Some(pending) = self.pending.take() else {
            // 中身を直に載せた `Request`（枠組みの単体テスト）。読むものは無い。
            if policy.stream {
                let bytes = std::mem::take(&mut self.body);
                self.stream = crate::websocket::Slot::new(BodyStream::new(
                    PendingBody::memory(bytes),
                    policy.limit,
                ));
                self.streaming = true;
            }
            return Ok(());
        };

        let declared = self
            .header("content-length")
            .and_then(|value| value.trim().parse::<u64>().ok());
        if declared.is_some_and(|length| length > policy.limit) {
            return Err(too_large());
        }

        if policy.stream {
            self.stream = crate::websocket::Slot::new(BodyStream::new(pending, policy.limit));
            self.streaming = true;
            return Ok(());
        }
        self.body = read_all(pending, policy.limit, declared).await?;
        Ok(())
    }

    /// 本文を少しずつ読む口を取り出す。`.stream_body()` を付けたルートだけで使えます。
    ///
    /// ```ignore
    /// let mut body = req.body_stream()?;
    /// while let Some(chunk) = body.chunk().await? {
    ///     // chunk: Vec<u8>
    /// }
    /// ```
    ///
    /// | 起きたこと | 結果 |
    /// |---|---|
    /// | `.stream_body()` のルート（1 回目） | `Ok(BodyStream)` |
    /// | 2 回目（`Request` を複製したものも含む） | `Err`（500） |
    /// | `.stream_body()` が無いルート | `Err`（500）。本文は `body()` で読めます |
    pub fn body_stream(&self) -> Result<crate::http::body::BodyStream> {
        if !self.streaming {
            return Err(Error::msg(
                "このルートには `.stream_body()` が付いていません。本文は `req.body()` で読めます",
            ));
        }
        self.stream.take().ok_or_else(|| {
            Error::msg("本文はもう取り出してあります（`body_stream()` は 1 回だけ呼べます）")
        })
    }

    /// WebSocket への切り替えの口を載せる。`server.rs` が入れます。
    #[cfg_attr(not(feature = "websocket"), allow(dead_code))]
    pub(crate) fn with_upgrade(mut self, handoff: crate::websocket::Handoff) -> Self {
        self.upgrade = crate::websocket::Slot::new(handoff);
        self
    }

    /// WebSocket への切り替えの口。
    pub(crate) fn upgrade_slot(&self) -> &crate::websocket::Slot<crate::websocket::Handoff> {
        &self.upgrade
    }

    /// WebSocket への切り替えを始める。
    ///
    /// ```ignore
    /// req.websocket()?.on_upgrade(|mut socket| async move { /* ... */ })
    /// ```
    ///
    /// | 起きたこと | 結果 |
    /// |---|---|
    /// | 切り替えの要求でない（GET・`Upgrade`・`Connection`・鍵） | Err（400） |
    /// | `Sec-WebSocket-Version` が 13 でない | Err（426 ＋ `Sec-WebSocket-Version: 13`） |
    /// | サーバーが停止の途中・同時接続の上限 | Err（503） |
    /// | 機能フラグ `websocket` が無い | Err（「features に "websocket" を足してください」） |
    ///
    /// **`Origin` は確かめません。** 切り替えは GET なので CSRF の確認も掛かりません。
    /// 他のサイトからの接続を断るには、`req.header("origin")` を自分で確かめてください。
    pub fn websocket(&self) -> Result<crate::websocket::WebSocketUpgrade> {
        crate::websocket::WebSocketUpgrade::from_request(self)
    }

    /// 切り替えの要求か（`Upgrade: websocket` と `Connection: upgrade` があるか）。
    pub fn wants_websocket(&self) -> bool {
        crate::websocket::wants_websocket(self)
    }

    pub(crate) fn with_query(mut self, query: impl Into<String>) -> Self {
        self.query = query.into();
        self
    }

    pub(crate) fn with_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.headers = headers;
        self
    }

    /// 読み終えた本文を直に載せる（枠組みの単体テスト用）。
    ///
    /// サーバーとテストのクライアントは [`with_pending_body`](Self::with_pending_body) を使います。
    #[cfg(test)]
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

    pub(crate) fn set_route_name(&mut self, name: Option<Arc<str>>) {
        self.route_name = name;
    }

    /// 当たったルートの登録パスを載せる。`Application` が照合の直後に呼びます。
    pub(crate) fn set_route_path(&mut self, path: Option<Arc<str>>) {
        self.route_path = path;
    }

    /// 当たったルートの登録パス（`/posts/{id}` の形）。当たらなければ `None`。
    ///
    /// 公開していません。使うのは `Throttle` の鍵だけです。
    pub(crate) fn route_path(&self) -> Option<&str> {
        self.route_path.as_deref()
    }

    /// ルートに当たったことを覚える。`Application` が照合の直後に呼びます。
    pub(crate) fn mark_matched(&mut self) {
        self.matched = true;
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
            "セッションが用意されていません。bootstrap/app.rs の with_middleware で \
             StartSession を登録してください",
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
        // パス引数は文字列で届く。整数の主キーなら `find` が整数に直して束縛する
        // （PostgreSQL は `bigint = text` を断る。`Model::key_value` を参照）。
        //
        // **整数に読めない値（`/posts/abc`）は、問い合わせずに 404 です。**
        // `where_(主キー, ..).first()` と書いていたころは、当たらないと分かっている
        // `where 1 = 0` を 1 往復ぶん流していました。
        T::find_or_fail(value).await
    }

    /// CSRF のトークン。フォームの `_token` に入れて返してもらいます。
    ///
    /// # パニック
    ///
    /// セッションが無いとパニックします。
    pub fn csrf_token(&self) -> String {
        crate::session::csrf_token(self.session())
    }

    /// テンプレートを描いて、HTML のレスポンスで返す。機能フラグ `view` が要ります。
    ///
    /// ```ignore
    /// req.view("posts.create", json!({ "title": "新しい記事" }))
    /// ```
    ///
    /// [`bengara::view`](crate::view()) と同じです。加えて、テンプレートの中で
    /// `csrf_field()`・`csrf_token()`・`old(key)` が使えます。
    /// セッションが無い（`StartSession` を通っていない）ときは、`csrf_field()` と
    /// `csrf_token()` が描画のエラーになり、`old()` は既定値を返します。
    ///
    /// `context` に `csrf_token`・`csrf_field`・`old` の鍵があると 500 です。
    pub fn view(
        &self,
        name: &str,
        context: impl serde::Serialize,
    ) -> Result<crate::http::Response> {
        crate::view::request_view(self.try_session(), name, context)
    }

    /// HTTP メソッド。
    ///
    /// 届いた文字列のままです。**大文字小文字は直しません**（HTTP のメソッドは
    /// 大文字小文字を区別します）。正しいリクエストなら `GET` のように大文字です。
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
    ///
    /// | 起きたこと | 結果 |
    /// |---|---|
    /// | 値が型に合わない（`/posts/abc` を `u32` で読む） | **404**。相手が送った URL の問題 |
    /// | ルートにその名前の引数が無い | **500**。ルートかハンドラの書き間違い |
    ///
    /// 名前が無いときも 404 にしていたころは、書き間違えたハンドラが
    /// 「ページがありません」を返すだけで、ログにも残らず気づけませんでした。
    /// `model()` と同じ扱いにそろえてあります。
    pub fn param_as<T: std::str::FromStr>(&self, name: &str) -> Result<T> {
        let raw = self.param(name).ok_or_else(|| {
            Error::msg(format!(
                "パス引数 `{{{name}}}` がありません。ルートの書き方を確かめてください"
            ))
        })?;
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
        self.query_pairs()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    /// クエリの一覧。
    pub fn query_all(&self) -> Vec<(String, String)> {
        self.query_pairs().to_vec()
    }

    /// クエリを解析した結果。1リクエストに1回だけ解析します。
    ///
    /// `page()`・`input_pairs()`・`has_valid_signature()` が同じ道を通るので、
    /// 呼ぶたびに解析すると組ごとの `String` が積み上がります。
    pub(crate) fn query_pairs(&self) -> &[(String, String)] {
        self.query_cache.get_or_init(|| parse_query(&self.query))
    }

    /// ページ番号（`?page=2`）。無い・読めない・0 のときは 1 になります。
    ///
    /// 上限は `u32::MAX`（約 43 億ページ）です。それより大きい値はこの上限に丸めます。
    /// `paginate` は `(page - 1) * per_page` を計算するので、`u64` の端の値を
    /// そのまま通すと桁あふれになります。
    ///
    /// ページ分けに渡します。Laravel は暗黙にこの値を見ますが、bengara では
    /// `paginate(15, req.page())` のように自分で渡します（決定記録 #039）。
    pub fn page(&self) -> u64 {
        self.query("page")
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .unwrap_or(1)
            .clamp(1, MAX_PAGE)
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
    ///
    /// `X-Forwarded-For` も何本かに分かれて届くことがあります（HAProxy の
    /// `option forwardfor` は既定で新しいヘッダー行を足します）。1本目は相手が
    /// 付けた値かもしれないので、**そのまま信用しないでください**。
    /// `headers()` で全部を見て、カンマでつないだ1本として扱います。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// 同じ名前のヘッダーを**全部**返す（届いた順）。
    ///
    /// RFC 9110 では、同名のヘッダーが何本か届くのは、カンマでつないだ 1 本と
    /// 同じ意味です。`X-Forwarded-For` のように**右端から辿る**値は、1 本目だけを
    /// 見ると相手の言い値を拾ってしまいます（HAProxy の `option forwardfor` は
    /// 既定で新しいヘッダー行を足すので、2 本に分かれて届きます）。
    /// `Cookie` を `cookie_pairs` が全部見るのと同じ形です。
    pub(crate) fn header_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers
            .iter()
            .filter(move |(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// ヘッダーの一覧。
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// 本文。
    ///
    /// `.stream_body()` のルートでは空です（[`body_stream`](Self::body_stream) で読みます）。
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
        self.header("content-type").is_some_and(json_content_type)
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
            out.extend_from_slice(self.query_pairs());
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

    /// **本文だけ**から、名前で1つ取り出す（クエリは見ない）。
    ///
    /// 本文はフォームなら組として、JSON なら最上位の値だけを読みます。
    /// CSRF のトークンを探すのに使います（URL に載った値は漏れやすいため）。
    ///
    /// [`input_pairs`](Self::input_pairs) は「本文 → クエリ」の順に並べているので、
    /// 末尾のクエリのぶんを除いた前半が本文です。解析し直しません。
    pub(crate) fn body_input(&self, key: &str) -> Option<String> {
        let pairs = self.input_pairs();
        let body_len = pairs.len().saturating_sub(self.query_pairs().len());
        pairs[..body_len]
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
    /// 落ちたとき、**置き場所から読めたセッション**があれば入力を覚えておきます
    /// （`session.old("title")` で読めます）。
    ///
    /// 値は前後の空白を落としてから検査します（Laravel の `TrimStrings` と同じ）。
    ///
    /// Cookie を持たない相手には覚えません。覚えると `flash` が保存を要求するので、
    /// **壊れた本文を投げるだけでセッションのファイルが1つできます。**
    /// CSRF の確認を外した口に投げ続けると、ファイルが限りなく増えました。
    /// 「無いページではセッションを配らない」という判断と同じ理由です。
    pub fn validate(&self, rules: &[(&str, &str)]) -> Result<crate::validation::Validated> {
        let input = crate::validation::Input::new(self.input_pairs().to_vec());
        match crate::validation::validate(&input, rules) {
            Ok(ok) => Ok(ok),
            Err(e) => {
                // 覚え直させる入力は、ここだけで作る。`redact` を通さない道を作らない。
                if let Some(session) = self.try_session() {
                    if session.was_loaded() {
                        session.flash_input(&redact(self.input_pairs()));
                    }
                }
                Err(e)
            }
        }
    }

    /// 入力を検査し、通った値を型に読む。Laravel の FormRequest に当たります。
    ///
    /// ```ignore
    /// let input: StorePost = req.validate_as()?;
    /// ```
    ///
    /// 規則は型の [`FormRequest::RULES`](crate::validation::FormRequest::RULES) です。
    /// 検査に落ちたら [`validate`](Self::validate) と同じく 422（前の入力も覚えます）、
    /// 通った値を型に読めなければ 500 です
    /// （[`Validated::to_typed`](crate::validation::Validated::to_typed)）。
    ///
    /// `RULES` の書き間違いは、ここを使う所で**コンパイルが止まります**。
    /// ただし確かめるのは `cargo build`（`serve`・`test` を含む）のときで、
    /// `cargo check` では確かめません。`RULES` を `rules!` で書けば `cargo check` でも止まります。
    pub fn validate_as<T: crate::validation::FormRequest>(&self) -> Result<T> {
        const { crate::validation::check_rules(T::RULES) };
        self.validate(T::RULES)?.to_typed()
    }
}

/// セッションに残してはいけない語。
///
/// 名前を `_` `-` `.` `[` `]` 空白と**大文字の境目**で区切り、1語ずつ突き合わせます。
/// 語の**末尾の数字は落として**から突き合わせるので、一覧には数字を付けずに入れます
/// （`password1` は `password` として当たります）。
/// `api_key` や `private_key` は `key` の語で当たるので、一覧には1語ずつ入れます。
///
/// **`code` を入れています。** その引き換えに、`zip_code` や `country_code` も
/// 覚えなくなります（検査に落ちたフォームを出し直すと、郵便番号の欄が空に戻ります）。
/// `recovery_code` や `verification_code` のような、それ自体が合い言葉になる値を
/// 平文でセッションのファイルに残すほうが重いので、**覚えないほうが安全な側**に
/// 倒しました（判断 120 と同じ考え方）。
const SENSITIVE: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "pass",
    "passphrase",
    "secret",
    "token",
    "key",
    "apikey",
    "credential",
    "credentials",
    "cvv",
    "cvc",
    "csc",
    "card",
    "ssn",
    "otp",
    "totp",
    "pin",
    "code",
    "seed",
    "mnemonic",
    "recovery",
    "backup",
    "answer",
    "signature",
    // `sig` は署名の短い書き方です（Azure の SAS の `?sig=...` など）。
    "sig",
    "jwt",
    // `author` は語の単位で見るので当たりません。
    "authorization",
    "session",
    "sid",
    // WebSocket はクエリで使い捨てのチケットを渡すのが普通です（`/gateway?ticket=...`）。
    // 一覧に無いと、アクセスログに平文で残ります。
    "ticket",
];

/// `page()` が返すページ番号の上限。
const MAX_PAGE: u64 = u32::MAX as u64;

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
///
/// **語の単位で突き合わせます。** 以前は部分一致だったので、`shipping_address`
/// （`pin` を含む）・`opinion`・`passenger`・`keyword` のような普通の項目まで
/// 黙って落ちていました。再表示したフォームで住所欄だけが空に戻る、という
/// 原因の分かりにくい不具合になります。
///
/// **語の末尾の数字は落として**突き合わせます。語の単位だけで見ていたころは、
/// `password1` `password2` `pass2` `token2` `otp1` がどの語にも当たらず、
/// 検査に落ちた入力が**平文でセッションのファイルに残っていました。**
/// `password1` / `password2` の組はパスワード変更フォームでよくある形で、
/// 1 回検査に落ちるだけで両方がディスクに残ります。
/// 落とすのは末尾だけなので、`keyword` や `shipping_address` は今までどおり残ります。
///
/// アクセスログでクエリの値を伏せるときも、同じ判定を使います。
pub(crate) fn is_sensitive(name: &str) -> bool {
    for_each_word(name, |word| {
        let word = word.trim_end_matches(|c: char| c.is_ascii_digit());
        SENSITIVE
            .iter()
            .any(|known| word.eq_ignore_ascii_case(known))
    })
}

/// 名前を語に区切って1語ずつ渡す。真を返した時点で止めて真を返します。
///
/// 区切りは `_` `-` `.` `[` `]` 空白と、次の 2 つの大文字の境目です。
///
/// | 境目 | 例 |
/// |---|---|
/// | 小文字から大文字へ変わる所 | `apiKey` → `api` と `Key` |
/// | 大文字が続いた後、「大文字＋小文字」が来る所（その大文字の前） | `APIKey` → `API` と `Key` |
///
/// 2 つ目が無かったころは、`APIKey` や `JWTToken` が 1 語になり、素通りしていました。
///
/// `[` `]` を区切りにしているのは、PHP 風の配列の名前のためです。`?token[]=...` は
/// `token[]` という 1 語になり、どの語にも当たらずに素通りしていました
/// （アクセスログにも、検査に落ちた入力のセッションにも平文で残りました）。
///
/// 大文字が続くところでは切りません。1文字ずつに切ると、`PASSWORD` や `API_KEY`
/// のような全部大文字の名前がどの語にも当たらなくなります。
/// 区切りに使う文字はどれも1バイトなので、UTF-8 の途中で切ることはありません。
/// 文字列を作らないので、1リクエストごとに呼んでも確保が増えません。
fn for_each_word(name: &str, mut found: impl FnMut(&str) -> bool) -> bool {
    let bytes = name.as_bytes();
    let mut start = 0;
    for index in 0..bytes.len() {
        let byte = bytes[index];
        let separator = matches!(byte, b'_' | b'-' | b'.' | b' ' | b'[' | b']');
        // `aB` の `B`、または `ABc` の `B`（大文字の並びの最後の 1 文字が次の語の頭）。
        let upper = byte.is_ascii_uppercase()
            && index > start
            && (bytes[index - 1].is_ascii_lowercase()
                || (bytes[index - 1].is_ascii_uppercase()
                    && bytes.get(index + 1).is_some_and(u8::is_ascii_lowercase)));
        if !separator && !upper {
            continue;
        }
        if index > start && found(&name[start..index]) {
            return true;
        }
        start = if separator { index + 1 } else { index };
    }
    start < bytes.len() && found(&name[start..])
}

/// `Content-Type` が JSON か。
///
/// `application/json` と、`application/vnd.api+json` のような `+json` で終わる形を見ます。
/// **大文字小文字は区別しません。** 以前は `+json` の判定だけ素の比較だったので、
/// `application/vnd.api+JSON` の本文が読まれませんでした。
pub(crate) fn json_content_type(value: &str) -> bool {
    let kind = value.split(';').next().unwrap_or("").trim();
    if kind.eq_ignore_ascii_case("application/json") {
        return true;
    }
    matches!(
        kind.rsplit_once('+'),
        Some((head, suffix)) if !head.is_empty() && suffix.eq_ignore_ascii_case("json")
    )
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
        out.push((super::percent::decode(k), super::percent::decode(v)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn デバッグ表示に秘密を出さない() {
        // 以前は `derive(Debug)` だったので、`{req:?}` で全部出ていた。
        let req = Request::new("POST", "/login")
            .with_query("page=2&token=abc")
            .with_headers(vec![
                ("Cookie".into(), "session=s3cret".into()),
                ("authorization".into(), "Bearer t0ken".into()),
                ("proxy-authorization".into(), "Basic cHJveHk=".into()),
                ("x-csrf-token".into(), "csrfvalue".into()),
                ("accept".into(), "text/html".into()),
            ])
            .with_body(b"email=a%40example.com&password=hunter2".to_vec());
        let shown = format!("{req:?}");
        for secret in [
            "s3cret",
            "t0ken",
            "cHJveHk=",
            "csrfvalue",
            "hunter2",
            "abc",
            "example.com",
        ] {
            assert!(!shown.contains(secret), "{secret} が出ている: {shown}");
        }
        // 調べるのに要るものは出す。
        for kept in [
            "POST",
            "/login",
            "page=2&token=***",
            "Cookie",
            "authorization",
            "text/html",
            "body_len: 38",
        ] {
            assert!(shown.contains(kept), "{kept} が無い: {shown}");
        }
    }

    #[test]
    fn ページ番号は1から始まる() {
        assert_eq!(Request::new("get", "/").page(), 1);
        assert_eq!(Request::new("get", "/").with_query("page=3").page(), 3);
        assert_eq!(Request::new("get", "/").with_query("page=0").page(), 1);
        assert_eq!(Request::new("get", "/").with_query("page=x").page(), 1);
    }

    #[test]
    fn ページ番号は上限で丸める() {
        // `u64::MAX` をそのまま返すと `paginate` の掛け算があふれる。
        let huge = Request::new("get", "/").with_query("page=18446744073709551615");
        assert_eq!(huge.page(), u64::from(u32::MAX));
        let edge = Request::new("get", "/").with_query("page=4294967295");
        assert_eq!(edge.page(), u64::from(u32::MAX));
    }

    #[test]
    fn クエリを分解できる() {
        let req = Request::new("get", "/search").with_query("q=hello+world&page=2");
        assert_eq!(req.query("q").as_deref(), Some("hello world"));
        assert_eq!(req.query("page").as_deref(), Some("2"));
        assert_eq!(req.query("none"), None);
    }

    #[test]
    fn メソッドは受け取ったまま持つ() {
        // HTTP のメソッドは大文字小文字を区別する。大文字にそろえていたころは
        // `get /x` が `GET` として照合され、ハンドラまで通っていた（本来は 405）。
        assert_eq!(Request::new("POST", "/").method(), "POST");
        assert_eq!(Request::new("post", "/").method(), "post");
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

    /// `model()` を試すためのモデル。整数の主キー（既定）です。
    struct Post {
        id: i64,
    }

    impl crate::database::Model for Post {
        const TABLE: &'static str = "posts";
        const PRIMARY_KEY: &'static str = "id";
        const COLUMNS: &'static [&'static str] = &["id"];

        fn from_row(row: &crate::database::Row) -> Result<Self> {
            Ok(Self { id: row.get("id")? })
        }

        fn attributes(&self) -> Vec<(&'static str, crate::database::Value)> {
            Vec::new()
        }

        fn key(&self) -> crate::database::Value {
            crate::database::Value::Int(self.id)
        }

        fn set_key(&mut self, _value: crate::database::Value) {}
    }

    #[tokio::test]
    async fn 整数に読めないパス引数のmodelは問い合わせずに404になる() {
        // 既定の接続には流れない（流れれば、つなぐところで別のエラーになる）。
        for value in ["abc", "12abc", "1.5", "99999999999999999999999"] {
            let mut req = Request::new("get", "/posts/x");
            req.set_params(vec![("post".into(), value.into())]);
            let error = req.model::<Post>("post").await.err().expect("404 になる");
            assert_eq!(error.status(), 404, "{value}: {error}");
            // 文は `find_or_fail` と同じ（以前の `model()` とも同じ）。
            assert!(
                error.to_string().contains("posts が見つかりません"),
                "{value}: {error}"
            );
        }

        // ルートに無い名前は、これまでどおり開発者の書き間違い（500）。
        let req = Request::new("get", "/posts/1");
        let error = req.model::<Post>("post").await.err().expect("断られる");
        assert_eq!(error.status(), 500);
        assert!(error.to_string().contains("{post}"), "{error}");
    }

    #[test]
    fn パス引数の名前が無ければ500で値が合わなければ404() {
        let mut req = Request::new("get", "/posts/abc");
        req.set_params(vec![("post".into(), "abc".into())]);
        // 値が型に合わないのは、相手が送った URL の問題。404 のまま。
        assert_eq!(req.param_as::<i64>("post").unwrap_err().status(), 404);
        // ルートに無い名前は開発者の書き間違い。以前は 404 で、気づきにくかった。
        // `model()` と同じ 500 にそろえる。
        let error = req.param_as::<i64>("id").unwrap_err();
        assert_eq!(error.status(), 500);
        assert!(error.to_string().contains("`{id}`"), "{error}");
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
            // 署名付き URL の署名も、それ自体が認証情報なので残さない。
            "signature",
            "X-Signature",
        ];
        for name in banned {
            assert!(is_sensitive(name), "{name} は残してはいけない");
        }
        for name in [
            "title",
            "email",
            "age",
            "body",
            // 以前は部分一致だったので、ここから下が黙って落ちていた。
            "shipping_address",
            "shippingAddress",
            "opinion",
            "topping",
            "spinner",
            "keyword",
            "monkey",
            "turkey",
            "passenger",
            "passport",
            "discard",
            "cardiac",
        ] {
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

    #[test]
    fn 名前を語に区切る() {
        let collect = |name: &str| {
            let mut out = Vec::new();
            for_each_word(name, |word| {
                out.push(word.to_string());
                false
            });
            out
        };
        assert_eq!(
            collect("password_confirmation"),
            ["password", "confirmation"]
        );
        assert_eq!(collect("apiKey"), ["api", "Key"]);
        assert_eq!(collect("_token"), ["token"]);
        assert_eq!(collect("a.b-c d"), ["a", "b", "c", "d"]);
        // 日本語の名前でも、UTF-8 の途中で切らない。
        assert_eq!(collect("題名_副題"), ["題名", "副題"]);
        assert_eq!(collect(""), Vec::<String>::new());
        // 大文字が続くところでは切らない。1文字ずつにすると語として当たらなくなる。
        assert_eq!(collect("API_KEY"), ["API", "KEY"]);
        assert_eq!(collect("PASSWORD"), ["PASSWORD"]);
        assert_eq!(collect("X-CSRF-TOKEN"), ["X", "CSRF", "TOKEN"]);
        // 大文字の並びの後に「大文字＋小文字」が来たら、その大文字の前で切る。
        assert_eq!(collect("APIKey"), ["API", "Key"]);
        assert_eq!(collect("XMLHttpRequest"), ["XML", "Http", "Request"]);
        assert_eq!(collect("userJWTToken"), ["user", "JWT", "Token"]);
    }

    #[test]
    fn 大文字の並びに続く語も伏せる() {
        // 以前は `APIKey` が 1 語になり、どの語にも当たらなかった。
        for name in [
            "APIKey",
            "JWTToken",
            "OAuthToken",
            "myAPISecret",
            "AWSSecretKey",
        ] {
            assert!(is_sensitive(name), "{name} は残してはいけない");
        }
        for name in ["XMLHttpRequest", "HTMLTitle", "URLPath"] {
            assert!(!is_sensitive(name), "{name} は残してよい");
        }
    }

    #[test]
    fn 署名と認証の語も伏せる() {
        for name in [
            "sig",
            "jwt",
            "id_jwt",
            "authorization",
            "Authorization",
            "session",
            "session_id",
            "sid",
            "PHPSESSID_sid",
        ] {
            assert!(is_sensitive(name), "{name} は残してはいけない");
        }
        // 語の単位で見るので、似た綴りの普通の項目は残る。
        for name in [
            "author",
            "author_name",
            "side",
            "signal",
            "sigma",
            "sessions_count",
        ] {
            assert!(!is_sensitive(name), "{name} は残してよい");
        }
    }

    #[test]
    fn 全部大文字の名前も伏せる() {
        // 以前は大文字が1文字ずつの語に切れていたので、どれも素通りしていた。
        for name in [
            "PASSWORD",
            "API_KEY",
            "TOKEN",
            "SSN",
            "CVV",
            "X-CSRF-TOKEN",
            "SIGNATURE",
        ] {
            assert!(is_sensitive(name), "{name} は残してはいけない");
        }
    }

    #[test]
    fn 数字で終わる名前も伏せる() {
        // 語の単位だけで見ていたころは、数字で終わる名前が1語になるので
        // どれも素通りし、平文でセッションのファイルに残っていた。
        // `password1` / `password2` はパスワード変更フォームでよくある形。
        for name in [
            "password1",
            "password2",
            "pass2",
            "token2",
            "otp1",
            "apiKey2",
            "PASSWORD1",
            "secret_key2",
            "card1",
        ] {
            assert!(is_sensitive(name), "{name} は残してはいけない");
        }
        // 末尾の数字を落としても、普通の項目は残る。
        for name in ["keyword", "shipping_address", "address1", "line2", "item_1"] {
            assert!(!is_sensitive(name), "{name} は残してよい");
        }

        let pairs = vec![
            ("title".to_string(), "のこる".to_string()),
            ("password1".to_string(), "きえる".to_string()),
            ("password2".to_string(), "きえる".to_string()),
        ];
        assert_eq!(
            redact(&pairs),
            vec![("title".to_string(), "のこる".to_string())]
        );
    }

    #[test]
    fn かっこ付きの名前も伏せる() {
        // `?token[]=abc` は `token[]` という 1 語になり、素通りしていた。
        for name in ["token[]", "user[password]", "data[api_key]", "otp[0]"] {
            assert!(is_sensitive(name), "{name} は残してはいけない");
        }
        assert_eq!(
            {
                let mut out = Vec::new();
                for_each_word("token[]", |word| {
                    out.push(word.to_string());
                    false
                });
                out
            },
            ["token"]
        );
    }

    #[test]
    fn 合い言葉になる語も伏せる() {
        for name in [
            "cvc",
            "csc",
            "passphrase",
            "totp",
            "seed",
            "mnemonic",
            "recovery_code",
            "backup_codes",
            "security_answer",
            "verification_code",
        ] {
            assert!(is_sensitive(name), "{name} は残してはいけない");
        }
        // `code` を足した引き換え。郵便番号や国コードも覚えなくなる。
        assert!(is_sensitive("zip_code"), "覚えないほうが安全な側に倒した");
        assert!(is_sensitive("country_code"));
        // 普通の項目はこれまでどおり残る。
        for name in ["keyword", "shipping_address", "title", "codex", "answered"] {
            assert!(!is_sensitive(name), "{name} は残してよい");
        }
    }

    #[test]
    fn 読めていないセッションには旧入力を覚えない() {
        // Cookie を持たない相手が壊れた本文を投げるだけで、セッションの
        // ファイルが1つできてしまう（`flash` が保存を要求するため）。
        let mut req = Request::new("post", "/api/posts").with_query("title=");
        let session = crate::session::Session::empty();
        req.set_session(session.clone());
        assert!(req.validate(&[("title", "required")]).is_err());
        assert!(session.old("title").is_none(), "覚えない");
        assert!(!session.should_save(), "セッションを保存させない");
    }

    #[test]
    fn validate_asは422か型付きの値を返す() {
        #[derive(Debug, serde::Deserialize)]
        struct StorePost {
            title: String,
            age: Option<i64>,
        }
        impl crate::validation::FormRequest for StorePost {
            const RULES: &'static [(&'static str, &'static str)] = crate::rules! {
                "title" => "required|max:10",
                "age"   => "nullable|integer",
            };
        }

        let req = Request::new("post", "/posts").with_query("title=題&age=30&extra=x");
        let input: StorePost = req.validate_as().unwrap();
        assert_eq!(input.title, "題");
        assert_eq!(input.age, Some(30));

        // `nullable` で空なら None。
        let req = Request::new("post", "/posts").with_query("title=題");
        assert_eq!(req.validate_as::<StorePost>().unwrap().age, None);

        // 検査に落ちたら 422。
        let req = Request::new("post", "/posts").with_query("title=&age=x");
        let error = req.validate_as::<StorePost>().unwrap_err();
        assert_eq!(error.status(), 422);
        match error {
            crate::Error::Validation(errors) => {
                assert!(errors.has("title") && errors.has("age"));
            }
            other => panic!("検査のエラーのはずです: {other:?}"),
        }
    }

    #[test]
    fn validate_asは規則と型が食い違えば500() {
        // 規則に `age` が無いのに、型は `Option` でない `age` を求めている。
        #[derive(Debug, serde::Deserialize)]
        #[allow(dead_code)]
        struct Mismatch {
            age: i64,
        }
        impl crate::validation::FormRequest for Mismatch {
            const RULES: &'static [(&'static str, &'static str)] = &[("title", "required")];
        }
        let req = Request::new("post", "/posts").with_query("title=題&age=3");
        let error = req.validate_as::<Mismatch>().unwrap_err();
        assert_eq!(error.status(), 500);
        assert!(error.to_string().contains("項目 `age`"), "{error}");
    }

    #[test]
    fn 読めたセッションには旧入力を覚える() {
        // 画面を使っている人には、今までどおり `old()` が働く。
        let mut req = Request::new("post", "/posts").with_query("title=&body=本文");
        let session = crate::session::Session::empty().mark_loaded();
        req.set_session(session.clone());
        assert!(req.validate(&[("title", "required")]).is_err());
        assert_eq!(session.old("body").as_deref(), Some("本文"));
        assert!(session.should_save());
    }

    #[test]
    fn content_typeのjsonは大文字小文字を区別しない() {
        assert!(json_content_type("application/json"));
        assert!(json_content_type("APPLICATION/JSON"));
        assert!(json_content_type("application/json; charset=utf-8"));
        assert!(json_content_type("application/vnd.api+json"));
        // 以前は `+json` の判定だけ素の比較で、これが通らなかった。
        assert!(json_content_type("application/vnd.api+JSON"));
        assert!(!json_content_type("application/x-www-form-urlencoded"));
        assert!(!json_content_type("+json"), "前が空なら形として認めない");
        assert!(!json_content_type(""));
    }

    #[test]
    fn jsonの本文は種類の大文字小文字を問わず読める() {
        let req = Request::new("post", "/")
            .with_headers(vec![(
                "content-type".into(),
                "application/vnd.api+JSON".into(),
            )])
            .with_body(r#"{"title":"あ"}"#.as_bytes().to_vec());
        assert_eq!(req.input("title").as_deref(), Some("あ"));
    }

    #[test]
    fn クエリは1回だけ解析する() {
        let req = Request::new("get", "/search").with_query("q=hello+world&page=2");
        let first = req.query_pairs().as_ptr();
        assert_eq!(req.query("q").as_deref(), Some("hello world"));
        assert_eq!(req.query_all().len(), 2);
        assert_eq!(req.page(), 2);
        assert_eq!(req.query_pairs().as_ptr(), first, "解析し直していない");
    }
}
