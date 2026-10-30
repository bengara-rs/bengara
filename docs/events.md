# イベント

「何かが起きた」と知らせて、別の場所で受け取る仕組みです。
呼ぶ側は、誰が聞いているかを知りません。処理を足すときに、呼ぶ側のコードを触らずに済みます。

使うまでの手順は 3 つです。

| 手順            | すること                       |
|-----------------|--------------------------------|
| 1. 聞く側を書く | `app/Listeners/` に `handle`   |
| 2. 登録する     | `bootstrap/app.rs` で `listen` |
| 3. 知らせる     | `Event::dispatch` を呼ぶ       |

## 聞く側を書く

`app/Listeners/` に置きます。

```rust
//! app/Listeners/NotifyAdmin.rs
use bengara::prelude::*;

pub async fn handle(payload: String) -> Result<()> {
    // 時間のかかることは、ここでやらずにキューへ。
    Queue::push_raw("SendWelcome", &payload).await?;
    Cache::increment("stats.registered", 1).await?;
    Ok(())
}
```

形はジョブと同じです。

```rust
pub async fn handle(payload: String) -> Result<()>
```

## 登録する

`bootstrap/app.rs` に書きます。 **自動では登録しません。**

```rust
pub fn app() -> Application {
    Application::configure()
        .with_events(|e| {
            e.listen("user.registered", crate::app::listeners::notify_admin::handle)
        })
        .with_routing(|r| r.web(crate::routes::web::routes))
        .create()
}
```

`listen` は重ねて書けます。1 つのイベントに複数の聞く側を付けられます。

```rust
.with_events(|e| {
    e.listen("user.registered", crate::app::listeners::notify_admin::handle)
        .listen("user.registered", crate::app::listeners::send_slack::handle)
        .listen("post.published", crate::app::listeners::clear_cache::handle)
})
```

**走っている途中では増やせません。** 登録は `Application` を組み立てるときに決まります。

### 組み立て直すと置き換わる

登録の表はプロセスに 1 つです。**`Application` を組み立てるたびに丸ごと置き換わります**
（聞く側が 1 件も無い登録では置き換えません）。

本番では起動時に 1 回なので気にしなくて構いません。**効くのはテストです。**
テストは同じプロセスの別スレッドで並ぶので、`with_events` の中身が違うアプリを 2 件の
テストが組むと、後から組んだほうの表で `Event::dispatch` が動きます。
聞く側が居なくても `dispatch` は成功するので、**黙って 0 件**になります。

**`with_events` を既定と違う形で組み、かつ `Event::dispatch` 系を使うテストは、
`bengara::testing::exclusive()` の札を取ってください**（[testing.md](testing.md)）。

`with_events` のクロージャが受け取る `Events` には、中身を見るメソッド
（`is_empty()` / `count(名前)` / `names()`）があります。

## 知らせる

```rust
Event::dispatch("user.registered", &Payload { user_id: user.id }).await?;
```

| メソッド                           | 失敗したとき                     |
|------------------------------------|----------------------------------|
| `Event::dispatch(名前, &中身)`     | エラーを返す（呼んだ側に伝わる） |
| `Event::try_dispatch(名前, &中身)` | ログに出すだけ。先へ進む         |
| `Event::dispatch_raw(名前, 文字)`  | すでに JSON のとき               |

聞く側は **書いた順に、1 つずつ**動きます。
`dispatch` では、途中で失敗するとそこで止まります。

```rust
// 聞く側が失敗しても、登録そのものは成功させたいとき
Event::try_dispatch("user.registered", &payload).await?;
```

誰も聞いていないときは何も起きません。エラーにもなりません。

**どこから呼んでも届きます。** コントローラ・ジョブ・自作コマンド・定期処理のどれでも
同じです。ただし呼ぶたびに登録の表を読むので、**1 リクエストで何千回も呼ぶ作りには
向きません。**

```rust
if Event::has_listeners("user.registered") { /* ... */ }
Event::listener_count("user.registered");   // 何人聞いているか
```

## イベント名の付け方

名前はただの文字列です。 **Laravel のイベントクラスに当たるものはありません。**

| おすすめ              | 例                                   |
|-----------------------|--------------------------------------|
| `<もの>.<起きたこと>` | `user.registered` / `post.published` |

名前を定数にしておくと、書き間違いを防げます。

```rust
pub const USER_REGISTERED: &str = "user.registered";
```

## 中身の受け渡し

`serde::Serialize` が付いていれば何でも渡せます。中で JSON になります。

```rust
#[derive(bengara::serde::Serialize, bengara::serde::Deserialize)]
#[serde(crate = "bengara::serde")]
pub struct Payload {
    pub user_id: i64,
}
```

聞く側では自分で読みます。

```rust
let payload: Payload = bengara::serde_json::from_str(&payload)?;
```

**型は効きません。** 知らせる側と聞く側で形を合わせてください。

## 何をどこでやるか

聞く側は、知らせた場所でそのまま動きます。
リクエストの中で知らせたなら、リクエストの中で動きます。
**ここで待つと、画面の応答が遅くなります。**

| やること                                 | 置き場所               |
|------------------------------------------|------------------------|
| すぐ終わること（数を足す、印を置く）     | 聞く側で直接           |
| 時間のかかること（メール、外部への通信） | 聞く側からキューに積む |

## 無いもの

イベントのクラスはありません。名前は文字列、中身は構造体です。
`app/Listeners/` の自動登録もないので、`bootstrap/app.rs` に書きます。
モデルのイベント（`created` など）は `Event::dispatch` を自分で呼んでください。
ほかにまだ無いものは [backlog.md](backlog.md) にまとめてあります。

## 関連

- [queue.md](queue.md) — キュー。時間のかかる処理はこちらへ
- [mail.md](mail.md) — メール
- [configuration.md](configuration.md) — `bootstrap/app.rs` の組み立て
