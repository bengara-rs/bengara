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

**登録は起動時の 1 回だけです。** 走っている途中では増やせません。

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
同じです。`bootstrap/app.rs` は、置き場所が要らないコマンド（`init` / `--version` /
`--help`）以外のすべてで読まれます。

以前は `serve` と `route:list` のときだけ読んでいたので、`queue:work` や自作コマンドの
中で `dispatch` を呼んでも、**エラーも警告も出さずに何も起きませんでした。**

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

| 項目                                | 代わりにすること               |
|-------------------------------------|--------------------------------|
| イベントのクラス                    | 名前は文字列。中身は構造体     |
| 自動登録（`app/Listeners/` を見る） | `bootstrap/app.rs` に書く      |
| モデルのイベント（`created` など）  | `Event::dispatch` を自分で呼ぶ |
| 購読者（`EventSubscriber`）         | `listen` を並べる              |
| 放送（Broadcasting / WebSocket）    | ありません                     |

## 関連

- [queue.md](queue.md) — キュー。時間のかかる処理はこちらへ
- [mail.md](mail.md) — メール
- [configuration.md](configuration.md) — `bootstrap/app.rs` の組み立て
