# ルーティング

どの URL でどの処理（ハンドラ）を呼ぶかを決めます。

## ルートを書く

ルートは `routes/web.rs` に書きます。

```rust
use crate::app::http::controllers::HomeController;
use bengara::prelude::*;

pub fn routes() {
    Route::get("/", HomeController::index).name("home");
}
```

この関数を `bootstrap/app.rs` で取り込みます。

```rust
pub fn app() -> Application {
    Application::configure()
        .with_routing(|r| r.web(crate::routes::web::routes).health("/up"))
        .create()
}
```

- `r.web(関数)` でルートを取り込みます。
- `r.health(パス)` は生存確認のルートを足します。`{"status":"ok","name":"アプリ名"}` を返します。
- **`Route::*` は `r.web(...)` に渡した関数の中でだけ効きます。** 外で呼ぶと、エラーログを出して無視します。

## メソッド

```rust
Route::get("/posts", PostController::index);
Route::post("/posts", PostController::store);
Route::put("/posts/{post}", PostController::update);
Route::patch("/posts/{post}", PostController::patch);
Route::delete("/posts/{post}", PostController::destroy);
```

- 書けるのはこの 5 つです。
- `GET` のルートは `HEAD` にも答えます（本文は空）。

ハンドラの形は 2 つです。

```rust
pub async fn index() -> Result<Response>
pub async fn show(req: Request) -> Result<Response>
```

## パス引数

URL の一部を値として受け取ります。

| 書き方           | 意味                                   |
|------------------|----------------------------------------|
| `/posts/{post}`  | 1 区間を `post` という名前で取る       |
| `/files/{*path}` | 末尾までをまとめて `path` で取る       |
| `/file-{name}`   | `/file-abc` に当たる（`file-` は固定） |

```rust
pub async fn show(req: Request) -> Result<Response> {
    let id: u32 = req.param_as("post")?;
    text(format!("post {id}"))
}
```

- 読み方は [requests-and-responses.md](requests-and-responses.md) にあります。
- 日本語のパスも書けます（`/記事-{id}` は `/記事-12` に当たります）。

### 書けない形

次の形は、起動時に理由を出して止まります。

| 書き方                           | 理由                                       |
|----------------------------------|--------------------------------------------|
| `/posts/{id}.json`               | パス引数の後ろに固定の文字は書けない       |
| `/{year}-{month}`                | 1 区間にパス引数は 1 つだけ                |
| `/files/{*path}/edit`            | `{*名前}` は末尾にだけ書ける               |
| `/posts/{}`                      | 名前が空。`{` `}` が対になっていないときも |
| `/posts/{id}` と `/posts/{post}` | 名前だけ違うパスは同じパスとみなす         |

- 拡張子で分けたいときは、`/posts/{id}` で受けて、ハンドラで値の末尾を見ます。
- `{` そのものはパスに書けません。

## 名前と URL

ルートに名前を付けると、名前から URL を作れます。

```rust
Route::get("/posts/{post}", PostController::show).name("posts.show");
```

| 関数                                              | 返すもの                   |
|---------------------------------------------------|----------------------------|
| `route("home")`                                   | パスだけ（`/`）            |
| `route_with("posts.show", &[("post", "12")])`     | パスだけ（`/posts/12`）    |
| `url("/posts")`                                   | `APP_URL` を前に付けた URL |
| `route_url("home")`                               | 名前付きルートの完全な URL |
| `route_url_with("posts.show", &[("post", "12")])` | 同上（パス引数つき）       |

- `url()` 以外は `Result<String>` を返します。知らない名前と、足りないパス引数はエラーです。
- `url()` に `http://` か `https://` で始まる文字列を渡すと、そのまま返します。
- リダイレクトでも同じ名前を使えます（`redirect().route("home")`）。
- パスに使わなかった引数は、クエリとして後ろに付きます（`/posts?page=2`）。
- 値は `%xx` の形に符号化します。`/` も `%2F` になります。`{*path}` の引数だけは `/` を残します。

## 署名付き URL

書き換えると通らなくなるリンクです。
「退会する」のように、**ログインしていない人に渡すリンク**で使います。

```rust
// 作る
let link = signed_url("/unsubscribe", &[("user", "12")])?;
let temp = temporary_signed_url("/unsubscribe", &[("user", "12")], 3600)?;  // 1 時間だけ

// 確かめる
pub async fn unsubscribe(req: Request) -> Result<Response> {
    if !has_valid_signature(&req)? {
        return abort(403);
    }
    // ...
}
```

- **`APP_KEY` が要ります（64 文字以上）。** くわしくは [configuration.md](configuration.md) にあります。
- **署名が合わなくても自動では止まりません。** 何を返すかは自分で決めます。
- クエリの並び順が変わっても、末尾に `/` が付いても通ります。
- `signature` と `expires` は引数の名前に使えません（仕組みが使います）。
- 渡すのはパスだけです。完全な URL（`https://...`）と `//` で始まるものはエラーです。
- 空白・`?`・`#`・日本語を含むパスもエラーです。先に `%xx` に符号化して渡します。

## ミドルウェアを付ける

ミドルウェア（前後に挟む処理）を名前で付けます。名前の登録は [middleware.md](middleware.md) にあります。

```rust
Route::get("/admin", AdminController::index)
    .middleware("admin")
    .name("admin.index");
```

## 本文の受け取り方を決める

ルートの後ろに続けて書きます。

```rust
Route::post("/imports", ImportController::store).max_body(16 * 1024 * 1024);

Route::post("/uploads/{name}", UploadController::store)
    .middleware("auth")
    .stream_body()
    .max_body(512 * 1024 * 1024);
```

| メソッド            | すること                                                                     |
|---------------------|------------------------------------------------------------------------------|
| `.max_body(バイト)` | このルートで受け取る本文の上限。無ければ `APP_MAX_BODY_BYTES`（既定 2 MiB）  |
| `.stream_body()`    | 本文を読まずにハンドラへ渡す。ハンドラは `req.body_stream()?` で少しずつ読む |

- 上限を超えると 413 を返します。
- `.max_body(0)` は「本文を受け付けない」です。空でない本文はすべて 413 です。
- `.stream_body()` の無いルートは、30 秒で読み終える必要があります（過ぎると 408）。
- **ルートごとにだけ付けられます。** グループには付けられません。
- くわしくは [requests-and-responses.md](requests-and-responses.md) の「本文の上限」と「本文を少しずつ読む」にあります。

## グループ

同じ設定をまとめて付けます。

```rust
Route::prefix("admin")
    .middleware("admin")
    .name("admin.")
    .group(|| {
        Route::get("/", AdminController::index).name("index");       // /admin        admin.index
        Route::get("/users", AdminController::users).name("users");  // /admin/users  admin.users
    });
```

| 付けるもの   | 書き方                       | 重ね方                                 |
|--------------|------------------------------|----------------------------------------|
| パスの頭     | `Route::prefix("admin")`     | 外側から順につなぐ                     |
| ルート名の頭 | `Route::name("admin.")`      | 外側から順につなぐ。**点は自分で書く** |
| ミドルウェア | `Route::middleware("admin")` | 外側が先、内側が後                     |

- 3 つは好きな順で書けます。1 つだけでも使えます。
- 入れ子にできます。外側の設定は内側へ引き継がれます。
- `prefix("/admin/")` の前後の `/` は落ちます。
- `Route::name("admin")` と点を忘れると、名前は `adminindex` になります。

## 起動時に止まる場合

次のときは起動時に止まります。リクエストが来てから落ちることはありません。

- 同じメソッドと同じパスのルートが 2 本ある
- パスが「書けない形」に当たる
- ルート名が重なっている
- 登録していないミドルウェアの名前を使っている

`health` のパスが `GET` のルートと重なるときは止まりません。警告を出して、ルートのほうを残します。

## 当たらなかったとき

| 状況                         | 返すもの                                                 |
|------------------------------|----------------------------------------------------------|
| パスもメソッドも当たらない   | `public/` に同じ名前のファイルがあれば返す。無ければ 404 |
| パスは当たるがメソッドが違う | 405。通るメソッドを `allow` ヘッダーに入れる             |

- 静的ファイルを探すのは `GET` と `HEAD` のときだけです。
- **メソッドは大文字小文字を区別します。** `get /` は 405 です。
- 末尾の `/` は無視します。`/posts` と `/posts/` は同じルートに当たります（転送はしません）。

## 一覧を見る

```sh
cargo artisan route:list
```

METHOD・URI・NAME・MIDDLEWARE の列が出ます。ミドルウェアを付けたルートが無ければ、MIDDLEWARE の列は出ません。

## 関連

- [requests-and-responses.md](requests-and-responses.md)
- [middleware.md](middleware.md)
