# ルーティング

どの URL でどの処理を呼ぶかを決めます。ルートは `routes/web.rs` に書きます。

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

| 書くもの                | 中身                       |
|-------------------------|----------------------------|
| `with_routing(f)`       | ルートの設定を組み立てる   |
| `Routing::web(define)`  | ルート定義の関数を取り込む |
| `Routing::health(path)` | 生存確認のルートを足す     |

**`Route::*` は `routes/*.rs` の中でだけ有効です。**
それ以外の場所で呼ぶと、エラーログを出して無視されます。

## メソッド

```rust
Route::get("/posts", PostController::index);
Route::post("/posts", PostController::store);
Route::put("/posts/{post}", PostController::update);
Route::patch("/posts/{post}", PostController::patch);
Route::delete("/posts/{post}", PostController::destroy);
```

**書けるのはこの 5 本だけです。** `GET` のルートは `HEAD` にも応答するので
（本文は空）、`HEAD` を別に書く必要はありません。
`any` / `match` / `options` はありません。メソッドごとに 1 行ずつ書いてください。

## ハンドラの形

受け付ける形は 2 つだけです。

```rust
pub async fn index() -> Result<Response>
pub async fn show(req: Request) -> Result<Response>
```

## パス引数

| 書き方           | 意味                                         |
|------------------|----------------------------------------------|
| `/posts/{post}`  | 1 区間にあたる部分を `post` という名前で取る |
| `/files/{*path}` | 末尾をまとめて `path` という名前で取る       |
| `/file-{name}`   | `/file-abc` に当たる（`file-` は固定）       |
| `/v{n}/x`        | `/v2/x` に当たる                             |

取り出し方はハンドラの中で `Request` から行います。

```rust
pub async fn show(req: Request) -> Result<Response> {
    let id: u32 = req.param_as("post")?;
    text(format!("post {id}"))
}
```

くわしくは [requests-and-responses.md](requests-and-responses.md) にあります。

- パスに日本語などの非 ASCII を書いても動きます。
  登録のときに `%xx` の形に直すので、ブラウザが送ってくるパスと同じ形で照合されます。
- **パスに `{` そのものを書く方法はありません。** `{` は必ずパス引数の始まりです。
  `{{` で逃がすこともできません。書くとエラーになります。

## 名前付きルート

名前を付けておくと、あとで URL を引けます。

```rust
Route::get("/", HomeController::index).name("home");
Route::get("/posts/{post}", PostController::show).name("posts.show");
```

## URL を作る

| 関数                                              | 返すもの                     |
|---------------------------------------------------|------------------------------|
| `route("home")`                                   | パスだけ（`/`）              |
| `route_with("posts.show", &[("post", "12")])`     | パスだけ（`/posts/12`）      |
| `url("/posts")`                                   | `APP_URL` をつないだ絶対 URL |
| `route_url("home")`                               | 名前付きルートの絶対 URL     |
| `route_url_with("posts.show", &[("post", "12")])` | 同上（パス引数つき）         |

`route()` と `route_with()` は `Result<String>` を返します。

```rust
url("/posts")           // http://localhost:8000/posts
url("https://x.test")   // そのまま返る
```

リダイレクトでも同じ名前が使えます。

```rust
redirect().route("home");
redirect().route_with("posts.show", &[("post", "12")]);
```

### パスに無い引数はクエリになる

**パスに使わなかった引数は、クエリ文字列として後ろに足します。** Laravel と同じです。

```rust
route_with("posts.index", &[("page", "2")])                 // /posts?page=2
route_with("posts.show", &[("post", "12"), ("page", "2")])  // /posts/12?page=2
```

並びは渡した順のままです。名前も値もパーセント符号化します。

### パス引数は符号化される

| 書いたもの                                     | できる URL                     |
|------------------------------------------------|--------------------------------|
| `route_with("posts.show", &[("post", "a b")])` | `/posts/a%20b`                 |
| `route_with("posts.show", &[("post", "a/b")])` | `/posts/a%2Fb`                 |
| `route_with("files.show", &[("path", "a/b")])` | `/files/a/b`（`{*path}` のみ） |

- **`/` も `%2F` になります。** 末尾全取り（`{*path}`）のときだけ `/` を残します。
- そのまま残る文字は英数字と `-` `_` `.` `~` です。

## 署名付き URL

「退会する」のリンクのように、**ログインしていない人に渡すリンク**で使います。
クエリを 1 文字でも書き換えると、確認が通らなくなります。

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

- **`APP_KEY` が要ります。64 文字以上です**（[configuration.md](configuration.md)）。
- **署名が合わなくても自動では止まりません。** 何を返すかは自分で決めてください。
- クエリの並び順が変わっても通ります。
- `signature` と `expires` は仕組みが使う名前です。自分では指定できません。
  `signature` が 2 つ以上付いているときは、必ず偽になります。

### パスのそろえ方

| そろえること                     | 作るとき   | 確かめるとき |
|----------------------------------|------------|--------------|
| 末尾の `/` を落とす              | する       | する         |
| 先頭の `/` を **1 本だけ**落とす | する       | する         |
| `%xx` の 16 進を大文字にする     | **しない** | **しない**   |

- **末尾の `/` は無視されます。** `/unsubscribe/` になっても通ります。
- **先頭の `/` は 1 本しか落としません。** `//x` は `/x` の署名では通りません。
- **`%E3%81%82` で作ったリンクを `%e3%81%82` に書き換えると通りません。**

### 渡せないパス

`signed_url()` と `temporary_signed_url()` は、**URL にそのまま書けない文字を
含むパスを `Err` で断ります**。通すのは次の文字だけです。

```
英数字  - . _ ~  ! $ & ' ( ) * + , ; =  : @ / %
```

空白・`?`・`#`・`<`・`>`・`"`・`\`・`^`・`` ` ``・`{`・`}`・`|`・制御文字・
非 ASCII はすべてエラーになります。**先に符号化してから渡してください**
（`/hello/%E3%81%82` は通ります）。空のパスは `/` として扱います。

## ミドルウェア

```rust
Route::get("/admin", AdminController::index)
    .middleware("admin")
    .name("admin.index");
```

名前は `bootstrap/app.rs` で登録します（[middleware.md](middleware.md)）。

## グループ

同じ設定を何本ものルートにまとめて付けられます。

```rust
Route::prefix("admin")
    .middleware("admin")
    .name("admin.")
    .group(|| {
        Route::get("/", AdminController::index).name("index");       // /admin        admin.index
        Route::get("/users", AdminController::users).name("users");  // /admin/users  admin.users
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

グループは**ルートを登録する瞬間に畳まれます**。
`cargo artisan route:list` にも畳んだ結果が出ます。

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

## 起動時に止まる場合

`create()` は次のときに**起動時にパニックします**。

- 同じメソッドと同じパスのルートが 2 本ある
- ルート名が重なっている

`health` のパスが `routes/*.rs` にもある場合は、警告を出して `health` 側を飛ばします。
`routes/*.rs` に書いたほうが残ります。パニックはしません。
`health("up")` のように先頭の `/` を書かなくても同じです。

## 当たらなかったとき

| 状況                               | 返すもの                                                           |
|------------------------------------|--------------------------------------------------------------------|
| パスもメソッドも当たらない         | `public/` に同じ名前のファイルがあればそれを返す。無ければ **404** |
| **パスは当たるが、メソッドが違う** | **405**。どのメソッドなら通るかを `allow` ヘッダーに入れる         |

```
$ curl -i -X DELETE http://127.0.0.1:8000/
HTTP/1.1 405 Method Not Allowed
allow: GET, HEAD
```

- `allow` の中身はメソッド名をアルファベット順に並べ、`, ` でつなぎます。
  **`GET` があるときは `HEAD` も並びます。**
- 静的ファイルを探すのは `GET` と `HEAD` のときだけです。
- **`GET` のルートが 1 本も無いパスでは、405 を返す前に静的ファイルを探します。**
  `Route::post("/report", ...)` だけ書いてあっても、`public/report` が
  `GET /report` で見えます。

### 照合の前にパスをそろえる

照合する前に、届いたパスを 2 つだけそろえます。**転送（リダイレクト）はしません。**

| そろえること           | 例                          |
|------------------------|-----------------------------|
| 末尾の `/` を落とす    | `/posts/` → `/posts`        |
| `%xx` の 16 進を大文字 | `/%e3%81%82` → `/%E3%81%82` |

- **`/posts` と `/posts/` は同じルートに当たります。** ルート名とパス引数は
  `/posts` のものになります。`Route::get("/posts/")` と `Route::get("/posts")` も
  同じ 1 本です。
- `//` のように `/` だけのパスは `/` として扱います。
- `%xx` を戻す（復号する）わけではありません。形をそろえるだけです。
  16 進として読めない並び（`%zz` や末尾の `%4`）はそのままです。
- **署名付き URL の照合には通しません**（上の「署名付き URL」）。

### パスの読み方

パス引数と静的ファイルのパスは、`%xx` を元に戻してから使います。

- **`+` は空白にしません。** `/a+b.css` は `public/a+b.css` を指します。
  `+` を空白として読むのはフォームの規則で、クエリと本文だけがその読み方です。
- ルートの照合そのものは、符号化されたままのパスで行います。

## 一覧を見る

```sh
cargo artisan route:list
```

METHOD / URI / NAME / MIDDLEWARE の表が出ます。

```
METHOD  URI                   NAME               MIDDLEWARE
GET     /                     home
GET     /team/members/{name}  team.members.show  admin
```

## まだ無いもの

まだ無いものは [backlog.md](backlog.md) にまとめてあります。

## 関連

- [requests-and-responses.md](requests-and-responses.md)
- [middleware.md](middleware.md)
