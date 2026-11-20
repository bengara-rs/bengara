# セッションと CSRF

セッションは、同じ人のリクエストをまたいで値を覚えておく仕組みです。
CSRF は、別のサイトから勝手にフォームを送らせる攻撃です。このページは、その 2 つと Cookie の扱いを説明します。

## 準備

### 1. 鍵を作る

**`APP_KEY` が要ります。** Cookie の署名（書き換えを見抜くための印）に使います。

```sh
cargo artisan key:generate
```

| こと       | 内容                                                  |
|------------|-------------------------------------------------------|
| 書き込み先 | `.env`                                                |
| 長さ       | **64 文字以上**。無い・短いとリクエストが 500 になる  |
| 作り直すと | いまのセッションと署名付き URL が**全部使えなくなる** |

くわしくは [configuration.md](configuration.md) にあります。

### 2. ミドルウェアを登録する

`bootstrap/app.rs` に 2 つ登録します。**この順番で書きます。** 逆にすると 500 になります。

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

| メソッド                | すること                                                                   |
|-------------------------|----------------------------------------------------------------------------|
| `get(key)`              | 読む。無ければ `None`                                                      |
| `get_or(key, default)`  | 読む。無ければ既定値                                                       |
| `put(key, value)`       | 入れる。次のリクエストでも読める                                           |
| `put_json(key, &value)` | `serde` の型を JSON にして入れる。できなければ `Err`                       |
| `get_json::<T>(key)`    | JSON を型に読む。無ければ `Ok(None)`、読めなければ `Err`                   |
| `flash(key, value)`     | 入れる。**次のリクエストまで**だけ残る                                     |
| `has(key)`              | あるか                                                                     |
| `forget(key)`           | 消す                                                                       |
| `pull(key)`             | 読んでから消す                                                             |
| `all()`                 | 全部の組（`__` で始まる鍵は入らない）                                      |
| `old(key)`              | 検査に落ちたときの入力（[validation.md](validation.md)）                   |
| `intended()`            | ログイン後の戻り先。読むと消える（[authentication.md](authentication.md)） |
| `id()`                  | いまのセッションの ID                                                      |
| `flush()`               | 中身を全部消す。ID は変えない                                              |
| `regenerate()`          | ID を作り直す。中身は残し、CSRF トークンは作り直す                         |
| `invalidate()`          | 中身を消して、ID も作り直す                                                |

- 値は文字列です。構造体は `put_json` / `get_json` で JSON にして入れます。

```rust
req.session().put_json("cart", &cart)?;
let cart: Option<Cart> = req.session().get_json("cart")?;
```

- 同じ鍵に `put` と `flash` の両方を書かないでください。`get` と `all()` で読める値が食い違います。
- `all()` には、ログインに使う `bengara_auth_id`・`bengara_auth_at`・`bengara_auth_intended` が入ります。
  画面やログにそのまま出さないでください。

`flash` の値は、入れた回と次の回で読めます。

```rust
req.session().flash("status", "保存しました");   // 1 回目: この回でも読める
req.session().get("status")                      // 2 回目: Some("保存しました")
req.session().get("status")                      // 3 回目: None
```

### `__` で始まる鍵は使わない

`__` で始まる鍵は、フレームワークが中で使います（CSRF のトークン、検査に落ちた入力など）。

- `put` / `flash` に渡すと警告が出ます。
- `all()` には入らないので、`all()` を画面に出してもトークンは漏れません。

### ログインとログアウトは `req.auth()` で

```rust
req.auth().login(&user)?;    // セッション ID を作り直す
req.auth().logout()?;        // 中身を捨てて、Cookie を消す
```

- **ログインの状態を自分で `put` しないでください。** `auth` ミドルウェアにも `req.auth()` にも見えません。
- `login` が ID を作り直すのは、セッション固定（前もって仕込んだ ID を使わせる攻撃）を防ぐためです。
- **ID を作り直すと、CSRF トークンも変わります。** ログイン後のフォームでは、トークンを取り直します。
- `logout` は、置き場所からセッションを消し、ブラウザの Cookie も消します。
- **古いセッションを消せなかったときは 500 です。** 盗まれた Cookie を使えなくするためです。

くわしくは [authentication.md](authentication.md) にあります。

## 置き場所

中身はサーバー側に置きます。ブラウザには、署名した ID だけを Cookie で渡します。

| `SESSION_DRIVER`         | 置き場所                                   |
|--------------------------|--------------------------------------------|
| `file`（既定）           | `storage/framework/sessions/`              |
| `memory`（別名 `array`） | プロセスのメモリ。**テストと手元の確認用** |

- 知らない値（`redis` や書き間違い）は、警告を出して `file` にします。
- 本番（`APP_ENV=production`）で `memory` にすると、起動時に警告を出します。止めはしません。
- `SESSION_LIFETIME` は分で書きます。既定は 120 です。
- **期限は「最後に使ったとき」から数えます。** 読むだけのリクエストでも延びます。実際に切れるのは、最大 60 秒早くなります。
- 期限切れは自動で少しずつ消します。まとめて消すなら `cargo artisan session:gc` です。
- **プロセスを 2 つ以上動かすときは、全部が同じ `storage/` を見るようにします。** `memory` は本番で使えません。
- ファイルは平文です。秘密を入れすぎないでください。

`.env` を見ずに決める作り方もあります。

| 作り方                              | 置き場所                       |
|-------------------------------------|--------------------------------|
| `StartSession::from_env()`          | `.env` で決める                |
| `StartSession::file()`              | ファイル。期限は 120 分        |
| `StartSession::memory()`            | メモリ。期限は 120 分          |
| `StartSession::new(置き場所, 設定)` | 自分で作った置き場所（下の節） |

### 同じセッションのリクエストは 1 本ずつ

同じセッションのリクエストが同時に来ると、後のものは前の応答が終わるまで待ちます。
片方が入れた値が、もう片方の保存で消えないようにするためです。

- 別のセッションどうしは待ちません。
- 長くかかる処理は、同じ人の次のリクエストを待たせます。`.stream_body()` の大きな受け取りも同じです。
- Laravel の既定とは違います（Laravel は `block()` を付けたルートだけ）。
- 1 本ずつにするのは 1 つのプロセスの中だけです。プロセスをまたぐと、後から保存したほうが勝ちます。

### ルートに当たらないリクエストは読むだけ

静的ファイル・404・405 でも、`req.session()` と `req.auth()` は使えます。ただし「読むだけ」です。

- 待たされず、期限も延びません。CSS や画像は並んで返ります。
- `flash` は読めますが消えません。転送先が 404 でも、次の画面まで残ります。

## Cookie の設定

セッションの Cookie には、いつも `HttpOnly`（JavaScript から読めない）が付きます。
ほかは `SessionConfig` で変えられます。

```rust
StartSession::from_env().with_config(SessionConfig {
    cookie: "myapp_session".into(),
    same_site: SameSite::Strict,
    lifetime_secs: 60 * 120,
    ..SessionConfig::default()
})
```

| 欄              | 既定              | 中身                                                       |
|-----------------|-------------------|------------------------------------------------------------|
| `cookie`        | `bengara_session` | Cookie の名前                                              |
| `lifetime_secs` | 7200              | 放っておいて切れるまでの秒数                               |
| `secure`        | `false`           | HTTPS のときだけ送る。`APP_ENV=production` では必ず `true` |
| `same_site`     | `SameSite::Lax`   | 別のサイトからの送信に付けるか（`Lax` は付けない）         |
| `path`          | `/`               | Cookie を送る範囲                                          |

- **`with_config` は `SESSION_LIFETIME` を上書きします。** `lifetime_secs` を省くと 7200 秒です。

## CSRF

CSRF を防ぐために、フォームに「トークン」（その人だけが知る合い言葉）を入れて送ってもらいます。
`VerifyCsrfToken` は `GET` / `HEAD` / `OPTIONS` を素通しし、それ以外でトークンを確かめます。
合わなければ **419** を返します。

### フォームから送る

`req.csrf_token()` で取り出し、`_token` という名前で送り返してもらいます。

```rust
let token = req.csrf_token();
html(format!(
    r#"<form method="post" action="/posts">
         <input type="hidden" name="_token" value="{token}">
       </form>"#
))
```

### JavaScript の画面から送る

トークンを配るルートを 1 本作ります。以後は `X-CSRF-TOKEN` ヘッダーに入れて送ります。

```rust
// routes/web.rs
Route::get("/csrf-token", PostController::token).name("csrf.token");

// コントローラ
pub async fn token(req: Request) -> Result<Response> {
    json(&json!({ "token": req.csrf_token() }))
}
```

ログインのあとは、このルートをもう一度呼んで取り直します。

### トークンを探す場所

1. `X-CSRF-TOKEN` ヘッダー
2. 本文の `_token`（フォーム、または JSON の最上位）

**URL のクエリ（`?_token=...`）は見ません。** URL のトークンは、ログ・履歴・`Referer` ヘッダーから漏れるためです。

- トークンは要るときにだけ作ります。Cookie を持たない人の `GET` では、セッションのファイルも増えません。
- **ルートに当たらなかったリクエストは確かめません。** 無いページへの POST は 404 のままです。

### 確かめないルートを決める

外し方は 2 つあります。**ルート名で外すほうを勧めます。** パスで外すと、ルートを直したときに外しすぎ・外し漏れが起きます。

```rust
VerifyCsrfToken::new()
    .except_route("webhook")      // 名前が webhook のルート
    .except_route("api.stripe.*") // api.stripe. で始まる名前のルート
    .except("/webhook")           // パスで外す
```

- どちらも末尾の `*` で前方一致になります。
- 名前の無いルートは `except_route` では外せません。
- パスの `*` の前は `/` にします。`.except("/admin*")` は `/administration` にも当たります（起動時に警告が出ます）。

### 外してよいのは Cookie でログインを見ない処理だけ

**`/api/*` のようにまとめて外さないでください。**
`StartSession` は全ルートに掛かるので、`/api/*` も Cookie でログインを見ます。
そこで CSRF だけ外すと、別のサイトから Cookie に乗って書き込めます。

| 外してよいもの                  | 相手の確かめ方       |
|---------------------------------|----------------------|
| トークンや署名で認証する API    | そのトークン・署名   |
| 外から呼ばれる受け口（webhook） | 署名の検証など       |
| ログイン中の人を見ない処理      | 確かめる相手がいない |

## 自分で Cookie を扱う

```rust
let theme = req.cookie("theme").unwrap_or_else(|| "light".into());

let cookie = Cookie::new("theme", "dark")
    .with_max_age(3600)
    .with_same_site(SameSite::Strict);
Ok(response.with_cookie(&cookie))
```

| メソッド               | 既定     | すること                                     |
|------------------------|----------|----------------------------------------------|
| `with_path(path)`      | `/`      | 送る範囲のパス                               |
| `with_domain(domain)`  | 付けない | 送る範囲のドメイン                           |
| `with_max_age(秒)`     | 付けない | 生きる秒数。付けなければブラウザを閉じるまで |
| `with_secure(bool)`    | `false`  | HTTPS のときだけ送る                         |
| `with_http_only(bool)` | `true`   | JavaScript から読めなくする                  |
| `with_same_site(値)`   | `Lax`    | `Strict` / `Lax` / `None`                    |

- 消すときは `Cookie::removal("theme")` です。
- `SameSite::None` にすると、`Secure` も付きます。
- 名前と値は自動で符号化します。`;` や空白を入れても壊れません。

## `StartSession` を忘れたとき

| 呼んだもの                                          | 起きること             |
|-----------------------------------------------------|------------------------|
| `req.session()` / `req.csrf_token()`                | パニック（500）        |
| `VerifyCsrfToken`                                   | 500 のエラー           |
| `req.auth()` の `login` / `attempt` / `logout` など | 500 のエラー           |
| `req.auth()` の `check()` / `guest()` / `id()`      | 「ログインしていない」 |

- エラーには「`StartSession` を登録してください」と出ます。
- **`check()` などは黙って「ログインしていない」になります。** 画面の出し分けだけだと気づけません。
- セッションが無くてよい場面では `req.try_session()` を使います。

## 自分で置き場所を作る

組み込みの `file` と `memory` で足りるなら、読まなくて構いません。
DB など別の置き場所を使うときは、`bengara::session::SessionStore` を実装します。

```rust
StartSession::new(MyStore::new(7200), SessionConfig::default())
```

**期限の秒数は、置き場所にも自分で渡します。** `with_config` で期限を変えても作り直されません（警告が出ます）。

| メソッド                          | 書かないと                                               |
|-----------------------------------|----------------------------------------------------------|
| `read` / `write` / `destroy`      | 必ず書く。`read` は**期限切れに `None` を返す**          |
| `touch` / `read_and_touch`        | 動くが、読み書きが余分に走る                             |
| `destroy_for_user` / `..._except` | `logout_other_devices()` などが警告を出して 0 を返す     |
| `mark_revoked` / `revoked_at`     | 消したセッションが、処理中の別リクエストに書き戻されうる |

- 各メソッドの決まりは、トレイトのドキュメントコメントにあります。
- 組み込みの `file` のファイルは、権限 `0600`（ディレクトリは `0700`）で作ります（Unix だけ）。

## 関連

- [middleware.md](middleware.md)
- [validation.md](validation.md)
- [configuration.md](configuration.md)
- [authentication.md](authentication.md)
