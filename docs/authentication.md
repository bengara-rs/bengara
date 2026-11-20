# 認証

認証は「この人は誰か」を確かめる仕組み、つまりログインです。
誰に何を許すかは [authorization.md](authorization.md) にあります。

## 用意するもの

| 手順 | すること                                       |
|------|------------------------------------------------|
| 1    | `users` 表を作る（表と列の名前は自由）         |
| 2    | `User` のモデルに `Authenticatable` を実装する |
| 3    | `StartSession` と `Authenticate` を登録する    |

### 1. 表を作る

`users` 表に、照合に使う列（`email` など）とパスワードの列（`password`）を作ります。

```rust
schema.create("users", |t| {
    t.id();
    t.string("name");
    t.string("email").unique();
    t.string("password");
    t.boolean("is_admin").default(false);
    t.timestamps();
});
```

### 2. モデル

書くのは `password_hash()` だけです。表と主キーは `#[derive(Model)]` から取ります（[models.md](models.md)）。

```rust
// app/Models/User.rs
use bengara::prelude::*;
use bengara::Carbon;

#[derive(Model, Debug, Clone)]
#[model(table = "users")]
pub struct User {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub password: String,   // Hash::make_async が作った文字列。平文は入れない
    pub is_admin: bool,
    pub created_at: Carbon,
    pub updated_at: Carbon,
}

impl Authenticatable for User {
    fn password_hash(&self) -> &str {
        &self.password
    }
}
```

パスワードの列が `password` でないときは、`auth_password_name()` も書いて列名を返します。
ログインのついでにパスワードを作り直すとき、この列に書くためです（下の「ハッシュの強さを上げたあと」）。

### 3. 登録

```rust
// bootstrap/app.rs
.with_middleware(|m| {
    m.append(StartSession::from_env())
        .alias("auth", Authenticate::new())
})
```

セッションの準備（`APP_KEY` など）は [session.md](session.md) にあります。

## パスワードを保存する

パスワードは平文で保存せず、元に戻せない形（ハッシュ）に変えて保存します。

```rust
user.password = Hash::make_async(password).await?;
```

| 関数                                   | すること                                         |
|----------------------------------------|--------------------------------------------------|
| `Hash::make_async(平文)`               | 保存する形に変える。同じ平文でも毎回違う値になる |
| `Hash::check_async(平文, 保存した値)`  | 合っているか                                     |
| `Hash::make(平文)` / `Hash::check(..)` | 上の 2 つの同期版                                |
| `Hash::make_with(平文, 回数)`          | 回数を決めて変える（同期。いつも PBKDF2）        |
| `Hash::needs_rehash(保存した値)`       | 作り直すべきか                                   |

- **`async fn` の中では `make_async` / `check_async` を使います。** 同期版はスレッドを約 0.2 秒止めます。
- 同期版は、シーダーやテストの準備など同期の関数の中でだけ使います。

### パスワードの検査に `max:1024` を必ず付ける

```rust
("password", "required|min:8|max:1024|confirmed")
```

- 平文の上限は 4096 バイトです。`check` は、上限を超える平文を偽とします。
- ところが `make` は、上限を超えてもハッシュを作ります。**そのハッシュでは、二度とログインできません。**
- 登録・変更・再設定の 3 か所すべてに `max:1024` を付けます。1024 文字なら必ず 4096 バイトに収まります。

## ログインする

```rust
pub async fn login(req: Request) -> Result<Response> {
    let input = req.validate(&[
        ("email", "required|email"),
        ("password", "required|max:1024"),
    ])?;

    let ok = req
        .auth()
        .attempt::<User>("email", input.get("email"), input.get("password"))
        .await?;

    if !ok {
        // どちらが違うかは言わない（登録の有無が漏れないように）
        return abort_with(422, "メールアドレスかパスワードが違います");
    }
    json(&json!({ "id": req.auth().id() }))
}
```

`attempt` の 1 つ目の引数は、照合に使う列です。メールアドレス以外でもログインさせられます。

| メソッド                                    | すること                                          |
|---------------------------------------------|---------------------------------------------------|
| `attempt::<U>(列, 値, パスワード)`          | 1 件引いて照合し、合えばログイン。返るのは `bool` |
| `login(&user)`                              | 照合せずにログイン（登録の直後など）              |
| `login_using_id(値)`                        | 主キーの値だけでログイン                          |
| `check()` / `guest()`                       | ログインしているか / していないか                 |
| `id()`                                      | ログイン中の主キーの値（`Option<String>`）        |
| `user::<U>()`                               | ログイン中の利用者を DB から読む                  |
| `user_or_fail::<U>()`                       | 同じ。いなければ 401                              |
| `validate_password::<U>(&user, パスワード)` | ログインせずに照合だけする                        |

- **ログインすると、セッション ID と CSRF トークンが変わります。**
  ログイン後の画面で取り直します（[session.md](session.md)）。
- `login(&user)` は、保存していないモデル（主キーが `0`・空など）では `Err` です。先に `save()` します。
- `user()` は呼ぶたびに DB を引きます。何度も使うときは変数に入れます。
- `check()` / `guest()` / `id()` と `auth` ミドルウェアは、セッションだけを見ます。
  利用者の行を消しても、セッションが残るあいだは通ります。**利用者を消したら `logout_user(id)` を呼びます。**
- 利用者がいないときも、`attempt` は照合と同じだけ時間をかけます。応答の速さで登録の有無が分からないようにするためです。

### ログインの回数を制限する

総当たり（パスワードを次々に試す攻撃）を防ぐため、ログインの口には必ず回数の制限を付けます（[middleware.md](middleware.md)）。

```rust
Route::post("/api/login", AuthController::login).middleware("throttle");
```

- ログインの口の制限は、`Throttle::per_minute(5).by_ip()` のように接続元で数えます。
- ログイン中の人を止める `guest` も合わせて掛けます（下の節）。

## ログアウトする

切る範囲で 4 つに分かれます。

| メソッド                 | 切る範囲                     | 使う場面                         |
|--------------------------|------------------------------|----------------------------------|
| `logout()`               | **いまのセッションだけ**     | 画面の「ログアウト」ボタン       |
| `logout_other_devices()` | 同じ人の、いま以外のすべて   | パスワードを変えたあと           |
| `logout_all_devices()`   | 同じ人のすべて（自分も切る） | 「全部の端末からログアウト」     |
| `logout_user(id)`        | **指定した人**のすべて       | パスワードの再設定、利用者の削除 |

`logout()` は `Result<()>`、ほかの 3 つは消した数を返します。

```rust
// パスワードを書き換えたあとで、ほかの端末を切る
user.password = Hash::make_async(&new_password).await?;
user.save().await?;
req.auth().logout_other_devices_async().await?;
```

- **`async fn` の中では `_async` の付いた版を使います。** 同期版は置き場所を全部見るので、ほかのリクエストが止まります。
  `logout()` だけは同期のままで構いません。
- **パスワードを変えたら `logout_other_devices()` を呼びます。** 変えただけでは、盗まれたセッションが期限まで残ります。
- `logout()` と `logout_all_devices()` は、セッションの Cookie も消します。
- ログインしていなければ、何もせず `0` を返します（`logout_user` を除く）。

## ログイン必須にする

```rust
// routes/web.rs
Route::get("/api/me", AuthController::me).middleware("auth");

// グループにまとめて掛ける
Route::prefix("api/my/articles").middleware("auth").group(|| {
    Route::get("/", MyArticleController::index);
});
```

| 登録の書き方                          | ログインしていないとき                 |
|---------------------------------------|----------------------------------------|
| `Authenticate::new()`                 | 401                                    |
| `Authenticate::redirect_to("/login")` | 302（JSON を求めるリクエストには 401） |

- `StartSession` を登録していないと、`Authenticate` は 500 を返し、直し方をログに出します。
- ハンドラでは `req.auth().user_or_fail::<User>().await?` で取ります（いなければ 401）。

### ログイン後に元のページへ戻す

`redirect_to` で転送したときは、元のパスをセッションに覚えます。
`req.session().intended()` で取り出します。読むと消えます。

```rust
let back = req.session().intended().unwrap_or_else(|| "/".to_string());
redirect().to(back)
```

- 覚えるのは、`GET` か `HEAD` を 302 で転送したときだけです。自分のサイトの中のパスに限ります。
- **クエリ文字列ごと覚えます。** トークンを URL に載せたページ（署名付き URL など）に `auth`
  を掛けると、そのトークンがセッションのファイルに平文で残ります。

## ログイン中の人を通さない（guest）

ログイン・登録・パスワード再設定の口に掛けます。Laravel の `guest` に当たります。

```rust
// bootstrap/app.rs
use bengara::auth::RedirectIfAuthenticated;

.alias("guest", RedirectIfAuthenticated::new())

// routes/web.rs
Route::post("/register", AuthController::register)
    .middleware("guest")
    .middleware("throttle");
```

| 登録の書き方                                    | ログインしているとき                   |
|-------------------------------------------------|----------------------------------------|
| `RedirectIfAuthenticated::new()`                | 403                                    |
| `RedirectIfAuthenticated::redirect_to("/home")` | 302（JSON を求めるリクエストには 403） |

- `prelude` には入っていません。`bengara::auth` から `use` します。
- 転送先を決めないと 403 です（Laravel と違います）。
- `Throttle` はログイン中の人を ID で数えます。登録し直すたびに新しい枠ができるので、`guest` で止めるか `by_ip()` で数えます。

## パスワードの再設定

bengara が作るのは、トークン（一度だけ使える合い言葉）とリンクまでです。
メールの文面・送信・画面は自分で書きます。送るときは `Mail` を使います（[mail.md](mail.md)）。

先にトークンを入れる `password_resets` 表を作ります。

```rust
// database/migrations/..._create_password_resets_table.rs
pub fn up(schema: &mut Schema) {
    PasswordReset::define(schema);
}
```

```rust
// 1. リンクを作る（60 秒以内に作ったばかりなら、作り直さない）
if !PasswordReset::recently_created(&email).await? {
    let token = PasswordReset::create(&email).await?;
    let link = PasswordReset::link("/password/reset", &email, &token, 3600)?;
    // link をメールで送る
}
// 送ったかどうかに関わらず、同じ応答を返す

// 2. 受け取ったトークンを照合して、その場で使い切る
if !PasswordReset::consume_if_valid(&email, &token).await? {
    return abort_with(422, "リンクが正しくないか、期限が切れています");
}

// 3. パスワードを書き換えて、その人のセッションを全部切る
user.password = Hash::make_async(&new_password).await?;
user.save().await?;
req.auth().logout_user_async(user.key()).await?;
```

| メソッド                      | すること                                                    |
|-------------------------------|-------------------------------------------------------------|
| `create(email)`               | トークンを作って表に入れ、平文のトークンを返す              |
| `recently_created(email)`     | そのアドレスのトークンを 60 秒以内に作ったか                |
| `link(パス, email, 鍵, 秒)`   | 署名付き・期限つきの URL を作る（[routing.md](routing.md)） |
| `consume_if_valid(email, 鍵)` | 照合して、合っていればその場で消す                          |
| `verify(email, 鍵)`           | 照合だけする。入力欄を出してよいかの下調べ用                |
| `consume(email)`              | トークンを消す。再設定をやめたときの片付け用                |
| `sweep_expired()`             | 期限切れをまとめて消す                                      |
| `lifetime_minutes()`          | トークンを使える時間（60 分）                               |
| `define(&mut Schema)`         | 表を作る定義                                                |

- **再設定の判定には、必ず `consume_if_valid` を使います。** `verify` の後に `consume` だと、間に同じトークンを 2
  回使われることがあります。
- トークンは 1 つのアドレスに 1 本です。作り直すと前のものは使えません。
- `create` 自体は回数を止めません。再送の間隔は `recently_created` で確かめます。
- **知らないアドレスを頼まれたときも、同じ応答を返します。** 応答が違うと、登録の有無が分かります。
- リンクは `GET` で開かれます。同じパスに `GET` のルートも用意します。

## 暗号化

値を他人に読めない形にします。機能フラグ `encryption` が要ります。

```toml
bengara = { version = "0.1", features = ["sqlite", "encryption"] }
```

```rust
let hidden = encrypt("ひみつ")?;     // 毎回違う文字列になる
let plain = decrypt(&hidden)?;       // 改ざんされていればエラー
```

- 鍵のもとは `APP_KEY` です（[configuration.md](configuration.md)）。
- 改ざんを見抜くだけでよいなら、`signed_url` を使います（[routing.md](routing.md)）。

## ハッシュの方式と強さ

方式は 2 つです。`HASH_DRIVER` で選びます。

| 方式           | 選び方                                        | 保存される形                                     |
|----------------|-----------------------------------------------|--------------------------------------------------|
| PBKDF2（既定） | 何もしない                                    | `$pbkdf2-sha256$i=240000$<塩>$<ハッシュ>`        |
| Argon2id       | 機能フラグ `argon2` と `HASH_DRIVER=argon2id` | `$argon2id$v=19$m=65536,t=4,p=1$<塩>$<ハッシュ>` |

- Argon2id は、計算に大量のメモリを使わせる方式です。GPU での総当たりに PBKDF2 より強いです。
- `check` は、どちらの方式の値でも照合します。方式を替えても、古い値でログインできます。
- Laravel（PHP の `password_hash`）で作った `$argon2id$` の値は、そのまま照合できます。
- **bcrypt（`$2y$`、Laravel の既定）と `$argon2i$` は照合できません。** 偽になります。
- `cargo artisan about` の「パスワード」の行で、いまの方式と強さが分かります。

### PBKDF2 の回数

回数は `config/hashing.rs` で変えます。

```rust
// config/hashing.rs
pub fn config() -> HashConfig {
    HashConfig {
        iterations: env("HASH_ITERATIONS", 240_000u32),
    }
}
```

| 項目         | 値                                                               |
|--------------|------------------------------------------------------------------|
| 既定         | 240,000 回（リリースビルドで約 0.2 秒、デバッグビルドで約 4 秒） |
| テストのとき | 1,000 回。`.env` は見ない（[testing.md](testing.md)）            |
| 上限         | 10,000,000 回。超える設定は上限で計算し、超える保存値は偽とする  |

- テストで回数を変えるときは、環境変数で渡します（`HASH_ITERATIONS=20000 cargo test`）。

### Argon2id の強さ

```toml
bengara = { version = "0.1", features = ["sqlite", "argon2"] }
```

| 環境変数        | 既定     | 意味                   | 範囲                         |
|-----------------|----------|------------------------|------------------------------|
| `HASH_DRIVER`   | `pbkdf2` | `pbkdf2` か `argon2id` | —                            |
| `ARGON_MEMORY`  | `65536`  | 使うメモリ（KiB）      | `8 × ARGON_THREADS` 〜 1 GiB |
| `ARGON_TIME`    | `4`      | 繰り返し               | 1〜64                        |
| `ARGON_THREADS` | `1`      | 並列の数               | 1〜16                        |

- 名前と既定値は Laravel 12 の `config/hashing.php` と同じです。
- 範囲の外の値は、範囲に寄せて使います。ログに警告が出ます。
- テストのときは `m=8, t=1, p=1` に下げます。`ARGON_*` を環境変数で渡したときは、その値を使います。
- フラグが無いのに `HASH_DRIVER=argon2id` にしたときと、知らない名前のときは、ログに出して PBKDF2 で作ります。

設定ファイルで決めるときは、`config/argon.rs` を置きます（[configuration.md](configuration.md)）。

```rust
// config/argon.rs
use bengara::prelude::*;
use bengara::ArgonConfig;

pub fn config() -> ArgonConfig {
    ArgonConfig {
        memory: env("ARGON_MEMORY", 65_536u32),
        time: env("ARGON_TIME", 4u32),
        threads: env("ARGON_THREADS", 1u32),
    }
}
```

デバッグビルドの Argon2id は、照合 1 回に約 3 秒かかります。
アプリの `Cargo.toml` に次を書くと、約 0.2 秒になります。

```toml
[profile.dev.package.argon2]
opt-level = 3
[profile.dev.package.blake2]
opt-level = 3
```

### ハッシュの強さを上げたあと

**保存済みのパスワードは、古い強さのままです。** `attempt` が、ログインのついでに作り直します（Laravel の `rehashOnLogin`）。

- 作り直すのは、`Hash::needs_rehash` が真のときです。

| いまの方式 | `needs_rehash` が真になる値                                     |
|------------|-----------------------------------------------------------------|
| `pbkdf2`   | 回数が設定より少ない値。形の読めない値                          |
| `argon2id` | PBKDF2 の値。`m`・`t`・`p` が設定と違う値（設定より強くても真） |

- 書く列は `Authenticatable::auth_password_name()` です。既定は `password` です。
- 作り直しに失敗しても、ログインは成功します。ログに警告が出ます。
- `HASH_DRIVER=pbkdf2` に戻しても、Argon2id の値は作り直しません。
- ログインしない人の値は、古いまま残ります。

`attempt` を使わずに自分で照合するときは、次のように書きます。

```rust
if ok && Hash::needs_rehash(&user.password) {
    user.password = Hash::make_async(input.get("password")).await?;
    user.save_only(&["password"]).await?;
}
```

`save()` ではなく `save_only` でパスワードの列だけを書きます。
`save()` は丸ごと書き戻すので、別のリクエストが変えた列を古い値で上書きします。

### 同時に計算する数

パスワードの計算は、同時に走らせる数を `HASH_MAX_CONCURRENCY` までに抑えます。
Argon2id は 1 件で既定 64 MiB を使うので、ログインが押し寄せてもメモリを使いすぎないためです。

- 既定は CPU の数です。上限を超えた分は、空くまで待ちます。
- 上限が効くのは `make_async`・`check_async`・`attempt`・`validate_password` です。
- **同期版の `Hash::make` / `Hash::check` には効きません。**

## 関連

- [authorization.md](authorization.md) — 誰に何を許すか
- [session.md](session.md) — セッションと CSRF
- [models.md](models.md) — モデル
- [testing.md](testing.md) — テスト
