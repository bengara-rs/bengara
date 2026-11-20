# リクエストとレスポンス

届いた入力の読み方と、返すものの作り方です。ハンドラは `Result<Response>` を返します。

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
| ルート     | `route_matched()`（ルートに当たったか）                           |
| 接続元     | `ip()`                                                            |
| パス引数   | `param(name)` `param_as::<T>(name)` `params()`                    |
| クエリ     | `query(key)` `query_all()` `page()`                               |
| ヘッダー   | `header(name)` `headers()`                                        |
| Cookie     | `cookie(name)` `cookies()`                                        |
| 入力       | `input(key)` `input_all()` `form(key)` `form_all()`               |
| 本文       | `body()` `body_text()` `json::<T>()` `body_stream()`              |
| 検査       | `validate(rules)` `validate_as::<T>()`                            |
| セッション | `session()` `try_session()` `csrf_token()`                        |
| ログイン   | `auth()`                                                          |
| モデル     | `model::<T>(param)`                                               |
| WebSocket  | `websocket()` `wants_websocket()`                                 |

```rust
let body: MyInput = req.json()?;
let post = req.model::<Post>("post").await?;          // 無ければ 404
let input = req.validate(&[("title", "required")])?;  // 落ちれば 422
```

入力と検査は [validation.md](validation.md)、セッションと Cookie は [session.md](session.md)、
`auth()` は [authentication.md](authentication.md)、`websocket()` は [websocket.md](websocket.md) にあります。

### 読むときの決まり

| メソッド                     | 決まり                                                             |
|------------------------------|--------------------------------------------------------------------|
| `method()`                   | 届いたまま。大文字小文字を区別する（比べるときは `"POST"` と書く） |
| `header(name)`               | 名前の大文字小文字は区別しない。同じ名前が何本もあれば 1 本目      |
| `query` / `query_all`        | `%xx` と `+` を元に戻す。同じ名前が何個もあれば 1 個目             |
| `param` / `param_as`         | `%xx` を元に戻す。`+` は空白にしない                               |
| `page()`                     | `?page=2` の番号。無い・読めない・0 なら 1                         |
| `json::<T>()`                | 読めないとエラー。そのまま返すと 500                               |
| `session()` / `csrf_token()` | `StartSession` が無いとパニック。無くてよい場面は `try_session()`  |
| `ip()`                       | 実際の接続元。`X-Forwarded-For` は見ない                           |

- 壊れた JSON を 500 にしたくないときは、`req.json()` のエラーを自分で 400 や 422 に変えます。
- 同じ名前のヘッダーを全部見たいときは、`req.headers()` を回します。
- `X-Forwarded-For` の 1 本目は相手が勝手に付けた値かもしれません。そのまま信用しないでください。
- `#[bengara::test]` から送ると、`ip()` は `127.0.0.1` です。

### パス引数とモデル

`param_as::<T>` と `model::<T>` は `?` を付けて使えます。

| 起きたこと                                    | 結果                                |
|-----------------------------------------------|-------------------------------------|
| 値が型に合わない（`/posts/abc` を数値で読む） | 404                                 |
| その値の行が無い（`model` だけ）              | 404                                 |
| ルートにその名前のパス引数が無い              | 500（ルートかハンドラの書き間違い） |

`model::<T>(名前)` は、パス引数の値を主キーとして 1 件読みます（[models.md](models.md)）。

### 本文の上限

本文の上限は、既定で 2 MiB（2097152 バイト）です。

| 決める場所                               | 効く範囲                                   |
|------------------------------------------|--------------------------------------------|
| `.env` の `APP_MAX_BODY_BYTES`（バイト） | `.max_body` の無いルート全部               |
| ルートの `.max_body(バイト)`             | そのルートだけ（[routing.md](routing.md)） |

```rust
Route::post("/imports", ImportController::store).max_body(16 * 1024 * 1024);
```

- `APP_MAX_BODY_BYTES` は起動時に 1 回だけ読みます。`0` や読めない値は、警告を出して既定にします。
- ルートの `.max_body(0)` は「本文を受け付けない」です。

| 状況                                   | 返すもの |
|----------------------------------------|----------|
| 本文が上限を超えた                     | 413      |
| 途中で接続が切れた・送り方が壊れていた | 400      |
| 合計 30 秒たっても読み終わらない       | 408      |

- 本文はミドルウェアより前に受け取ります。この 3 つは、ミドルウェアも `with_exceptions` も通りません。
- 何 MB もある本文は、次の「本文を少しずつ読む」を使います。
- 接続の待ち時間と同時接続数の上限は、前段のプロキシで掛けます（[deployment.md](deployment.md)）。

### 本文を少しずつ読む

大きな本文を、全部メモリに載せずに扱えます。
ルートに `.stream_body()` を付け、ハンドラは `req.body_stream()?` で受け取ります。

```rust
// routes/web.rs
Route::post("/uploads/{name}", UploadController::store)
    .middleware("auth")
    .stream_body()
    .max_body(512 * 1024 * 1024);
```

```rust
pub async fn store(req: Request) -> Result<Response> {
    let mut body = req.body_stream()?;
    while let Some(bytes) = body.chunk().await? {
        // bytes: Vec<u8> を使う
    }
    text(format!("{} バイト受け取りました", body.received()))
}
```

- ファイルに書くだけなら、`Storage::disk(..).write_stream(path, body)` が使えます（[storage.md](storage.md)）。
- 型は `bengara::BodyStream` です（`prelude` には入っていません）。

| メソッド        | 返すもの                                |
|-----------------|-----------------------------------------|
| `chunk().await` | `Result<Option<Vec<u8>>>`。次のかたまり |
| `received()`    | ここまで読んだバイト数（`u64`）         |
| `limit()`       | 上限のバイト数（`u64`）                 |

`chunk()` の結果は次のとおりです。

| 起きたこと                   | 結果                        |
|------------------------------|-----------------------------|
| 次のかたまりが届いた         | `Ok(Some(バイト列))`        |
| 本文の終わり                 | `Ok(None)`（以後も `None`） |
| 読んだ合計が上限を超えた     | `Err`（413）                |
| 次のかたまりが 30 秒届かない | `Err`（408）                |
| 途中で接続が切れた           | `Err`（400）                |

- `?` で返せば、その状態コードの応答になります。
- 申告された長さ（`Content-Length`）が上限を超えていれば、ハンドラを呼ばずに 413 です。
- 時間の上限は「次のかたまりが 30 秒届かない」だけです。**全体の時間は数えません。**
  わざと少しずつ送り続ける相手は、前段のプロキシで切ります。

`body_stream()` は次のとき 500 のエラーになります。

- `.stream_body()` の無いルートで呼んだ（本文はもう読んである）
- 2 回目に呼んだ

`.stream_body()` のルートでは、ほかの読み方が変わります。

| 読み方                                            | 動き                                                      |
|---------------------------------------------------|-----------------------------------------------------------|
| `body()` `input()` `form()` `json()` `validate()` | 本文は見えない（空）。クエリは見える                      |
| CSRF の検査                                       | 本文の `_token` は読めない。`X-CSRF-TOKEN` ヘッダーで送る |
| ミドルウェア                                      | 本文を読む前に通る                                        |

- ミドルウェアが先に通るので、ログインしていない相手の大きな送信を、受け取らずに断れます。
- **受け取っているあいだ、同じセッションのほかのリクエストは待たされます**（[session.md](session.md)）。

## Response を作る

```rust
Response::text("ok")
Response::html("<h1>hi</h1>")
Response::json(&value)?        // Result<Response>
Response::no_content()         // 204
Response::bytes("image/png", data)
Response::new(204)
```

関数の `text(..)` `html(..)` `json(&..)` は `Result<Response>` を返します。そのまま戻り値に書けます。

```rust
pub async fn index() -> Result<Response> {
    html("<h1>hello</h1>")
}
```

テンプレートから画面を作るときは `req.view(..)` を使います（機能フラグ `view` が要ります。[views.md](views.md)）。
フラグが無くても、`html()` に文字列を渡すか、JSON を返せます。

### HTML に文字列を埋め込む

外から来た文字列は `escape_html` を通します。
通さないと、勝手なスクリプトを埋め込まれます（XSS と呼ばれる攻撃）。

```rust
pub async fn hello(req: Request) -> Result<Response> {
    let name = req.param("name").unwrap_or("世界");
    html(format!("<p>{} さん、こんにちは", escape_html(name)))
}
```

### JSON を作る

`json!` マクロは `prelude` に入っています。

```rust
json(&json!({ "status": "ok" }))
```

`serde` と `serde_json` は `bengara::serde` / `bengara::serde_json` から使えます。`Cargo.toml` に足す必要はありません。

### 整える

```rust
Response::text("ok")
    .with_status(201)
    .with_header("X-Foo", "bar")
    .with_body("changed")
```

| メソッド                         | すること                                       |
|----------------------------------|------------------------------------------------|
| `with_status(code)`              | ステータスを変える                             |
| `with_header(name, value)`       | ヘッダーを**置き換える**                       |
| `with_added_header(name, value)` | ヘッダーを**足す**（同じ名前を何本も送るとき） |
| `with_cookie(&cookie)`           | Cookie を 1 本足す（[session.md](session.md)） |
| `with_body(body)`                | 本文を差し替える                               |

読むほうは `status()` `header(name)` `headers()` `body()` `body_text()` です。

`{:?}`（`Debug`）で出すと、`Request` も `Response` も秘密を伏せます。
Cookie・`Authorization` などのヘッダーの値は `***`、本文は長さだけです。

| 場合                                   | 動き                                              |
|----------------------------------------|---------------------------------------------------|
| ステータスが 100〜599 の外             | 警告を出して 500 にする                           |
| ヘッダーの値に改行などの制御文字がある | その 1 本だけ入れない（タブは通す）。警告が出る   |
| ステータスが 1xx・204・304             | 本文と `content-type` `content-length` を付けない |

## リダイレクト

別の URL へ移ってもらう応答です。

```rust
redirect().to("/login")                                 // 302
redirect().route("home")
redirect().route_with("posts.show", &[("post", "12")])
redirect().permanent().to("/new")                       // 301
redirect().with_status(303).to("/other")                // 303
```

- どれも `Result<Response>` を返します。
- `permanent()` と `with_status()` は `to()` より先に書きます。
- `with_status()` に使えるのは 201・301・302・303・307・308 だけです。ほかの値ではエラーになります。
- 行き先に改行などの制御文字があるとエラーです。`?` で返せば 500 です。
- 行き先の空白と日本語などは `%XX` にして送ります（`/記事` → `/%E8%A8%98%E4%BA%8B`）。

### 外から来た値を行き先にしない

**`to()` は行き先を確かめません。** `?next=...` の値をそのまま渡すと、よそのサイトへ飛ばされます。

- ログイン後に元のページへ戻すなら、`req.session().intended()` を使います（[authentication.md](authentication.md)）。
- 決まった行き先なら、`redirect().route(...)` か固定のパスを使います。
- どうしても使うなら、少なくとも次を確かめます。「`/` で始まる」「`//` で始まらない」「`\` と制御文字を含まない」。

## 中断する（abort）

```rust
return abort(403);
return abort_with(404, "記事が見つかりません");
```

## エラー

ハンドラが `Err` を返すと、エラーの種類でステータスが決まります。

| 種類                                            | ステータス |
|-------------------------------------------------|------------|
| `Error::Http`（`abort` / `Error::http` が作る） | 指定した値 |
| `Error::Validation`（入力の検査に落ちた）       | 422        |
| それ以外（`Message` `Io` `Json` `Other`）       | 500        |

| 作り方                 | 中身                                 |
|------------------------|--------------------------------------|
| `Error::msg("...")`    | 文字列のエラー                       |
| `Error::http(404, ..)` | ステータスを決めたエラー             |
| `Error::other(e)`      | ほかのエラーを包む。元のエラーを残す |

- `io::Error` と `serde_json::Error` は `?` でそのまま変換されます。
- 500 以上のエラーはログに `error` で出ます。

### HTML で返すか JSON で返すか

次のどれかに当たれば JSON、当たらなければ HTML の画面で返します。

| 見るもの           | JSON になる条件                                  |
|--------------------|--------------------------------------------------|
| `Accept`           | `application/json` か `+json` を含む             |
| `Accept`           | `text/html` が無く `*/*` がある（`curl` の既定） |
| `X-Requested-With` | `XMLHttpRequest`                                 |
| `Content-Type`     | 相手が JSON を送ってきた                         |

```json
{ "message": "記事が見つかりません" }
```

- `abort_with` で渡した文が `message` に入ります。
- 文が無いとき（`abort(404)` や 500）は、`Not Found` のような英語の決まり文句です。
- 入力の検査（422）では、項目ごとの理由も入ります（[validation.md](validation.md)）。
- **`APP_DEBUG=true` のときだけ、原因の連鎖まで出します。** 本番では 500 の中身を出しません。

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

- `render` は何回でも書けます。上から試し、最初に `Some` を返したものを使います。
- どれも返さなければ、既定の形になります。

## 静的ファイル

ルートに当たらなかった `GET` / `HEAD` は、ファイルを探します。パスの頭で探す先が分かれます。

| パス           | 探す先                                                       |
|----------------|--------------------------------------------------------------|
| `/storage/...` | `storage/app/public/` だけ                                   |
| それ以外       | 埋め込んだ `public/`（リリースビルド）→ ディスクの `public/` |

- **リリースビルドでは `public/` がバイナリに入ります。** 本番に `public/` を置かなくて済みます。
- デバッグビルドでは毎回ディスクを読みます。直せばすぐ反映されます。
- パスの組み立てには `public_path(rel)` / `storage_path(rel)` / `base_path()` が使えます。

### パスの決まり

- `..` や絶対パスは受け付けません。シンボリックリンクで外へ出たファイルも返しません。
- `public/storage/` の中身は配りません。ファイルは `storage/app/public/` に置きます。
- 配るのは小文字の `/storage/...` だけです。`/STORAGE/x.css` と `/storage` だけのパスは 404 です。
- ディレクトリを指すパスには、中の `index.html` を返します。

### 付けるヘッダー

| ヘッダー                                            | 付けるとき                                                          |
|-----------------------------------------------------|---------------------------------------------------------------------|
| `Content-Type`                                      | いつも。拡張子から決める（不明なら `application/octet-stream`）     |
| `Cache-Control: public, max-age=0, must-revalidate` | いつも                                                              |
| `ETag`（中身の目印）                                | いつも。`If-None-Match` が合えば 304 を返す                         |
| `X-Content-Type-Options: nosniff`                   | 304 以外                                                            |
| `Content-Security-Policy: sandbox`                  | `/storage/...` だけ。上げられた `.svg` などのスクリプトを動かさない |

- 長く持たせたい（`max-age=31536000` など）ときは、前段のプロキシで上書きします。

### 大きなファイルは前段で配る

**ディスクのファイルは、全部をメモリに読んでから返します。** 途中からの取得（`Range`）にも答えません。
200 MB の動画を 10 人が同時に取ると、約 2 GB 使います。 **動画や大きなファイルは、前段のプロキシ（nginx など）で配ります。**

## ログ

リクエストごとに `GET /posts?page=2 -> 200 (0 ms)` の形で出ます。

- 秘密になりうる名前のクエリの値は `***` に伏せます（`?signature=***` など）。
- 同じ名前のパス引数も伏せます。`/reset/{token}` のルートは `GET /reset/*** -> 200` と出ます。
- 伏せる名前は、`old()` が覚えない項目と同じです（[validation.md](validation.md)）。
- 細かさは `RUST_LOG` で決めます。無ければ、`APP_DEBUG` が真なら `debug`、偽なら `info` です。

## 関連

- [routing.md](routing.md)
- [middleware.md](middleware.md)
- [configuration.md](configuration.md)
