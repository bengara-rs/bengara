# 認証

このページは 3 つを扱います。

1. **パスワードの保存** — 平文を保存する形に変える
2. **ログインとログアウト** — 照合、セッション、ログイン必須にする設定
3. **パスワードの再設定** — トークンとリンクを作る

誰に何を許すかは [authorization.md](authorization.md) にあります。

## 用意するもの

| 手順 | すること                                             |
|------|------------------------------------------------------|
| 1    | `users` 表を作る（名前と列は自由です）               |
| 2    | `User` のモデルに `Authenticatable` を実装する       |
| 3    | `StartSession` と `Authenticate` を登録する          |

### 1. 表を作る

```rust
// database/migrations/2026_10_05_000200_create_users_table.rs
use bengara::prelude::*;

pub fn up(schema: &mut Schema) {
    schema.create("users", |t| {
        t.id();
        t.string("name");
        t.string("email").unique();
        t.string("password");
        t.boolean("is_admin").default(false);
        t.timestamps();
    });
}

pub fn down(schema: &mut Schema) {
    schema.drop_if_exists("users");
}
```

### 2. モデル

```rust
// app/Models/User.rs
use bengara::prelude::*;

#[derive(Model, Debug, Clone)]
#[model(table = "users")]
pub struct User {
    pub id: i64,
    pub name: String,
    pub email: String,
    /// Hash::make_async が作った文字列。平文は入れません。
    pub password: String,
    pub is_admin: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl Authenticatable for User {
    fn password_hash(&self) -> &str {
        &self.password
    }
}
```

書くのは `password_hash()` の 1 つだけです。
表の名前と主キーは `#[derive(Model)]` から受け継ぎます。

> `#[derive(Model)]` が使えるのは、**自動で番号が振られる整数の主キー**だけです。
> 詳しくは [models.md](models.md) にあります。

### 3. 登録

```rust
// bootstrap/app.rs
.with_middleware(|m| {
    m.append(StartSession::from_env())
        .alias("auth", Authenticate::new())
})
```

## パスワードを保存する

変換には 0.2 秒ほどかかります。
`async fn` の中では **`Hash::make_async` を使ってください。**

```rust
pub async fn register(name: &str, email: &str, password: &str) -> Result<User> {
    Ok(User {
        id: 0,
        name: name.to_string(),
        email: email.to_string(),
        password: Hash::make_async(password).await?,
        is_admin: false,
        created_at: String::new(),
        updated_at: String::new(),
    })
}
```

| 関数                                  | すること                                     |
|---------------------------------------|----------------------------------------------|
| `Hash::make_async(平文)`              | 保存する形に変える。**非同期の中ではこちら** |
| `Hash::check_async(平文, 保存した値)` | 合っているか。**非同期の中ではこちら**       |
| `Hash::make(平文)`                    | 同期版。**同じ平文でも毎回違う値**になります |
| `Hash::check(平文, 保存した値)`       | 同期版                                       |
| `Hash::needs_rehash(保存した値)`      | 回数の設定を上げた後、作り直すべきか         |

**照合に渡せる平文は 1024 バイトまでです。**
それより長い値は計算せずに偽を返します（警告がログに出ます）。
入口で `max:1024` を付けておくと、利用者には 422 で返せます。
上限が無いと、長い文字列を送るだけで CPU を使わせられます。

### 同期版と非同期版の使い分け

| 場面                                             | 使うもの                     |
|--------------------------------------------------|------------------------------|
| コントローラ・ジョブ・コマンドの `async fn` の中 | `make_async` / `check_async` |
| モデルの `async fn` の中                         | `make_async` / `check_async` |
| 同期の関数（シーダの組み立て、テストの準備など） | `make` / `check`             |

同期版は 0.2 秒のあいだスレッドを塞ぎます。
そのあいだ、同じスレッドで動くほかのリクエストが止まります。
非同期版は裏のスレッドに逃がすので、ほかのリクエストを待たせません。

`req.auth().attempt(...)` は中で裏のスレッドに逃がしています。
ログインのときは、そのまま呼んで大丈夫です。

### 保存される形

方式は PBKDF2-HMAC-SHA256 です。保存されるのはこういう文字列です。

```
$pbkdf2-sha256$i=120000$<塩>$<ハッシュ>
```

方式と回数が入っています。
そのため **あとから回数を上げても古いパスワードは照合できます。**

照合に使う回数は、保存されている値の `i=` から読みます。
その値には **上限（1,000 万回）があります。**
上限を超える値は照合せずに偽とします。
DB に細工した値を入れられたときに、1 回の照合で何十秒も計算させられないようにするためです。

### 回数を変える

```rust
// config/hashing.rs
use bengara::prelude::*;

pub fn config() -> HashConfig {
    HashConfig {
        iterations: env("HASH_ITERATIONS", 120_000u32),
    }
}
```

| 場面                   | 回数                  | 1 回あたり |
|------------------------|-----------------------|------------|
| 本番（リリースビルド） | 120,000（既定）       | 約 0.23 秒 |
| 開発（デバッグビルド） | 同じ                  | 約 3.6 秒  |
| テスト                 | 1,000（自動で下がる） | 約 0.03 秒 |

テストのときは `#[bengara::test]` が自動で下げます。
**`.env` に書いた値はテストでは見ません。**
開発用に下げた値でテストが走ると、件数が増えたときに時間がかかるためです。

テストで変えたいときは環境変数で渡します。

```sh
HASH_ITERATIONS=20000 cargo test
```

## ログインする

```rust
pub async fn login(req: Request) -> Result<Response> {
    let input = req.validate(&[("email", "required|email"), ("password", "required")])?;

    let ok = req
        .auth()
        .attempt::<User>("email", input.get("email"), input.get("password"))
        .await?;

    if !ok {
        // どちらが違うかは言わない（登録の有無が漏れないように）
        return abort_with(422, "メールアドレスかパスワードが違います");
    }
    json(&bengara::serde_json::json!({ "id": req.auth().id() }))
}
```

`attempt` の第 1 引数は **照合に使う列**です。
メールアドレス以外でもログインさせられます。

| メソッド                                    | すること                                          |
|---------------------------------------------|---------------------------------------------------|
| `attempt::<U>(列, 値, パスワード)`          | 1 件引いて照合し、合えばログイン。返るのは `bool` |
| `login(&user)`                              | 照合せずにログイン（登録の直後など）              |
| `login_using_id(値)`                        | 主キーの値だけでログイン                          |
| `check()` / `guest()`                       | ログインしているか / していないか                 |
| `id()`                                      | ログイン中の主キーの値（`Option<String>`）        |
| `user::<U>()`                               | ログイン中の利用者を DB から読む                  |
| `user_or_fail::<U>()`                       | 同じ。いなければ 401 のエラー                     |
| `validate_password::<U>(&user, パスワード)` | ログインせずに照合だけする                        |

- `attempt` と `login` は **セッション ID を作り直します。**
  他人のセッション ID を押し付ける攻撃への対策です。
- **`user()` は呼ぶたびに DB を引きます。** 何度も使うときは変数に入れてください。
- 利用者が見つからないときも、`attempt` は **照合と同じだけ時間をかけます。**
  すぐ帰ると、応答の速さから「このアドレスは登録されていない」ことが分かるためです。

> セッション ID を作り直すと、**CSRF トークンも作り直されます。**
> ログインの画面で取ったトークンは、ログインしたあとでは使えません。
> ログイン後の画面で、新しいトークンを取り直してください（[session.md](session.md)）。

## ログアウトする

切る範囲が 4 つに分かれています。

| メソッド                 | 切る範囲                         | 返るもの     | 使う場面                   |
|--------------------------|----------------------------------|--------------|----------------------------|
| `logout()`               | **いまのセッションだけ**         | `Result<()>` | 画面の「ログアウト」ボタン |
| `logout_other_devices()` | 同じ利用者の、いま以外のすべて   | 消した数     | パスワードを変えたあと     |
| `logout_all_devices()`   | 同じ利用者のすべて（自分も切る） | 消した数     | 「全部の端末からログアウト」|
| `logout_user(id)`        | **指定した利用者**のすべて       | 消した数     | パスワードの再設定         |

- **パスワードを変えたら `logout_other_devices()` を呼んでください。**
  変えただけでは、盗まれたセッションが期限まで生き残ります。
- パスワードの再設定で `logout_user(id)` を使うのは、本人がログインしていないからです。
  利用者を指定する口が必要になります。

```rust
// パスワードを書き換えたあとで、ほかの端末を切る
user.password = Hash::make_async(&new_password).await?;
user.save().await?;
req.auth().logout_other_devices()?;
```

気をつけること。

- ログインしていなければ、何もせず `0` が返ります。
- 置き場所の中を全部見ます。**時間のかかる操作です。**
  索引は持っていないので、呼ぶ回数は少なくしてください。
- 自分で作った `SessionStore` では、`destroy_for_user` と `destroy_for_user_except`
  を実装していなければ警告が出て `0` が返ります。

## ログイン必須にする

```rust
// bootstrap/app.rs
.alias("auth", Authenticate::new())
```

```rust
// routes/web.rs
Route::get("/api/me", UserController::me).middleware("auth");

// グループにまとめて掛ける
Route::prefix("api/my").middleware("auth").group(|| {
    Route::get("/articles", MyArticleController::index);
});
```

| 書き方                                | ログインしていないとき                       |
|---------------------------------------|----------------------------------------------|
| `Authenticate::new()`                 | 401                                          |
| `Authenticate::redirect_to("/login")` | 302（JSON を期待しているリクエストには 401） |

### 止めたときの行き先

`req.session().intended()` で取り出せます。読むと消えます。

```rust
let back = req.session().intended().unwrap_or_else(|| "/".to_string());
redirect().to(back)
```

- 覚えるのは **302 で転送するときだけ**です。`Authenticate::new()`（401）や、
  JSON を期待しているリクエストでは覚えません。401 を返すだけなら使い道が無く、
  401 のたびにセッションを書くことになるからです。
- 覚えるのは自分のサイトの中を指すパスだけです。

### ログインの回数を制限する

既存の `Throttle` を重ねるだけです。

```rust
Route::post("/api/login", AuthController::login).middleware("throttle");
```

総当たりを防ぐために、ログインには必ず付けてください
（[middleware.md](middleware.md)）。

## 今ログインしている人を使う

```rust
pub async fn me(req: Request) -> Result<Response> {
    let user = req.auth().user_or_fail::<User>().await?;   // 401 か User
    json(&bengara::serde_json::json!({ "name": user.name }))
}
```

`Auth::user()` のように **どこからでも読める形はありません。**
`req` から取ります。
ハンドラは内部で別のタスクとして動くため、どこからでも読める置き場所が作れないためです。

## パスワードの再設定

**メールの送信はまだありません。** トークンとリンクを作るところまでです。

```rust
// database/migrations/..._create_password_resets_table.rs
pub fn up(schema: &mut Schema) {
    PasswordReset::define(schema);
}
```

```rust
// 1. リンクを作る
let token = PasswordReset::create(&email).await?;
let link = PasswordReset::link("/password/reset", &email, &token, 3600)?;

// 2. 受け取ったトークンを照合して、その場で使い切る
if !PasswordReset::consume_if_valid(&email, &token).await? {
    return abort_with(422, "リンクが正しくないか、期限が切れています");
}

// 3. パスワードを書き換えて、その人のセッションを全部切る
user.password = Hash::make_async(&new_password).await?;
user.save().await?;
req.auth().logout_user(user.id)?;
```

| メソッド                      | すること                                           |
|-------------------------------|----------------------------------------------------|
| `create(email)`               | トークンを作って表に入れ、**平文のトークン**を返す |
| `consume_if_valid(email, 鍵)` | 照合して、合っていれば**その場で消す**             |
| `verify(email, 鍵)`           | 照合だけする。**入力欄を出してよいかの下調べ用**   |
| `consume(email)`              | トークンを消す                                     |
| `sweep_expired()`             | 期限切れをまとめて消す                             |
| `link(パス, email, 鍵, 秒)`   | 署名付き・期限つきの URL を作る                    |
| `define(&mut Schema)`         | 表を作る定義                                       |

**`consume_if_valid` を使ってください。**
照合と削除を 1 つの文で行うので、トークンは 1 回しか使えません。
`verify` で真を見てから `consume` を呼ぶ 2 段の書き方はやめてください。
その間に別のリクエストが入ると、同じトークンが 2 回使えてしまいます。

- 表に入るのは **トークンのハッシュ**です。表が漏れても、そのままでは使えません。
- 期限は 60 分です。
- 期限切れ・不一致・記録なしは、すべて同じ「偽」です。理由は区別しません。
- **知らないメールアドレスを頼まれたときも、同じ応答を返してください。**
  応答が違うと、登録の有無が分かってしまいます。

## 暗号化

値を他人に読めない形にします。機能フラグ `encryption` が必要です。

```toml
bengara = { version = "0.1", features = ["sqlite", "encryption"] }
```

```rust
let hidden = encrypt("ひみつ")?;     // 毎回違う文字列になる
let plain = decrypt(&hidden)?;       // 改ざんされていればエラー
```

- 鍵のもとは `APP_KEY` です。`cargo artisan key:generate` で作ってください。
- 実際に使う鍵は `hmac_sha256(APP_KEY, "bengara:encryption")` です。
  `APP_KEY` の生バイトはそのまま使いません。
  セッション Cookie の署名や署名付き URL とは **別の鍵**になります。
- 改ざんも検出できます（中身を変えられたら復号がエラーになります）。
- 署名だけで足りるなら `signed_url` を使ってください（[routing.md](routing.md)）。

> **互換性の注意** — 鍵の作り方を変えました。
> いまある暗号文は読めなくなります。
> 配布済みのセッションと署名付き URL も無効になります。
> `APP_KEY` を作り直したときと同じ扱いです。

### `APP_KEY` の長さ

`APP_KEY` は **64 文字以上**が必須です。
`cargo artisan key:generate` は 32 バイトを 16 進にした 64 文字を作ります。

短い値を入れると、セッション・署名付き URL・暗号化を使う最初のリクエストでエラーになります。
短い鍵でも計算自体は通ってしまうので、入口で断っています。

## まだ無いもの

| 無いもの                                    | 補足                           |
|---------------------------------------------|--------------------------------|
| 「ログイン状態を覚える」Cookie              | [backlog.md](backlog.md)       |
| 複数の認証の仕組み（Laravel の guard）      | [backlog.md](backlog.md)       |
| API トークン                                | [backlog.md](backlog.md)       |
| Argon2 / bcrypt                             | [backlog.md](backlog.md)       |
| 再設定のメールの送信                        | `SMTP` が無いため（[mail.md](mail.md)） |

## 関連

- [authorization.md](authorization.md) — 誰に何を許すか
- [session.md](session.md) — セッションと CSRF
- [models.md](models.md) — モデル
- [testing.md](testing.md) — テスト
