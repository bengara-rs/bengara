# ミドルウェア

リクエストの前後に処理を挟む仕組みです。Laravel の `app/Http/Middleware/` に当たります。

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

- 形は `async fn handle(req: Request, next: Next) -> Result<Response>` です。
- `next.run(req).await` で先へ進みます。
- 呼ばなければ、そこで止まります。`abort(403)` がその形です。

コントローラと同じく、**トレイトを手で実装する必要はありません**。

## 登録する

`bootstrap/app.rs` で行います。

```rust
Application::configure()
    .with_middleware(|m| {
        m.append(AddPoweredBy::handle)            // 全ルートに掛ける
            .alias("admin", EnsureAdmin::handle)  // 名前を付ける
    })
    .with_routing(|r| r.web(crate::routes::web::routes).health("/up"))
    .create()
```

| メソッド | 中身 |
|---|---|
| `append(mw)` | 全ルートの**後ろ**に足す |
| `prepend(mw)` | 全ルートの**前**に足す |
| `alias(name, mw)` | 名前を付ける。ルート側から呼べるようになる |

## ルートに付ける

```rust
// routes/web.rs
Route::get("/admin", AdminController::index)
    .middleware("admin")
    .name("admin.index");
```

`.middleware()` は何回でも書けます。書いた順に通ります。

## 通る順番

```
リクエスト
   ↓  append / prepend で登録したもの（登録順）
   ↓  ルートに付けたもの（書いた順）
   ↓  ハンドラ
レスポンス（逆順に戻る）
```

戻りは逆順です。**外側のミドルウェアが最後にレスポンスへ手を入れます。**

## エラーのときも通る

`next.run(req).await` が返すのは**いつも成功**です。
内側で `abort(403)` が起きても、ここではもうレスポンスになっています。

```rust
pub async fn handle(req: Request, next: Next) -> Result<Response> {
    let response = next.run(req).await?;   // 403 でもここへ来る
    Ok(response.with_header("x-powered-by", "bengara"))
}
```

こうしてあるのは、レスポンスに手を入れるミドルウェアが、
エラーのときだけ素通りされるのを防ぐためです。

内側で起きたエラーの中身（`Error` そのもの）はミドルウェアからは見えません。
必要になったら足します（[backlog.md](backlog.md)）。

## 起動時に止まる場合

| 状況 | 起きること |
|---|---|
| 登録していない名前を `.middleware()` で使った | 起動時にパニック。登録済みの名前の一覧を出す |
| 同じ名前を 2 回 `alias` した | 起動時にパニック |

どちらも `create()` のときに気づけます。リクエストが来てから落ちることはありません。

## 一覧を見る

```sh
cargo artisan route:list
```

ミドルウェアが 1 つでもあれば、MIDDLEWARE の列が出ます。

```
METHOD  URI            NAME         MIDDLEWARE
GET     /              home
GET     /admin         admin.index  admin
```

## 回数の制限（throttle）

```rust
// bootstrap/app.rs
.alias("throttle", Throttle::per_minute(60))

// routes/web.rs
Route::post("/login", AuthController::login).middleware("throttle");
```

| 作り方 | 中身 |
|---|---|
| `Throttle::per_minute(60)` | 1分に60回 |
| `Throttle::new(10, Duration::from_secs(30))` | 30秒に10回 |
| `Throttle::from_spec("60,1")?` | Laravel の `throttle:60,1` と同じ書き方 |

超えると **429** を返し、`Retry-After`（秒）が付きます。
通ったときは `X-RateLimit-Limit` と `X-RateLimit-Remaining` が付きます。

数えるのは「相手とパスの組」です。相手は `X-Forwarded-For` の先頭 → `X-Real-IP` の順で見ます。

### 大事な制限

**回数はプロセスのメモリに数えます。**
プロセスを2つ動かすと、それぞれが別々に数えるので、**実際の上限は指定の約2倍になります。**

厳密に守りたいときは、前段のプロキシ（nginx の `limit_req` など）で掛けてください。
共通の置き場所に載せ替えるのは、キャッシュを作ってからです（[backlog.md](backlog.md)）。

## 状態を持たせたいとき

設定を持つミドルウェアは、`Middleware` トレイトを自分で実装します。

```rust
pub struct MaxLength(pub usize);

impl Middleware for MaxLength {
    fn handle(&self, req: Request, next: Next) -> BoxFuture {
        let limit = self.0;
        Box::pin(async move {
            if req.body().len() > limit {
                return abort(413);
            }
            next.run(req).await
        })
    }
}
```

`self` は future の中へ持ち込めないので、必要な値を先に取り出してください。

## 動きの前提

- 並びは**起動時に組み立てて固定**します。リクエスト処理中はロックを取りません。
- ミドルウェアがパニックしても、ハンドラと同じく 1 リクエストに閉じ込めて 500 を返します。

## まだ無いもの

ミドルウェアへの引数（`throttle:60,1`）と、優先順の指定はまだありません。
[backlog.md](backlog.md) を参照してください。

ルートのグループはあります（[routing.md](routing.md)）。

---

関連: [routing.md](routing.md) / [requests-and-responses.md](requests-and-responses.md)
