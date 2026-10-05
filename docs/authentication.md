# 認証

ログイン・ログアウト・パスワードの扱いです。
誰に何を許すかは [authorization.md](authorization.md) にあります。

## 用意するもの

1. `users` 表（名前と列は自由です）
2. `User` のモデルに `Authenticatable` を実装する
3. `bootstrap/app.rs` に `StartSession` と `Authenticate` を登録する

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
    /// Hash::make が作った文字列。平文は入れません。
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

### 3. 登録

```rust
// bootstrap/app.rs
.with_middleware( | m| {
m.append(StartSession::from_env())
.alias("auth", Authenticate::new())
})
```

## パスワードを保存する

```rust
let mut user = User {
id: 0,
name: "アリス".to_string(),
email: "alice@example.com".to_string(),
password: Hash::make("ひみつの言葉"),   // ここで変換する
is_admin: false,
created_at: String::new(),
updated_at: String::new(),
};
user.save().await?;
```

| 関数                             | すること                                                 |
|----------------------------------|----------------------------------------------------------|
| `Hash::make(平文)`               | 保存する形に変える。**同じ平文でも毎回違う値**になります |
| `Hash::check(平文, 保存した値)`  | 合っているか                                             |
| `Hash::needs_rehash(保存した値)` | 回数の設定を上げた後、作り直すべきか                     |

方式は PBKDF2-HMAC-SHA256 です。保存されるのはこういう文字列です。

```
$pbkdf2-sha256$i=120000$<塩>$<ハッシュ>
```

方式と回数が入っているので、 **あとから回数を上げても古いパスワードは照合できます。**

> **`Hash::make` と `Hash::check` は時間がかかります**（既定で 0.2 秒ほど）。
> 非同期の処理の中で直接呼ぶと、その間ほかのリクエストを待たせます。
> ログインには `req.auth().attempt(...)` を使ってください（裏のスレッドに逃がします）。

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

テストのときは `#[bengara::test]` が自動で下げます。 **`.env` に書いた値はテストでは見ません。** 開発用に下げた値でテストが走ると、
件数が増えたときに時間がかかるためです。テストで変えたいときは環境変数で渡します。

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

`attempt` の第1引数は **照合に使う列**です。メールアドレス以外でもログインさせられます。

| メソッド                                    | すること                                          |
|---------------------------------------------|---------------------------------------------------|
| `attempt::<U>(列, 値, パスワード)`          | 1 件引いて照合し、合えばログイン。返るのは `bool` |
| `login(&user)`                              | 照合せずにログイン（登録の直後など）              |
| `login_using_id(値)`                        | 主キーの値だけでログイン                          |
| `logout()`                                  | セッションの中身を捨て、ID を作り直す             |
| `check()` / `guest()`                       | ログインしているか / していないか                 |
| `id()`                                      | ログイン中の主キーの値（`Option<String>`）        |
| `user::<U>()`                               | ログイン中の利用者を DB から読む                  |
| `user_or_fail::<U>()`                       | 同じ。いなければ 401 のエラー                     |
| `validate_password::<U>(&user, パスワード)` | ログインせずに照合だけする                        |

- `attempt` と `login` は **セッション ID を作り直します**（他人のセッション ID を押し付ける攻撃への対策）。
- **`user()` は呼ぶたびに DB を引きます。** 何度も使うときは変数に入れてください。
- 利用者が見つからないときも、`attempt` は **照合と同じだけ時間をかけます。**
  すぐ帰ると「このアドレスは登録されていない」ことが応答の速さから分かるためです。

## ログイン必須にする

```rust
// bootstrap/app.rs
.alias("auth", Authenticate::new())
```

```rust
// routes/web.rs
Route::get("/api/me", UserController::me).middleware("auth");

// グループにまとめて掛ける
Route::prefix("api/my").middleware("auth").group(| | {
Route::get("/articles", MyArticleController::index);
});
```

| 書き方                                | ログインしていないとき                       |
|---------------------------------------|----------------------------------------------|
| `Authenticate::new()`                 | 401                                          |
| `Authenticate::redirect_to("/login")` | 302（JSON を期待しているリクエストには 401） |

止めたときの行き先は、セッションの `bengara_auth_intended` に入ります。
ログイン後に元の画面へ戻したいときに使えます。

## ログインの回数を制限する

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

`Auth::user()` のように **どこからでも読める形はありません。** `req` から取ります。
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
let token = PasswordReset::create( & email).await?;
let link = PasswordReset::link("/password/reset", & email, & token, 3600) ?;

// 2. 受け取ったトークンを確かめる
if ! PasswordReset::verify( & email, & token).await? {
return abort_with(422, "リンクが正しくないか、期限が切れています");
}

// 3. パスワードを書き換えて、トークンを使い切る
user.password = Hash::make( & new_password);
user.save().await?;
PasswordReset::consume( & email).await?;
```

| メソッド                       | すること                                           |
|--------------------------------|----------------------------------------------------|
| `create(email)`                | トークンを作って表に入れ、**平文のトークン**を返す |
| `verify(email, token)`         | 合っているか。期限切れ（60 分）も偽                |
| `consume(email)`               | 使い終わったトークンを消す                         |
| `sweep_expired()`              | 期限切れをまとめて消す                             |
| `link(パス, email, token, 秒)` | 署名付き・期限つきの URL を作る                    |
| `define(&mut Schema)`          | 表を作る定義                                       |

- 表に入るのは **トークンのハッシュ**です。表が漏れても、そのままでは使えません。
- 期限切れ・不一致・記録なしは、すべて同じ「偽」です。理由は区別しません。
- **知らないメールアドレスを頼まれたときも、同じ応答を返してください。**
  応答が違うと、登録の有無が分かってしまいます。

## 暗号化

値を他人に読めない形にします。機能フラグ `encryption` が必要です。

```toml
bengara = { version = "0.1", features = ["sqlite", "encryption"] }
```

```rust
let hidden = encrypt("ひみつ") ?;     // 毎回違う文字列になる
let plain = decrypt( & hidden) ?;       // 改ざんされていればエラー
```

- 鍵は `APP_KEY` です。`cargo artisan key:generate` で作ってください。
- 改ざんも検出できます（認証付き暗号）。
- 署名だけで足りるなら `signed_url` を使ってください（[routing.md](routing.md)）。

## まだ無いもの

| 項目                                          | 状況                    |
|-----------------------------------------------|-------------------------|
| 「ログイン状態を覚える」Cookie（remember me） | 作っていません          |
| 複数の認証の仕組み（Laravel の guard）        | セッション 1 本だけです |
| API トークン                                  | ありません              |
| Argon2 / bcrypt                               | PBKDF2 だけです         |
| メールの送信                                  | **SMTP がありません**。`log` か `array` だけです（[mail.md](mail.md)） |

一覧は [backlog.md](backlog.md) にあります。

## 関連

- [authorization.md](authorization.md) — 誰に何を許すか
- [session.md](session.md) — セッションと CSRF
- [models.md](models.md) — モデル
- [testing.md](testing.md) — テスト
