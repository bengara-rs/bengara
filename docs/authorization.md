# 認可（誰に何を許すか）

「この記事を直してよいのは持ち主だけ」のような判定のしかたです。
ログインしているかどうかは [authentication.md](authentication.md) にあります。

## 考え方

判定（ポリシー）は、**`app/Policies/` に置いたただの関数**です。
フレームワークが持つのは、偽のときに 403 を返す `authorize` だけです。

```rust
authorize(PostPolicy::update(&user, &post))?;
```

関数を直接呼ぶので、名前を間違えるとコンパイルで止まります。

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
- 呼ぶときのパスは `crate::app::policies::PostPolicy` です。
- **判定は同期の関数にします。** DB を引きたいときは、呼ぶ側で先に引いて渡します。

## 使う

```rust
// app/Http/Controllers/MyArticleController.rs
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

止まる順番は、書いた行の順（401 → 404 → 403）です。

| 関数                               | すること                                         |
|------------------------------------|--------------------------------------------------|
| `authorize(bool)`                  | 偽なら 403。文は「この操作は許可されていません」 |
| `authorize_with(bool, メッセージ)` | 403 の文を決める                                 |

403 を HTML と JSON のどちらで返すかは、リクエストで決まります（[requests-and-responses.md](requests-and-responses.md)）。

### 呼ばないと効かない

**ポリシーを呼び忘れても、コンパイルは通り、警告も出ません。**

- 書いたポリシーと、呼んでいる場所を突き合わせます。
- `view` のような「見てよいか」は特に忘れやすいです。忘れると、下書きが誰にでも見えます。
- ルートを足したら、対になるテスト（他人なら 403）も足します。
- 隠したいものには、403 でなく 404 を返す形も考えます。403 だと「そこに何かある」と分かるためです。

## 止めずに分岐する

返るのは真偽値なので、画面の出し分けにそのまま使えます。

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

`User::register` と `Post::draft` は、アプリのモデルに書いた関数です。

リクエストを送って確かめるなら、ログインしてから送ります（[testing.md](testing.md)）。
`PUT` には `post_with_csrf` が無いので、トークンは自分で付けます。

```rust
client
    .post_with_csrf("/api/login", "email=bob@example.com&password=password123", "/csrf-token")
    .await
    .assert_ok();

let token = client.csrf_token("/csrf-token").await;
client
    .send("PUT", "/api/my/articles/1", b"title=x".to_vec(),
          &[("content-type", "application/x-www-form-urlencoded"),
            ("x-csrf-token", &token)])
    .await
    .assert_status(403);
```

**`VerifyCsrfToken` を掛けたルートでは、トークンを忘れると 419 になります。**
403 のつもりが 419 になるときは、ここを疑います（[session.md](session.md)）。

## Laravel の書き方との対応

| Laravel                              | bengara                                        |
|--------------------------------------|------------------------------------------------|
| `Gate::allows('update-post', $post)` | `PostPolicy::update(&user, &post)`             |
| `$this->authorize('update', $post)`  | `authorize(PostPolicy::update(&user, &post))?` |
| `@can('update', $post)`              | 真偽値を `if` で分ける                         |

`Gate::define` での登録や、ポリシーの自動の対応づけはありません。呼ぶ側で関数を指定します。

## 関連

- [authentication.md](authentication.md) — ログイン
- [middleware.md](middleware.md) — ルートに掛ける処理
- [models.md](models.md) — モデル
