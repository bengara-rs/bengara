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

| 書き方 | 意味 |
|---|---|
| `/posts/{post}` | 1 区間にあたる部分を `post` という名前で取る |
| `/files/{*path}` | 末尾をまとめて `path` という名前で取る |

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

## 書ける場所

`Route::*` は `routes/*.rs` の中（= `Routing::web` から呼ばれている間）でだけ有効です。
それ以外の場所で呼ぶと、エラーログを出して無視されます。

## 起動時に止まる場合

`create()` は次のときに**起動時にパニックします**。早く気づけるようにするためです。

- 同じメソッドと同じパスのルートが 2 本ある
- ルート名が重なっている

`health` のパスが `routes/*.rs` にもある場合は、警告を出して `health` 側を飛ばします（パニックはしません）。

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

## まだ無いもの

ルートのグループ・プレフィックス、ミドルウェアはまだありません。
[backlog.md](backlog.md) を参照してください。
