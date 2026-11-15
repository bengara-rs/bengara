# 認証

ログインの仕組みです。このページで分かることは 4 つです。

- パスワードの保存と、ログイン・ログアウトの書き方
- ルートをログイン必須にする方法
- パスワードの再設定
- 値の暗号化

誰に何を許すかは [authorization.md](authorization.md) にあります。

## 用意するもの

| 手順 | すること                                       |
|------|------------------------------------------------|
| 1    | `users` 表を作る（名前と列は自由です）         |
| 2    | `User` のモデルに `Authenticatable` を実装する |
| 3    | `StartSession` と `Authenticate` を登録する    |

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

書くのは `password_hash()` の 1 つだけです。
表の名前と主キーは `#[derive(Model)]` から受け継ぎます（[models.md](models.md)）。

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
| `Hash::make_async(平文)`              | 保存する形に変える。**同じ平文でも毎回違う値**になる |
| `Hash::check_async(平文, 保存した値)` | 合っているか                                 |
| `Hash::make(平文)` / `Hash::check(..)` | 上の 2 つの同期版                           |
| `Hash::make_with(平文, 回数)`         | 回数を指定して変える（同期）                 |
| `Hash::needs_rehash(保存した値)`      | 回数の設定を上げた後、作り直すべきか         |

### 同期版と非同期版の使い分け

| 場面                                             | 使うもの                     |
|--------------------------------------------------|------------------------------|
| コントローラ・ジョブ・コマンド・モデルの `async fn` の中 | `make_async` / `check_async` |
| 同期の関数（シーダの組み立て、テストの準備など） | `make` / `check`             |

- 同期版は約 0.2 秒のあいだスレッドを塞ぎ、同じスレッドのほかのリクエストを止めます。
- `req.auth().attempt(...)` は、そのまま呼んで大丈夫です。中で裏のスレッドに回しています。

### パスワードの検査に `max:1024` を必ず付ける

```rust
("password", "required|min:8|max:1024|confirmed")
```

| 関数    | 1024 バイトを超える平文を渡すと                   |
|---------|---------------------------------------------------|
| `check` | 計算せずに偽を返す                                |
| `make`  | **そのままハッシュを作る**。ログに `error` が出る |

**そのハッシュは `check` が必ず偽を返すので、その人は二度とログインできません。**
パスワードを受け取る口は、ひとつ残らず `max:1024` を付けます。
登録・変更・再設定の 3 か所です。

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

`attempt` の第 1 引数は **照合に使う列**です。メールアドレス以外でもログインさせられます。

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

- **ログインするとセッション ID を作り直します。CSRF トークンも変わります。**
  ログイン後の画面で取り直します（[session.md](session.md)）。
- **`user()` は呼ぶたびに DB を引きます。** 何度も使うときは変数に入れます。
  セッションに入っている主キーの値が整数に読めないときだけは、問い合わせずに `None` を返します
  （[models.md](models.md)）。
- **`check()` / `guest()` / `id()` と `auth` ミドルウェアは DB を引きません。**
  見るのはセッションだけです。利用者の行を消しても、セッションが残っているあいだは
  `auth` を通ります。**利用者を消したら `logout_user(id)` を呼びます。**
- 利用者が見つからないときも、`attempt` は **照合と同じだけ時間をかけます。**
  応答の速さから登録の有無が分かるのを防ぐためです。
- **`Auth::user()` のように、どこからでも読める形はありません。** `req` から取ります。

### ログインの回数を制限する

**総当たりを防ぐために、ログインの口には必ず回数の制限を付けます**
（[middleware.md](middleware.md)）。

```rust
Route::post("/api/login", AuthController::login).middleware("throttle");
```

## ログアウトする

切る範囲が 4 つに分かれています。

| メソッド                 | 切る範囲                         | 使う場面                         |
|--------------------------|----------------------------------|----------------------------------|
| `logout()`               | **いまのセッションだけ**         | 画面の「ログアウト」ボタン       |
| `logout_other_devices()` | 同じ利用者の、いま以外のすべて   | パスワードを変えたあと           |
| `logout_all_devices()`   | 同じ利用者のすべて（自分も切る） | 「全部の端末からログアウト」     |
| `logout_user(id)`        | **指定した利用者**のすべて       | パスワードの再設定、利用者の削除 |

`logout()` は `Result<()>` を返します。ほかの 3 つは消した数を返します。

```rust
// パスワードを書き換えたあとで、ほかの端末を切る
user.password = Hash::make_async(&new_password).await?;
user.save().await?;
req.auth().logout_other_devices_async().await?;
```

**`async fn` の中では、名前の後ろに `_async` を付けたものを使います**
（`logout_other_devices_async()` / `logout_all_devices_async()` / `logout_user_async(id)`）。
同期版は置き場所の中を全部見るので、セッションが多いとほかのリクエストが止まります。
`logout()` は 1 本だけなので、同期のままで構いません。

- **パスワードを変えたら `logout_other_devices()` を呼びます。**
  変えただけでは、盗まれたセッションが期限まで生き残ります。
- **利用者を消したときも `logout_user(id)` を呼びます。**
- `logout_other_devices()` を呼んだ本人は残ります。
  そのあと `regenerate()` で ID を作り直しても残ります。
- `logout()` と `logout_all_devices()` は、セッションの Cookie も消します。
- 切った瞬間に処理中だった別の端末のリクエストも、復活しません。
- ログインしていなければ、何もせず `0` が返ります（`logout_user` を除く）。
- 置き場所の中を全部見るので、**時間のかかる操作です。** 呼ぶ回数は少なくします。

## ログイン必須にする

```rust
// routes/web.rs
Route::get("/api/me", AuthController::me).middleware("auth");

// グループにまとめて掛ける
Route::prefix("api/my/articles").middleware("auth").group(|| {
    Route::get("/", MyArticleController::index);
});
```

| 登録の書き方                          | ログインしていないとき                       |
|---------------------------------------|----------------------------------------------|
| `Authenticate::new()`                 | 401                                          |
| `Authenticate::redirect_to("/login")` | 302（JSON を期待しているリクエストには 401） |

### 今ログインしている人を使う

```rust
pub async fn me(req: Request) -> Result<Response> {
    let user = req.auth().user_or_fail::<User>().await?;   // 401 か User
    json(&json!({ "name": user.name }))
}
```

### ログイン後に元のページへ戻す

`redirect_to` で転送したときは、もとのパスをセッションに覚えます。
`req.session().intended()` で取り出します。読むと消えます。

```rust
let back = req.session().intended().unwrap_or_else(|| "/".to_string());
redirect().to(back)
```

覚えるのは、次の 3 つをすべて満たすときだけです。満たさなければ `None` が返ります。

| 条件                         | 理由                                                 |
|------------------------------|------------------------------------------------------|
| 302 で転送した               | 401 を返すだけなら、戻る相手がいない                 |
| メソッドが `GET` か `HEAD`   | ログイン後は `GET` で飛ぶ。`POST` 専用のパスは 405 になる |
| 自分のサイトの中を指すパス   | よそのサイトへ飛ばさないため                         |

自分のサイトの中かどうかは、次のように見ます。

| 断るもの         | 例                       |
|------------------|--------------------------|
| `/` で始まらない | `http://evil.example`    |
| `//` で始まる    | `//evil.example`         |
| `\` を含む       | `/\evil.example`、`/a\b` |
| 制御文字を含む   | 改行やタブが混ざったパス |

`\` を断るのは、ブラウザがこれを `/` と同じに扱うためです。

## パスワードの再設定

**文面と送信、画面は自分で書きます。** bengara が作るのはトークンとリンクまでです。
送るときは `Mail` を使います（[mail.md](mail.md)）。

トークンは `password_resets` という表に入れます。先に表を作ります。

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
req.auth().logout_user_async(user.key()).await?;
```

| メソッド                      | すること                                           |
|-------------------------------|----------------------------------------------------|
| `create(email)`               | トークンを作って表に入れ、**平文のトークン**を返す |
| `link(パス, email, 鍵, 秒)`   | 署名付き・期限つきの URL を作る（[routing.md](routing.md)） |
| `consume_if_valid(email, 鍵)` | 照合して、合っていれば**その場で消す**             |
| `verify(email, 鍵)`           | 照合だけする。**入力欄を出してよいかの下調べ用**   |
| `consume(email)`              | トークンを消す。再設定をやめたときの片付け用       |
| `sweep_expired()`             | 期限切れをまとめて消す                             |
| `lifetime_minutes()`          | トークンが使える時間（分）。60 で固定              |
| `define(&mut Schema)`         | 表を作る定義                                       |

**再設定の判定には、必ず `consume_if_valid` を使います。**
`verify` で真を見てから `consume` を呼ぶ 2 段の書き方はやめてください。
その間に別のリクエストが入ると、同じトークンが 2 回使えてしまいます。

- トークンは 1 つのメールアドレスに 1 本です。作り直すと、前のものは使えなくなります。
- 表に入るのは **トークンのハッシュ**です。表が漏れても、そのままでは使えません。
- 期限は 60 分です。期限切れ・不一致・記録なしは、すべて同じ「偽」です。
- **知らないメールアドレスを頼まれたときも、同じ応答を返します。**
  応答が違うと、登録の有無が分かってしまいます。
- リンクは `GET` で開かれます。同じパスに `GET` のルートも用意します。

## 暗号化

値を他人に読めない形にします。機能フラグ `encryption` が必要です。
無いまま呼ぶと、エラーが返ります。

```toml
bengara = { version = "0.1", features = ["sqlite", "encryption"] }
```

```rust
let hidden = encrypt("ひみつ")?;     // 毎回違う文字列になる
let plain = decrypt(&hidden)?;       // 改ざんされていればエラー
```

- 鍵のもとは `APP_KEY` です。セッションや署名付き URL とは**別の鍵**を作って使います
  （[configuration.md](configuration.md)）。
- 署名だけで足りるなら `signed_url` を使います（[routing.md](routing.md)）。

## パスワードの変換の細かい話

ふだんは読まなくて構いません。回数を変えるときに見てください。

### 保存される形

方式は PBKDF2-HMAC-SHA256 です。保存されるのはこういう文字列です。

```
$pbkdf2-sha256$i=120000$<塩>$<ハッシュ>
```

- 方式と回数が入っているので、**あとから回数を上げても古いパスワードは照合できます。**
- 照合に使う回数は、保存されている値の `i=` から読みます。
  **1,000 万回を超える値は、照合せずに偽とします。**

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
**`.env` に書いた値はテストでは見ません。** 変えたいときは環境変数で渡します。

```sh
HASH_ITERATIONS=20000 cargo test
```

### 回数を上げたら作り直す

**設定を上げただけでは、保存済みのパスワードは古い回数のままです。**
`Hash::needs_rehash` が真なら、ログインできたついでに作り直します。

```rust
if ok && Hash::needs_rehash(&user.password) {
    user.password = Hash::make_async(input.get("password")).await?;
    user.save().await?;
}
```

## まだ無いもの

まだ無いものは [backlog.md](backlog.md) にまとめてあります。

## 関連

- [authorization.md](authorization.md) — 誰に何を許すか
- [session.md](session.md) — セッションと CSRF
- [models.md](models.md) — モデル
- [testing.md](testing.md) — テスト
