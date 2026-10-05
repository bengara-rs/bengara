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
- パスワードの変換の回数を 1,000 に下げる
- メールの送り先を `array` にする（ **本当には送りません**）

**ソケットは開きません。** ポートの衝突を気にせず並行して実行できます。

`use bengara::testing::*;` は書きません。必要なものは注入されます。

### 付けられる関数

`async fn` で、引数が無い関数にだけ付けられます。違う場合は分かりやすいコンパイルエラーになります。

## 使える道具

関数の中で、次のものがそのまま使えます。

| 名前                 | 中身                                      |
|----------------------|-------------------------------------------|
| `client`             | `TestClient`                              |
| `get(uri)`           | `client.get(uri)` の短縮                  |
| `post(uri, body)`    | `client.post(uri, body)` の短縮           |
| `put(uri, body)`     | PUT で送る。本文の形は `post` と同じ      |
| `patch(uri, body)`   | PATCH で送る。本文の形は `post` と同じ    |
| `delete(uri)`        | DELETE で送る。本文は無し                 |
| `refresh_database()` | テスト用の DB を作り直す（下の節）        |
| `seed_database(&db)` | シーダーを流す（下の節）                  |
| `exclusive()`        | DB 以外のプロセス共通のものを使うときの札 |

`put` / `patch` の本文は `application/x-www-form-urlencoded` で送られます。

### TestClient

| メソッド                                                                         |
|----------------------------------------------------------------------------------|
| `get(uri)`                                                                       |
| `post(uri, body)`                                                                |
| `post_json(uri, body)` — `body` は `&serde_json::Value`                          |
| `send(method, uri, body, headers)` — `body: Vec<u8>`、`headers: &[(&str, &str)]` |
| `post_with_csrf(uri, body, token_uri)`                                           |
| `csrf_token(token_uri)`                                                          |
| `fresh()` — Cookie を捨てた別のクライアント                                      |
| `cookie(name)` — 覚えている Cookie                                               |

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

| メソッド                                      | 中身                                            |
|-----------------------------------------------|-------------------------------------------------|
| `client.fresh()`                              | Cookie を捨てた、**別の人**としてのクライアント |
| `client.cookie(name)`                         | いま持っている Cookie の値                      |
| `client.csrf_token(uri)`                      | トークンを返すルートを叩いて `token` を取り出す |
| `client.post_with_csrf(uri, body, token_uri)` | トークンを取ってから POST する                  |

`send` に `cookie` ヘッダーを自分で指定したときは、そちらが優先されます。

### テストでディスクに書かないようにする

`.env` に `SESSION_DRIVER=memory` を書いておくと、セッションがファイルに残りません。

### TestResponse

| 確かめる                     | 読む           |
|------------------------------|----------------|
| `assert_ok()`                | `status()`     |
| `assert_status(code)`        | `body()`       |
| `assert_see(text)`           | `json()`       |
| `assert_dont_see(text)`      | `header(name)` |
| `assert_header(name, value)` |                |
| `assert_redirect(location)`  |                |

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

| すること | 内容                                                                            |
|----------|---------------------------------------------------------------------------------|
| つなぐ先 | `.env` の `DB_TEST_DATABASE`（既定 `:memory:`）。**開発用の DB には触りません** |
| 表       | 全部消してから `database/migrations/` を流し直します                            |
| 順番     | 札を持っている間、ほかの DB テストは待ちます                                    |

- **札は必ず受け取ってください**（`let _db = ...`）。`refresh_database().await;` だけだと
  その場で手放され、ほかのテストと混ざります。
- DB を使わないテストは待たされません。
- **1 つのテストで 2 回呼ばないでください。** 札は取り直せません。
  2 回呼ぶと、理由を書いたメッセージでパニックします。

### シーダーも流す

`seed_database(&db)` に、`refresh_database()` の戻りを渡します。

```rust
#[bengara::test]
async fn 一覧が見える() {
    let db = refresh_database().await;
    seed_database(&db).await;

    get("/api/articles").await.assert_ok();
}
```

札を引数で受け取るのは、札を取っていないテストから呼べないようにするためです。
`let _db` と書くと渡せないので、使うときは `let db` にします。

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

    // いまの client でログインする。Cookie を使う口なので CSRF を通す
    client
        .post_with_csrf(
            "/api/login",
            "email=alice@example.com&password=password123",
            "/csrf-token",
        )
        .await
        .assert_ok();

    // 別の人として送る
    let bob = client.fresh();
    bob.post_with_csrf("/api/my/articles/1", "title=x", "/csrf-token")
        .await
        .assert_status(403);
}
```

CSRF を確かめないルート（JSON の API など）なら、`post` / `put` / `patch` / `delete` を
そのまま使えます。

```rust
put("/api/posts/1", "title=x").await.assert_ok();
delete("/api/posts/1").await.assert_status(204);
```

パスワードの変換は時間がかかるので、 **テストでは回数が自動で 1,000 回に下がります**
（本番の既定は 120,000 回）。`.env` に書いた値は見ません。
変えたいときは `HASH_ITERATIONS=20000 cargo test` のように環境変数で渡します。

## プロセス共通のものを使うテスト

`tests/Feature/` は 1 つのバイナリで並行して走ります。
次の 2 つはプロセスで 1 つしかないので、触るテストは `exclusive()` で札を取ります。

| 触るもの                              | 理由                            |
|---------------------------------------|---------------------------------|
| `Mail::sent()` / `Mail::clear_sent()` | 溜まったメールがプロセスで 1 つ |
| `Lang`（`Lang::set` など）            | いまの言語がプロセスで 1 つ     |

```rust
#[bengara::test]
async fn メールを送る() {
    let _lock = exclusive().await;
    Mail::clear_sent();
    // ... 送る処理 ...
    assert_eq!(Mail::sent().len(), 1);
}
```

- 札は DB の札とは **別の錠**です。
- **両方要るときは `refresh_database()` → `exclusive()` の順**で取ってください。
  順番をそろえないと、取り合いで止まります。
- `exclusive()` も 1 つのテストで 2 回呼ぶとパニックします。

```rust
#[bengara::test]
async fn DBとメールの両方を使う() {
    let _db = refresh_database().await;
    let _lock = exclusive().await;
    Mail::clear_sent();
    // ...
}
```

## 回数の制限（throttle）

テストから送るリクエストの接続元は `127.0.0.1` です。
そのため **回数の制限は本番と同じように効きます。**
同じルートへ繰り返し送るテストでは、上限に当たって 429 が返ることを見込んでください。

## 周辺機能のテスト

### メール

送り先が `array` になっているので、`Mail::sent()` で中身を確かめます。 **溜まったメールはテストの間ずっと残ります。**
数を確かめる前に `exclusive()` の札を取り、`clear_sent()` を呼んでください（上の節）。

### キャッシュとファイル

**自動では消えません。** テストは並んで走るので、 **鍵とパスを重ねないでください。**

```rust
#[bengara::test]
async fn 数える() {
    Cache::forget("test.count").await.unwrap();      // 前の残りを消す
    assert_eq!(Cache::increment("test.count", 1).await.unwrap(), 1);
    Cache::forget("test.count").await.unwrap();      // 片付ける
}
```

`.env` に `CACHE_DRIVER=memory` と書くと、ファイルに残らなくなります。

### キュー

worker は動きません。 **入ったことだけを確かめます。**

```rust
#[bengara::test]
async fn ジョブが積まれる() {
    let _db = refresh_database().await;
    client.post("/api/register", "name=x&email=a@example.com&password=password123").await;
    assert_eq!(Queue::size().await.unwrap(), 1);
}
```

処理の中身は `handle` をそのまま呼んで確かめます。

```rust
crate::app::jobs::send_welcome::handle(r#"{"user_id":1}"#.to_string()).await.unwrap();
```

### 定期処理

`routes/console.rs` の関数を自分で呼ぶと、登録された内容を確かめられます。

```rust
let mut schedule = Schedule::new();
crate::routes::console::schedule( & mut schedule);
assert_eq!(schedule.len(), 3);
```

## ユニットテスト

`tests/Unit/` のファイルも同じように自動検出されます。
`#[bengara::test]` が要らない（アプリを組み立てる必要がない）場合は、普通の `#[test]` を書いてください。

## 関連

- [routing.md](routing.md)
- [requests-and-responses.md](requests-and-responses.md)
- [database.md](database.md) — 接続とクエリ
- [models.md](models.md) — モデルとシーダー
- [authentication.md](authentication.md) — ログイン
- [cache.md](cache.md) / [queue.md](queue.md) / [mail.md](mail.md) — 周辺機能
