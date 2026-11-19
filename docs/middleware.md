# ミドルウェア

ミドルウェアは、リクエストの前後に挟む処理です。Laravel の `app/Http/Middleware/` に当たります。

## 書く

`app/Http/Middleware/` に `.rs` を置くだけです。登録ファイルの編集は要りません。

```rust
// app/Http/Middleware/EnsureAdmin.rs
use bengara::prelude::*;

pub struct EnsureAdmin;

impl EnsureAdmin {
    pub async fn handle(req: Request, next: Next) -> Result<Response> {
        if req.header("x-admin").is_none() {
            return abort(403);
        }
        next.run(req).await
    }
}
```

`next.run(req).await` で先へ進みます。呼ばなければ、そこで止まります。

## 登録する

`bootstrap/app.rs` で登録します。

```rust
Application::configure()
    .with_middleware(|m| {
        m.append(AddPoweredBy::handle)            // 全部のリクエストに掛ける
            .alias("admin", EnsureAdmin::handle)  // 名前を付ける
    })
    .with_routing(|r| r.web(crate::routes::web::routes).health("/up"))
    .create()
```

| メソッド          | すること                             |
|-------------------|--------------------------------------|
| `append(mw)`      | 共通のミドルウェアの**後ろ**に足す   |
| `prepend(mw)`     | 共通のミドルウェアの**前**に足す     |
| `alias(name, mw)` | 名前を付ける。ルートから名前で呼べる |

このページでは、`append` / `prepend` で登録したものを「共通のミドルウェア」と呼びます。

ルートには名前で付けます。`.middleware()` は何回でも書けて、書いた順に通ります。
グループにまとめて付ける書き方は [routing.md](routing.md) にあります。

```rust
// routes/web.rs
Route::get("/admin", AdminController::index).middleware("admin");
```

## 通る順番

```
リクエスト
   ↓  共通のミドルウェア（登録順）
   ↓  ルートに付けたもの（書いた順）
   ↓  ハンドラ
レスポンス（逆順に戻る）
```

外側のミドルウェアほど、最後にレスポンスへ手を入れます。

## ルートに当たらなくても通る

共通のミドルウェアは、ルートに当たらなかったリクエストにも掛かります。

| リクエスト                              | 共通のもの         | ルートに付けたもの |
|-----------------------------------------|--------------------|--------------------|
| ルートに当たった                        | 通る               | 通る               |
| 404・405・静的ファイル                  | 通る               | 通らない           |
| 本文を受け取れなかった（413・400・408） | 通らない           | 通らない           |
| `.stream_body()` のルート               | 本文を読む前に通る | 本文を読む前に通る |

- 本文はルートの照合の後、ミドルウェアより前に受け取ります（[requests-and-responses.md](requests-and-responses.md)）。
- `.stream_body()` のルートでも、申告された長さが上限を超えていれば、ミドルウェアを通らずに 413 です。
- 組み込みの `StartSession` と `VerifyCsrfToken` は、ルートが無いと動きを変えます（[session.md](session.md)）。

自分のミドルウェアで分けたいときは `req.route_matched()` を見ます。

```rust
pub async fn handle(req: Request, next: Next) -> Result<Response> {
    if !req.route_matched() {
        return next.run(req).await;   // 404 には何もしない
    }
    // ルートがあるときだけの処理
    next.run(req).await
}
```

## エラーのときも通る

`next.run(req).await` は**いつも成功**を返します。
内側で `abort(403)` が起きても、ここではもうレスポンスになっています。

```rust
pub async fn handle(req: Request, next: Next) -> Result<Response> {
    let response = next.run(req).await?;   // 403 でもここへ来る
    Ok(response.with_header("x-powered-by", "bengara"))
}
```

- 内側のエラーの中身は見えません。見えるのはステータスだけです。
- ミドルウェアがパニックしても、そのリクエストだけが 500 になります。

## 起動時に止まる場合

| 状況                                          | 止まるときに出るもの |
|-----------------------------------------------|----------------------|
| 登録していない名前を `.middleware()` で使った | 登録済みの名前の一覧 |
| 同じ名前を 2 回 `alias` した                  | 重なった名前         |

## 設定を持たせる

値を持つミドルウェアは、`Middleware` トレイトを自分で実装します。

```rust
// app/Http/Middleware/MaxLength.rs
pub struct MaxLength(pub usize);

impl Middleware for MaxLength {
    fn handle(&self, req: Request, next: Next) -> BoxFuture {
        let limit = self.0;
        Box::pin(async move {
            if req.body().len() > limit {
                return abort_with(413, "本文が長すぎます");
            }
            next.run(req).await
        })
    }
}
```

```rust
// bootstrap/app.rs
.alias("max_length", MaxLength(1024))
```

`self` は `async move` の中へ持ち込めません。要る値を先に取り出します。

## 回数の制限（Throttle）

同じ相手から短い間に何度も来るのを止めます。超えると **429** を返します。

```rust
// bootstrap/app.rs
.alias("throttle", Throttle::per_minute(60))

// routes/web.rs
Route::post("/login", AuthController::login).middleware("throttle");
```

| 作り方                                       | 意味                                              |
|----------------------------------------------|---------------------------------------------------|
| `Throttle::per_minute(60)`                   | 1 分に 60 回                                      |
| `Throttle::new(10, Duration::from_secs(30))` | 30 秒に 10 回                                     |
| `Throttle::from_spec("60,1")?`               | 1 分に 60 回。Laravel の `throttle:60,1` と同じ形 |
| `Throttle::per_minute(5).by_ip()`            | ログイン中でも接続元のアドレスで数える            |

- `from_spec` の 2 つ目（分）は省けます。省くと 1 分です。回数か分が 0 ならエラーです。
- `new` と `per_minute` は、回数 0 か期間 0 を渡すと**起動時にパニック**します。
- 最初の 1 回から数え始め、期間が過ぎたら 0 に戻ります。

| 結果    | 付くヘッダー                                                             |
|---------|--------------------------------------------------------------------------|
| 通った  | `X-RateLimit-Limit` と `X-RateLimit-Remaining`                           |
| **429** | `X-RateLimit-Limit` と `X-RateLimit-Remaining: 0` と `Retry-After`（秒） |

### 誰を数えるか

数えるのは「相手とルートの組」です。

| 相手の状況           | 数える単位                 |
|----------------------|----------------------------|
| ログインしている     | 利用者の ID                |
| していない           | 接続元のアドレス           |
| アドレスが分からない | 数えない（制限せずに通す） |

- **ログイン・登録・パスワード再設定の口には `by_ip()` を付けます。**
  ID で数えると、登録し直すたびに新しい枠ができるためです。
- 共通のミドルウェアにするときは、`StartSession` より後ろに置きます。前に置くと、いつもアドレスで数えます。
- アドレスが分からないのは、アドレスを入れずに `Request` を作ったときだけです。警告を 1 回出します。
- `#[bengara::test]` から送ると `127.0.0.1` です。テストでも制限が掛かります（[testing.md](testing.md)）。

ルートは「ルート名」、無ければ「登録したパス（`/api/items/{id}`）」で見分けます。
末尾に `/` を足しても、パス引数を変えても、枠は増えません。

- 名前の無いルートは、メソッドが違っても同じパスなら同じ枠です。分けたいときは名前を付けます。
- ルートに当たらなかったときは、届いたパスで数えます。

### 前段のプロキシ

前にプロキシ（中継するサーバー）がいると、接続元はいつもプロキシです。
本当の相手を知るために `X-Forwarded-For` と `X-Real-IP` を見ます。 **見るのは `TRUSTED_PROXIES`
に載っている相手から届いたときだけです。**

| `TRUSTED_PROXIES` | 動き                       |
|-------------------|----------------------------|
| 空（既定）        | どちらのヘッダーも見ない   |
| アドレスの列      | 載っている相手からだけ見る |
| `*`               | すべて信頼する             |

- アドレスはカンマで 1 つずつ並べます。`10.0.0.0/8` のような範囲は読めず、警告を出して捨てます。
- IPv4 は `10.0.0.1` の形で書きます。`::ffff:10.0.0.1` で届いても同じ相手とみなします。
- どのアドレスを採るかは [configuration.md](configuration.md) にあります。

### 気をつけること

| 制限                           | どうするか                                                     |
|--------------------------------|----------------------------------------------------------------|
| 回数はプロセスのメモリで数える | プロセスを 2 つ動かすと、実際の上限はおよそ 2 倍になる         |
| 数えるときは 1 本ずつ処理する  | 全部のルートには掛けない。ログイン・登録・送信の口だけに付ける |
| 覚える枠は 1 万件まで          | 超えたら、回数の少ない枠から忘れる                             |

厳密に守りたいときは、前段のプロキシ（nginx の `limit_req` など）で掛けます。

## 一覧を見る

`cargo artisan route:list` の MIDDLEWARE の列に、ルートに付けた名前が出ます。共通のミドルウェアは出ません。

まだ無い機能は [backlog.md](backlog.md) にあります。

## 関連

- [routing.md](routing.md)
- [requests-and-responses.md](requests-and-responses.md)
- [session.md](session.md)
