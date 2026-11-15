# リクエストとレスポンス

届いた入力を読む方法と、返すものを作る方法です。このページで分かることは 3 つです。

- `Request` から何を読めるか
- `Response`・リダイレクト・エラーの作り方
- 静的ファイルの配られ方

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

## Request から読む

| 分類       | メソッド                                                          |
|------------|-------------------------------------------------------------------|
| 基本       | `method()` `path()` `query_string()` `full_path()` `route_name()` |
| ルート     | `route_matched()`                                                 |
| 接続元     | `ip()`                                                            |
| パス引数   | `param(name)` `param_as::<T>(name)` `params()`                    |
| クエリ     | `query(key)` `query_all()` `page()`                               |
| ヘッダー   | `header(name)` `headers()`                                        |
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

くわしい説明が別のページにあるものです。

| メソッド                                     | 書いてあるページ                         |
|----------------------------------------------|------------------------------------------|
| `input` / `form` / `validate`                | [validation.md](validation.md)           |
| `session` / `try_session` / `csrf_token` / `cookie` | [session.md](session.md)          |
| `auth`                                       | [authentication.md](authentication.md)   |
| `route_matched`                              | [middleware.md](middleware.md)           |

### 読むときの決まり

| メソッド                 | 決まり                                                        |
|--------------------------|---------------------------------------------------------------|
| `method()`               | 届いた文字列のまま。**大文字小文字を区別する**                |
| `header(name)`           | 名前の大文字小文字は区別しない。同じ名前が何本もあれば 1 本目 |
| `query` / `query_all`    | `%xx` と `+` を元に戻す。同じ名前が何個もあれば 1 個目        |
| `param` / `param_as`     | `%xx` を元に戻す。**`+` は空白にしない**                      |
| `page()`                 | `?page=2` の番号。無い・読めない・0 なら 1                    |
| `json::<T>()`            | 読めないとエラー。そのまま返すと **500**                      |
| `session()` / `csrf_token()` | `StartSession` を登録していないと**パニック**。無くてもよい場面は `try_session()` |

- 自分でメソッドを比べるときは、`req.method() == "POST"` のように大文字で書きます。
- `input` / `input_all` / `form_all` / `cookies` / `query` は、何度呼んでも遅くなりません。
  中身の解析は 1 リクエストにつき 1 回だけです。
- 相手が壊れた JSON を送ってきたことを 500 にしたくないときは、
  `req.json()` のエラーを自分で 400 や 422 に変えます。

### パス引数とモデル

`param_as::<T>` と `model::<T>` は、エラーを返すので `?` を付けて使えます。

| 起きたこと                                    | 結果                                    |
|-----------------------------------------------|-----------------------------------------|
| 値が型に合わない（`/posts/abc` を数値で読む） | **404**                                 |
| その値の行が無い（`model` だけ）              | **404**                                 |
| ルートにその名前のパス引数が無い              | **500**（ルートかハンドラの書き間違い） |

`model::<T>(名前)` は、パス引数の値を主キーとしてモデルを 1 件読みます
（[models.md](models.md)）。
整数に読めない値（`/posts/abc`）では、**データベースに問い合わせずに** 404 を返します。
データベースが止まっていても、このときは 404 です。

### 接続元を見る

```rust
let ip = req.ip();     // Option<std::net::IpAddr>
```

- つないできた相手のアドレスです。前段にプロキシがいれば、そのプロキシです。
- `X-Forwarded-For` は見ません。
- `#[bengara::test]` から送ったリクエストでは `127.0.0.1` になります。

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

### 本文の上限

本文の上限は 2 MiB です。

| 状況                                       | 返すもの |
|--------------------------------------------|----------|
| 申告された長さが上限を超えている           | **413**  |
| 読んでいる途中で失敗した                   | **400**  |
| 長さの申告が無いまま上限を超えた           | **400**  |
| 30 秒たっても読み終わらない                | **408**  |

- この 3 つのステータスは、ミドルウェアを通る前に返します。
- 接続そのものの待ち時間と、同時接続数の上限はありません。
  前段のプロキシで掛けます（[deployment.md](deployment.md)）。

## Response を作る

```rust
Response::text("ok")
Response::html("<h1>hi</h1>")
Response::json(&value)?        // Result<Response>
Response::no_content()         // 204
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

外から来た文字列は `escape_html` を通します。
通さないと、意図しないタグを書き込まれます（クロスサイトスクリプティング）。

```rust
pub async fn hello(req: Request) -> Result<Response> {
    let name = req.param("name").unwrap_or("世界");
    html(format!("<p>{} さん、こんにちは", escape_html(name)))
}
```

### JSON を作るとき

`json!` マクロは `prelude` に入っているので、そのまま書けます。

```rust
json(&json!({ "status": "ok" }))
```

- 関数の `json()` とマクロの `json!` は、同じファイルで両方使えます。
- `serde` と `serde_json` は `bengara::serde` / `bengara::serde_json` から使えます。
  `Cargo.toml` に書く必要はありません。

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
| `with_added_header(name, value)` | ヘッダーを**足す**。同じ名前を何本も送るとき               |
| `with_cookie(&cookie)`           | Cookie を 1 本足す（[session.md](session.md)）             |
| `with_body(body)`                | 本文を差し替える                                           |

読むほうは `status()` `header(name)` `headers()` `body()` `body_text()` です。
ヘッダーの名前の大文字小文字は区別しません。

決まりごとが 3 つあります。

- **ステータスは 100〜599 だけです。** 範囲の外（`abort(1000)` や `Response::new(0)`）は
  警告を出して **500** にします。
- **ヘッダーの値に改行などの制御文字が入っていると、その 1 本だけを入れません。**
  警告が 1 行出ます。水平タブだけは通ります。すでに入っていた値は残ります。
- **1xx・204・304 では、本文を付けません。** `content-type` と `content-length` も
  付きません。`abort(204)` から来た応答も、自分で組んだ応答も同じです。

## リダイレクト

```rust
redirect().to("/login")                                 // 302
redirect().route("home")
redirect().route_with("posts.show", &[("post", "12")])
redirect().permanent().to("/new")                       // 301
redirect().with_status(303).to("/other")                // 303
```

- どれも `Result<Response>` を返します。
- **`permanent()` と `with_status()` は `to()` より先に書きます。**
  `to()` / `route()` / `route_with()` の後ろには続けられません。
- **行き先に改行などの制御文字が入っていると、エラーを返します。**
  `?` で上に返せば **500** になります。

### 外から来た値を行き先にしない

**`to()` は行き先を選びません。** `https://evil.example` も `//evil.example` も
そのまま `Location` に入ります。`?next=...` の値をそのまま渡すと、
**自分のサイトのリンクを踏んだ人を、よそのサイトへ飛ばせます**
（偽のログイン画面へ誘う手口に使われます）。

| 場面                         | 使うもの                                                             |
|------------------------------|----------------------------------------------------------------------|
| ログイン後に元のページへ戻す | `req.session().intended()`（[authentication.md](authentication.md)） |
| 決まった行き先へ送る         | `redirect().route(...)` / `redirect().to("/固定のパス")`             |
| 外から来た値をどうしても使う | **自分で確かめてから**渡す                                           |

自分で確かめるときは、少なくとも次の 3 つを見ます。

- `/` で始まる
- `//` で始まらない
- `\` と制御文字を含まない

## 中断する（abort）

```rust
return abort(403);
return abort_with(404, "記事が見つかりません");
```

`abort` / `abort_with` は `Result<T>` を返すので、`return` でそのまま書けます。

## エラー

ハンドラが `Err` を返すと、エラーの種類でステータスが決まります。

| 種類                              | ステータス | 中身                               |
|-----------------------------------|------------|------------------------------------|
| `Error::Http { status, message }` | 指定した値 | `abort` / `abort_with` が作る      |
| `Error::Validation`               | **422**    | 入力の検査に落ちた                 |
| `Error::Message`                  | 500        | 文字列のエラー                     |
| `Error::Io`                       | 500        | `std::io::Error` から変換          |
| `Error::Json`                     | 500        | `serde_json::Error` から変換       |
| `Error::Other`                    | 500        | ほかのエラーを包む。原因をたどれる |

| 作り方                 | 中身                                 |
|------------------------|--------------------------------------|
| `Error::msg("...")`    | 文字列のエラー                       |
| `Error::http(404, ..)` | ステータスを指定する                 |
| `Error::other(e)`      | ほかのエラーを包む。元のエラーを残す |

- `io::Error` と `serde_json::Error` は、`?` でそのまま変換されます。
- 500 以上のエラーは `error` のログに出ます。

### HTML で返るか JSON で返るか

エラーの形は、リクエストを見て決めます（Laravel の `expectsJson()` と同じ考え方）。
次のどれかに当たれば JSON、当たらなければ HTML の画面です。

| 見るもの           | JSON になる条件                                  |
|--------------------|--------------------------------------------------|
| `Accept`           | `application/json` か `+json` を含む             |
| `Accept`           | `text/html` が無く `*/*` がある（`curl` の既定） |
| `X-Requested-With` | `XMLHttpRequest`                                 |
| `Content-Type`     | 自分が JSON を送ってきた                         |

```json
{
  "message": "記事が見つかりません"
}
```

| 場合                           | `message` の中身                           |
|--------------------------------|--------------------------------------------|
| `abort_with` で文を渡した      | その文                                     |
| 文が無い（`abort(404)` や 500）| 英語の決まり文句（`Not Found` など）       |
| 入力の検査（422）              | 項目ごとの理由も入る（[validation.md](validation.md)） |

**`APP_DEBUG=true` のときだけ、詳しい内容（原因の連鎖）が出ます。**
本番（`APP_DEBUG=false`）では、500 の中身は外に出しません。

### エラーの形を差し替える

```rust
// bootstrap/app.rs
Application::configure()
    .with_exceptions(|e| {
        e.render(|error| {
            (error.status() == 404).then(|| {
                Response::json(&json!({ "message": "そのページはありません" }))
                    .unwrap_or_else(|_| Response::text("not found"))
                    .with_status(404)
            })
        })
    })
```

- 登録した関数を**上から順に試し**、最初に `Some` を返したものを使います。
- どれも返さなければ、bengara の既定の形になります。
- `render` は何回でも呼べます。

## 404・405・HEAD

ハンドラを書かなくても、bengara が返します。

| 状況                                       | 返すもの                               |
|--------------------------------------------|----------------------------------------|
| ルートが無く、静的ファイルも無い           | 404                                    |
| パスは当たるが、メソッドが違う             | 405 と `allow` ヘッダー                |
| `GET` のルートに `HEAD` が来た             | `GET` と同じヘッダーで、本文だけが空   |

- 細かい決まりは [routing.md](routing.md) にあります。
- `HEAD` の応答には `Content-Length` が付きます。中身は `GET` で返るはずの長さです。
- 共通のミドルウェアは 404 と 405 にも掛かります（[middleware.md](middleware.md)）。

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
- パスの組み立てには `public_path(rel)` / `storage_path(rel)` / `base_path()` が使えます。

### パスの読み方

どちらの入口かを決める前に、パスを 1 回だけ整えます。

| やること              | 例                                    |
|-----------------------|---------------------------------------|
| 続いた `/` をまとめる | `//storage/x.css` → `/storage/x.css`  |
| `%xx` を元に戻す      | `/%73torage/x.css` → `/storage/x.css` |

- `%xx` を戻すのは 1 回だけです。`%2520` は空白になりません。
- **`+` は空白にしません。** `/a+b.css` は `public/a+b.css` を指します。
- `..` や絶対パスは受け付けません。
  シンボリックリンクで置き場所の外に出ているファイルも返しません。

### `/storage` の決まり

- **`public/storage/` の中身は、どんな書き方でも配りません。**
  ファイルは `storage/app/public/` に置きます。
- **配るのは小文字の `/storage/...` だけです。** `/STORAGE/x.css` は 404 です。
  どの OS でも同じです。`public/Storage/` のような名前のディレクトリも配りません。
- `/storage` と `/storage/` だけで来たときは 404 です。

### 付けるヘッダー

| ヘッダー                                 | 付く場面                                   |
|------------------------------------------|--------------------------------------------|
| `Content-Type`                           | 拡張子から決める。分からなければ `application/octet-stream` |
| `ETag`                                   | いつも                                     |
| `Cache-Control: public, max-age=0, must-revalidate` | いつも                          |
| `X-Content-Type-Options: nosniff`        | 304 以外                                   |
| `Content-Security-Policy: sandbox`       | `/storage/...` だけ（304 以外）            |

- ディレクトリを指すパスには、その中の `index.html` を返します。
- `If-None-Match` が合えば **304** を返します。本文は送りません。
- `Content-Security-Policy: sandbox` は、利用者が上げた `.svg` や `.html` を
  直接開かれたときに、スクリプトを動かさないためのものです。
- `HEAD` ではファイルを読まず、`Content-Length` だけを返します。

### ETag

| 配信元   | ETag の中身            |
|----------|------------------------|
| ディスク | 長さ ＋ 更新時刻（秒） |
| 埋め込み | 長さ ＋ 中身のハッシュ |

- `If-None-Match` は `*` とカンマ区切りの一覧に対応します。`W/` の有無は無視します。
- **同じ秒の内に、同じ長さで書き換えると ETag が変わりません。**
  本番では、名前にハッシュを付けたファイル名を使います。
- 長く持たせたい（`max-age=31536000` など）ときは、前段のプロキシで上書きします。

### 大きなファイルは前段で配る

**ディスクのファイルは、全部をメモリに読んでから返します。**
大きさの上限はありません。途中からの取得（`Range`）にも応えません。

- 同時に取りに来た数だけメモリを使います。
  200 MB の動画を 10 人が同時に取ると、約 2 GB です。
- 画像・CSS・JavaScript のような小さなファイルなら、気にしなくて構いません。
- **動画や大きなダウンロード用のファイルは、前段のプロキシ（nginx など）で配ります。**
  利用者が上げたファイルを `/storage/...` で配るときは、特に気をつけます。

## ログ

リクエストごとに `GET /posts?page=2 -> 200 (0 ms)` の形で出ます。

- **秘密になりうるクエリの値は `***` に伏せます。** 署名付き URL は
  `?signature=***&expires=...` と出ます。
- 伏せる名前の決まりは、`old()` が覚えない項目と同じです（[validation.md](validation.md)）。
  `code` が入っているので、`zip_code` や `country_code` も伏せます。
- 細かさは `RUST_LOG` で指定します。
  指定がなければ、`APP_DEBUG` が真なら `debug`、そうでなければ `info` です。

## まだ無いもの

まだ無いものは [backlog.md](backlog.md) にまとめてあります。

## 関連

- [routing.md](routing.md)
- [middleware.md](middleware.md)
- [configuration.md](configuration.md)
