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

| 分類 | メソッド |
|---|---|
| 基本 | `method()` `path()` `query_string()` `full_path()` `route_name()` |
| パス引数 | `param(name)` `param_as::<T>(name)` `params()` |
| クエリ | `query(key)` `query_all()` |
| ヘッダ | `header(name)` `headers()` |
| 本文 | `body()` `body_text()` `json::<T>()` |

- `header(name)` は大文字小文字を区別しません。
- `query` / `query_all` は `%xx` と `+` を元に戻します。
- `param` / `param_as` も `%xx` を元に戻した値を返します。
- `param_as::<T>` は変換できないときにエラーを返すので、`?` を付けて使えます。

```rust
let page: u32 = req.query("page").and_then(|v| v.parse().ok()).unwrap_or(1);
let body: MyInput = req.json()?;
```

本文の上限は 2 MiB です。超えると 413 を返します。

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

| メソッド | 中身 |
|---|---|
| `with_status(code)` | ステータスを変える |
| `with_header(name, value)` | ヘッダーを**置き換える**。同じ名前があれば消してから入れる |
| `with_added_header(name, value)` | ヘッダーを**足す**。`set-cookie` のように何本も送るとき |
| `with_body(body)` | 本文を差し替える |

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

| 種類 | 中身 |
|---|---|
| `Error::Http { status, message }` | ステータスを指定したエラー |
| `Error::Message` | 文字列のエラー |
| `Error::Io` | `std::io::Error` から変換 |
| `Error::Json` | `serde_json::Error` から変換 |

作り方は `Error::msg("...")` と `Error::http(404, "...")` です。
`io::Error` と `serde_json::Error` からは `From` があるので `?` で変換されます。

`Err` が返ったときの扱い:

- `Error::Http` → そのステータス
- それ以外 → 500

## 404 と 405

ハンドラを書かなくても、bengara がこの 2 つを返します。

| 状況 | 返すもの |
|---|---|
| ルートが無く、`public/` にもファイルが無い | 404 |
| **パスは当たるが、メソッドが違う** | 405 と `allow` ヘッダー |

`allow` には、そのパスで通るメソッドがアルファベット順に入ります（`allow: GET, POST`）。
くわしくは [routing.md](routing.md) を見てください。

## エラーページ

- `APP_DEBUG=true` のときだけ、エラーページに詳しい内容（原因の連鎖）が出ます。
- 本番（`APP_DEBUG=false`）では詳細を出しません。

## 静的ファイル

ルートに当たらなかった `GET` / `HEAD` は `public/` の中を探します。

- ディレクトリなら `index.html` を返します。
- `..` や絶対パスは受け付けません。
- シンボリックリンクで `public/` の外に出ていないかを確かめます。
- Content-Type は拡張子から決めます。

パスの組み立てには `public_path(rel)` / `storage_path(rel)` / `base_path()` が使えます。

リリースビルドでの `public/` の埋め込みはまだありません（[backlog.md](backlog.md)）。

## ログ

リクエストごとに `GET / -> 200 (0 ms)` の形で出ます。
細かさは `RUST_LOG` で指定します。指定がなければ `APP_DEBUG` が真なら `debug`、そうでなければ `info` です。

## 関連

- [routing.md](routing.md)
- [configuration.md](configuration.md)
