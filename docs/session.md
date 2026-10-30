# セッションと CSRF

リクエストをまたいで値を覚えておく仕組みです。
あわせて、別のサイトからの書き込みを防ぐ仕組み（CSRF）も扱います。

## 準備

### 1. 鍵を作る

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

### 2. ミドルウェアを登録する

`bootstrap/app.rs` に 2 つ登録します。**順番が大事です。**
逆に置くと 500 になります。

```rust
Application::configure()
    .with_middleware(|m| {
        m.append(StartSession::from_env())      // 先に
            .append(VerifyCsrfToken::new())     // 後に
    })
    .with_routing(|r| r.web(crate::routes::web::routes))
    .create()
```

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
| `id()`                 | いまのセッションの ID                  |
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

**同じ鍵に `put` と `flash` の両方を書いたときは、後から書いたほうが読めます。**

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

## 置き場所

中身は **サーバー側**に置きます。ブラウザには ID だけを署名つきの Cookie で渡します。
中身がブラウザに出ないので、Cookie の 4 KB という上限にも縛られません。

| `SESSION_DRIVER` | 置き場所                                   |
|------------------|--------------------------------------------|
| `file`（既定）   | `storage/framework/sessions/`              |
| `memory`         | プロセスのメモリ。**テストと手元の確認用** |

- `SESSION_LIFETIME` は分です（既定 120）。
  **残りの期限が半分を切ったら、読むだけのリクエストでも期限を延ばします。**
- 期限切れのセッションは、放っておいても増え続けません。
  書き込みのついでに少しずつ消します。まとめて消すなら `cargo artisan session:gc` です。
- **プロセスを 2 つ以上動かすときは、全プロセスが同じ `storage/` を見るようにしてください。**
  `memory` はプロセスをまたげないので、本番では使えません。

### 同じセッションのリクエストは順番に処理されます

**同じセッションのリクエストが並行して 2 本来ると、2 本目は 1 本目の応答が
終わるまで待ちます。** Laravel もセッションのファイルを錠で押さえるので同じ動きです。

- **別のセッションどうしは待ちません。**
- 長くかかる処理は、**同じ利用者の次のリクエストを待たせます。**

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
    lifetime_secs: 60 * 120,
    ..SessionConfig::default()
})
```

- `StartSession::from_env()` が `.env` の `SESSION_DRIVER` と `SESSION_LIFETIME` を読みます。
- `lifetime_secs` を省くと既定の 7200 秒（120 分）になります。
  **`.env` の `SESSION_LIFETIME` と合わせたいときは、同じ秒数を書いてください。**
- `with_config` で期限を変えると、組み込みの置き場所も作り直します。
  `memory` のときは、それまでに入れた中身が消えます。
  `StartSession::new(自分の置き場所)` で渡したときは作り直せないので、
  食い違っていれば警告が出ます。

## CSRF

`GET` / `HEAD` / `OPTIONS` は素通しします。
それ以外のメソッドでトークンを確かめ、合わなければ **419** を返します。

### フォームから送る

トークンは `req.csrf_token()` で取り出せます。

```html
<input type="hidden" name="_token" value="{{ token }}">
```

### SPA から送る

トークンを配るルートを 1 本作ります。以後は `X-CSRF-TOKEN` ヘッダーに入れて送ります。

```rust
// routes/web.rs
Route::get("/csrf-token", AuthController::token);

// コントローラ
pub async fn token(req: Request) -> Result<Response> {
    json(&bengara::serde_json::json!({ "token": req.csrf_token() }))
}
```

トークンは **ヘッダーを先に**見ます。ヘッダーに無いときだけ本文の `_token` を見ます。

### ルートが無いときは確かめない

**ルートに当たらなかったリクエストでは、CSRF を確かめません。**
無いページへの POST は 419 ではなく 404 のままです。
**トークンも用意しません。** Cookie を持たない人が無いページを叩いても、
`Set-Cookie` は返りませんし、セッションのファイルも増えません
（[middleware.md](middleware.md)）。

### 確かめない場所を決める

外し方は 2 つあります。**ルート名で外すほうを勧めます。**
パスで外すとルート表と二重に管理することになり、書き間違えると
思っていない所まで外れます。

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

- どちらも末尾の `*` で前方一致になります。
- **名前の無いルートは `except_route` では外せません。**
  `.name(...)` を付けるか、パスで外してください。
- **`*` の直前は `/` にしてください。** `.except("/admin*")` は `/administration` にも
  当たります。`.except("/admin/*")` と書きます。直前が `/` でないときは、
  起動時に警告が 1 回出ます（禁止はしていません）。

### 外してよいのは Cookie で認証しない API だけ

**`/api/*` のようにまとめて外さないでください。**
`StartSession` は全ルートに掛かるので `/api/*` も Cookie で認証されます。
そこで CSRF だけ外すと、**別のサイトから Cookie に便乗して書き込めます。**

外してよいのは、次のどちらかです。

| 外してよいもの                  | 相手の確かめ方           |
|---------------------------------|--------------------------|
| トークンや署名で認証する API    | そのトークン・署名       |
| 外から呼ばれる受け口（webhook） | 署名の検証など、別の方法 |

**外すときは、必ず別の方法で相手を確かめてください。**

## 自分で Cookie を扱う

```rust
let theme = req.cookie("theme").unwrap_or_else(|| "light".into());

let cookie = Cookie::new("theme", "dark")
    .with_max_age(3600)
    .with_same_site(SameSite::Strict);
Ok(response.with_cookie(&cookie))
```

- 消すときは `Cookie::removal("theme")` です。
- 同じ名前を何本も送りたいときは `with_added_header` を使ってください。
- **名前と値は自動で逃がします。** `;` や空白を入れても壊れません。
- **`with_path` と `with_domain` に渡した値からは、改行・`\0`・`;` を落とします。**
  落としたときは警告が 1 行出ます。

## 気をつけること

| こと                            | 内容                                                 |
|---------------------------------|------------------------------------------------------|
| セッションに秘密を入れすぎない  | 中身はサーバー側ですが、置き場所は平文のファイルです |
| `StartSession` を登録し忘れない | 下の表のとおりになります                             |
| `VerifyCsrfToken` は後に置く    | `StartSession` より前に置くと 500 になります         |

### `StartSession` を忘れたときの出方

| 呼んだもの                                          | 起きること                   |
|-----------------------------------------------------|------------------------------|
| `req.session()` / `req.csrf_token()`                | **パニック**（500）          |
| `req.auth()` の `login` / `attempt` / `logout` など | **500 のエラー**             |
| `req.auth()` の `check()` / `guest()` / `id()`      | 「ログインしていない」になる |

- `req.auth()` の側はパニックしません。メッセージには
  「`StartSession` を登録してください」が入ります。
- `check()` のように真偽値を返すものは、**黙って「ログインしていない」になります。**
  画面の出し分けだけに使っていると気づけません。
- 無くてもよい場面では `req.try_session()` を使ってください。

## 自分で置き場所を作るとき

組み込みの `file` と `memory` で足りるなら、この節は読まなくて構いません。
別の置き場所（DB など）を使いたいときは、`SessionStore` を実装します。

| メソッド                                 | 既定の実装                  |
|------------------------------------------|-----------------------------|
| `read` / `write` / `destroy`             | なし。必ず書く              |
| `touch(id)`                              | あり（`read` して `write`） |
| `destroy_for_user(user_id)`              | あり                        |
| `destroy_for_user_except(user_id, keep)` | あり                        |
| `mark_revoked(user_id, except)`          | あり（何もしない）          |
| `revoked_at(user_id, session_id)`        | あり（`None`）              |

- 後ろの 5 つは **実装しなくても壊れません。** 必要になったときに足してください。
- **`read` は期限切れに `None` を返してください。** 返さないと、既定の `touch` が
  切れたセッションを生き返らせます。
- `touch` は期限を延ばすために呼ばれます。もっと安く書ける置き場所
  （DB なら `update` 1 文）では、自分で実装してください。
  そのとき **期限切れのセッションを延命しないでください。**
- `mark_revoked` / `revoked_at` は「切った」印です。`destroy_for_user` 系で消した
  瞬間に処理中だった別のリクエストが、同じ ID を書き直して復活させないために使います。
  `destroy_for_user` 系を実装するなら、消す前に `mark_revoked` を呼ぶ形にしてください。
  **印は、途中が読まれない形で書いてください**（別の名前に書いてから置き換える）。
- `destroy_for_user` 系を実装していないと、警告を出して 0 を返します。
  呼ぶのは `Auth::logout_all_devices()` と `logout_other_devices()` です
  （[authentication.md](authentication.md)）。

### 組み込みの置き場所の中身

参考までに、組み込みの置き場所がしていることです。

| こと             | `file`                                             | `memory`           |
|------------------|----------------------------------------------------|--------------------|
| 掃除の起こり方   | 書き込みの **6/256**（約 2.3%）                    | 書き込みの 100 回に 1 回 |
| 1 回で見る数     | **最大 500 件**。残りは次の機会                    | 全部               |
| どこで走るか     | **別のスレッド**。応答は待ちません                 | その場             |
| 二重に走るか     | 走りません                                          | —                  |
| ファイルの権限   | ファイル `0600`、ディレクトリ `0700`（Unix だけ）  | —                  |

`file` は同じときに、書きかけのまま残った一時ファイル（5 分より古いもの）と、
1 時間より古い「切った」印も消します。
`cargo artisan session:gc` だけは上限なしで全部を見ます
（出る件数は期限切れセッションの数で、一時ファイルの数は別の行に出ます）。

## まだ無いもの

まだ無いものは [backlog.md](backlog.md) にまとめてあります。
ログインの仕組みは **あります**（[authentication.md](authentication.md)）。

## 関連

- [middleware.md](middleware.md)
- [validation.md](validation.md)
- [configuration.md](configuration.md)
- [authentication.md](authentication.md)
