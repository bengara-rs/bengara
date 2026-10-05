# ルーティング

ルートは `routes/web.rs` に書きます。

```rust
use bengara::prelude::*;
use crate::app::http::controllers::HomeController;

pub fn routes() {
    Route::get("/", HomeController::index).name("home");
}
```

この関数を `bootstrap/app.rs` で取り込みます。

```rust
use bengara::prelude::*;

pub fn app() -> Application {
    Application::configure()
        .with_routing(|r| r.web(crate::routes::web::routes).health("/up"))
        .create()
}
```

- `with_routing` のクロージャは `Routing` を受け取って `Routing` を返します。
- `Routing::web(define)` … ルート定義の関数を取り込みます。
- `Routing::health(path)` … 生存確認のルートを足します。

## メソッド

```rust
Route::get("/posts", PostController::index);
Route::post("/posts", PostController::store);
Route::put("/posts/{post}", PostController::update);
Route::patch("/posts/{post}", PostController::patch);
Route::delete("/posts/{post}", PostController::destroy);
```

`GET` のルートは `HEAD` にも応答します（本文は空になります）。`HEAD` を別に書く必要はありません。

## パス引数

`matchit` の記法を使います。

| 書き方           | 意味                                         |
|------------------|----------------------------------------------|
| `/posts/{post}`  | 1 区間にあたる部分を `post` という名前で取る |
| `/files/{*path}` | 末尾をまとめて `path` という名前で取る       |

取り出し方はハンドラの中で `Request` から行います。

```rust
pub async fn show(req: Request) -> Result<Response> {
    let id: u32 = req.param_as("post")?;
    text(format!("post {id}"))
}
```

詳しくは [requests-and-responses.md](requests-and-responses.md) を参照してください。

## ハンドラの形

受け付ける形は 2 つだけです。

```rust
pub async fn index() -> Result<Response>
pub async fn show(req: Request) -> Result<Response>
```

## 名前付きルート

```rust
Route::get("/", HomeController::index).name("home");
Route::get("/posts/{post}", PostController::show).name("posts.show");
```

URL を引くには次を使います。どちらも `Result<String>` を返します。

```rust
let url = route("home")?;                                     // "/"
let url = route_with("posts.show", &[("post", "12")])?;       // "/posts/12"
```

リダイレクトでも使えます。

```rust
redirect().route("home")
redirect().route_with("posts.show", &[("post", "12")])
```

## URL を作る

| 関数                                              | 返すもの                     |
|---------------------------------------------------|------------------------------|
| `route("home")`                                   | パスだけ（`/`）              |
| `route_with("posts.show", &[("post", "12")])`     | パスだけ（`/posts/12`）      |
| `url("/posts")`                                   | `APP_URL` をつないだ絶対 URL |
| `route_url("home")`                               | 名前付きルートの絶対 URL     |
| `route_url_with("posts.show", &[("post", "12")])` | 同上（パス引数つき）         |

```rust
url("/posts")           // http://localhost:8000/posts
url("https://x.test")   // そのまま返る
```

## 署名付き URL

「退会する」のリンクのように、 **ログインしていない人に渡すリンク**で使います。
クエリを1文字でも書き換えると、確認が通らなくなります。

```rust
// 作る
let link = signed_url("/unsubscribe", &[("user", "12")])?;
let temp = temporary_signed_url("/unsubscribe", &[("user", "12")], 3600)?;  // 1時間だけ

// 確かめる
pub async fn unsubscribe(req: Request) -> Result<Response> {
    if !has_valid_signature(&req)? {
        return abort(403);
    }
    // ...
}
```

- `APP_KEY` が要ります（[session.md](session.md)）。
- クエリの並び順が変わっても通ります。
- `signature` と `expires` は仕組みが使う名前なので、自分では指定できません。
- **署名が合わなくても自動では止まりません。** 何を返すかは自分で決めてください。

## 書ける場所

`Route::*` は `routes/*.rs` の中（= `Routing::web` から呼ばれている間）でだけ有効です。
それ以外の場所で呼ぶと、エラーログを出して無視されます。

## 起動時に止まる場合

`create()` は次のときに **起動時にパニックします**。早く気づけるようにするためです。

- 同じメソッドと同じパスのルートが 2 本ある
- ルート名が重なっている

`health` のパスが `routes/*.rs` にもある場合は、警告を出して `health` 側を飛ばします（パニックはしません）。

## 当たらなかったとき

| 状況                               | 返すもの                                                           |
|------------------------------------|--------------------------------------------------------------------|
| パスもメソッドも当たらない         | `public/` に同じ名前のファイルがあればそれを返す。無ければ **404** |
| **パスは当たるが、メソッドが違う** | **405**。どのメソッドなら通るかを `allow` ヘッダーに入れる         |

```
$ curl -i -X DELETE http://127.0.0.1:8000/
HTTP/1.1 405 Method Not Allowed
allow: GET
```

`allow` の中身はメソッド名をアルファベット順に並べ、`, ` でつなぎます。
`GET` のルートは `HEAD` にも応答するので、`HEAD` で 405 になることはありません。

静的ファイルを探すのは `GET` と `HEAD` のときだけです。

## 一覧を見る

```sh
cargo artisan route:list
```

METHOD / URI / NAME の表が出ます。

```
GET   /     home
GET   /up
```

## 動きの前提

- ルートは起動時に組み立てて固定します。
- 照合は matchit（radix trie）で行います。リクエスト処理中にロックは取りません。

## ミドルウェア

```rust
Route::get("/admin", AdminController::index)
    .middleware("admin")
    .name("admin.index");
```

名前は `bootstrap/app.rs` で登録します。くわしくは [middleware.md](middleware.md)。

## グループ

同じ設定を何本ものルートにまとめて付けられます。

```rust
Route::prefix("admin")
    .middleware("admin")
    .name("admin.")
    .group(|| {
        Route::get("/", AdminController::index).name("index");   // /admin        admin.index
        Route::get("/users", AdminController::users).name("users"); // /admin/users  admin.users
    });
```

| 付けるもの   | 書き方                       | 畳み方                                 |
|--------------|------------------------------|----------------------------------------|
| パスの頭     | `Route::prefix("admin")`     | 外側から順につなぐ                     |
| ルート名の頭 | `Route::name("admin.")`      | 外側から順につなぐ。**点は自分で書く** |
| ミドルウェア | `Route::middleware("admin")` | 外側が先、内側が後                     |

3 つは好きな順でつなげられます。どれか 1 つだけでも使えます。

```rust
Route::middleware("auth").group(|| {
    Route::post("/posts", PostController::store);
});
```

### 入れ子にできる

外側の設定は内側へ引き継がれます。

```rust
Route::prefix("admin").middleware("auth").name("admin.").group(|| {
    Route::prefix("posts").name("posts.").group(|| {
        Route::get("/{post}", PostController::show).name("show");
        // → GET /admin/posts/{post}   名前 admin.posts.show   ミドルウェア auth
    });
});
```

### 畳むのは登録のときだけ

グループは **ルートを登録する瞬間に畳まれます**。
畳んだ後のルートだけが残るので、リクエストを処理する速さは、グループを使わないときと同じです。

`cargo artisan route:list` にも畳んだ結果が出ます。

```
METHOD  URI                   NAME               MIDDLEWARE
GET     /team                 team.index         admin
GET     /team/members/{name}  team.members.show  admin
```

### 細かい決まり

| 書いたもの                   | どうなるか                                     |
|------------------------------|------------------------------------------------|
| `prefix("")` / `prefix("/")` | 何も足さない                                   |
| `prefix("/admin/")`          | 前後の `/` は落ちて `/admin` になる            |
| `name("admin")`（点なし）    | `adminindex` になる。**点は自分で書く**        |
| グループの中でパニック       | 積んだ設定は必ず降ろす。後続のルートに漏れない |

### `Route::name` と `.name` の違い

| 書き方                          | 意味                                        |
|---------------------------------|---------------------------------------------|
| `Route::name("admin.")`         | **グループを作る。** 中のルート名の頭に付く |
| `Route::get(...).name("index")` | **1 本に名前を付ける**                      |

Laravel と同じ使い分けです。

## まだ無いもの

サブドメインでの振り分け（`domain()`）と `Route::resource()` はありません。
[backlog.md](backlog.md) を参照してください。
