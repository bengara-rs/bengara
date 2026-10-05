# 認可（誰に何を許すか）

「この記事を直してよいのは持ち主だけ」のような判定です。
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

`app/Policies/` は自動で取り込まれます。登録は要りません。
呼ぶときは `crate::app::policies::PostPolicy` です。

## 使う

```rust
// app/Http/Controllers/MyArticleController.rs
use bengara::prelude::*;

use crate::app::models::{Post, User};
use crate::app::policies::PostPolicy;

pub async fn update(req: Request) -> Result<Response> {
    let user = req.auth().user_or_fail::<User>().await?;   // 401
    let mut post = req.model::<Post>("post").await?;       // 404
    authorize(PostPolicy::update(&user, &post))?;          // 403

    let input = req.validate(&[("title", "required|max:50")])?;
    post.title = input.get("title").to_string();
    post.save().await?;

    json(&bengara::serde_json::json!({ "id": post.id }))
}
```

3 行で「ログイン必須」「対象を読む」「持ち主か確かめる」が並びます。
**止まる順番も、この並びのとおり**です（401 → 404 → 403）。

| 関数 | すること |
|---|---|
| `authorize(bool)` | 偽なら 403。本文は「この操作は許可されていません」 |
| `authorize_with(bool, メッセージ)` | 403 のメッセージを指定する |

403 を HTML で返すか JSON で返すかは、リクエストの `Accept` で決まります
（[requests-and-responses.md](requests-and-responses.md)）。

## 分けて使う

真偽値なので、止めずに分岐もできます。

```rust
let can_edit = PostPolicy::update(&user, &post);
json(&bengara::serde_json::json!({
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

    let mut alice = User::register("アリス", "alice@example.com", "password123");
    alice.save().await.unwrap();
    let mut bob = User::register("ボブ", "bob@example.com", "password123");
    bob.save().await.unwrap();

    let mut post = Post::draft("下書き").owned_by(&alice);
    post.save().await.unwrap();

    assert!(PostPolicy::update(&alice, &post));
    assert!(!PostPolicy::update(&bob, &post));
}
```

画面ごしに確かめるなら、ログインしてから叩きます。

```rust
bob.send("PUT", "/api/my/articles/1", b"title=x".to_vec(), &[FORM])
    .await
    .assert_status(403);
```

## Laravel との違い

| Laravel | bengara |
|---|---|
| `Gate::define('update-post', fn ($user, $post) => ...)` | 書きません。関数を直接定義します |
| `Gate::allows('update-post', $post)` | `PostPolicy::update(&user, &post)` |
| `$this->authorize('update', $post)` | `authorize(PostPolicy::update(&user, &post))?` |
| ポリシーの自動対応づけ | ありません。呼ぶ側で関数を指定します |
| `@can('update', $post)` | 真偽値なので `if` で分けます |

## 気をつけること

| こと | 内容 |
|---|---|
| 判定は同期の関数にする | DB を引きたくなったら、呼ぶ側で先に引いてから渡してください |
| 先にログインを確かめる | `user_or_fail()` を先に呼べば 401 が先に返ります |
| 判定を忘れないようにする | ルートを足したら、対になるテスト（他人で 403）も足してください |

## 関連

- [authentication.md](authentication.md) — ログイン
- [middleware.md](middleware.md) — ルートに掛ける処理
- [models.md](models.md) — モデル
