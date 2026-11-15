# イベント

「何かが起きた」と知らせて、別の場所で受け取る仕組みです。
知らせる側は、誰が聞いているかを知りません。処理を足すときに、知らせる側のコードを触らずに済みます。
このページで、聞く側の書き方・登録・知らせ方が分かります。

| 手順            | すること                       |
|-----------------|--------------------------------|
| 1. 聞く側を書く | `app/Listeners/` に `handle`   |
| 2. 登録する     | `bootstrap/app.rs` で `listen` |
| 3. 知らせる     | `Event::dispatch` を呼ぶ       |

## 聞く側を書く

`app/Listeners/` に置きます。形はジョブと同じで、`handle` という関数 1 つです。

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

**聞く側は、知らせた場所でそのまま動きます。** ここで待つと、画面の応答が遅くなります。

| やること                                 | 置き場所               |
|------------------------------------------|------------------------|
| すぐ終わること（数を足す、印を置く）     | 聞く側で直接           |
| 時間のかかること（メール、外部への通信） | 聞く側からキューに積む |

## 登録する

`bootstrap/app.rs` に書きます。**自動では登録しません。**

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

`listen` は続けて書けます。1 つの名前に、聞く側をいくつでも付けられます。

```rust
.with_events(|e| {
    e.listen("user.registered", crate::app::listeners::notify_admin::handle)
        .listen("user.registered", crate::app::listeners::send_slack::handle)
        .listen("post.published", crate::app::listeners::clear_cache::handle)
})
```

- **起動したあとは増やせません。** 登録はアプリを組み立てるときに決まります。
- 登録した中身は `e.count(名前)`・`e.names()`・`e.is_empty()` で見られます。

## 知らせる

```rust
Event::dispatch("user.registered", &Payload { user_id: user.id }).await?;
```

| メソッド                           | 聞く側が失敗したとき                   |
|------------------------------------|----------------------------------------|
| `Event::dispatch(名前, &中身)`     | そこで止めて、エラーを返す             |
| `Event::dispatch_raw(名前, 文字)`  | 同上。中身がすでに JSON のときに使う   |
| `Event::try_dispatch(名前, &中身)` | ログに出すだけ。残りの聞く側も呼ぶ     |

- 聞く側は**登録した順に、1 つずつ**動きます。
- 誰も聞いていないときは、何もせず成功します。
- コントローラ・ジョブ・自作コマンド・定期処理の、どこから呼んでも届きます。

聞く側が失敗しても本体は成功させたいときは、`try_dispatch` を使います。

```rust
// 通知に失敗しても、登録そのものは成功させる
Event::try_dispatch("user.registered", &payload).await?;
```

聞いている数は、次の 2 つで分かります。

```rust
if Event::has_listeners("user.registered") { /* ... */ }
Event::listener_count("user.registered");   // 何件登録されているか
```

### dispatch が返すエラー

聞く側が返したエラーの**種類を保ちます。**

| 聞く側が返したもの                        | `dispatch` が返すもの        | そのまま `?` で返したときの応答 |
|-------------------------------------------|------------------------------|---------------------------------|
| `abort(404)` などステータスを持つエラー   | そのまま                     | 404 など、元のステータス        |
| 検査のエラー（`validate` の失敗）         | そのまま                     | 422                             |
| それ以外（`Error::msg`・DB のエラーなど） | イベントの名前を添えて包む   | 500                             |

包んだエラーの文は次の形です。元のエラーは `source()` でたどれます。

```
イベント `user.registered` の処理に失敗しました: 元のエラーの文
```

**聞く側の 404 や 422 を利用者に見せたくないときは、`try_dispatch` を使ってください。**
`dispatch` の結果を呼ぶ側で受けて、自分のエラーに直す方法もあります。

## 名前と中身

名前はただの文字列です。`<もの>.<起きたこと>` の形をおすすめします
（`user.registered`、`post.published`）。定数にしておくと、書き間違いを防げます。

```rust
pub const USER_REGISTERED: &str = "user.registered";
```

中身は、`serde::Serialize` が付いていれば何でも渡せます。中で JSON になります。

```rust
#[derive(bengara::serde::Serialize, bengara::serde::Deserialize)]
#[serde(crate = "bengara::serde")]
pub struct Payload {
    pub user_id: i64,
}
```

聞く側では、自分で読みます。

```rust
let payload: Payload = bengara::serde_json::from_str(&payload)?;
```

**中身の形は、コンパイルでは確かめられません。** 知らせる側と聞く側で、同じ構造体を使ってください。

## テスト

登録の表は、プロセスに 1 つです。**アプリを組み立てるたびに、丸ごと置き換わります**
（聞く側が 1 件も無い登録では置き換えません）。

本番は起動時に 1 回なので、気にしなくて構いません。効くのはテストです。
テストは並んで走るので、登録の違うアプリを別のテストが組むと、後から組んだほうの表で動きます。

**`with_events` の中身を変えたアプリを組み、`Event::dispatch` を呼ぶテストは、
`exclusive()` の札を取ってください**（[testing.md](testing.md)）。
`bootstrap/app.rs` の登録のまま使うテストには、札は要りません。

```rust
#[bengara::test]
async fn 聞く側がいなければ何も起きない() {
    assert!(!Event::has_listeners("なにも登録していない"));
    Event::dispatch("なにも登録していない", &42).await.unwrap();
}
```

## できないこと

| できないこと                             | 代わりにすること                       |
|------------------------------------------|----------------------------------------|
| イベントをクラスで書く                   | 名前は文字列、中身は構造体で渡す       |
| `app/Listeners/` を自動で登録する        | `bootstrap/app.rs` に `listen` を書く  |
| モデルの保存で自動的に知らせる（`created` など） | `Event::dispatch` を自分で呼ぶ |
| 1 リクエストで何千回も知らせる           | 向きません。呼ぶたびに登録の表を読みます |

まだ無い機能の一覧は [backlog.md](backlog.md) にあります。

## 関連

- [queue.md](queue.md) — キュー。時間のかかる処理はこちらへ
- [mail.md](mail.md) — メール
- [configuration.md](configuration.md) — `bootstrap/app.rs` の組み立て
