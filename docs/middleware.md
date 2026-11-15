# ミドルウェア

リクエストの前後に処理を挟む仕組みです。Laravel の `app/Http/Middleware/` に当たります。
このページで分かることは 3 つです。

- ミドルウェアの書き方と登録のしかた
- どの順に、どのリクエストに掛かるか
- 回数の制限（`Throttle`）の使い方

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
- `next.run(req).await` で先へ進みます。呼ばなければ、そこで止まります。

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

| メソッド          | 中身                                       |
|-------------------|--------------------------------------------|
| `append(mw)`      | 共通のミドルウェアの**後ろ**に足す         |
| `prepend(mw)`     | 共通のミドルウェアの**前**に足す           |
| `alias(name, mw)` | 名前を付ける。ルート側から呼べるようになる |

このページでは、`append` / `prepend` で登録したものを「共通のミドルウェア」と呼びます。

## ルートに付ける

```rust
// routes/web.rs
Route::get("/admin", AdminController::index)
    .middleware("admin")
    .name("admin.index");
```

`.middleware()` は何回でも書けます。書いた順に通ります。
グループにまとめて付ける書き方は [routing.md](routing.md) にあります。

## 通る順番

```
リクエスト
   ↓  共通のミドルウェア（登録順）
   ↓  ルートに付けたもの（書いた順）
   ↓  ハンドラ
レスポンス（逆順に戻る）
```

戻りは逆順です。**外側のミドルウェアが最後にレスポンスへ手を入れます。**

## ルートに当たらなくても通る

**共通のミドルウェアは、ルートに当たらなかったリクエストにも掛かります。**
ルートに付けたミドルウェアは、そのルートに当たったときだけ通ります。

| リクエスト                              | 共通のミドルウェア |
|-----------------------------------------|--------------------|
| ルートに当たった                        | 通る               |
| 404（ルートが無い）                     | **通る**           |
| 405（メソッドが違う）                   | **通る**           |
| `public/` や `/storage/` の静的ファイル | **通る**           |
| 413 / 400 / 408（本文を受け取れない）   | **通らない**       |

つまり、自分で付けたヘッダーは 404 の応答にも付きます。
組み込みの 2 つは、ルートに当たらないときに動きを変えます。

| ミドルウェア      | ルートに当たらないとき                                               |
|-------------------|----------------------------------------------------------------------|
| `StartSession`    | **セッションは読むだけ**です。書き換えなければ Cookie も返しません（[session.md](session.md)） |
| `VerifyCsrfToken` | 確かめません。無いページへの POST は 419 ではなく 404 のままです     |

自分のミドルウェアで同じことをしたいときは `req.route_matched()` を見ます。

```rust
pub async fn handle(req: Request, next: Next) -> Result<Response> {
    if !req.route_matched() {
        return next.run(req).await;   // 404 には何もしない
    }
    // ここにルートがあるときだけの処理を書く
    next.run(req).await
}
```

## エラーのときも通る

`next.run(req).await` が返すのは**いつも成功**です。
内側で `abort(403)` が起きても、ここではもうレスポンスになっています。

```rust
pub async fn handle(req: Request, next: Next) -> Result<Response> {
    let response = next.run(req).await?;   // 403 でもここへ来る
    Ok(response.with_header("x-powered-by", "bengara"))
}
```

- 内側で起きたエラーの中身は、ミドルウェアからは見えません。見えるのはステータスです。
- ミドルウェアがパニックしても、そのリクエストだけが 500 になります。

## 起動時に止まる場合

次のときは、起動時にパニックします。リクエストが来てから落ちることはありません。

| 状況                                          | 出るもの                 |
|-----------------------------------------------|--------------------------|
| 登録していない名前を `.middleware()` で使った | 登録済みの名前の一覧     |
| 同じ名前を 2 回 `alias` した                  | 重なった名前             |

`with_middleware(...)` を一度も書いていないときも、1 行目と同じです。

## 設定を持たせたいとき

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

`self` は `async move` の中へ持ち込めません。必要な値を先に取り出します。

## 回数の制限（throttle）

同じ相手から短い間に何度も来るのを止めます。超えると **429** を返します。

```rust
// bootstrap/app.rs
.alias("throttle", Throttle::per_minute(60))

// routes/web.rs
Route::post("/login", AuthController::login).middleware("throttle");
```

| 作り方                                       | 中身                                             |
|----------------------------------------------|--------------------------------------------------|
| `Throttle::per_minute(60)`                   | 1 分に 60 回                                     |
| `Throttle::new(10, Duration::from_secs(30))` | 30 秒に 10 回                                    |
| `Throttle::from_spec("60,1")?`               | 1 分に 60 回。Laravel の `throttle:60,1` と同じ書き方 |

- `from_spec` の 2 つ目（分）は省けます。省くと 1 分です。0 はエラーです。
- 数え始めは最初の 1 回です。期間が過ぎたら 0 に戻ります。

通したときも、止めたときも、残り回数を知らせるヘッダーを付けます。

| 結果    | 付くヘッダー                                                             |
|---------|--------------------------------------------------------------------------|
| 通った  | `X-RateLimit-Limit` と `X-RateLimit-Remaining`                           |
| **429** | `X-RateLimit-Limit` と `X-RateLimit-Remaining: 0` と `Retry-After`（秒） |

### 誰を数えるか

数えるのは「相手とルートの組」です。相手は次のように決まります。

| 状況                 | 数える相手                   |
|----------------------|------------------------------|
| ログインしている     | **その人**（利用者の ID）    |
| していない           | **つないできたアドレス**     |
| アドレスが分からない | 数えない。制限を掛けずに通す |

- **`Throttle` を共通のミドルウェアにするときは、`StartSession` より後ろに置きます。**
  前に置くと、いつも接続元のアドレスで数えます。
  ルートに `.middleware("throttle")` で付ける形なら、気にしなくて済みます。
- アドレスが分からないのは、アドレスを入れずに `Request` を組み立てたときだけです。
  警告を 1 回出して通します。
- **`#[bengara::test]` から送ったリクエストは `127.0.0.1` になります。**
  テストでも回数の制限は掛かります（[testing.md](testing.md)）。

ルートは次の順で見分けます。

| 順 | 見分けに使うもの                   | 例                |
|----|------------------------------------|-------------------|
| 1  | ルート名                           | `items.show`      |
| 2  | 名前が無ければ、**登録したパス**   | `/api/items/{id}` |
| 3  | ルートに当たらなければ、届いたパス | `/missing`        |

**相手が URL の書き方を変えても、枠は増えません。** 名前の無いルートでも同じです。

| 相手がやること                              | どうなるか        |
|---------------------------------------------|-------------------|
| 末尾に `/` を足す（`/login/` `/login//`）   | `/login` と同じ枠 |
| パス引数を変える（`/api/items/1` と `/99`） | 同じ枠            |
| `%xx` の大文字小文字を変える                | 同じ枠            |

- 名前の無いルートは、**同じパスならメソッドが違っても同じ枠**です
  （`GET /login` と `POST /login`）。分けたいときは名前を付けます。
- 3 は、`Throttle` を共通のミドルウェアにして 404 や 405 に掛かったときです。
  末尾の `/` と `%xx` の大文字小文字は、ここでもそろえてから数えます。

### プロキシのヘッダー

前段にプロキシがいると、つないでくる相手はいつもプロキシです。
本当の相手を知るために、`X-Forwarded-For` と `X-Real-IP` を見ます。
**見るのは、`TRUSTED_PROXIES` に載っている相手から届いたものだけです。**

| `TRUSTED_PROXIES` | 動き                       |
|-------------------|----------------------------|
| 空（既定）        | どちらのヘッダーも見ない   |
| アドレスの列      | 載っている相手からだけ見る |
| `*`               | すべて信頼する             |

- **範囲の書き方（`10.0.0.0/8` など）は書けません。** アドレスを 1 つずつ並べます。
  アドレスとして読めない値は、警告を出して捨てます。
- **IPv4 は `10.0.0.1` の形で書きます。** 相手が `::ffff:10.0.0.1`
  （IPv4 を IPv6 の形で表したもの）で届いても、同じ相手として扱います。
- どのアドレスを採るかは [configuration.md](configuration.md) にあります。

同じ名前のヘッダーが何本も届いたときの扱いは、2 つで違います。

| ヘッダー          | 何本も届いたとき                                       |
|-------------------|--------------------------------------------------------|
| `X-Forwarded-For` | カンマでつないだ 1 本として扱う                        |
| `X-Real-IP`       | **2 本以上なら使わない**。つないできたアドレスで数える |

### 大事な制限

| 制限                           | どうするか                                                   |
|--------------------------------|--------------------------------------------------------------|
| 回数はプロセスのメモリに数える | プロセスを 2 つ動かすと、**実際の上限は指定の約 2 倍**になる |
| 数えるときは 1 本ずつ処理する  | **全ルートには掛けない。** ログイン・登録・送信の受け口だけに付ける |
| 覚える枠は 1 万件まで          | 超えたら、回数の少ない枠から忘れる                           |

厳密に守りたいときは、前段のプロキシ（nginx の `limit_req` など）で掛けます。

## 一覧を見る

`cargo artisan route:list` の MIDDLEWARE の列に、ルートに付けた名前が出ます
（[routing.md](routing.md)）。共通のミドルウェアは出ません。

## まだ無いもの

まだ無いものは [backlog.md](backlog.md) にまとめてあります。

## 関連

- [routing.md](routing.md)
- [requests-and-responses.md](requests-and-responses.md)
- [session.md](session.md)
