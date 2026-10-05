# リクエストとレスポンス

ハンドラは `Result<Response>` を返します。

```rust
use bengara::prelude::*;

pub struct PostController;

impl PostController {
    pub async fn show(req: Request) -> Result<Response> {
        let id: u32 = req.param_as("post")?;
        text(format!("post {id}"))
    }
}
```

## Request

| 分類     | メソッド                                                         |
|----------|------------------------------------------------------------------|
| 基本     | `method()` `path()` `query_string()` `full_path()` `route_name()` |
| ルート   | `route_matched()`                                                |
| 接続元   | `ip()`                                                           |
| パス引数 | `param(name)` `param_as::<T>(name)` `params()`                   |
| クエリ   | `query(key)` `query_all()`                                       |
| ヘッダ   | `header(name)` `headers()`                                       |
| 入力     | `input(key)` `input_all()` `form(key)` `form_all()`              |
| 本文     | `body()` `body_text()` `json::<T>()`                             |

- `header(name)` は大文字小文字を区別しません。
- `query` / `query_all` は `%xx` と `+` を元に戻します。
- `param` / `param_as` は `%xx` を元に戻します。 **`+` は空白にしません。**
- `param_as::<T>` は変換できないときにエラーを返すので、`?` を付けて使えます。

```rust
let page: u32 = req.query("page").and_then(|v| v.parse().ok()).unwrap_or(1);
let body: MyInput = req.json()?;
```

入力の読み方は [validation.md](validation.md) にあります。

### 接続元を見る

```rust
let ip = req.ip();     // Option<std::net::IpAddr>
```

- つないできた相手のアドレスです。前段にプロキシがいれば、そのプロキシです。
- `X-Forwarded-For` は見ません。
- ソケットを使わない呼び出し（テスト）では `127.0.0.1` になります。

### ルートがあるかを見る

```rust
if !req.route_matched() {
    // ここは 404 になるリクエスト
}
```

共通のミドルウェアは 404 と 405 にも掛かります（[middleware.md](middleware.md)）。
「ルートがあるときだけ働きたい」ミドルウェアは、これを見てください。

### 本文の上限

本文の上限は 2 MiB です。超えたときの返し方は 2 通りです。

| 状況                               | 返すもの |
|------------------------------------|----------|
| 申告された長さが上限を超えている   | **413**  |
| 読んでいる途中で失敗した           | **400**  |

読み取りの失敗は「大きすぎる」とは限りません（接続が切れただけのこともあります）。
だから 413 と決めつけません。

### 解析は 1 回だけ

`input` / `input_all` / `form_all` / `cookies` は、**1 リクエストにつき 1 回だけ**
中身を解析します。2 回目からは取っておいた結果を返します。何度呼んでも遅くなりません。

## Response

### 作る

```rust
Response::text("ok")
Response::html("<h1>hi</h1>")
Response::json(&value)?        // Result<Response>
Response::no_content()
Response::bytes("image/png", data)
Response::new(204)
```

自由関数の `text(..)` `html(..)` `json(&..)` は `Result<Response>` を返すので、ハンドラの戻り値にそのまま書けます。

```rust
pub async fn index() -> Result<Response> {
    html("<h1>hello</h1>")
}
```

### HTML に文字列を埋め込むとき

外から来た文字列は `escape_html` を通してください。通さないと、意図しないタグを
書き込まれます（クロスサイトスクリプティング）。

```rust
pub async fn hello(req: Request) -> Result<Response> {
    let name = req.param("name").unwrap_or("世界");
    html(format!("<p>{} さん、こんにちは", escape_html(name)))
}
```

### JSON を作るとき

`serde` と `serde_json` は bengara から使えます。`Cargo.toml` に書く必要はありません
（フレームワークと同じ版が使われます）。

```rust
json(&bengara::serde_json::json!({ "status": "ok" }))
```

### 整える

```rust
Response::text("ok")
    .with_status(201)
    .with_header("X-Foo", "bar")
    .with_body("changed")
```

| メソッド                         | 中身                                                       |
|----------------------------------|------------------------------------------------------------|
| `with_status(code)`              | ステータスを変える                                         |
| `with_header(name, value)`       | ヘッダーを**置き換える**。同じ名前があれば消してから入れる |
| `with_added_header(name, value)` | ヘッダーを**足す**。`set-cookie` のように何本も送るとき    |
| `with_body(body)`                | 本文を差し替える                                           |

名前の大文字小文字は区別しません（内部で小文字にそろえます）。

### 読む

`status()` `header(name)` `headers()` `body()` `body_text()`

## リダイレクト

```rust
redirect().to("/login")                                  // 302
redirect().route("home")
redirect().route_with("posts.show", &[("post", "12")])
redirect().to("/new").permanent()                        // 301
redirect().to("/other").with_status(303)
```

どれも `Result<Response>` を返します。

## 中断する（abort）

```rust
return abort(403);
return abort_with(404, "記事が見つかりません");
```

`abort` / `abort_with` は `Result<T>` を返すので、`return` でそのまま書けます。

## エラー

| 種類                              | 中身                         |
|-----------------------------------|------------------------------|
| `Error::Http { status, message }` | ステータスを指定したエラー   |
| `Error::Message`                  | 文字列のエラー               |
| `Error::Io`                       | `std::io::Error` から変換    |
| `Error::Json`                     | `serde_json::Error` から変換 |

作り方は `Error::msg("...")` と `Error::http(404, "...")` です。
`io::Error` と `serde_json::Error` からは `From` があるので `?` で変換されます。

`Err` が返ったときの扱い:

- `Error::Http` → そのステータス
- それ以外 → 500

## 404 と 405

ハンドラを書かなくても、bengara がこの 2 つを返します。

| 状況                                       | 返すもの                |
|--------------------------------------------|-------------------------|
| ルートが無く、`public/` にもファイルが無い | 404                     |
| **パスは当たるが、メソッドが違う**         | 405 と `allow` ヘッダー |

`allow` には、そのパスで通るメソッドがアルファベット順に入ります（`allow: GET, HEAD, POST`）。
`GET` があるときは `HEAD` も並びます。くわしくは [routing.md](routing.md) を見てください。

**共通のミドルウェアは、この 2 つにも掛かります。** ルートがあるかどうかは
`req.route_matched()` で見分けます（[middleware.md](middleware.md)）。

## HEAD

`GET` のルートは `HEAD` にも応答します。本文は空になりますが、ヘッダーは `GET` と同じです。

- **`Content-Length` が付きます。** 中身は `GET` で返るはずの長さです。
- 自分で `HEAD` のルートを書く必要はありません。

## HTML で返るか JSON で返るか

エラーの形は、リクエストを見て決めます（Laravel の `expectsJson()` と同じ考え方）。

| 見るもの           | JSON になる条件                                  |
|--------------------|--------------------------------------------------|
| `Accept`           | `application/json` か `+json` を含む             |
| `Accept`           | `text/html` が無く `*/*` がある（`curl` の既定） |
| `X-Requested-With` | `XMLHttpRequest`                                 |
| `Content-Type`     | 自分が JSON を送ってきた                         |

```json
{ "message": "そのページはありません" }
```

入力の検査（422）だけは、項目ごとの理由も入ります（[validation.md](validation.md)）。

## エラーの形を差し替える

```rust
// bootstrap/app.rs
Application::configure()
    .with_exceptions(|e| {
        e.render(|error| {
            (error.status() == 404).then(|| {
                Response::json(&bengara::serde_json::json!({ "message": "ありません" }))
                    .unwrap_or_else(|_| Response::text("not found"))
                    .with_status(404)
            })
        })
    })
```

- 登録した関数を **上から順に試し**、最初に `Some` を返したものを使います。
- どれも返さなければ、bengara の既定の形になります。
- `render` は何回でも呼べます。

## エラーページ

- `APP_DEBUG=true` のときだけ、エラーページに詳しい内容（原因の連鎖）が出ます。
- 本番（`APP_DEBUG=false`）では詳細を出しません。

## 静的ファイル

ルートに当たらなかった `GET` / `HEAD` は、ファイルを探します。探す順番は次のとおりです。

| 順 | 探す先                                 | いつ                     |
|----|----------------------------------------|--------------------------|
| 1  | `storage/app/public/`                  | パスが `/storage/` で始まるとき |
| 2  | バイナリに埋め込んだ `public/`         | リリースビルドのとき     |
| 3  | ディスクの `public/`                   | 埋め込みに無いとき       |

- **`public/` はリリースビルドでバイナリに入ります。** 本番に `public/` を置く必要はありません。
- デバッグビルドでは常にディスクを読みます。ファイルを直せばすぐ反映されます。
- `/storage/` で始まるパスは `public/` を探しません。逆も同じです。

決まりごと:

- ディレクトリなら `index.html` を返します。
- **`/storage/` だけで来たときは 404 です。** 置き場所の `index.html` は配信しません。
- `..` や絶対パスは受け付けません。
- シンボリックリンクで `public/` の外に出ていないかを確かめます。
- Content-Type は拡張子から決めます。
- パスの `%xx` は元に戻します。 **`+` は空白にしません。**

パスの組み立てには `public_path(rel)` / `storage_path(rel)` / `base_path()` が使えます。

## ログ

リクエストごとに `GET / -> 200 (0 ms)` の形で出ます。
細かさは `RUST_LOG` で指定します。指定がなければ `APP_DEBUG` が真なら `debug`、そうでなければ `info` です。

## 関連

- [routing.md](routing.md)
- [configuration.md](configuration.md)
