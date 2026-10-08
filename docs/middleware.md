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
        m.append(AddPoweredBy::handle)        // 全ルートに掛ける
            .alias("admin", EnsureAdmin::handle)  // 名前を付ける
    })
    .with_routing(|r| r.web(crate::routes::web::routes).health("/up"))
    .create()
```

| メソッド          | 中身                                       |
|-------------------|--------------------------------------------|
| `append(mw)`      | 全ルートの**後ろ**に足す                   |
| `prepend(mw)`     | 全ルートの**前**に足す                     |
| `alias(name, mw)` | 名前を付ける。ルート側から呼べるようになる |

**登録していない名前を `.middleware(...)` に書くと、起動時に止まります。**
`with_middleware(...)` を一度も書いていないときも同じです。
黙って素通しにすると、`admin` を付けたつもりの画面が誰にでも見えてしまいます。

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

## ルートが無いときも通る

**`append` / `prepend` で登録した共通のミドルウェアは、ルートに当たらなかった
リクエストにも掛かります。** Laravel と同じです。

| リクエスト               | 共通のミドルウェア |
|--------------------------|--------------------|
| ルートに当たった         | 通る               |
| 404（ルートが無い）      | **通る**           |
| 405（メソッドが違う）    | **通る**           |
| `public/` の静的ファイル | **通る**           |

つまり、セッションの Cookie や独自のヘッダーは **404 の応答にも付きます**。

例外が 1 つあります。

- **`VerifyCsrfToken` は、ルートが無いときは確かめません。**
  無いページへの POST は 419 ではなく 404 のままです。
  トークンも用意しないので、**404 の応答にはセッションの Cookie も付きません**
  （もともと Cookie を持っていた人には付きます）。

自分のミドルウェアで同じことをしたいときは `req.route_matched()` を見ます。

```rust
pub async fn handle(req: Request, next: Next) -> Result<Response> {
    if !req.route_matched() {
        return next.run(req).await;   // 404 には何もしない
    }
    next.run(req).await
}
```

ルートに付けたミドルウェア（`.middleware("admin")`）は、そのルートに当たったときだけ通ります。

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

内側で起きたエラーの中身そのものは、ミドルウェアからは見えません。

## 起動時に止まる場合

| 状況                                          | 起きること                                   |
|-----------------------------------------------|----------------------------------------------|
| 登録していない名前を `.middleware()` で使った | 起動時にパニック。登録済みの名前の一覧を出す |
| 同じ名前を 2 回 `alias` した                  | 起動時にパニック                             |

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

## 状態を持たせたいとき

設定の値を持つミドルウェアは、`Middleware` トレイトを自分で実装します。

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

`self` は future の中へ持ち込めません。必要な値を先に取り出してください。

## 回数の制限（throttle）

同じ相手から短い間に何度も来るのを止めます。

```rust
// bootstrap/app.rs
.alias("throttle", Throttle::per_minute(60))

// routes/web.rs
Route::post("/login", AuthController::login).middleware("throttle");
```

| 作り方                                       | 中身                                    |
|----------------------------------------------|-----------------------------------------|
| `Throttle::per_minute(60)`                   | 1 分に 60 回                            |
| `Throttle::new(10, Duration::from_secs(30))` | 30 秒に 10 回                           |
| `Throttle::from_spec("60,1")?`               | Laravel の `throttle:60,1` と同じ書き方 |

超えると **429** を返し、`Retry-After`（秒）が付きます。
通ったときは `X-RateLimit-Limit` と `X-RateLimit-Remaining` が付きます。

### 誰を数えるか

数えるのは「相手と**ルート名**の組」です。ルートに名前が無いときだけパスで数えます。

ルート名で数えるので、`/api/items/{id}` のようなルートでは `/api/items/1` と
`/api/items/99` が**同じ枠**になります。パスで数えると、相手が値を変えるだけで
枠を無限に増やせてしまいます。**制限を掛けるルートには名前を付けてください。**

相手の決め方は次のとおりです。

| 状況                 | 数える相手                   |
|----------------------|------------------------------|
| ログインしている     | **その人**（利用者の ID）    |
| していない           | **つないできたアドレス**     |
| アドレスが分からない | 数えない。制限を掛けずに通す |

ログインしている人ごとに数えます。
同じ回線にいる別の人（会社や学校）が巻き込まれません。

アドレスが分からないのは、ソケットを使わない呼び出し（テストなど）です。
そのときは**警告を 1 回だけ出して通します**。

### プロキシのヘッダー

**`X-Forwarded-For` と `X-Real-IP` は、`TRUSTED_PROXIES` に載っている相手からのものだけ見ます。**

| `TRUSTED_PROXIES` | 動き                       |
|-------------------|----------------------------|
| 空（既定）        | どちらのヘッダーも見ない   |
| アドレスの列      | 載っている相手からだけ見る |
| `*`               | すべて信頼する             |

- **範囲の書き方（CIDR）は書けません。** アドレスを 1 つずつ並べます。
- 無条件に信じると、偽の値を送るだけで制限を回せます。だから既定は「見ない」です。
- 書き方は [configuration.md](configuration.md) にあります。

### 大事な制限

**回数はプロセスのメモリに数えます。**
プロセスを 2 つ動かすと、それぞれが別々に数えます。
**実際の上限は指定の約 2 倍になります。**

厳密に守りたいときは、前段のプロキシ（nginx の `limit_req` など）で掛けてください。

期限切れの掃除は、**件数ではなく時刻**で間隔を決めます。
どれだけ混んでも、指定した期間に 1 回だけ走ります。

## 動きの前提

- 並びは**起動時に組み立てて固定**します。リクエスト処理中はロックを取りません。
- ミドルウェアがパニックしても、ハンドラと同じく 1 リクエストに閉じ込めて 500 を返します。

## まだ無いもの

ミドルウェアへの引数（`throttle:60,1`）と、優先順の指定はありません。
[backlog.md](backlog.md) を参照してください。

ルートのグループはあります（[routing.md](routing.md)）。

## 関連

- [routing.md](routing.md)
- [requests-and-responses.md](requests-and-responses.md)
