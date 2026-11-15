# 認可（誰に何を許すか）

「この記事を直してよいのは持ち主だけ」のような判定のしかたです。
このページで分かることは 3 つです。

- 判定（ポリシー）の書き方と置き場所
- コントローラでの使い方と、止まる順番
- テストのしかた

ログインしているかどうかは [authentication.md](authentication.md) にあります。

## 考え方

判定は **`app/Policies/` に置いたただの関数**です。
フレームワークが持っているのは、真偽値から 403 を作る `authorize` だけです。

```rust
authorize(PostPolicy::update(&user, &post))?;
```

文字列で引く表（Laravel の `Gate::define`）はありません。
関数呼び出しなので、**名前を間違えればコンパイルで止まります。**

## 書く

```rust
// app/Policies/PostPolicy.rs
use crate::app::models::{Post, User};

pub struct PostPolicy;

impl PostPolicy {
    /// 見てよいか。公開済みなら誰でも、下書きは持ち主だけ。
    pub fn view(user: Option<&User>, post: &Post) -> bool {
        if post.status == "published" {
            return true;
        }
        user.is_some_and(|user| Self::owns(user, post))
    }

    /// 直してよいのは持ち主だけ。
    pub fn update(user: &User, post: &Post) -> bool {
        Self::owns(user, post)
    }

    /// 消してよいのは持ち主か管理者。
    pub fn delete(user: &User, post: &Post) -> bool {
        Self::owns(user, post) || user.is_admin
    }

    fn owns(user: &User, post: &Post) -> bool {
        post.user_id != 0 && post.user_id == user.id
    }
}
```

- `app/Policies/` は自動で取り込まれます。登録は要りません。
- 呼ぶときは `crate::app::policies::PostPolicy` です。
- **判定は同期の関数にします。** DB を引きたくなったら、呼ぶ側で先に引いてから渡します。

## 使う

```rust
// app/Http/Controllers/MyArticleController.rs
use bengara::prelude::*;

use crate::app::models::{Post, User};
use crate::app::policies::PostPolicy;

pub async fn destroy(req: Request) -> Result<Response> {
    let user = req.auth().user_or_fail::<User>().await?;   // 401
    let post = req.model::<Post>("post").await?;           // 404
    authorize(PostPolicy::delete(&user, &post))?;          // 403

    let removed = post.delete().await?;
    json(&json!({ "deleted": removed }))
}
```

3 行で「ログイン必須」「対象を読む」「許されているか確かめる」が並びます。
**止まる順番も、この並びのとおり**です（401 → 404 → 403）。

| 関数                               | すること                                           |
|------------------------------------|----------------------------------------------------|
| `authorize(bool)`                  | 偽なら 403。文は「この操作は許可されていません」   |
| `authorize_with(bool, メッセージ)` | 403 のメッセージを指定する                         |

403 を HTML で返すか JSON で返すかは、リクエストを見て決まります
（[requests-and-responses.md](requests-and-responses.md)）。

### 呼ばないと効かない

**ポリシーはただの関数です。呼び忘れても、何も起きません。**
コンパイルも通り、警告も出ません。書いただけでは誰も止まりません。

- 書いたポリシーの一覧と、呼んでいる場所を突き合わせます。
- `view` のような「見てよいか」は、特に呼び忘れやすいです。
  忘れると**下書きが誰にでも見えます。**
- ルートを足したら、対になるテスト（他人で 403）も足します。
- 見せてはいけないものを隠すときは、403 ではなく **404** を返す形も考えます。
  403 だと「そこに何かある」ことが伝わります。

## 止めずに分岐する

返るのは真偽値です。画面の出し分けにそのまま使えます。

```rust
let can_edit = PostPolicy::update(&user, &post);

json(&json!({
    "title": post.title,
    "can_edit": can_edit,     // 画面でボタンを出すかどうか
}))
```

## テストする

ポリシーはただの関数なので、そのまま呼べます。

```rust
#[bengara::test]
async fn 他人の記事は直せない() {
    let _db = refresh_database().await;

    let mut alice = User::register("アリス", "alice@example.com", "password123").await.unwrap();
    alice.save().await.unwrap();
    let mut bob = User::register("ボブ", "bob@example.com", "password123").await.unwrap();
    bob.save().await.unwrap();

    let mut post = Post::draft("下書き").owned_by(&alice);
    post.save().await.unwrap();

    assert!(PostPolicy::update(&alice, &post));
    assert!(!PostPolicy::update(&bob, &post));
}
```

`User::register` と `Post::draft` は、アプリ側のモデルに書いた関数です。

画面ごしに確かめるなら、ログインしてから送ります（[testing.md](testing.md)）。

```rust
// ボブでログインする（Cookie が client に残ります）
client
    .post_with_csrf("/api/login", "email=bob@example.com&password=password123", "/csrf-token")
    .await
    .assert_ok();

// 他人の記事を直そうとすると 403。
// POST 以外には `post_with_csrf` が無いので、トークンは自分で付けます。
let token = client.csrf_token("/csrf-token").await;
client
    .send("PUT", "/api/my/articles/1", b"title=x".to_vec(),
          &[("content-type", "application/x-www-form-urlencoded"),
            ("x-csrf-token", &token)])
    .await
    .assert_status(403);
```

**`VerifyCsrfToken` を掛けているルートでは、トークンを忘れると 419 になります。**
403 を確かめたいのに 419 が返るときは、ここを疑います（[session.md](session.md)）。

## Laravel との違い

| Laravel                                                 | bengara                                        |
|---------------------------------------------------------|------------------------------------------------|
| `Gate::define('update-post', fn ($user, $post) => ...)` | 書きません。関数を直接定義します               |
| `Gate::allows('update-post', $post)`                    | `PostPolicy::update(&user, &post)`             |
| `$this->authorize('update', $post)`                     | `authorize(PostPolicy::update(&user, &post))?` |
| ポリシーの自動対応づけ                                  | ありません。呼ぶ側で関数を指定します           |
| `@can('update', $post)`                                 | 真偽値なので `if` で分けます                   |

## 関連

- [authentication.md](authentication.md) — ログイン
- [middleware.md](middleware.md) — ルートに掛ける処理
- [models.md](models.md) — モデル
