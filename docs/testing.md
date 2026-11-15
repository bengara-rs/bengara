# テスト

テストの書き方と、テストの中で使える道具をまとめたページです。
並んで走るテストどうしがぶつからないための決まりも、ここにあります。

| 置き場所         | 入れるもの                       |
|------------------|----------------------------------|
| `tests/Feature/` | アプリにリクエストを送るテスト   |
| `tests/Unit/`    | 関数を 1 つずつ確かめるテスト    |

- `cargo test` で動きます。
- **`tests/` の直下には置きません。** 置くと、`tests/Feature/` へ案内する警告がビルド時に出ます。

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

- `use bengara::testing::*;` は要りません。`get` などの道具は `#[bengara::test]` が用意します。
- テスト関数の名前は日本語で書けます。
  結果には `__bengara_tests_feature_home_test::トップページが表示される` の形で出ます。

### テスト関数に書けないもの

`#[bengara::test]` を付けられるのは、**引数も戻り値も無い `async fn`** だけです。
違う形はコンパイルエラーになります。

| 書けないもの                           | 代わりにすること                      |
|----------------------------------------|---------------------------------------|
| 引数                                   | 関数の中で `client` などを使う        |
| 戻り値（`-> Result<()>` など）         | 中で `unwrap()` か `assert!` を使う   |
| 型引数・ライフタイム・`where`          | 中で型を決める                        |
| `unsafe`                               | 関数の中で `unsafe { .. }` で囲む     |
| `const` / `extern ".."`                | 書けない                              |
| マクロの引数（`#[bengara::test(..)]`） | 引数は取らない                        |

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

シーダー（初期データ）も流すときは、札を `seed_database()` に渡します。
渡すので、変数の名前は `db` にします。

```rust
#[bengara::test]
async fn 一覧が見える() {
    let db = refresh_database().await;
    seed_database(&db).await;

    get("/api/articles").await.assert_ok();
}
```

| DB の種類          | 用意すること                                                       |
|--------------------|--------------------------------------------------------------------|
| SQLite             | 何も要りません。`DB_TEST_DATABASE` の既定が `:memory:` です        |
| MySQL / PostgreSQL | テスト用のデータベースを別に作り、名前を `DB_TEST_DATABASE` に書く |

くわしくは [database.md](database.md) の「ドライバの違い」にあります。

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
- `post_with_csrf` の 3 つめは、トークンを返すルートです。
  `{"token": req.csrf_token()}` を返すルートを自分で用意します。
- ログアウトの応答は Cookie を消します。`client` もその Cookie を捨てます。

CSRF を確かめないルートなら、`post` / `put` / `patch` / `delete` をそのまま使えます。

```rust
put("/api/articles/1", "title=x").await.assert_ok();
delete("/api/articles/1").await.assert_ok();
```

### ヘッダーを自分で付けて送る

`send` を使います。CSRF のトークンは `csrf_token()` で取り、ヘッダーに付けます。

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

`"POST"` を `"PUT"` や `"DELETE"` に替えれば、CSRF を確かめるルートへ POST 以外でも送れます。

### ユニットテスト

`tests/Unit/` のファイルも自動で見つけます。
アプリを組み立てる必要が無ければ、`#[bengara::test]` ではなく普通の `#[test]` を書きます。

## 道具の一覧

### 関数の中で使えるもの

| 名前                 | すること                                           |
|----------------------|----------------------------------------------------|
| `client`             | テスト用のクライアント（下の `TestClient`）        |
| `get(uri)`           | GET で送る                                         |
| `post(uri, body)`    | POST で送る                                        |
| `put(uri, body)`     | PUT で送る                                         |
| `patch(uri, body)`   | PATCH で送る                                       |
| `delete(uri)`        | DELETE で送る。本文は付けない                      |
| `refresh_database()` | テスト用の DB を作り直して、札を返す               |
| `seed_database(&db)` | シーダーを流す                                     |
| `exclusive()`        | DB 以外の「プロセスに 1 つのもの」を使うときの札   |

- `post` / `put` / `patch` の本文は、フォームの形（`application/x-www-form-urlencoded`）で送ります。
- クエリは `get("/search?q=1")` のように書けます。

### TestClient

`client` の型です。上の短い関数は、どれも `client` の同じ名前のメソッドを呼びます。

| メソッド                               | すること                                             |
|----------------------------------------|------------------------------------------------------|
| `get` / `post` / `put` / `patch` / `delete` | 上の表と同じ                                    |
| `post_json(uri, &値)`                  | JSON で POST する。値は `serde_json::Value`          |
| `post_with_csrf(uri, body, token_uri)` | トークンを取ってから POST する                       |
| `csrf_token(token_uri)`                | トークンを返すルートを叩いて、トークンを取り出す     |
| `send(method, uri, body, headers)`     | メソッドとヘッダーを自分で決めて送る                 |
| `fresh()`                              | Cookie を捨てた、**別の人**としてのクライアントを作る |
| `cookie(name)`                         | いま持っている Cookie の値（`Option<String>`）       |

- `send` の本文は `Vec<u8>`、ヘッダーは `&[(&str, &str)]` です。
- `send` に `cookie` ヘッダーを自分で書いたときは、そちらを使います。
- **`csrf_token` は、応答に `token` が無いとパニックします。** 指定したルートが違うことに、その場で気づけます。

```rust
#[bengara::test]
async fn JSONを受け取れる() {
    let res = client
        .post_json("/api/posts", &bengara::serde_json::json!({ "title": "hi" }))
        .await;
    res.assert_status(201);
}
```

### TestResponse

送った結果です。

| 確かめる                     | 読む           |
|------------------------------|----------------|
| `assert_ok()`（200）         | `status()`     |
| `assert_status(code)`        | `body()`       |
| `assert_see(text)`           | `json()`       |
| `assert_dont_see(text)`      | `header(name)` |
| `assert_header(name, value)` |                |
| `assert_redirect(location)`  |                |

- `assert_*` は自分自身を返すので、つなげて書けます。
- ステータスや本文の確かめが失敗したときは、URI と本文の先頭（400 文字まで）を出します。
- **`json()` は、本文が JSON として読めないとパニックします。**
  HTML のエラー画面が返っているのに気づけるようにするためです。JSON でない本文は `body()` で見ます。

```rust
get("/").await
    .assert_ok()
    .assert_header("content-type", "text/html; charset=utf-8")
    .assert_see("myapp")
    .assert_dont_see("エラー");
```

### #[bengara::test] が用意するもの

| すること                                   | 補足                                        |
|--------------------------------------------|---------------------------------------------|
| `.env` を読み込む                          | プロセスで 1 回だけ                         |
| `config/` の設定を登録する                 | プロセスで 1 回だけ                         |
| `bootstrap/app.rs` の `app()` で組み立てる | テスト 1 件ごと                             |
| 非同期の実行環境（tokio）を用意する        | テスト 1 件ごと。**スレッドは 1 本**        |
| メールの送り先を `array` にする            | **本当には送りません**                      |
| パスワードの変換の回数を 1,000 に下げる    | 本番の既定は 120,000                        |
| `APP_KEY` が無ければテスト用の鍵を入れる   | `.env` が無くてもテストが通る               |
| `APP_ENV` が無ければ `testing` にする      | `.env` の有無で結果が変わらない             |

- `APP_KEY` と `APP_ENV` は、環境変数にも `.env` にも無いときだけ入れます。
  いまの環境は `config::<AppConfig>().is_testing()` で見分けられます。
- **ソケットは開きません。** ポートの衝突を気にせず、並べて実行できます。
- `tokio::spawn` と `spawn_blocking` は、そのまま動きます。
- `route()` が見るルート名の表は、テストで組み立てたアプリのものです。

## テストどうしがぶつからないために

`tests/Feature/` のテストは、1 つのプロセスの中で並んで走ります。
共有しているものを触るテストは、札を取って順番に走らせます。

| 触るもの                                           | 取る札               |
|----------------------------------------------------|----------------------|
| データベース                                       | `refresh_database()` |
| 溜まったメール（`Mail::sent()` / `Mail::clear_sent()`） | `exclusive()`   |
| いまの言語を変える（`Lang::set`）                  | `exclusive()`        |
| イベントの登録を変えたアプリで `Event::dispatch` を呼ぶ | `exclusive()`   |

3 つめと 4 つめは、[localization.md](localization.md) と [events.md](events.md) に理由があります。

### 札の決まり

| 決まり                                               | 守らないと                               |
|------------------------------------------------------|------------------------------------------|
| 札は変数で受け取る（`let _db = ...`）                | その場で手放され、ほかのテストと混ざる   |
| 1 つのテストで同じ札を 2 回取らない                  | パニックする                             |
| 両方要るときは `refresh_database()` → `exclusive()` の順 | 取り合いで止まる                     |

```rust
#[bengara::test]
async fn DBとメールの両方を使う() {
    let _db = refresh_database().await;
    let _lock = exclusive().await;
    Mail::clear_sent();
    // ...
}
```

札を取らないテストは、待たされません。

### refresh_database() がすること

| こと     | 内容                                                                            |
|----------|---------------------------------------------------------------------------------|
| つなぐ先 | `.env` の `DB_TEST_DATABASE`（既定 `:memory:`）。**開発用の DB には触りません** |
| 表       | 全部消してから `database/migrations/` を流し直す                                |
| 失敗     | つなげない・マイグレーションが失敗した、のどちらもパニックする                  |

### キャッシュとファイルは自動で消えない

**鍵とパスを、テストごとに変えてください。** 同じ名前を使うと、並んで走るテストが互いに踏みます。

```rust
#[bengara::test]
async fn 数を足し引きできる() {
    Cache::forget("test.count").await.unwrap();      // 前の残りを消す
    assert_eq!(Cache::increment("test.count", 1).await.unwrap(), 1);
    Cache::forget("test.count").await.unwrap();      // 片付ける
}
```

`.env` に次を書くと、テストがディスクにファイルを残しません。

| 設定                    | 効く先     |
|-------------------------|------------|
| `SESSION_DRIVER=memory` | セッション |
| `CACHE_DRIVER=memory`   | キャッシュ |

### 本番と違う点・同じ点

| こと                 | テストでの動き                                                         |
|----------------------|------------------------------------------------------------------------|
| パスワードの変換     | 1,000 回に下がる。`.env` の値は見ない                                  |
| 回数の制限（throttle） | **本番と同じように効く。** 接続元は `127.0.0.1` として数える         |

- 変換の回数を変えるときは、`HASH_ITERATIONS=20000 cargo test` のように環境変数で渡します。
- 同じルートへ繰り返し送るテストは、上限に当たって 429 が返ることを見込んでください。

## 周辺機能のテスト

| 機能     | 確かめ方                                         | くわしい例                         |
|----------|--------------------------------------------------|------------------------------------|
| メール   | `Mail::sent()` に溜まった中身を見る              | [mail.md](mail.md) の「テスト」    |
| キュー   | 積まれた数を見る。中身は `handle` を直接呼ぶ     | [queue.md](queue.md) の「テスト」  |
| 定期処理 | `routes/console.rs` の関数を自分で呼ぶ           | 下の例                             |

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

## 関連

- [routing.md](routing.md)
- [requests-and-responses.md](requests-and-responses.md)
- [database.md](database.md) — 接続とクエリ
- [models.md](models.md) — モデルとシーダー
- [authentication.md](authentication.md) — ログイン
- [cache.md](cache.md) / [queue.md](queue.md) / [mail.md](mail.md) — 周辺機能
