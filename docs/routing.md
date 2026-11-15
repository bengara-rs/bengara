# ルーティング

どの URL でどの処理を呼ぶかを決めます。このページで分かることは 3 つです。

- ルートの書き方（メソッド、パス引数、名前、グループ）
- URL の作り方（名前から引く、署名を付ける）
- どのルートにも当たらなかったときの動き

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
use bengara::prelude::*;

pub fn app() -> Application {
    Application::configure()
        .with_routing(|r| r.web(crate::routes::web::routes).health("/up"))
        .create()
}
```

| 書くもの          | 中身                                                         |
|-------------------|--------------------------------------------------------------|
| `with_routing(f)` | ルートの設定を組み立てる                                     |
| `r.web(関数)`     | ルート定義の関数を取り込む                                   |
| `r.health(パス)`  | 生存確認のルートを足す。`{"status":"ok","name":"アプリ名"}` を返す |

**`Route::*` は、`r.web(...)` に渡した関数の中でだけ有効です。**
それ以外の場所で呼ぶと、エラーログを出して無視します。

## メソッド

```rust
Route::get("/posts", PostController::index);
Route::post("/posts", PostController::store);
Route::put("/posts/{post}", PostController::update);
Route::patch("/posts/{post}", PostController::patch);
Route::delete("/posts/{post}", PostController::destroy);
```

- **書けるのはこの 5 つだけです。** `any` / `match` / `options` はありません。
  メソッドごとに 1 行ずつ書きます。
- `GET` のルートは `HEAD` にも応答します（本文は空）。`HEAD` を別に書く必要はありません。

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

値はハンドラの中で `Request` から取り出します。
読み方の決まりは [requests-and-responses.md](requests-and-responses.md) にあります。

```rust
pub async fn show(req: Request) -> Result<Response> {
    let id: u32 = req.param_as("post")?;
    text(format!("post {id}"))
}
```

### 書けない形

次の形は、**起動時に理由を添えて止まります。**

| 書き方                            | 理由                                         |
|-----------------------------------|----------------------------------------------|
| `/posts/{id}.json`                | パス引数の後ろに固定文字は書けない           |
| `/{year}-{month}`                 | 1 つの区間に書けるパス引数は 1 つだけ        |
| `/files/{*path}/edit`             | `{*名前}` はパスの末尾にだけ書ける           |
| `/posts/{}`                       | 名前が空。`{` と `}` が対になっていないときも同じ |
| `/posts/{id}` と `/posts/{post}`  | 名前だけが違うパスは、同じパスとして扱う     |

- 拡張子で分けたいときは、`/posts/{id}` で受けて、ハンドラの中で値の末尾を見ます。
- **`{` そのものをパスに書く方法はありません。** `{` は必ずパス引数の始まりです。

### 日本語のパス

パスに日本語などを書いても動きます（`/記事-{id}` は `/記事-12` に当たります）。
登録のときに `%xx` の形に直すので、ブラウザが送ってくるパスと同じ形で照合します。

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

- `url()` 以外は `Result<String>` を返します。
  登録していない名前と、足りないパス引数はエラーです。
- `url()` に `http://` か `https://` で始まる文字列を渡すと、そのまま返します。

リダイレクトでも同じ名前が使えます。

```rust
redirect().route("home");
redirect().route_with("posts.show", &[("post", "12")]);
```

### パスに無い引数はクエリになる

パスに使わなかった引数は、クエリ文字列として後ろに足します。並びは渡した順のままです。

```rust
route_with("posts.index", &[("page", "2")])                 // /posts?page=2
route_with("posts.show", &[("post", "12"), ("page", "2")])  // /posts/12?page=2
```

### 値は符号化される

| 書いたもの                                     | できる URL     |
|------------------------------------------------|----------------|
| `route_with("posts.show", &[("post", "a b")])` | `/posts/a%20b` |
| `route_with("posts.show", &[("post", "a/b")])` | `/posts/a%2Fb` |
| `route_with("files.show", &[("path", "a/b")])` | `/files/a/b`   |

- そのまま残る文字は、英数字と `-` `_` `.` `~` だけです。クエリの名前と値も同じです。
- `/` も `%2F` になります。末尾をまとめて取る引数（`{*path}`）のときだけ `/` を残します。

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
- **署名が合わなくても自動では止まりません。** 何を返すかは自分で決めます。
- クエリの並び順が変わっても通ります。
- `signature` と `expires` は仕組みが使う名前です。自分では指定できません。
- `signature` が 2 つ以上付いているときは、必ず偽になります。

### パスの扱い

| 届いたリンクの変わり方                  | 通るか     |
|-----------------------------------------|------------|
| 末尾に `/` が付いた（`/unsubscribe/`）  | 通る       |
| 先頭の `/` が増えた（`//unsubscribe`）  | 通らない   |
| `%E3` が `%e3` に変わった               | 通らない   |

### 渡せないパス

URL にそのまま書けない文字を含むパスは、作るときにエラーになります。
通すのは次の文字だけです。

```
英数字  - . _ ~  ! $ & ' ( ) * + , ; =  : @ / %
```

- 空白・`?`・`#`・日本語などはエラーです。**先に符号化してから渡します**
  （`/hello/%E3%81%82` は通ります）。
- 空のパスは `/` として扱います。

## ミドルウェア

```rust
Route::get("/admin", AdminController::index)
    .middleware("admin")
    .name("admin.index");
```

名前は `bootstrap/app.rs` で登録します（[middleware.md](middleware.md)）。

## グループ

同じ設定を何本ものルートにまとめて付けます。

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

3 つは好きな順でつなげられます。1 つだけでも使えます。

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

重ねた結果は `cargo artisan route:list` で確かめられます。

### 細かい決まり

| 書いたもの                      | どうなるか                                  |
|---------------------------------|---------------------------------------------|
| `prefix("")` / `prefix("/")`    | 何も足さない                                |
| `prefix("/admin/")`             | 前後の `/` は落ちて `/admin` になる         |
| `Route::name("admin")`（点なし） | 中のルート名が `adminindex` になる          |
| `Route::name("admin.")`         | **グループを作る。** 中のルート名の頭に付く |
| `Route::get(...).name("index")` | **1 本に名前を付ける**                      |

## 起動時に止まる場合

次のときは、起動時にパニックします。リクエストが来てから落ちることはありません。

- 同じメソッドと同じパスのルートが 2 本ある
- パスが上の「書けない形」に当たる
- ルート名が重なっている
- 登録していないミドルウェアの名前を使っている（[middleware.md](middleware.md)）

`health` のパスが `routes/*.rs` の `GET` のルートと重なるときは、止まりません。
警告を出して、`routes/*.rs` に書いたほうを残します。

## 当たらなかったとき

| 状況                               | 返すもの                                                           |
|------------------------------------|--------------------------------------------------------------------|
| パスもメソッドも当たらない         | `public/` に同じ名前のファイルがあればそれを返す。無ければ **404** |
| **パスは当たるが、メソッドが違う** | **405**。通るメソッドを `allow` ヘッダーに入れる                   |

```
$ curl -i -X DELETE http://127.0.0.1:8000/
HTTP/1.1 405 Method Not Allowed
allow: GET, HEAD
```

- `allow` にはメソッド名がアルファベット順に並びます。`GET` があれば `HEAD` も並びます。
- 静的ファイルを探すのは `GET` と `HEAD` のときだけです
  （[requests-and-responses.md](requests-and-responses.md)）。
- **`GET` のルートが無いパスでは、405 を返す前に静的ファイルを探します。**
  `Route::post("/report", ...)` だけなら、`public/report` が `GET /report` で見えます。
- **メソッドは大文字小文字を区別します。** `get /` は `GET` のルートに当たらず 405 です。

### 照合の前にパスをそろえる

照合する前に、届いたパスを 2 つだけそろえます。**転送（リダイレクト）はしません。**

| そろえること           | 例                          |
|------------------------|-----------------------------|
| 末尾の `/` を落とす    | `/posts/` → `/posts`        |
| `%xx` の 16 進を大文字 | `/%e3%81%82` → `/%E3%81%82` |

- **`/posts` と `/posts/` は同じルートに当たります。**
  `Route::get("/posts/")` と `Route::get("/posts")` も同じ 1 本です。
- `//` のように `/` だけのパスは `/` として扱います。
- `%xx` を元に戻すわけではありません。照合は符号化されたままのパスで行います。

## 一覧を見る

```sh
cargo artisan route:list
```

```
METHOD  URI                   NAME               MIDDLEWARE
GET     /                     home
GET     /team/members/{name}  team.members.show  admin
```

ミドルウェアを付けたルートが 1 本も無ければ、MIDDLEWARE の列は出ません。

## まだ無いもの

まだ無いものは [backlog.md](backlog.md) にまとめてあります。

## 関連

- [requests-and-responses.md](requests-and-responses.md)
- [middleware.md](middleware.md)
