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
        self.send(
            "POST",
            uri,
            body.as_bytes().to_vec(),
            &[("content-type", "application/x-www-form-urlencoded")],
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
                ("content-type", "application/x-www-form-urlencoded"),
                ("x-csrf-token", &token),
            ],
        )
        .await
    }

    /// トークンを返すルートを叩いて、`token` の値を取り出す。
    pub async fn csrf_token(&self, token_uri: &str) -> String {
        let res = self.get(token_uri).await;
        res.json()["token"].as_str().unwrap_or_default().to_string()
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

        let req = Request::new(method, path)
            .with_query(query)
            .with_headers(headers)
            .with_body(body);
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

    /// `Set-Cookie` を覚える。`Max-Age=0` のものは捨てます。
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
            let removed = value
                .split(';')
                .any(|p| p.trim().eq_ignore_ascii_case("Max-Age=0"));
            if removed {
                cookies.remove(key.trim());
            } else {
                cookies.insert(key.trim().to_string(), val.trim().to_string());
            }
        }
    }
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
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(self.inner.body()).unwrap_or(serde_json::Value::Null)
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

/// `#[bengara::test]` が呼ぶ入口。利用者が直接呼ぶことはありません。
#[doc(hidden)]
pub fn run<F, Fut>(hooks: Hooks, factory: fn() -> Application, body: F)
where
    F: FnOnce(TestClient) -> Fut,
    Fut: Future<Output = ()>,
{
    kernel_impl::boot_for_tests(hooks);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("テスト用のランタイムを作れません");
    let client = TestClient::new(factory());
    runtime.block_on(body(client));
}
