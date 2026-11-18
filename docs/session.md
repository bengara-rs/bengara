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

| メソッド               | すること                                                                   |
|------------------------|----------------------------------------------------------------------------|
| `get(key)`             | 読む。無ければ `None`                                                      |
| `get_or(key, default)` | 読む。無ければ既定値                                                       |
| `put(key, value)`      | 入れる。次のリクエストでも読める                                           |
| `flash(key, value)`    | 入れる。**次のリクエストまで**だけ残る                                     |
| `has(key)`             | あるか                                                                     |
| `forget(key)`          | 消す                                                                       |
| `pull(key)`            | 読んでから消す                                                             |
| `all()`                | 全部の組（`__` で始まる鍵は入らない）                                      |
| `old(key)`             | 検査に落ちたときの入力（[validation.md](validation.md)）                   |
| `intended()`           | ログイン後の戻り先。読むと消える（[authentication.md](authentication.md)） |
| `id()`                 | いまのセッションの ID                                                      |
| `flush()`              | 中身を全部消す。ID は変えない                                              |
| `regenerate()`         | ID を作り直す。中身は残し、CSRF トークンは作り直す                         |
| `invalidate()`         | 中身を消して、ID も作り直す                                                |

- 値は文字列です。
- 同じ鍵に `put` と `flash` の両方を書かないでください。`get` と `all()` で読める値が食い違います。

`flash` の値は、入れた回と次の回で読めます。

```rust
req.session().flash("status", "保存しました");   // 1 回目: この回でも読める
req.session().get("status")                      // 2 回目: Some("保存しました")
req.session().get("status")                      // 3 回目: None
```

### `__` で始まる鍵は使わない

`__` で始まる鍵は、フレームワークが中で使います。

| 鍵             | 中身                   | 取り出し方         |
|----------------|------------------------|--------------------|
| `__csrf_token` | CSRF のトークン        | `req.csrf_token()` |
| `__old_input`  | 検査に落ちたときの入力 | `old(key)`         |
| `__flash:名前` | flash の目印           | `get(名前)`        |

`put` / `flash` に渡すと警告が出ます。`all()` には入らないので、`all()` を画面に出してもトークンは漏れません。

### ログインとログアウトは `req.auth()` で

```rust
req.auth().login(&user)?;    // セッション ID を作り直す
req.auth().logout()?;        // 中身を捨てて、Cookie を消す
```

- **ログインの状態を自分で `put` しないでください。** `auth` ミドルウェアにも `req.auth()` にも見えません。
- `login` が ID を作り直すのは、セッション固定（前もって仕込んだ ID を使わせる攻撃）を防ぐためです。
- **ID を作り直すと、CSRF トークンも変わります。** ログイン後のフォームでは、トークンを取り直します。
- `logout` は、置き場所からセッションを消し、ブラウザの Cookie も消します。新しい ID は配りません。
- ただし、ログアウトの後に `flash` などで値を入れると、その値は新しい ID で保存されます。
- **古い ID を置き場所から消せなかったときは 500 です。** 消せないまま 200 を返すと、盗まれた Cookie が使えてしまうためです。

くわしくは [authentication.md](authentication.md) にあります。

## 置き場所

中身はサーバー側に置きます。ブラウザには、署名した ID だけを Cookie で渡します。

| `SESSION_DRIVER`         | 置き場所                                   |
|--------------------------|--------------------------------------------|
| `file`（既定）           | `storage/framework/sessions/`              |
| `memory`（別名 `array`） | プロセスのメモリ。**テストと手元の確認用** |

- 知らない値（`redis` や書き間違い）は、警告を出して `file` にします。
- `SESSION_LIFETIME` は分で書きます。既定は 120 で、1 より小さい値は 1 にします。
- **期限は「最後に使ったとき」から数えます。** 読むだけのリクエストでも延びます。
- 延ばす書き込みは、前の書き込みから 60 秒（期限の 1/10 のほうが短ければそちら）たったときだけです。
  そのため、実際に切れるのは期限より最大 60 秒早くなります。
- 期限切れは書き込みのついでに少しずつ消します。まとめて消すなら `cargo artisan session:gc` です。
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
待たせないと、片方が入れた値（作ったばかりの CSRF トークンなど）が、もう片方の保存で消えるためです。

- 別のセッションどうしは待ちません。
- 長くかかる処理は、同じ人の次のリクエストを待たせます。`.stream_body()` の大きな受け取りも同じです。
- Laravel の既定とは違います（Laravel は `block()` を付けたルートだけ）。
- 1 本ずつにするのは 1 つのプロセスの中だけです。プロセスをまたぐと、後から保存したほうが勝ちます。

### ルートに当たらないリクエストは読むだけ

静的ファイル・404・405 でも、`req.session()` と `req.auth()` は使えます。ただし扱いは「読むだけ」です。

| こと           | ルートに当たった     | 当たらなかった         |
|----------------|----------------------|------------------------|
| 1 本ずつにする | する                 | しない（待たない）     |
| 期限を延ばす   | する                 | しない                 |
| flash          | 読めて、そこで消える | 読めるが消えない       |
| `Set-Cookie`   | 返す                 | 書き換えたときだけ返す |

画面と CSS・画像は待たずに並んで返ります。転送先が 404 でも、`flash` は次の画面まで残ります。

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
- 期限を変えると、組み込みの置き場所を作り直します。`memory` では中身が消えます。

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

### トークンを作るとき

トークンは要るときにだけ作ります。

- `req.csrf_token()` を呼んだとき
- すでにセッションを持っている人が `GET` / `HEAD` / `OPTIONS` で来たとき

Cookie を持たない人の `GET` では作らず、セッションのファイルも増えません。
監視が数秒ごとに叩く `/up` などで、ファイルが増え続けないためです。

**ルートに当たらなかったリクエストは確かめません。** 無いページへの POST は 404 のままです。

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
- `with_path` と `with_domain` の値からは、制御文字と `;` を落とします（警告が出ます）。

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
DB など別の置き場所を使うときは、`SessionStore` を実装します。

```rust
StartSession::new(MyStore::new(7200), SessionConfig::default())
```

**期限の秒数は、置き場所にも自分で渡します。** `with_config` で期限を変えても作り直されません（警告が出ます）。

| メソッド                                 | 既定の実装          | 守ること                                        |
|------------------------------------------|---------------------|-------------------------------------------------|
| `read`                                   | なし                | **期限切れには `None` を返す**                  |
| `write` / `destroy`                      | なし                |                                                 |
| `touch(id)`                              | `read` して `write` | 期限だけ延ばす。期限切れは延ばさない            |
| `read_and_touch(id)`                     | `read` して `touch` | ルートに当たったリクエストの最初に 1 回呼ばれる |
| `destroy_for_user(user_id)`              | 何もせず 0          | 消す前に `mark_revoked` を呼ぶ                  |
| `destroy_for_user_except(user_id, keep)` | 何もせず 0          | 同上                                            |
| `mark_revoked(user_id, except)`          | 何もしない          | 「切った」印を、途中を読まれない形で書く        |
| `revoked_at(user_id, session_id)`        | `None`              | 印の時刻を返す。古い印は返さない                |

- `read` / `write` / `destroy` の 3 つは必ず書きます。ほかは書かなくても壊れません。
- `touch` などを既定のままにすると、読み書きが余分に走ります。DB なら `update` 1 文で書けます。
- 「切った」印は、処理中だった別のリクエストが、消したセッションを書き戻して生き返らせないためのものです。
- `destroy_for_user` 系が無いと、`logout_other_devices()` などは警告を出して 0 を返します。

### 組み込みの置き場所の掃除

| 置き場所     | 掃除のしかた                                                              |
|--------------|---------------------------------------------------------------------------|
| `file`       | 書き込みの約 2.3% で掃除する。1 回で見るのは最大 500 件。応答は待たせない |
| `memory`     | 書き込みの 100 回に 1 回、全部を見る                                      |
| `session:gc` | 上限なしで全部を見る                                                      |

- `file` の掃除は、書きかけの一時ファイル（5 分より古いもの）と、古い「切った」印も消します。
- 印を残すのは、1 時間とセッションの期限の長いほうです。
- `file` のファイルの権限は `0600`、ディレクトリは `0700` です（Unix だけ）。

まだ無い機能は [backlog.md](backlog.md) にあります。

## 関連

- [middleware.md](middleware.md)
- [validation.md](validation.md)
- [configuration.md](configuration.md)
- [authentication.md](authentication.md)
