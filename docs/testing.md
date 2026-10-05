# テスト

テストは `tests/Feature/` と `tests/Unit/` に置きます。`cargo test` で動きます。

```rust
// tests/Feature/HomeTest.rs
#[bengara::test]
async fn トップページが表示される() {
    get("/").await.assert_ok().assert_see("myapp");
}

#[bengara::test]
async fn 生存確認が応答する() {
    get("/up").await.assert_ok().assert_see("ok");
}

#[bengara::test]
async fn 無いページは404になる() {
    get("/no-such-page").await.assert_status(404);
}
```

## #[bengara::test] がすること

- `.env` を読み込む
- `config/` の設定を登録する
- `bootstrap/app.rs` の `app()` でアプリを組み立てる
- tokio ランタイムを用意する
- テスト用の道具を関数の中に入れる

**ソケットは開きません。** ポートの衝突を気にせず並行して実行できます。

`use bengara::testing::*;` は書きません。必要なものは注入されます。

### 付けられる関数

`async fn` で、引数が無い関数にだけ付けられます。違う場合は分かりやすいコンパイルエラーになります。

## 使える道具

関数の中で、次の 3 つがそのまま使えます。

| 名前 | 中身 |
|---|---|
| `client` | `TestClient` |
| `get(uri)` | `client.get(uri)` の短縮 |
| `post(uri, body)` | `client.post(uri, body)` の短縮 |

### TestClient

| メソッド |
|---|
| `get(uri)` |
| `post(uri, body)` |
| `post_json(uri, body)` — `body` は `&serde_json::Value` |
| `send(method, uri, body, headers)` — `body: Vec<u8>`、`headers: &[(&str, &str)]` |

```rust
#[bengara::test]
async fn JSONを受け取れる() {
    let res = client.post_json("/api/posts", &bengara::serde_json::json!({ "title": "hi" })).await;
    res.assert_status(201);
}
```

### TestResponse

| 確かめる | 読む |
|---|---|
| `assert_ok()` | `status()` |
| `assert_status(code)` | `body()` |
| `assert_see(text)` | `json()` |
| `assert_dont_see(text)` | `header(name)` |
| `assert_header(name, value)` | |
| `assert_redirect(location)` | |

`assert_*` は自分自身を返すので、つなげて書けます。

```rust
get("/").await
    .assert_ok()
    .assert_header("content-type", "text/html; charset=utf-8")
    .assert_see("myapp")
    .assert_dont_see("エラー");
```

## テスト名

テスト関数は日本語で書けます。実行結果には `__bengara_tests_feature_home_test::トップページが表示される` のような名前で出ます。

## ユニットテスト

`tests/Unit/` のファイルも同じように自動検出されます。
`#[bengara::test]` が要らない（アプリを組み立てる必要がない）場合は、普通の `#[test]` を書いてください。

## 関連

- [routing.md](routing.md)
- [requests-and-responses.md](requests-and-responses.md)
