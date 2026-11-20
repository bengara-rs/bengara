# イベント

「何かが起きた」と知らせて、別の場所で受け取る仕組みです。
知らせる側は、誰が聞いているかを知りません。処理を足すときに、知らせる側を触らずに済みます。

| 手順            | すること                       |
|-----------------|--------------------------------|
| 1. 聞く側を書く | `app/Listeners/` に `handle`   |
| 2. 登録する     | `bootstrap/app.rs` で `listen` |
| 3. 知らせる     | `Event::dispatch` を呼ぶ       |

## 聞く側を書く

`app/Listeners/` に置きます。ジョブと同じく、`handle` という関数 1 つです。

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
メールや外部への通信は、聞く側からキューに積みます。

## 登録する

`bootstrap/app.rs` に書きます。**自動では登録しません。**

```rust
pub fn app() -> Application {
    Application::configure()
        .with_events(|e| {
            e.listen("user.registered", crate::app::listeners::notify_admin::handle)
                .listen("user.registered", crate::app::listeners::send_slack::handle)
                .listen("post.published", crate::app::listeners::clear_cache::handle)
        })
        .with_routing(|r| r.web(crate::routes::web::routes))
        .create()
}
```

- `listen` は続けて書けます。1 つの名前に、聞く側をいくつでも付けられます。
- 起動したあとは増やせません。
- 登録した中身は `e.count(名前)`・`e.names()`・`e.is_empty()` で見られます。

## 知らせる

```rust
Event::dispatch("user.registered", &Payload { user_id: user.id }).await?;
```

| メソッド                           | 聞く側が失敗したとき               |
|------------------------------------|------------------------------------|
| `Event::dispatch(名前, &中身)`     | そこで止めて、エラーを返す         |
| `Event::dispatch_raw(名前, 文字)`  | 同上。中身がすでに JSON のとき     |
| `Event::try_dispatch(名前, &中身)` | ログに出すだけ。残りの聞く側も呼ぶ |

- 聞く側は、登録した順に 1 つずつ動きます。
- 誰も聞いていないときは、何もせず成功します。
- 聞く側のパニックも失敗として扱います。
- コントローラ・ジョブ・自作コマンド・定期処理の、どこから呼んでも届きます。
- `Lang::with` の言語は聞く側に引き継ぎます。

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

### `dispatch` が返すエラー

| 聞く側が返したもの                      | そのまま `?` で返したときの応答 |
|-----------------------------------------|---------------------------------|
| `abort(404)` などステータスを持つエラー | 404 など、元のステータス        |
| 検査のエラー（`validate` の失敗）       | 422                             |
| それ以外のエラー・パニック              | 500                             |

500 になるエラーには、イベントの名前が付きます。元のエラーは `source()` でたどれます。

```
イベント `user.registered` の処理に失敗しました: 元のエラーの文
```

聞く側の 404 や 422 を利用者に見せたくないときは、`try_dispatch` を使ってください。

## 名前と中身

名前はただの文字列です。`<もの>.<起きたこと>` の形をおすすめします（`user.registered`）。
定数にしておくと、書き間違いを防げます。

```rust
pub const USER_REGISTERED: &str = "user.registered";
```

中身は、`serde::Serialize` が付いていれば何でも渡せます。JSON にして渡します。

```rust
#[derive(bengara::serde::Serialize, bengara::serde::Deserialize)]
#[serde(crate = "bengara::serde")]
pub struct Payload {
    pub user_id: i64,
}
```

聞く側は、中身を型で受け取れます。引数を `Json<T>` にして、`listen_json` で登録します。

```rust
// app/Listeners/NotifyAdmin.rs
use bengara::events::Json;
use bengara::prelude::*;

pub async fn handle(Json(payload): Json<Payload>) -> Result<()> {
    // payload.user_id を使う
    Ok(())
}

// bootstrap/app.rs
.with_events(|e| e.listen_json("user.registered", crate::app::listeners::notify_admin::handle))
```

| 聞く側の引数             | 登録するメソッド | 受け取るもの                    |
|--------------------------|------------------|---------------------------------|
| `Json(payload): Json<T>` | `listen_json`    | 中身を `T` に読んだもの         |
| `payload: String`        | `listen`         | JSON の文字列そのまま。自分で読む |

- 型は聞く側の引数から決まります。`listen_json` に型を書く必要はありません。
- 中身を `T` に読めないときは、聞く側の失敗です。`dispatch` はそこで止まり、`try_dispatch` はログに出して次を呼びます。
- `Json` は `bengara::queue::Json` と同じ型です。

**中身の形は、コンパイルでは確かめられません。** 知らせる側と聞く側で、同じ構造体を使ってください。

## テスト

登録の表はプロセスに 1 つで、アプリを組み立てるたびに置き換わります。
テストは並んで走るので、ほかのテストの登録で動くことがあります。

- **`with_events` の中身を変えたアプリで `Event::dispatch` を呼ぶテストは、`exclusive()`
  の札を取ってください**（[testing.md](testing.md)）。
- `bootstrap/app.rs` の登録のまま使うテストには、札は要りません。

```rust
#[bengara::test]
async fn 聞く側がいなければ何も起きない() {
    assert!(!Event::has_listeners("なにも登録していない"));
    Event::dispatch("なにも登録していない", &42).await.unwrap();
}
```

## 知っておくこと

- イベントはクラスでなく、名前（文字列）と中身（構造体）で書きます。
- 呼ぶたびに登録の表を読むので、1 リクエストで何千回も知らせる使い方には向きません。

まだ無い機能は [backlog.md](backlog.md) にあります。

## 関連

- [queue.md](queue.md) — キュー。時間のかかる処理はこちらへ
- [mail.md](mail.md) — メール
- [configuration.md](configuration.md) — `bootstrap/app.rs` の組み立て
