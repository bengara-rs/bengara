//! テスト用のクライアント。ソケットを開かずに本体を呼び出します。
//!
//! `tests/Feature/*.rs` では `#[bengara::test]` を付けると、`get` と `post` が
//! そのまま使える状態になります。
//!
//! ```ignore
//! #[bengara::test]
//! async fn home_page_is_ok() {
//!     get("/").await.assert_ok().assert_see("bengara");
//! }
//! ```

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use crate::application::Application;
use crate::http::{Request, Response};
use crate::{kernel_impl, Hooks};

/// フォームで送るときの `content-type`。`post` / `put` / `patch` で同じものを使う。
const FORM_CONTENT_TYPE: &str = "application/x-www-form-urlencoded";

/// テストから送るときの接続元のアドレス。
///
/// ソケットを開かないので本物の接続元はありません。`127.0.0.1` を入れておくと、
/// 回数の制限（`Throttle`）が本番と同じように接続元ごとに数えます。
const TEST_REMOTE_ADDR: std::net::SocketAddr = std::net::SocketAddr::new(
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),
    0,
);

/// テスト用のクライアント。
///
/// **Cookie を覚えます。** ブラウザと同じように、1つのクライアントで続けて送ると
/// セッションがつながります。別の人として送りたいときは `fresh()` で作り直してください。
#[derive(Clone)]
pub struct TestClient {
    app: Application,
    cookies: Arc<Mutex<BTreeMap<String, String>>>,
}

impl TestClient {
    /// 組み立て済みのアプリからクライアントを作る。
    pub fn new(app: Application) -> Self {
        app.install_names();
        Self {
            app,
            cookies: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Cookie を捨てた、別の人としてのクライアントを作る。
    pub fn fresh(&self) -> Self {
        Self {
            app: self.app.clone(),
            cookies: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// いま持っている Cookie の値。
    pub fn cookie(&self, name: &str) -> Option<String> {
        self.cookies
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
    }

    /// GET を送る。クエリは `"/search?q=1"` のように書けます。
    pub async fn get(&self, uri: &str) -> TestResponse {
        self.send("GET", uri, Vec::new(), &[]).await
    }

    /// POST を送る（`application/x-www-form-urlencoded`）。
    pub async fn post(&self, uri: &str, body: &str) -> TestResponse {
        self.form("POST", uri, body).await
    }

    /// PUT を送る（`application/x-www-form-urlencoded`）。
    pub async fn put(&self, uri: &str, body: &str) -> TestResponse {
        self.form("PUT", uri, body).await
    }

    /// PATCH を送る（`application/x-www-form-urlencoded`）。
    pub async fn patch(&self, uri: &str, body: &str) -> TestResponse {
        self.form("PATCH", uri, body).await
    }

    /// DELETE を送る。本文は付けません。
    pub async fn delete(&self, uri: &str) -> TestResponse {
        self.send("DELETE", uri, Vec::new(), &[]).await
    }

    /// フォームの本文を付けて送る。`post` / `put` / `patch` の中身。
    async fn form(&self, method: &str, uri: &str, body: &str) -> TestResponse {
        self.send(
            method,
            uri,
            body.as_bytes().to_vec(),
            &[("content-type", FORM_CONTENT_TYPE)],
        )
        .await
    }

    /// JSON を送る。
    pub async fn post_json(&self, uri: &str, body: &serde_json::Value) -> TestResponse {
        self.send(
            "POST",
            uri,
            body.to_string().into_bytes(),
            &[("content-type", "application/json")],
        )
        .await
    }

    /// CSRF のトークンを取ってから POST する。
    ///
    /// `/csrf-token` のような、トークンを返すルートを先に叩きます。
    /// トークンは `X-CSRF-TOKEN` ヘッダーで送ります。
    ///
    /// ```ignore
    /// client.post_with_csrf("/posts", "title=x", "/csrf-token").await;
    /// ```
    pub async fn post_with_csrf(&self, uri: &str, body: &str, token_uri: &str) -> TestResponse {
        let token = self.csrf_token(token_uri).await;
        self.send(
            "POST",
            uri,
            body.as_bytes().to_vec(),
            &[
                ("content-type", FORM_CONTENT_TYPE),
                ("x-csrf-token", &token),
            ],
        )
        .await
    }

    /// トークンを返すルートを叩いて、`token` の値を取り出す。
    ///
    /// # パニック
    ///
    /// 応答が JSON の `token`（文字列）を持たないときはパニックします。
    /// 以前は空文字を返していたので、トークンの口が 500 を返しても黙って
    /// `""` を送り、テストの失敗は「419」や「500」として遠い場所に出ていました。
    pub async fn csrf_token(&self, token_uri: &str) -> String {
        let res = self.get(token_uri).await;
        // `json()` は読めない本文でパニックする。ここでは下の文
        //（どのルートを指定すればよいか）で知らせたいので、読めなければ `Null` にする。
        let json = res.try_json().unwrap_or(serde_json::Value::Null);
        match json["token"].as_str() {
            Some(token) if !token.is_empty() => token.to_string(),
            _ => panic!(
                "GET {token_uri} が CSRF トークンを返しませんでした（{} / 本文: {}）。\
                 `{{\"token\": req.csrf_token()}}` を返すルートを指定してください",
                res.status(),
                res.body_preview()
            ),
        }
    }

    /// メソッド・ヘッダーまで自分で決めて送る。
    ///
    /// 覚えている Cookie は自動で付きます。`cookie` ヘッダーを自分で指定したときは、
    /// そちらを優先します。
    pub async fn send(
        &self,
        method: &str,
        uri: &str,
        body: Vec<u8>,
        headers: &[(&str, &str)],
    ) -> TestResponse {
        let (path, query) = match uri.split_once('?') {
            Some((path, query)) => (path, query),
            None => (uri, ""),
        };
        let mut headers: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
            .collect();

        if !headers.iter().any(|(k, _)| k == "cookie") {
            if let Some(header) = self.cookie_header() {
                headers.push(("cookie".to_string(), header));
            }
        }

        // 接続元のアドレスを入れておく。入れないと `req.ip()` が空になり、
        // 回数の制限（`Throttle`）が鍵を作れずに素通りしてしまう。
        let req = Request::new(method, path)
            .with_query(query)
            .with_headers(headers)
            .with_body(body)
            .with_remote_addr(TEST_REMOTE_ADDR);
        let response = self.app.handle(req).await;
        self.remember_cookies(&response);
        TestResponse {
            inner: response,
            uri: uri.to_string(),
        }
    }

    fn cookie_header(&self) -> Option<String> {
        let cookies = self.cookies.lock().unwrap_or_else(|e| e.into_inner());
        if cookies.is_empty() {
            return None;
        }
        Some(
            cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    /// `Set-Cookie` を覚える。期限が切れたもの（[`cookie_expired`]）は捨てます。
    fn remember_cookies(&self, response: &Response) {
        let mut cookies = self.cookies.lock().unwrap_or_else(|e| e.into_inner());
        for (name, value) in response.headers() {
            if name != "set-cookie" {
                continue;
            }
            let mut parts = value.split(';');
            let Some(pair) = parts.next() else { continue };
            let Some((key, val)) = pair.split_once('=') else {
                continue;
            };
            if cookie_expired(value, crate::support::time::now_seconds()) {
                cookies.remove(key.trim());
            } else {
                cookies.insert(key.trim().to_string(), val.trim().to_string());
            }
        }
    }
}

/// `Set-Cookie` の値が「消す」指示か（期限がもう切れているか）。
///
/// ブラウザと同じく次のどちらかなら消します。
///
/// - `Max-Age` が 0 以下（`Max-Age=-1` も含む）
/// - `Expires` が `now` 以前（フレームワークは `Thu, 01 Jan 1970 00:00:00 GMT` を出す）
///
/// 以前は `Max-Age=0` ちょうどだけを見ていて、ほかの消し方では残っていました。
/// `Max-Age` があれば `Expires` より優先します（RFC 6265 と同じ）。
/// 読めない `Max-Age` と `Expires` は無いものとして扱います。
fn cookie_expired(value: &str, now: i64) -> bool {
    let mut expires = None;
    // 最初の `名前=値` は飛ばす。
    for attribute in value.split(';').skip(1) {
        let Some((key, raw)) = attribute.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.eq_ignore_ascii_case("Max-Age") {
            if let Ok(seconds) = raw.trim().parse::<i64>() {
                return seconds <= 0;
            }
        } else if key.eq_ignore_ascii_case("Expires") {
            expires = parse_http_date(raw);
        }
    }
    expires.is_some_and(|at| at <= now)
}

/// `Thu, 01 Jan 1970 00:00:00 GMT` の形（RFC 1123）を UNIX 時刻（秒）にする。
///
/// テストの中で期限を見るだけなので、この形だけを読みます。
fn parse_http_date(text: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    // 曜日（`Thu,`）は見ない。
    let text = text.trim();
    let rest = text.split_once(',').map_or(text, |(_, rest)| rest);
    let mut parts = rest.split_whitespace();
    let day: u32 = parts.next()?.parse().ok()?;
    let month = parts.next()?;
    let month = MONTHS.iter().position(|m| m.eq_ignore_ascii_case(month))? + 1;
    let year: i64 = parts.next()?.parse().ok()?;
    let time = parts.next()?;
    crate::support::time::parse_timestamp(&format!("{year:04}-{month:02}-{day:02} {time}"))
}

/// 返ってきた内容を確かめるための型。
pub struct TestResponse {
    inner: Response,
    uri: String,
}

impl TestResponse {
    /// ステータスが 200 であることを確かめる。
    #[track_caller]
    pub fn assert_ok(self) -> Self {
        self.assert_status(200)
    }

    /// ステータスを確かめる。
    #[track_caller]
    pub fn assert_status(self, expected: u16) -> Self {
        assert_eq!(
            self.inner.status(),
            expected,
            "{} のステータスが {} ではなく {} でした\n本文: {}",
            self.uri,
            expected,
            self.inner.status(),
            self.body_preview()
        );
        self
    }

    /// 本文に文字列が含まれることを確かめる。
    #[track_caller]
    pub fn assert_see(self, needle: &str) -> Self {
        assert!(
            self.inner.body_text().contains(needle),
            "{} の本文に `{needle}` が見つかりません\n本文: {}",
            self.uri,
            self.body_preview()
        );
        self
    }

    /// 本文に文字列が含まれないことを確かめる。
    #[track_caller]
    pub fn assert_dont_see(self, needle: &str) -> Self {
        assert!(
            !self.inner.body_text().contains(needle),
            "{} の本文に `{needle}` が入っています\n本文: {}",
            self.uri,
            self.body_preview()
        );
        self
    }

    /// ヘッダーの値を確かめる。
    #[track_caller]
    pub fn assert_header(self, name: &str, expected: &str) -> Self {
        assert_eq!(
            self.inner.header(name),
            Some(expected),
            "{} の {name} が期待と違います",
            self.uri
        );
        self
    }

    /// リダイレクト先を確かめる。
    #[track_caller]
    pub fn assert_redirect(self, location: &str) -> Self {
        let status = self.inner.status();
        assert!(
            (300..400).contains(&status),
            "{} はリダイレクトではありません（{status}）",
            self.uri
        );
        self.assert_header("location", location)
    }

    /// ステータス。
    pub fn status(&self) -> u16 {
        self.inner.status()
    }

    /// 本文を文字列で見る。
    pub fn body(&self) -> String {
        self.inner.body_text().to_string()
    }

    /// 本文を JSON として読む。
    ///
    /// # パニック
    ///
    /// **本文が JSON として読めないときはパニックします。** URI・ステータス・
    /// 本文の先頭を付けて知らせます。以前は黙って `Null` を返していたので、
    /// HTML のエラー画面が返っているテストで `res.json()["x"]` が `Null` になり、
    /// 原因が見えませんでした。
    ///
    /// JSON でない本文を見たいときは [`TestResponse::body`] を使ってください。
    #[track_caller]
    pub fn json(&self) -> serde_json::Value {
        match self.try_json() {
            Ok(value) => value,
            Err(e) => panic!(
                "{} の本文を JSON として読めません（ステータス {}）: {e}\n本文: {}",
                self.uri,
                self.inner.status(),
                self.body_preview()
            ),
        }
    }

    /// 本文を JSON として読む。読めなければエラーを返す（[`TestResponse::json`] の中身）。
    fn try_json(&self) -> std::result::Result<serde_json::Value, serde_json::Error> {
        serde_json::from_slice(self.inner.body())
    }

    /// ヘッダーの値。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.inner.header(name)
    }

    fn body_preview(&self) -> String {
        let text = self.inner.body_text();
        if text.chars().count() <= 400 {
            text.to_string()
        } else {
            let head: String = text.chars().take(400).collect();
            format!("{head}…")
        }
    }
}

/// DB を使うテストの順番を守るための札。
///
/// `refresh_database()` が返します。これを持っている間、ほかの DB テストは待ちます。
/// テストが終わると自動で手放されます。
pub struct DatabaseGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl std::fmt::Debug for DatabaseGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DatabaseGuard")
    }
}

impl Drop for DatabaseGuard {
    fn drop(&mut self) {
        HOLDS_DATABASE.set(false);
    }
}

/// DB を使うテストの順番を決める錠。
static DATABASE_LOCK: Mutex<()> = Mutex::new(());

/// DB 以外も直列化するための錠。
static EXCLUSIVE_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    /// このスレッドが DB の札を持っているか。
    static HOLDS_DATABASE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// このスレッドが汎用の札を持っているか。
    static HOLDS_EXCLUSIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 1 つのテストで札を 2 回取ろうとしていないか確かめる。
///
/// `std::sync::Mutex` は再入できないので、気づかないと**何も言わずに永久に止まります**。
/// 分かるメッセージでパニックさせます。
///
/// 立っているかを見るだけです。**旗を立てるのは札を作る直前**にします。
/// ここで立ててしまうと、途中（錠の取得やマイグレーション）で失敗したときに
/// 旗が立ったまま残り、同じスレッドで走る後のテストが全部この文で落ちて、
/// 本当の原因が埋もれるためです。
//
// `#[track_caller]` は付けません。呼び元が `async fn` なので効かず、
// 付いていると効いているように見えて紛らわしいためです。
// パニックの場所はこのファイル（`testing.rs`）になります。
fn check_reentrant(held: &'static std::thread::LocalKey<std::cell::Cell<bool>>, what: &str) {
    if held.get() {
        panic!(
            "{what} を同じテストの中で 2 回呼んでいます。\n\
             札は 1 つのテストで 1 回だけ取ってください（取り直すと永久に止まります）。\n\
             先に取った札（`let _db = ...`）を使い回してください。"
        );
    }
}

/// テスト用のデータベースにつなげなかったときのパニックの文。
///
/// 機能フラグは「features に足す」と言います。以前は `features = ["sqlite"]` と
/// 1 行まるごと見せていたので、そのまま写すと他のフラグが落ちました。
/// MySQL や PostgreSQL を使う人にも合うように、`sqlite` と決めつけません。
fn connect_failure(cause: &dyn std::fmt::Display) -> String {
    format!(
        "テスト用のデータベースにつなげません: {cause}\n\
         使うデータベースの機能フラグ（\"sqlite\" / \"mysql\" / \"postgres\"）が \
         Cargo.toml の bengara の features に入っているか確かめてください。\
         無ければ足してください（いまある機能フラグは残します）"
    )
}

/// テスト用のデータベースを空から作り直す。
///
/// ```ignore
/// #[bengara::test]
/// async fn 記事を保存できる() {
///     let _db = refresh_database().await;
///     // ...
/// }
/// ```
///
/// - 置き場所は `DB_TEST_DATABASE`（既定は `:memory:`）です。開発用の DB には触りません。
/// - 表を全部消してから `database/migrations/` を流します。
/// - **返ってきた札を受け取ってください**（`let _db = ...`）。手放すと順番が守られません。
///
/// # パニック
///
/// つなげないときや、マイグレーションが失敗したときはパニックします。
/// テストの中なので、そこで止まるほうが分かりやすいためです。
///
/// **1 つのテストで 2 回呼んだときもパニックします。** 錠は再入できないので、
/// そのまま待つと何も言わずに永久に止まります。
// 錠を `await` の向こうまで持ち越すのは意図どおりです。これが DB を使うテストの
// 順番を守る仕組みで、持ち越さないと意味がありません。テストの中だけで使います。
#[allow(clippy::await_holding_lock)]
pub async fn refresh_database() -> DatabaseGuard {
    check_reentrant(&HOLDS_DATABASE, "refresh_database()");
    let guard = DATABASE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    crate::database::connect_for_tests()
        .await
        .unwrap_or_else(|e| panic!("{}", connect_failure(&e)));

    let hooks = kernel_impl::test_hooks();
    let migrations = (hooks.migrations)();
    let migrator = crate::database::migrator::Migrator::connect(None)
        .await
        .unwrap_or_else(|e| panic!("テスト用のデータベースを用意できません: {e}"));
    migrator
        .fresh(migrations)
        .await
        .unwrap_or_else(|e| panic!("テスト用のマイグレーションに失敗しました: {e}"));

    // ここまで来てから旗を立てる。札（`DatabaseGuard`）の `Drop` が下ろす。
    HOLDS_DATABASE.set(true);
    DatabaseGuard { _guard: guard }
}

/// シーダーをテストから流す。`refresh_database()` の後に呼びます。
///
/// ```ignore
/// #[bengara::test]
/// async fn 初期データが入る() {
///     let db = refresh_database().await;
///     seed_database(&db).await;
/// }
/// ```
///
/// 札（`&DatabaseGuard`）を受け取るのは、**札を取っていないテストから
/// 呼べないようにするため**です。型で縛っておくと、並列に走る別のテストの
/// DB を壊しません。
///
/// # パニック
///
/// シーダーが失敗したときはパニックします。
pub async fn seed_database(_db: &DatabaseGuard) {
    let hooks = kernel_impl::test_hooks();
    crate::database::migrator::seed((hooks.seeders)(), None)
        .await
        .unwrap_or_else(|e| panic!("シーダーの実行に失敗しました: {e}"));
}

/// DB 以外も直列化するための札。
///
/// `exclusive()` が返します。これを持っている間、ほかの `exclusive()` は待ちます。
pub struct ExclusiveGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl std::fmt::Debug for ExclusiveGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExclusiveGuard")
    }
}

impl Drop for ExclusiveGuard {
    fn drop(&mut self) {
        // `Broadcast::fake()` はこの札の間だけ効く。手放すときに終えて、記録も空にする。
        // 残すと、後から走るテストの publish が黙って記録に吸われる。
        crate::broadcast::reset_fake();
        HOLDS_EXCLUSIVE.set(false);
    }
}

/// プロセス共通のものを触るテストを直列にする。
///
/// `Mail::sent()` / `Mail::clear_sent()` と `Lang` はプロセス共通なので、
/// 並列に走ると互いに干渉します。`tests/Feature` は 1 つのバイナリで
/// 並列に走るので、**触るテストはこの札を取ってください。**
///
/// ```ignore
/// #[bengara::test]
/// async fn メールを送る() {
///     let _lock = exclusive().await;
///     Mail::clear_sent();
///     // ...
/// }
/// ```
///
/// DB の札（`refresh_database()`）とは別の錠です。両方要るときは
/// **先に `refresh_database()`、次に `exclusive()`** の順で取ってください
/// （順番をそろえないと、取り合いで止まります）。
///
/// # パニック
///
/// 1 つのテストで 2 回呼んだときはパニックします（錠は再入できません）。
// 錠を `await` の向こうまで持ち越すのは意図どおりです（`refresh_database` と同じ）。
#[allow(clippy::await_holding_lock)]
pub async fn exclusive() -> ExclusiveGuard {
    check_reentrant(&HOLDS_EXCLUSIVE, "exclusive()");
    let guard = EXCLUSIVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // 錠を取れてから旗を立てる。札（`ExclusiveGuard`）の `Drop` が下ろす。
    HOLDS_EXCLUSIVE.set(true);
    ExclusiveGuard { _guard: guard }
}

/// `#[bengara::test]` が呼ぶ入口。利用者が直接呼ぶことはありません。
#[doc(hidden)]
pub fn run<F, Fut>(hooks: Hooks, factory: fn() -> Application, body: F)
where
    F: FnOnce(TestClient) -> Fut,
    Fut: Future<Output = ()>,
{
    kernel_impl::boot_for_tests(hooks);
    // 1 スレッドのランタイムにします。テスト 1 件ごとに作るので、
    // 複数スレッドだとコアの数だけスレッドを生成して捨てることになります
    // （16 コアで 100 件なら 1,600 本前後）。
    // テスト本体は呼び出し元のスレッドの `block_on` が回すので、
    // `HOLDS_DATABASE` などのスレッドローカルの札はそのまま働きます。
    // `tokio::spawn` も `spawn_blocking` も 1 スレッドのランタイムで動きます。
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("テスト用のランタイムを作れません");
    let client = TestClient::new(factory());
    runtime.block_on(body(client));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 期限切れのクッキーを見分ける() {
        let now = 1_800_000_000;
        // Max-Age は 0 以下なら消す。以前は `Max-Age=0` ちょうどだけだった。
        assert!(cookie_expired("a=; Max-Age=0", now));
        assert!(cookie_expired("a=; Max-Age=-1", now));
        assert!(cookie_expired("a=; path=/; max-age = -100", now));
        assert!(!cookie_expired("a=1; Max-Age=60", now));
        assert!(!cookie_expired("a=1; Path=/; HttpOnly", now));

        // 過去の Expires も消す。フレームワークが出す 1970 年の日付を読む。
        assert!(cookie_expired(
            "a=; Path=/; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
            now
        ));
        assert!(!cookie_expired(
            "a=1; Expires=Fri, 01 Jan 9999 00:00:00 GMT",
            now
        ));
        // Max-Age があれば Expires より優先する。
        assert!(!cookie_expired(
            "a=1; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Max-Age=60",
            now
        ));
        // 読めない値は無いものとして扱う。
        assert!(!cookie_expired("a=1; Max-Age=abc; Expires=someday", now));
    }

    #[test]
    fn http_dateを読む() {
        assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(
            parse_http_date(" Fri, 02 Jan 1970 00:00:01 GMT"),
            Some(86_401)
        );
        assert_eq!(parse_http_date("Thu, 01 Foo 1970 00:00:00 GMT"), None);
        assert_eq!(parse_http_date(""), None);
    }

    thread_local! {
        /// テスト用の印。本物の錠には触らない。
        static DUMMY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    #[test]
    fn 札を二重に取ると分かる文でパニックする() {
        // 旗が下りている間は何度でも通る（旗を立てるのは呼び元の役目）。
        check_reentrant(&DUMMY, "refresh_database()");
        DUMMY.set(true);

        // 旗が立っていると、黙って止まるのではなくパニックする。
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            check_reentrant(&DUMMY, "refresh_database()")
        }));
        let payload = caught.expect_err("パニックする");
        let message = payload
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        assert!(message.contains("refresh_database()"), "{message}");
        assert!(message.contains("2 回"), "{message}");

        DUMMY.set(false);
        // 手放したあとはまた取れる。
        check_reentrant(&DUMMY, "refresh_database()");
    }

    #[test]
    fn つなげないときの文は機能フラグを足す形で案内する() {
        // 以前は `features = ["sqlite"]` と 1 行まるごと見せていた。そのまま写すと
        // 他のフラグが落ち、MySQL や PostgreSQL を使う人には見当違いだった。
        let message = connect_failure(&"原因");
        assert!(message.contains("原因"), "{message}");
        assert!(message.contains("足してください"), "{message}");
        assert!(!message.contains("features = ["), "{message}");
        for flag in ["\"sqlite\"", "\"mysql\"", "\"postgres\""] {
            assert!(message.contains(flag), "{message}");
        }
    }

    #[test]
    fn 旗を立てるのは呼び元なので失敗しても残らない() {
        // `check_reentrant` 自体は旗に触らない。錠やマイグレーションが途中で
        // 失敗しても、旗が立ったまま残らないことを確かめる。
        check_reentrant(&DUMMY, "refresh_database()");
        assert!(!DUMMY.get());
    }

    /// メソッドと content-type と本文をそのまま返すだけのアプリ。
    fn echo_app() -> Application {
        use crate::http::Route;

        async fn echo(req: Request) -> crate::Result<Response> {
            Ok(Response::text(format!(
                "{} {} {}",
                req.method(),
                req.header("content-type").unwrap_or("-"),
                req.body_text()
            )))
        }

        Application::configure()
            .with_routing(|r| {
                r.web(|| {
                    Route::put("/echo", echo);
                    Route::patch("/echo", echo);
                    Route::delete("/echo", echo);
                })
            })
            .create()
    }

    #[tokio::test]
    async fn putとpatchはフォームの本文で送る() {
        let client = TestClient::new(echo_app());

        client
            .put("/echo", "title=x")
            .await
            .assert_ok()
            .assert_see("PUT")
            .assert_see(FORM_CONTENT_TYPE)
            .assert_see("title=x");

        client
            .patch("/echo", "title=y")
            .await
            .assert_ok()
            .assert_see("PATCH")
            .assert_see(FORM_CONTENT_TYPE)
            .assert_see("title=y");
    }

    #[tokio::test]
    async fn deleteは本文もcontent_typeも付けない() {
        let client = TestClient::new(echo_app());
        let body = client.delete("/echo").await.assert_ok().body();
        assert_eq!(body, "DELETE - ");
    }

    /// JSON と、JSON でない本文を返すアプリ。
    fn json_app() -> Application {
        use crate::http::Route;

        async fn good(_req: Request) -> crate::Result<Response> {
            Response::json(&serde_json::json!({ "id": 7, "none": null }))
        }
        async fn html(_req: Request) -> crate::Result<Response> {
            Ok(Response::html("<h1>エラーの画面</h1>").with_status(500))
        }
        async fn empty(_req: Request) -> crate::Result<Response> {
            Ok(Response::text(""))
        }

        Application::configure()
            .with_routing(|r| {
                r.web(|| {
                    Route::get("/good", good);
                    Route::get("/html", html);
                    Route::get("/empty", empty);
                })
            })
            .create()
    }

    #[tokio::test]
    async fn jsonは読める本文をそのまま返す() {
        let client = TestClient::new(json_app());
        let json = client.get("/good").await.assert_ok().json();
        assert_eq!(json["id"], 7);
        // 本文に書かれた `null` と、無い鍵はどちらも `Null`（今までどおり）。
        assert!(json["none"].is_null());
        assert!(json["missing"].is_null());
    }

    #[tokio::test]
    async fn jsonは読めない本文でパニックする() {
        let client = TestClient::new(json_app());

        // 以前は黙って `Null` を返し、`res.json()["x"]` が `Null` になるだけだった。
        for path in ["/html", "/empty"] {
            let res = client.get(path).await;
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| res.json()));
            let payload = caught.expect_err("読めない本文はパニックする");
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_default();
            assert!(message.contains(path), "URI が出る: {message}");
            assert!(message.contains("JSON として読めません"), "{message}");
        }

        // ステータスと本文の先頭も出る。
        let res = client.get("/html").await;
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| res.json()));
        let message = caught
            .expect_err("パニックする")
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        assert!(message.contains("500"), "{message}");
        assert!(message.contains("エラーの画面"), "{message}");
    }

    #[test]
    fn トークンの口がjsonでなくても案内の文で知らせる() {
        // `json()` がパニックするようになっても、`csrf_token` は
        // 「どのルートを指定すればよいか」の文で知らせる。
        // 非同期の中のパニックを捕まえるので、ランタイムを自分で持つ。
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.block_on(async {
                let client = TestClient::new(json_app());
                client.csrf_token("/html").await
            })
        }));
        let payload = caught.expect_err("パニックする");
        let message = payload
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        assert!(
            message.contains("CSRF トークンを返しませんでした"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn 汎用の札は手放せばまた取れる() {
        {
            let guard = exclusive().await;
            assert_eq!(format!("{guard:?}"), "ExclusiveGuard");
        }
        // 手放したので、同じテストの中でも取り直せる。
        let _again = exclusive().await;
    }
}
