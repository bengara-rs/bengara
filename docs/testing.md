# テスト

テストの書き方と、テストで使える道具をまとめたページです。

| 置き場所         | 入れるもの                     |
|------------------|--------------------------------|
| `tests/Feature/` | アプリにリクエストを送るテスト |
| `tests/Unit/`    | 関数を 1 つずつ確かめるテスト  |

- `cargo test` で動きます。
- `tests/` の直下には置きません。置くと、ビルド時に警告が出ます。

## まず書いてみる

```rust
// tests/Feature/HomeTest.rs
#[bengara::test]
async fn トップページが表示される() {
    get("/").await.assert_ok().assert_see("myapp");
}

#[bengara::test]
async fn 無いページは404になる() {
    get("/no-such-page").await.assert_status(404);
}
```

- `get` などの道具は `#[bengara::test]` が用意します。`use` は要りません。
- テスト関数の名前は日本語で書けます。
- `#[bengara::test]` を付けられるのは、**引数も戻り値も無い `async fn`** だけです。`-> Result<()>` などは書けないので、中で `unwrap()` します。
- アプリを組み立てる必要が無いユニットテストは、普通の `#[test]` で書きます。

### データベースを使う

先頭で `let _db = refresh_database().await;` と書きます。
返ってくる札（`_db`）を持っている間だけ、DB を使えます。

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

シーダー（最初のデータ）も流すときは、札を `seed_database()` に渡します。

```rust
#[bengara::test]
async fn 一覧が見える() {
    let db = refresh_database().await;
    seed_database(&db).await;

    get("/api/articles").await.assert_ok();
}
```

`refresh_database()` がすることは次のとおりです。

- `.env` の `DB_TEST_DATABASE` につなぎます。**開発用の DB には触りません。**
- 表を全部消してから、`database/migrations/` を流し直します。
- つなげないときや、マイグレーションが失敗したときはパニックします。

| DB の種類          | 用意すること                                                       |
|--------------------|--------------------------------------------------------------------|
| SQLite             | 何も要りません（既定が `:memory:`）                                |
| MySQL / PostgreSQL | テスト用のデータベースを別に作り、名前を `DB_TEST_DATABASE` に書く |

### ログインしてから送る

`client` は **Cookie を覚えます。** ログインしたあと、続けて送れます。
別の人として送るときは `client.fresh()` で作り直します。

```rust
#[bengara::test]
async fn ログインすると自分の情報が見える() {
    let _db = refresh_database().await;
    let mut user = User::register("アリス", "alice@example.com", "password123")
        .await
        .unwrap();
    user.save().await.unwrap();

    // Cookie でログインする口なので、CSRF のトークンを付けて送る
    client
        .post_with_csrf(
            "/api/login",
            "email=alice@example.com&password=password123",
            "/csrf-token",
        )
        .await
        .assert_ok();

    // Cookie が残っているので、続けて送れる
    client.get("/api/me").await.assert_ok();

    // 別の人として送ると 401
    client.fresh().get("/api/me").await.assert_status(401);
}
```

- `User::register` はアプリの側の関数です（[authentication.md](authentication.md)）。
- `post_with_csrf` の 3 つめは、トークンを返すルートです。`{"token": req.csrf_token()}` を返すルートを自分で用意します。
- CSRF（別のサイトからの成りすまし送信）を確かめないルートなら、`post` / `put` / `patch` / `delete` をそのまま使えます。

```rust
put("/api/articles/1", "title=x").await.assert_ok();
delete("/api/articles/1").await.assert_ok();
```

### ヘッダーを自分で付けて送る

`send` を使います。CSRF のトークンは `csrf_token()` で取って、ヘッダーに付けます。

```rust
#[bengara::test]
async fn セッションに入れた値が次のリクエストで読める() {
    let token = client.csrf_token("/csrf-token").await;
    client
        .send(
            "POST",
            "/remember",
            b"value=yakisoba".to_vec(),
            &[
                ("content-type", "application/x-www-form-urlencoded"),
                ("x-csrf-token", &token),
            ],
        )
        .await
        .assert_ok();

    // 同じ client なので、次のリクエストでも読める
    let res = client.get("/recall").await.assert_ok();
    assert_eq!(res.json()["remembered"], "yakisoba");
}
```

## 道具の一覧

### 送る

| 名前                                   | すること                                         |
|----------------------------------------|--------------------------------------------------|
| `get(uri)`                             | GET で送る（`get("/search?q=1")` も書ける）      |
| `post` / `put` / `patch(uri, body)`    | 本文をフォームの形で送る                         |
| `delete(uri)`                          | DELETE で送る                                    |
| `client`                               | テスト用のクライアント。上の関数も持っている     |
| `client.post_json(uri, &値)`           | JSON で POST する                                |
| `client.post_with_csrf(uri, body, token_uri)` | トークンを取ってから POST する            |
| `client.csrf_token(token_uri)`         | トークンを取り出す。応答に `token` が無いとパニック |
| `client.send(method, uri, body, headers)` | メソッドとヘッダーを自分で決めて送る          |
| `client.fresh()`                       | Cookie を捨てた、別の人としてのクライアント      |
| `client.cookie(name)`                  | いま持っている Cookie の値                       |

テストのクライアントは、本文の扱いもサーバーと同じ道を通ります（[requests-and-responses.md](requests-and-responses.md)）。

- 本文が上限（`APP_MAX_BODY_BYTES` かルートの `.max_body`）を超えれば 413 です。
  長さの申告（`Content-Length`）は付けませんが、読んだ合計で 413 になります。
- `.stream_body()` のルートも、そのまま試せます。本文は 1 かたまりで届きます。

```rust
#[bengara::test]
async fn JSONを受け取れる() {
    let res = client
        .post_json("/api/posts", &bengara::serde_json::json!({ "title": "hi" }))
        .await;
    res.assert_status(201);
}
```

### 確かめる

| 確かめる                     | 読む           |
|------------------------------|----------------|
| `assert_ok()`（200）         | `status()`     |
| `assert_status(code)`        | `body()`       |
| `assert_see(text)`           | `json()`       |
| `assert_dont_see(text)`      | `header(name)` |
| `assert_header(name, value)` |                |
| `assert_redirect(location)`  |                |

```rust
get("/").await
    .assert_ok()
    .assert_header("content-type", "text/html; charset=utf-8")
    .assert_see("myapp")
    .assert_dont_see("エラー");
```

- `assert_*` はつなげて書けます。
- 失敗すると、URI と本文の先頭（400 文字まで）を出します。
- **`json()` は、本文が JSON でないとパニックします。** HTML のエラー画面が返ったことに気づけます。

### `#[bengara::test]` が用意するもの

- `.env` と `config/` を読みます（プロセスで 1 回）。
- `bootstrap/app.rs` の `app()` でアプリを組み立てます（テスト 1 件ごと）。
- メールの送り先を `array` にします。**本当には送りません。**
- パスワードの変換の回数を 1,000 に下げます（本番の既定は 120,000）。変えるなら `HASH_ITERATIONS=20000 cargo test` のように渡します。
- `APP_KEY` が無ければテスト用の鍵を、`APP_ENV` が無ければ `testing` を入れます。
- ソケットは開きません。ポートの衝突を気にせず並べて走らせられます。
- 回数の制限（throttle）は本番と同じく効きます。接続元は `127.0.0.1` として数えます。

## テストどうしがぶつからないために

`tests/Feature/` のテストは、1 つのプロセスの中で並んで走ります。
共有しているものを触るテストは、札を取って順番に走らせます。

| 触るもの                                                 | 取る札               |
|----------------------------------------------------------|----------------------|
| データベース                                             | `refresh_database()` |
| 溜まったメール（`Mail::sent()` / `Mail::clear_sent()`）  | `exclusive()`        |
| いまの言語を変える（`Lang::set`）                        | `exclusive()`        |
| イベントの登録を変えたアプリで `Event::dispatch` を呼ぶ  | `exclusive()`        |
| 配信の記録（`Broadcast::fake()` / `published()`）        | `exclusive()`        |

- 札は変数で受け取ります（`let _db = ...`）。受け取らないと、その場で手放されます。
- 1 つのテストで同じ札を 2 回取ると、パニックします。
- 両方要るときは `refresh_database()` → `exclusive()` の順です。逆だと止まります。

```rust
#[bengara::test]
async fn DBとメールの両方を使う() {
    let _db = refresh_database().await;
    let _lock = exclusive().await;
    Mail::clear_sent();
    // ...
}
```

### キャッシュとファイルは自動で消えない

**鍵とパスを、テストごとに変えてください。** 同じ名前だと、並んで走るテストが互いに踏みます。

```rust
#[bengara::test]
async fn 数を足し引きできる() {
    Cache::forget("test.count").await.unwrap();      // 前の残りを消す
    assert_eq!(Cache::increment("test.count", 1).await.unwrap(), 1);
    Cache::forget("test.count").await.unwrap();      // 片付ける
}
```

`.env` に `SESSION_DRIVER=memory` と `CACHE_DRIVER=memory` を書くと、テストがディスクにファイルを残しません。

## 周辺機能のテスト

| 機能     | 確かめ方                                     | くわしい例                        |
|----------|----------------------------------------------|-----------------------------------|
| メール   | `Mail::sent()` に溜まった中身を見る          | [mail.md](mail.md) の「テスト」   |
| キュー   | 積まれた数を見る。中身は `handle` を直接呼ぶ | [queue.md](queue.md) の「テスト」 |
| 定期処理 | `routes/console.rs` の関数を自分で呼ぶ       | 下の例                            |
| 配信     | `Broadcast::fake()` で記録して見る           | 下の例                            |
| WebSocket の受け口 | 切り替えのヘッダーを付けて送り、101 を見る | [websocket.md](websocket.md) の「テスト」 |

```rust
#[bengara::test]
async fn 定期処理が登録されている() {
    let mut schedule = Schedule::new();
    crate::routes::console::schedule(&mut schedule);

    assert_eq!(schedule.len(), 3);
    let names: Vec<&str> = schedule.tasks().iter().map(|t| t.name()).collect();
    assert!(names.contains(&"記事数の集計"));
}
```

### 配信を確かめる

`Broadcast::fake()` の後の `publish` は、送らずに記録だけします。
記録はプロセスで 1 つなので、`exclusive()` の札を取ってから呼びます。

```rust
use bengara::broadcast::Broadcast;

#[bengara::test]
async fn 投稿すると配信される() {
    let _lock = exclusive().await;
    Broadcast::fake();

    post("/api/posts", "title=hi").await.assert_status(201);

    Broadcast::assert_published("posts", "hi");
}
```

| 書き方                                        | すること                           |
|-----------------------------------------------|------------------------------------|
| `Broadcast::published(名前)`                  | そのチャンネルへの中身（送った順） |
| `Broadcast::assert_published(名前, 含む文字)` | 含む中身が無ければパニック         |
| `Broadcast::assert_nothing_published()`       | 何か記録があればパニック           |

- **札を手放すと fake は終わり、記録も空になります。** 札を取らずに `fake()` を呼ばないでください。
- `Broadcast` は `prelude` に入っていません。`use` で取り込みます。

### WebSocket の受け口を確かめる

`client.send` で切り替えのヘッダー（`upgrade`・`connection`・`sec-websocket-version`・`sec-websocket-key`）を付けて送ります。

- 認証などを通れば、101 が返ります。**ソケットが無いので、`on_upgrade` のクロージャは動きません。**
- 例は [websocket.md](websocket.md#テスト) にあります。

## 関連

- [routing.md](routing.md)
- [requests-and-responses.md](requests-and-responses.md)
- [database.md](database.md) — 接続とクエリ
- [models.md](models.md) — モデルとシーダー
- [authentication.md](authentication.md) — ログイン
- [cache.md](cache.md) / [queue.md](queue.md) / [mail.md](mail.md) — 周辺機能
- [websocket.md](websocket.md) — WebSocket と配信
