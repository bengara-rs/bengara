# セッションと CSRF

リクエストをまたいで値を覚えておく仕組みと、別のサイトからの書き込みを防ぐ仕組みです。

## 準備

**`APP_KEY` が要ります。** 署名に使います。

```sh
cargo artisan key:generate
```

`.env` に書き込まれます。**この鍵を変えると、いまのセッションと署名付き URL は全部無効になります。**

`bootstrap/app.rs` に登録します。**順番が大事です。**

```rust
Application::configure()
    .with_middleware(|m| {
        m.append(StartSession::from_env())            // 先に
            .append(VerifyCsrfToken::new())           // 後に
    })
    .with_routing(|r| r.web(crate::routes::web::routes))
    .create()
```

## 置き場所

中身は**サーバー側**に置き、ブラウザには ID だけを署名つきの Cookie で渡します。
中身がブラウザに出ないので、Cookie の 4 KB という上限にも縛られません。

| `SESSION_DRIVER` | 置き場所 |
|---|---|
| `file`（既定） | `storage/framework/sessions/` |
| `memory` | プロセスのメモリ。**テストと手元の確認用** |

`SESSION_LIFETIME` は分です（既定 120）。

**プロセスを2つ以上動かすときは、全プロセスが同じ `storage/` を見るようにしてください。**
`memory` はプロセスをまたげないので本番では使えません。

期限切れは読むときに消えますが、ファイルを掃除するコマンドもあります。

```sh
cargo artisan session:gc
```

## 使う

```rust
pub async fn store(req: Request) -> Result<Response> {
    req.session().put("cart", "12");
    req.session().flash("status", "保存しました");
    redirect().route("home")
}
```

| メソッド | 中身 |
|---|---|
| `get(key)` | 読む。無ければ `None` |
| `get_or(key, default)` | 読む。無ければ既定値 |
| `put(key, value)` | 入れる。次のリクエストでも読める |
| `flash(key, value)` | 入れる。**次のリクエストまで**だけ残る |
| `has(key)` | あるか |
| `forget(key)` | 消す |
| `pull(key)` | 読んでから消す |
| `all()` | 全部の組 |
| `old(key)` | 検査に落ちたときの入力 |
| `flush()` | 中身を全部消す |
| `regenerate()` | ID を作り直す |
| `invalidate()` | 中身を消して ID も作り直す |

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

## Cookie の既定

| 属性 | 既定 | 意味 |
|---|---|---|
| `HttpOnly` | 付く | JavaScript から読めない |
| `SameSite` | `Lax` | 他のサイトからの書き込みでは送らない |
| `Secure` | `APP_ENV=production` なら強制 | HTTPS のときだけ送る |
| `Path` | `/` | |

変えたいときは `SessionConfig` を渡します。

```rust
StartSession::from_env().with_config(SessionConfig {
    cookie: "myapp_session".into(),
    same_site: SameSite::Strict,
    ..Default::default()
})
```

## CSRF

`GET` / `HEAD` / `OPTIONS` は素通しし、それ以外でトークンを確かめます。
合わなければ **419** を返します。

### フォームから

```html
<input type="hidden" name="_token" value="{{ token }}">
```

トークンは `req.csrf_token()` で取り出せます。

### SPA から

トークンを配るルートを1本作ります。

```rust
// routes/web.rs
Route::get("/csrf-token", AuthController::token);

// コントローラ
pub async fn token(req: Request) -> Result<Response> {
    json(&bengara::serde_json::json!({ "token": req.csrf_token() }))
}
```

以後は `X-CSRF-TOKEN` ヘッダーに入れて送ります。

### 確かめない場所

```rust
VerifyCsrfToken::new().except("/api/*").except("/webhook")
```

末尾の `*` で前方一致になります。
**外から呼ばれる受け口（webhook）は、別の方法で相手を確かめてください。**

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

- **セッションに秘密を入れすぎない。** 中身はサーバー側にありますが、置き場所は平文のファイルです。
- `req.session()` は `StartSession` を登録していないと**パニックします**。登録し忘れにすぐ気づくためです。
- `VerifyCsrfToken` を `StartSession` より前に置くと 500 になります。

## まだ無いもの

DB / Redis への保存、1回だけ有効なトークン、認証（ログインそのもの）はありません。
[backlog.md](backlog.md) を参照してください。

---

関連: [middleware.md](middleware.md) / [validation.md](validation.md) / [configuration.md](configuration.md)
