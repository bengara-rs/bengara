# セッションと CSRF

リクエストをまたいで値を覚えておく仕組みの説明です。
あわせて、別のサイトからの書き込みを防ぐ仕組み（CSRF）も扱います。

## 準備

### 鍵を作る

**`APP_KEY` が要ります。** Cookie の署名に使います。

```sh
cargo artisan key:generate
```

| こと       | 内容                                                   |
|------------|--------------------------------------------------------|
| 書き込み先 | `.env`                                                 |
| 長さ       | **64 文字以上が必須**。短いと最初のリクエストでエラー   |
| 作り直すと | いまのセッションと署名付き URL が**全部無効**になります |

くわしくは [configuration.md](configuration.md) の APP_KEY を見てください。

### ミドルウェアを登録する

`bootstrap/app.rs` に 2 つ登録します。**順番が大事です。**

```rust
Application::configure()
    .with_middleware(|m| {
        m.append(StartSession::from_env())      // 先に
            .append(VerifyCsrfToken::new())     // 後に
    })
    .with_routing(|r| r.web(crate::routes::web::routes))
    .create()
```

逆に置くと 500 になります。

## 置き場所

中身は **サーバー側**に置きます。
ブラウザには ID だけを署名つきの Cookie で渡します。
中身がブラウザに出ないので、Cookie の 4 KB という上限にも縛られません。

| `SESSION_DRIVER` | 置き場所                                   |
|------------------|--------------------------------------------|
| `file`（既定）   | `storage/framework/sessions/`              |
| `memory`         | プロセスのメモリ。**テストと手元の確認用** |

`SESSION_LIFETIME` は分です（既定 120）。

- **プロセスを 2 つ以上動かすときは、全プロセスが同じ `storage/` を見るようにしてください。**
- `memory` はプロセスをまたげません。本番では使えません。
- 置き場所は `StartSession` に渡したものが、そのリクエストのセッションに結びつきます。
  `req.session()` の操作は必ずその置き場所に向かいます。

期限切れは読むときに消えます。
ファイルをまとめて掃除するコマンドもあります。

```sh
cargo artisan session:gc
```

### ファイルの権限（Unix）

| 対象                          | 権限   |
|-------------------------------|--------|
| セッションのファイル          | `0600` |
| `storage/framework/sessions/` | `0700` |

同じサーバーにいる別の利用者から読まれないようにしています。
Windows では設定しません（OS の既定のままです）。

### 自分で置き場所を作る

`SessionStore` を実装します。

| メソッド                                 | 既定の実装     |
|------------------------------------------|----------------|
| `read` / `write` / `destroy`             | なし。必ず書く |
| `destroy_for_user(user_id)`              | あり           |
| `destroy_for_user_except(user_id, keep)` | あり           |

- 後ろの 2 つは **実装しなくても壊れません。** 警告を出して 0 を返します。
- この 2 つを呼ぶのは `Auth::logout_all_devices()` と `logout_other_devices()` です
  （[authentication.md](authentication.md)）。
- 必要になったときに足してください。利用者で引く方法は置き場所ごとに違います。

## セッションを使う

```rust
pub async fn store(req: Request) -> Result<Response> {
    req.session().put("cart", "12");
    req.session().flash("status", "保存しました");
    redirect().route("home")
}
```

| メソッド               | 中身                                   |
|------------------------|----------------------------------------|
| `get(key)`             | 読む。無ければ `None`                  |
| `get_or(key, default)` | 読む。無ければ既定値                   |
| `put(key, value)`      | 入れる。次のリクエストでも読める       |
| `flash(key, value)`    | 入れる。**次のリクエストまで**だけ残る |
| `has(key)`             | あるか                                 |
| `forget(key)`          | 消す                                   |
| `pull(key)`            | 読んでから消す                         |
| `all()`                | 全部の組                               |
| `old(key)`             | 検査に落ちたときの入力                 |
| `intended()`           | ログイン後の戻り先。**読むと消える**   |
| `flush()`              | 中身を全部消す                         |
| `regenerate()`         | ID を作り直す                          |
| `invalidate()`         | 中身を消して ID も作り直す             |

### flash の寿命

```rust
// 1回目
req.session().flash("status", "保存しました");   // この回でも読める

// 2回目
req.session().get("status")   // Some("保存しました")

// 3回目
req.session().get("status")   // None
```

### ログインしたら必ず ID を作り直す

```rust
req.session().regenerate();
req.session().put("user_id", id);
```

人のブラウザにあらかじめ ID を仕込んでおく攻撃（セッション固定）を防ぎます。
ログアウトは `invalidate()` です。

**`regenerate()` は CSRF トークンも作り直します。** Laravel と同じです。

- ログイン前に受け取ったトークンは、ログイン後には使えません。
- ログイン後にフォームを出すときは、トークンを取り直してください。
- SPA では、ログインのあとにトークンを配るルートをもう一度呼びます。

## Cookie の既定

| 属性       | 既定                          | 意味                                 |
|------------|-------------------------------|--------------------------------------|
| `HttpOnly` | 付く                          | JavaScript から読めない              |
| `SameSite` | `Lax`                         | 他のサイトからの書き込みでは送らない |
| `Secure`   | `APP_ENV=production` なら強制 | HTTPS のときだけ送る                 |
| `Path`     | `/`                           |                                      |

変えたいときは `SessionConfig` を渡します。

```rust
StartSession::from_env().with_config(SessionConfig {
    cookie: "myapp_session".into(),
    same_site: SameSite::Strict,
    ..Default::default()
})
```

## CSRF

`GET` / `HEAD` / `OPTIONS` は素通しします。
それ以外のメソッドでトークンを確かめ、合わなければ **419** を返します。

### フォームから送る

```html
<input type="hidden" name="_token" value="{{ token }}">
```

トークンは `req.csrf_token()` で取り出せます。

### SPA から送る

トークンを配るルートを 1 本作ります。

```rust
// routes/web.rs
Route::get("/csrf-token", AuthController::token);

// コントローラ
pub async fn token(req: Request) -> Result<Response> {
    json(&bengara::serde_json::json!({ "token": req.csrf_token() }))
}
```

以後は `X-CSRF-TOKEN` ヘッダーに入れて送ります。

### ルートが無いときは確かめない

**ルートに当たらなかったリクエストでは、CSRF を確かめません。**
無いページへの POST は 419 ではなく 404 のままです。

**トークンも用意しません。** Cookie を持たない人が無いページを叩いても、
`Set-Cookie` は返りませんし、`storage/framework/sessions/` にファイルも増えません。
用意していた頃は、クローラが来るだけでファイルがたまりました。

共通のミドルウェアがルートの無いリクエストにも掛かる話は
[middleware.md](middleware.md) にあります。

### 確かめない場所を決める

外し方は 2 つあります。**ルート名で外すほうを勧めます。**

| 書き方                  | 見るもの     | 備考                           |
|-------------------------|--------------|--------------------------------|
| `except_route(pattern)` | ルートの名前 | **こちらを勧めます**           |
| `except(pattern)`       | パス         | 名前の無いルートを外すときだけ |

```rust
VerifyCsrfToken::new()
    .except_route("webhook")      // 名前が webhook のルート
    .except_route("api.stripe.*") // api.stripe. で始まる名前のルート
    .except("/webhook")           // パスで外す
```

どちらも末尾の `*` で前方一致になります。

ルート名のほうが安全です。
パスはルート表と二重に管理することになり、書き間違えると思っていない所まで外れます。

**名前の無いルートは `except_route` では外せません。**
`.name(...)` を付けるか、パスで外してください。

### 外してよいのは Cookie で認証しない API だけ

**`/api/*` のようにまとめて外さないでください。**

- `StartSession` は **全ルートに掛かります**。だから `/api/*` も Cookie で認証されます。
- そこで CSRF だけ外すと、**別のサイトから Cookie に便乗して書き込めます。**
- 支えているのは `SameSite=Lax` だけ、という状態になります。

外してよいのは、次のどちらかです。

| 外してよいもの                  | 相手の確かめ方           |
|---------------------------------|--------------------------|
| トークンや署名で認証する API    | そのトークン・署名       |
| 外から呼ばれる受け口（webhook） | 署名の検証など、別の方法 |

**外すときは、必ず別の方法で相手を確かめてください。**

### `*` の直前は `/` にする

```rust
.except("/admin*")     // /administration にも当たる
.except("/admin/*")    // /admin/ の下だけ
```

`*` の直前が `/` でないと、思っていない所まで外れます。
登録のときに **警告が出ます**（起動時に 1 回）。禁止はしていません。

## 自分で Cookie を扱う

```rust
let theme = req.cookie("theme").unwrap_or_else(|| "light".into());

let cookie = Cookie::new("theme", "dark")
    .with_max_age(3600)
    .with_same_site(SameSite::Strict);
Ok(response.with_cookie(&cookie))
```

消すときは `Cookie::removal("theme")` です。
同じ名前を何本も送りたいときは `with_added_header` を使ってください。

## 気をつけること

| こと                                 | 内容                                                     |
|--------------------------------------|----------------------------------------------------------|
| セッションに秘密を入れすぎない       | 中身はサーバー側ですが、置き場所は平文のファイルです     |
| `StartSession` を登録し忘れない      | `req.session()` が**パニックします**。すぐ気づくためです |
| `VerifyCsrfToken` は後に置く         | `StartSession` より前に置くと 500 になります             |

## まだ無いもの

DB / Redis への保存と、1 回だけ有効なトークンはありません。
[backlog.md](backlog.md) を参照してください。

ログインの仕組みは **あります**。[authentication.md](authentication.md) を見てください。

---

関連: [middleware.md](middleware.md) / [validation.md](validation.md) /
[configuration.md](configuration.md) / [authentication.md](authentication.md)
