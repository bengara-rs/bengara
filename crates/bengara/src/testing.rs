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

use std::future::Future;

use crate::application::Application;
use crate::http::{Request, Response};
use crate::{kernel_impl, Hooks};

/// テスト用のクライアント。
#[derive(Clone)]
pub struct TestClient {
    app: Application,
}

impl TestClient {
    /// 組み立て済みのアプリからクライアントを作る。
    pub fn new(app: Application) -> Self {
        app.install_names();
        Self { app }
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

    /// メソッド・ヘッダーまで自分で決めて送る。
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
        let headers = headers
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
            .collect();
        let req = Request::new(method, path)
            .with_query(query)
            .with_headers(headers)
            .with_body(body);
        TestResponse {
            inner: self.app.handle(req).await,
            uri: uri.to_string(),
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
