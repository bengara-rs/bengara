# リクエストとレスポンス

届いた入力を読む方法と、返すものを作る方法です。ハンドラは `Result<Response>` を返します。

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

## Request から読む

| 分類       | メソッド                                                          |
|------------|-------------------------------------------------------------------|
| 基本       | `method()` `path()` `query_string()` `full_path()` `route_name()` |
| ルート     | `route_matched()`                                                 |
| 接続元     | `ip()`                                                            |
| パス引数   | `param(name)` `param_as::<T>(name)` `params()`                    |
| クエリ     | `query(key)` `query_all()` `page()`                               |
| ヘッダ     | `header(name)` `headers()`                                        |
| Cookie     | `cookie(name)` `cookies()`                                        |
| 入力       | `input(key)` `input_all()` `form(key)` `form_all()`               |
| 本文       | `body()` `body_text()` `json::<T>()`                              |
| 検査       | `validate(rules)`                                                 |
| セッション | `session()` `try_session()` `csrf_token()`                        |
| ログイン   | `auth()`                                                          |
| モデル     | `model::<T>(param)`                                               |

```rust
let body: MyInput = req.json()?;
let post = req.model::<Post>("post").await?;          // 無ければ 404
let input = req.validate(&[("title", "required")])?;  // 落ちれば 422
```

一言添えておくものです。

| メソッド                      | 中身                                                     |
|-------------------------------|----------------------------------------------------------|
| `page()`                      | `?page=2` の番号。無い・読めない・0 なら 1               |
| `cookie(name)` / `cookies()`  | 届いた Cookie（[session.md](session.md)）                |
| `validate(rules)`             | 入力の検査（[validation.md](validation.md)）             |
| `session()` / `try_session()` | セッション（[session.md](session.md)）                   |
| `csrf_token()`                | フォームに入れるトークン（[session.md](session.md)）     |
| `auth()`                      | ログインの状態（[authentication.md](authentication.md)） |
| `model::<T>(param)`           | パス引数の値でモデルを 1 件読む。無ければ 404            |

**`session()` と `csrf_token()` は、`StartSession` を登録していないとパニックします。**
無くてもよい場面では `try_session()` を使ってください。

読むときの決まりごとは 5 つです。

- `header(name)` は大文字小文字を区別しません。
- **`method()` は大文字小文字を区別します。** 届いた文字列をそのまま返します。
- `query` / `query_all` は `%xx` と `+` を元に戻します。
- `param` / `param_as` は `%xx` を元に戻します。**`+` は空白にしません。**
- `param_as::<T>` は変換できないときにエラーを返すので、`?` を付けて使えます。

フォームの値の読み方は [validation.md](validation.md) にあります。

### メソッドは大文字小文字を区別する

| 届いたもの | どうなるか                         |
|------------|------------------------------------|
| `GET /x`   | `GET` のルートに当たる             |
| `get /x`   | **405**。`allow: GET, HEAD` が付く |

`req.method()` も届いたまま（`get`）を返します。
自分で比べるときは `req.method() == "POST"` のように大文字で書いてください。

### 接続元を見る

```rust
let ip = req.ip();     // Option<std::net::IpAddr>
```

- つないできた相手のアドレスです。前段にプロキシがいれば、そのプロキシです。
- `X-Forwarded-For` は見ません。
- ソケットを使わない呼び出し（テスト）では `127.0.0.1` になります。

### 同じ名前のヘッダーが何本も届く

**`req.header(name)` が返すのは 1 本目だけです。**
全部を見たいときは `req.headers()` を回します。

```rust
let all: Vec<&str> = req
    .headers()
    .iter()
    .filter(|(k, _)| k.eq_ignore_ascii_case("x-forwarded-for"))
    .map(|(_, v)| v.as_str())
    .collect();
```

- `X-Forwarded-For` が代表です。何本かに分かれて届いたときは、
  カンマでつないだ 1 本と同じ意味です。
- **1 本目は相手が付けた値かもしれません。** そのまま信用しないでください。
- 回数の制限（`Throttle`）は全部を見ます（[middleware.md](middleware.md)）。

### ルートがあるかを見る

```rust
if !req.route_matched() {
    // ここは 404 になるリクエスト
}
```

共通のミドルウェアは 404 と 405 にも掛かります（[middleware.md](middleware.md)）。
「ルートがあるときだけ働きたい」ミドルウェアは、これを見てください。

### 本文の上限

本文の上限は 2 MiB です。

| 状況                             | 返すもの |
|----------------------------------|----------|
| 申告された長さが上限を超えている | **413**  |
| 読んでいる途中で失敗した         | **400**  |
| 30 秒たっても読み終わらない      | **408**  |

接続そのものの待ち時間と同時接続数の上限は持ちません。
前段のプロキシで掛けてください（[deployment.md](deployment.md)）。

`input` / `input_all` / `form_all` / `cookies` は、**1 リクエストにつき 1 回だけ**
中身を解析します。何度呼んでも遅くなりません。

## Response を作る

```rust
Response::text("ok")
Response::html("<h1>hi</h1>")
Response::json(&value)?        // Result<Response>
Response::no_content()
Response::bytes("image/png", data)
Response::new(204)
```

自由関数の `text(..)` `html(..)` `json(&..)` は `Result<Response>` を返します。
ハンドラの戻り値にそのまま書けます。

```rust
pub async fn index() -> Result<Response> {
    html("<h1>hello</h1>")
}
```

**テンプレートはありません。** `html()` に文字列を渡すか、JSON を返します。

### HTML に文字列を埋め込むとき

外から来た文字列は `escape_html` を通してください。
通さないと、意図しないタグを書き込まれます（クロスサイトスクリプティング）。

```rust
pub async fn hello(req: Request) -> Result<Response> {
    let name = req.param("name").unwrap_or("世界");
    html(format!("<p>{} さん、こんにちは", escape_html(name)))
}
```

### JSON を作るとき

`serde` と `serde_json` は bengara から使えます。`Cargo.toml` に書く必要はありません。
`json!` マクロは `prelude` に入っているので、そのまま書けます。

```rust
json(&json!({ "status": "ok" }))
```

関数の `json()` とマクロの `json!` は、同じファイルで両方使えます。
`bengara::serde_json::json!` と長く書いても同じものです。

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
| `with_added_header(name, value)` | ヘッダーを**足す**。`set-cookie` のように何本も送るとき     |
| `with_body(body)`                | 本文を差し替える                                           |

名前の大文字小文字は区別しません。読むほうは
`status()` `header(name)` `headers()` `body()` `body_text()` です。

決まりごとが 3 つあります。

- **ステータスは 100〜599 だけです。** 範囲の外（`abort(1000)` や `Response::new(0)`）は
  警告を出して **500** にします。
- **ヘッダーの値に改行などの制御文字が入っていると、その 1 本だけを落とします。**
  警告が 1 行出ます。水平タブだけは値に使えるので通ります。
- **落とすときは、すでに入っていた値を残します。**
  `Response::text("ok").with_header("content-type", "a\nb")` は
  `content-type: text/plain; charset=utf-8` のままです。

### 本文を持てないステータス

**1xx・204・304 では、本文を付けません。** `content-type` と `content-length` も
付きません。`abort(204)` から来た応答も、自分で組んだ応答も同じです。

## リダイレクト

```rust
redirect().to("/login")                                 // 302
redirect().route("home")
redirect().route_with("posts.show", &[("post", "12")])
redirect().permanent().to("/new")                       // 301
redirect().with_status(303).to("/other")                // 303
```

どれも `Result<Response>` を返します。

**`permanent()` と `with_status()` は `to()` より先に書きます。**
`to()` / `route()` / `route_with()` は `Result<Response>` を返すので、
その後ろには続けられません。

### 行き先に使えない文字

**行き先に改行などの制御文字が入っていると、エラーを返します。**
`route` / `route_with` も同じです。

```rust
redirect().to(req.input("next").unwrap_or_default())?   // 外から来た値は落ちうる
```

- `?` で上に返せば、普段のエラーの道を通って **500** になります。
- そのとき、**直前のミドルウェアが付けた `Set-Cookie` は残ります。**
- 外から来た文字列を行き先にするときは、このエラーを受ける用意をしてください。

## 中断する（abort）

```rust
return abort(403);
return abort_with(404, "記事が見つかりません");
```

`abort` / `abort_with` は `Result<T>` を返すので、`return` でそのまま書けます。

## エラー

| 種類                              | 中身                               |
|-----------------------------------|------------------------------------|
| `Error::Http { status, message }` | ステータスを指定したエラー         |
| `Error::Message`                  | 文字列のエラー                     |
| `Error::Io`                       | `std::io::Error` から変換          |
| `Error::Json`                     | `serde_json::Error` から変換       |
| `Error::Validation`               | 入力の検査に落ちた。**422**        |
| `Error::Other`                    | ほかのエラーを包む。原因をたどれる |

| 作り方                 | 中身                                                |
|------------------------|-----------------------------------------------------|
| `Error::msg("...")`    | 文字列のエラー                                      |
| `Error::http(404, ..)` | ステータスを指定する                                |
| `Error::other(e)`      | ほかのエラーを包む。元のエラーを残す                |

`io::Error` と `serde_json::Error` からは `From` があるので、`?` で変換されます。
`Err` が返ったときは、`Error::Http` ならそのステータスになります。それ以外は 500 です。

## 404 と 405

ハンドラを書かなくても、bengara がこの 2 つを返します。

| 状況                                       | 返すもの                               |
|--------------------------------------------|----------------------------------------|
| ルートが無く、`public/` にもファイルが無い | 404                                    |
| **パスは当たるが、メソッドが違う**         | 405 と `allow` ヘッダー                |
| メソッドが違い、`GET` のルートも無い       | 先に静的ファイルを探す（無ければ 405） |

- **末尾の `/` は無視されます。** `/posts/` は `/posts` に当たります。
- `allow` には、そのパスで通るメソッドがアルファベット順に入ります。
  `GET` があるときは `HEAD` も並びます（[routing.md](routing.md)）。
- **共通のミドルウェアは、この 2 つにも掛かります。** ルートがあるかどうかは
  `req.route_matched()` で見分けます（[middleware.md](middleware.md)）。

## HEAD

`GET` のルートは `HEAD` にも応答します。本文は空になりますが、ヘッダーは `GET` と同じです。

- **`Content-Length` が付きます。** 中身は `GET` で返るはずの長さです。
- 本文を持てないステータス（1xx・204・304）のときだけ付きません。
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
{
  "message": "そのページはありません"
}
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

- 登録した関数を**上から順に試し**、最初に `Some` を返したものを使います。
- どれも返さなければ、bengara の既定の形になります。
- `render` は何回でも呼べます。

`APP_DEBUG=true` のときだけ、エラーページに詳しい内容（原因の連鎖）が出ます。
本番（`APP_DEBUG=false`）では詳細を出しません。

## 静的ファイル

ルートに当たらなかった `GET` / `HEAD` は、ファイルを探します。
**パスの頭で 2 つに分かれます。** 片方で見つからなくても、もう片方は探しません。

| パス           | 探す先                                                             |
|----------------|--------------------------------------------------------------------|
| `/storage/...` | `storage/app/public/` **だけ**                                     |
| それ以外       | 埋め込んだ `public/`（リリースビルドのとき）→ ディスクの `public/` |

- **`public/` はリリースビルドでバイナリに入ります。** 本番に `public/` を
  置く必要はありません。
- デバッグビルドでは常にディスクを読みます。ファイルを直せばすぐ反映されます。

パスの組み立てには `public_path(rel)` / `storage_path(rel)` / `base_path()` が使えます。

### 振り分けはパスを整えてから

**どちらの入口かを決める前に、パスを 1 回だけ整えます。**

| やること              | 例                                    |
|-----------------------|---------------------------------------|
| 続いた `/` をまとめる | `//storage/x.css` → `/storage/x.css`  |
| `%xx` を元に戻す      | `/%73torage/x.css` → `/storage/x.css` |

- **`%xx` を戻すのは 1 回だけです。** `%2520` は空白になりません。
  `%20` という名前のファイルを指します。
- **`+` は空白にしません。** `/a+b.css` は `public/a+b.css` を指します。
- **`/storage` に化ける書き方で `public/storage/` を見ることはできません。**

### 決まりごと

- ディレクトリなら `index.html` を返します。
  ただし **`/storage/` と `/storage` だけで来たときは 404 です。**
- `..` や絶対パスは受け付けません。
  シンボリックリンクで置き場所の外に出ていないかも確かめます。
- Content-Type は拡張子から決めます。
- `ETag` と `Cache-Control: public, max-age=0, must-revalidate` を付けます。
  `If-None-Match` が合えば **304** を返します（本文は送りません）。
- **`X-Content-Type-Options: nosniff` を付けます。**
- **`/storage/...` には `Content-Security-Policy: sandbox` も付けます。** 利用者が上げた
  `.svg` や `.html` を直接開いても、スクリプトが動かず、フォームも送れません。
  自分で置いた `public/` には付けません。304 にはどちらも付けません。
- `HEAD` ではファイルを読まず、`content-length` だけを返します。

### ETag と 304

| 配信元   | ETag の中身            |
|----------|------------------------|
| ディスク | 長さ ＋ 更新時刻（秒） |
| 埋め込み | 長さ ＋ 中身のハッシュ |

- 弱い ETag（`W/"..."`）です。`If-None-Match` は `*` とカンマ区切りの一覧に
  対応します。`W/` の有無は無視します。
- **同じ秒の内に、同じ長さで書き換えると ETag が変わりません。**
  本番では名前にハッシュを付けたファイル名を使ってください。
- 長く持たせたい（`max-age=31536000` など）ときは、前段のプロキシで上書きしてください。

## ログ

リクエストごとに `GET /posts?page=2 -> 200 (0 ms)` の形で出ます。

**秘密になりうるクエリの値は `***` に伏せます。** 判定は `old()` が覚えない語と
同じ決まりです（[validation.md](validation.md)）。署名付き URL をログに出しても、
`?signature=***&expires=...` となります。
伏せる語に `code` が入っているので、`zip_code` や `country_code` も伏せられます。

細かさは `RUST_LOG` で指定します。
指定がなければ、`APP_DEBUG` が真なら `debug`、そうでなければ `info` です。

## まだ無いもの

まだ無いものは [backlog.md](backlog.md) にまとめてあります。

## 関連

- [routing.md](routing.md)
- [configuration.md](configuration.md)
