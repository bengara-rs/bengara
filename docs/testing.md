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
| `post_with_csrf(uri, body, token_uri)` |
| `csrf_token(token_uri)` |
| `fresh()` — Cookie を捨てた別のクライアント |
| `cookie(name)` — 覚えている Cookie |

```rust
#[bengara::test]
async fn JSONを受け取れる() {
    let res = client.post_json("/api/posts", &bengara::serde_json::json!({ "title": "hi" })).await;
    res.assert_status(201);
}
```

## セッションと CSRF を使うテスト

`client` は **Cookie を覚えます。** 続けて送るとセッションがつながります。

```rust
#[bengara::test]
async fn セッションがつながる() {
    let token = client.csrf_token("/csrf-token").await;
    client
        .send("POST", "/remember", b"value=x".to_vec(),
              &[("content-type", "application/x-www-form-urlencoded"),
                ("x-csrf-token", &token)])
        .await
        .assert_ok();

    // 次のリクエストでも読める
    client.get("/recall").await.assert_ok().assert_see("x");
}
```

| メソッド | 中身 |
|---|---|
| `client.fresh()` | Cookie を捨てた、**別の人**としてのクライアント |
| `client.cookie(name)` | いま持っている Cookie の値 |
| `client.csrf_token(uri)` | トークンを返すルートを叩いて `token` を取り出す |
| `client.post_with_csrf(uri, body, token_uri)` | トークンを取ってから POST する |

`send` に `cookie` ヘッダーを自分で指定したときは、そちらが優先されます。

### テストでディスクに書かないようにする

`.env` に `SESSION_DRIVER=memory` を書いておくと、セッションがファイルに残りません。

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

## データベースを使うテスト

先頭で `let _db = refresh_database().await;` と書きます。

```rust
#[bengara::test]
async fn 記事を保存して読み出せる() {
    let _db = refresh_database().await;

    let id = DB::table("posts")
        .insert_get_id(&[("title", "やきそば".into())])
        .await
        .unwrap();

    let post = DB::table("posts").find(id).await.unwrap().unwrap();
    assert_eq!(post.get::<String>("title").unwrap(), "やきそば");
}
```

これが返す札（`_db`）を持っている間だけ DB を使えます。

| すること | 内容 |
|---|---|
| つなぐ先 | `.env` の `DB_TEST_DATABASE`（既定 `:memory:`）。**開発用の DB には触りません** |
| 表 | 全部消してから `database/migrations/` を流し直します |
| 順番 | 札を持っている間、ほかの DB テストは待ちます |

- **札は必ず受け取ってください**（`let _db = ...`）。`refresh_database().await;` だけだと
  その場で手放され、ほかのテストと混ざります。
- DB を使わないテストは待たされません。
- シーダーも流したいときは `seed_database().await;` を続けて呼びます。

```rust
#[bengara::test]
async fn 一覧が見える() {
    let _db = refresh_database().await;
    seed_database().await;

    get("/api/articles").await.assert_ok();
}
```

テストを流すときは、機能フラグを忘れないでください。

```sh
cargo test
```

`Cargo.toml` に `features = ["sqlite"]` が書かれていれば、そのまま動きます。

## ログインが要るテスト

`client` は Cookie を覚えるので、ログインしてから続けて送れます。
別の人として送りたいときは `client.fresh()` で作り直します。

```rust
#[bengara::test]
async fn 他人の記事は直せない() {
    let _db = refresh_database().await;

    let alice = client.fresh();
    alice
        .send("POST", "/api/login", b"email=alice@example.com&password=password123".to_vec(), &[FORM])
        .await
        .assert_ok();

    let bob = client.fresh();   // 別の人
    bob.send("PUT", "/api/my/articles/1", b"title=x".to_vec(), &[FORM])
        .await
        .assert_status(403);
}
```

パスワードの変換は時間がかかるので、**テストでは回数が自動で 1,000 回に下がります**
（本番の既定は 120,000 回）。`.env` に書いた値は見ません。
変えたいときは `HASH_ITERATIONS=20000 cargo test` のように環境変数で渡します。

## ユニットテスト

`tests/Unit/` のファイルも同じように自動検出されます。
`#[bengara::test]` が要らない（アプリを組み立てる必要がない）場合は、普通の `#[test]` を書いてください。

## 関連

- [routing.md](routing.md)
- [requests-and-responses.md](requests-and-responses.md)
- [database.md](database.md) — 接続とクエリ
- [models.md](models.md) — モデルとシーダー
- [authentication.md](authentication.md) — ログイン
