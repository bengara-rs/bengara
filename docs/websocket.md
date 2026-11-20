# WebSocket と配信

ブラウザとサーバーをつなぎっぱなしにして、どちらからでもすぐに送れるようにする仕組みです。
チャットや、画面の自動更新に使います。

| 機能                   | すること                                                    |
|------------------------|-------------------------------------------------------------|
| WebSocket の受け口     | 普通のルートのハンドラの中で、要求を WebSocket に切り替える |
| 配信（`Broadcast`）    | 接続を「チャンネル」（宛先の名前）に入れ、まとめて送る      |
| 別のプロセスからの配信 | 定期処理やキューから送ったものも、サーバーの接続に届ける    |

- WebSocket は、1 本の接続を開いたまま双方向に送る決まりです（RFC 6455）。
- やり取りする中身の形（JSON の決まりなど）は、アプリが決めます。

## 使えるようにする

Cargo の機能フラグ `websocket` を足します。**既定では入りません。**

```toml
[dependencies]
bengara = { version = "0.1", features = ["sqlite", "websocket"] }
```

- フラグが無いと、`req.websocket()` が「features に "websocket" を足してください」のエラー（500）を返します。
- `Broadcast::publish` はフラグが無くても呼べます。定期処理だけのプロセスなどで使えます。
- 使う型は `prelude` に入っていません。`use` で取り込みます。

```rust
use bengara::broadcast::Broadcast;
use bengara::websocket::{Message, WebSocket};
```

## 受け口を作る

ルートは普通の `Route::get` です。

```rust
// routes/web.rs
Route::get("/gateway", GatewayController::connect).name("gateway");
```

```rust
// app/Http/Controllers/GatewayController.rs
pub async fn connect(req: Request) -> Result<Response> {
    // 1. 切り替える前に、普通の HTTP として確かめる。
    let ticket = req.query("ticket").ok_or_else(|| Error::http(401, "チケットがありません"))?;
    let user_id = consume_ticket(&ticket).await?;   // アプリの関数

    // 2. 切り替える。戻り値は 101 の応答。クロージャは切り替えの後に別のタスクで動く。
    req.websocket()?.on_upgrade(move |mut socket: WebSocket| async move {
        socket.join(&format!("user.{user_id}"));
        while let Some(message) = socket.recv().await {
            match message {
                Ok(Message::Text(text)) => { /* 受け取った文を扱う */ }
                Ok(Message::Binary(_)) => {
                    socket.close(1003, "テキストだけを受け付けます").await;
                    break;
                }
                Err(_) => break,
            }
        }
        // ここで抜けると接続は閉じ、入っていたチャンネルからも外れる
    })
}
```

- **認証は切り替える前にします。** ここで `Err` を返せば、普通のエラー応答（401 など）になります。
- 切り替えの要求か知りたいだけなら、`req.wants_websocket()` を使います。
- 切り替えの要求もミドルウェアを通ります。101 の応答を作り直すミドルウェアがあると、500 になります。

### req.websocket() が断るもの

| 場合                                           | 返すもの |
|------------------------------------------------|----------|
| 切り替えの要求として正しくない                 | 400      |
| `Sec-WebSocket-Version` が `13` でない         | 426      |
| サーバーが止まる途中・同時接続が上限（10,000） | 503      |

### 切り替えの設定

`req.websocket()?` の後ろにつなげて書きます。

| 書き方                     | 既定     | 意味                                                 |
|----------------------------|----------|------------------------------------------------------|
| `max_message_size(バイト)` | 64 KiB   | 受け取る 1 通の上限。超えたら 1009 で閉じる          |
| `send_queue_limit(バイト)` | 1 MiB    | 送りきれずに溜まった量の上限。超えたら 1008 で閉じる |
| `ping_interval(長さ)`      | 送らない | サーバーから Ping（生きているかの確認）を送る間隔    |
| `idle_timeout(長さ)`       | 閉じない | 何も受け取らないまま過ぎたら 1000 で閉じる           |
| `protocols(&[..])`         | 選ばない | 受け付けるサブプロトコル（`Sec-WebSocket-Protocol`） |
| `protocol()`               | —        | 選ばれたサブプロトコル。`on_upgrade` の前に読める    |

```rust
use std::time::Duration;

req.websocket()?
    .max_message_size(16 * 1024)
    .ping_interval(Duration::from_secs(30))
    .idle_timeout(Duration::from_secs(75))
    .on_upgrade(move |socket| handle(socket))
```

- `ping_interval` と `idle_timeout` を組むと、黙って消えた相手を切れます（Pong も「受け取った」に数えます）。
- `protocols` は、相手が申し出た順に見て、最初に一致したものを選びます。
- 長さに 0 を渡すと「無し」と同じです。

## 送る・受け取る

| 書き方                           | すること                                                |
|----------------------------------|---------------------------------------------------------|
| `socket.recv().await`            | 次のメッセージを待つ。`None` は閉じたこと               |
| `socket.send_text(文字)`         | 文字を送る。**待たない**（送る列に積むだけ）            |
| `socket.send_binary(バイト列)`   | バイト列を送る。待たない                                |
| `socket.sender()`                | 別のタスクから送るための口（複製できる）                |
| `socket.close(番号, 理由).await` | Close を送り、相手の Close を最大 5 秒待つ              |
| `socket.close_frame()`           | `recv` が `None` を返した後、相手が送ってきた番号と理由 |
| `socket.id()`                    | 接続の ID。プロセスの中で一意。ログとの照合に使う       |

- `recv` に出てくるのは文字（`Message::Text`）とバイト列（`Message::Binary`）だけです。
- Ping には自動で Pong を返します。相手の Close にも応じます。`recv` を呼んでいない間も同じです。
- 大きすぎる・壊れたメッセージで閉じたときは、`None` の前に `Err` が 1 回来ます。
- 閉じた後の `send_text` は `SendError::Closed` です。`?` で `bengara::Error` に変えられます。
- `sender()` の口には `send_text`・`send_binary`・`close`（待たない）・`is_closed` があります。

### 読む・送るが追いつかないとき

| 起きたこと                                     | どうなるか                                                             |
|------------------------------------------------|------------------------------------------------------------------------|
| アプリが `recv` を呼ばず、受け取った分が溜まる | `max_message_size` の 16 通分（既定 1 MiB）を超えたら 1008 で閉じる    |
| 送る列が `send_queue_limit` を超えた           | `send_text` が `SendError::QueueFull` を返す。接続は 1008 で閉じている |
| 相手が読まず、1 通の書き込みが 30 秒進まない   | Close を送らずに切る                                                   |

- 重い処理は別のタスクに回し、`recv` を止めないでください。

### 閉じるとき

| 閉じ方                                     | 先に積んだもの   |
|--------------------------------------------|------------------|
| クロージャが返った（1000）                 | 送ってから閉じる |
| `close` / `Broadcast::disconnect`          | 送ってから閉じる |
| 送る列・受け取りの溜まりがあふれた（1008） | 送らずに閉じる   |
| 自分で 1008 を指定した `close` など        | 送ってから閉じる |
| サーバーが止まる（1001）                   | 送らずに閉じる   |
| 書き込みが 30 秒進まない                   | 送らずに切る     |

- 理由の文は 123 バイトまでです。超えた分は切ります。
- 送れない番号（1000 未満、1004〜1006、1015〜2999、5000 以上）は 1000 に直し、警告を出します。
- **クロージャがパニックしたら、その接続だけを 1011 で閉じます。** ほかの接続とサーバーは動き続けます。
- **閉じた後もクロージャが 5 秒で返らないときは、警告を出して止めます。** `recv` が `None` を返したら、クロージャから返ってください。

## チャンネル

チャンネルは、接続をまとめる名前です。`user.42` や `conversation.123` のように付けます。

| 書き方               | すること                         |
|----------------------|----------------------------------|
| `socket.join(名前)`  | チャンネルに入る                 |
| `socket.leave(名前)` | チャンネルから出る               |
| `socket.channels()`  | 入っているチャンネル（名前の順） |

- 閉じた接続は、入っていたすべてのチャンネルから自動で外れます。
- **誰をどのチャンネルに入れるかは、アプリが決めます。** 入れる前に、その人が見てよいか確かめてください。
- 名前は 1〜255 バイトにします。それ以外の名前に `publish` するとエラーです。`join` は警告を出して入りません。

## 配信する（Broadcast）

```rust
use bengara::broadcast::Broadcast;

// HTTP のハンドラから。トランザクションのコミットの後で呼ぶ。
Broadcast::publish("conversation.123", payload_json).await?;

// 値を JSON にして送る。
Broadcast::publish_json("user.42", &json!({ "op": "updated" })).await?;

// チャンネルに入っている接続をすべて閉じる（別のプロセスからも効く）。
Broadcast::disconnect("session.987", 4001, "session revoked").await?;
```

| 書き方                                    | すること                                             |
|-------------------------------------------|------------------------------------------------------|
| `Broadcast::publish(名前, 文字)`          | チャンネルの全接続へ同じ文字を送る                   |
| `Broadcast::publish_json(名前, &値)`      | 値を JSON にして送る                                 |
| `Broadcast::disconnect(名前, 番号, 理由)` | チャンネルの全接続を閉じる                           |
| `Broadcast::local_count(名前)`            | **このプロセスの中で**チャンネルに入っている接続の数 |

- **`publish` はトランザクションに入りません。** 巻き戻った操作を配信しないように、コミットの後に呼びます。
- 中身は加工しません。宛先ごとに変えたいなら、宛先ごとのチャンネルを使います。
- 送る列があふれた接続は、その接続だけを 1008 で閉じます。ほかへの配信は止めません。
- 同じプロセスから同じチャンネルへ送ったものは、送った順に届きます。
- 再送のための履歴は持ちません。取りこぼしを埋めたいなら、アプリが連番などを持ちます。
- `local_count` は、利用者ごとの接続数の上限を決めるときなどに使います。ほかのプロセスの分は数えません。

## 送り方を選ぶ（memory と database）

`.env` の `BROADCAST_CONNECTION`（Laravel 11 と同じ名前）で選びます。
空なら `BROADCAST_DRIVER` を見ます。どちらも空なら `memory` です。

| 送り方                           | 届く範囲                               | 使う場面                                                     |
|----------------------------------|----------------------------------------|--------------------------------------------------------------|
| `memory`（既定。`array` も同じ） | **同じプロセスの中だけ**               | サーバー 1 つで、ほかのプロセスから送らないとき              |
| `database`                       | **すべてのプロセス**（表を中継に使う） | 定期処理・キュー・コマンドから送るとき。サーバーが複数のとき |

- **知らない名前は `memory` に落としません。** `publish` と `disconnect` がエラーを返します。警告も 1 回出ます。
- `memory` のまま、サーバーではないプロセス（`schedule:run` など）から送ると、どこにも届きません。警告が 1 回出ます。

### database を使う

中継の表（`broadcasts`）を作るマイグレーションを置き、`migrate` を流します。

```rust
// database/migrations/2026_11_16_000000_create_broadcasts_table.rs
use bengara::broadcast::Broadcast;
use bengara::prelude::*;

pub fn up(schema: &mut Schema) {
    Broadcast::define(schema);
}

pub fn down(schema: &mut Schema) {
    schema.drop_if_exists(bengara::broadcast::TABLE);
}
```

```ini
BROADCAST_CONNECTION=database
```

`publish` は表に 1 行書きます。サーバーは表を定期的に読み、自分の接続に届けます。古い行は自動で消します。

| 環境変数                      | 既定  | 意味                                         |
|-------------------------------|-------|----------------------------------------------|
| `BROADCAST_POLL_MS`           | `250` | 表を読む間隔（ミリ秒）。10 より小さい値は 10 |
| `BROADCAST_RETENTION_SECONDS` | `60`  | 行を残す秒数。5 より小さい値は 5             |

- 表は**既定の接続**に作ります。
- ほかのプロセスから送ったものは、読む間隔（既定 250 ミリ秒）ほど遅れて届きます。
- **サーバーが起動する前に書かれた行は届けません。**
- 表が無いと、`publish` はエラーを返します。

### 設定ファイルで決める

`config/` に `BroadcastConfig` を返すファイルを置くと、環境変数の代わりにその値を使います。

```rust
// config/broadcasting.rs
use bengara::broadcast::BroadcastConfig;
use bengara::prelude::*;

pub fn config() -> BroadcastConfig {
    BroadcastConfig {
        driver: env("BROADCAST_CONNECTION", "database"),
        ..Default::default()
    }
}
```

| 欄                  | 意味                            |
|---------------------|---------------------------------|
| `driver`            | 送り方（`memory` / `database`） |
| `poll_ms`           | 表を読む間隔（ミリ秒）          |
| `retention_seconds` | 行を残す秒数                    |

`Default::default()` は、上の環境変数を読んで作ります。

## 上限と既定値

| 項目                       | 既定     | 変え方                                          |
|----------------------------|----------|-------------------------------------------------|
| 受け取る 1 通の上限        | 64 KiB   | `max_message_size`                              |
| 送る列の上限（溜まった量） | 1 MiB    | `send_queue_limit`                              |
| 受け取りの溜まりの上限     | 1 MiB    | `max_message_size` の 16 通分                   |
| サーバーからの Ping        | 送らない | `ping_interval`                                 |
| 無通信で閉じる             | 閉じない | `idle_timeout`                                  |
| 同時接続の数（1 プロセス） | 10,000   | `WEBSOCKET_MAX_CONNECTIONS`（`0` は既定と同じ） |
| 1 通の書き込みを待つ長さ   | 30 秒    | 変えられない                                    |
| 相手の Close を待つ長さ    | 5 秒     | 変えられない                                    |

### 閉じる番号

| 番号       | いつ                                              |
|------------|---------------------------------------------------|
| 1000       | 普通に終わった・`idle_timeout` が過ぎた           |
| 1001       | サーバーが止まる                                  |
| 1002       | 決まりに合わないフレームを受け取った              |
| 1003       | （アプリが使う。例: バイナリを断る）              |
| 1007       | 受け取った文字が UTF-8 ではない                   |
| 1008       | 送る列・受け取りの溜まりがあふれた                |
| 1009       | 受け取ったメッセージが大きすぎる                  |
| 1011       | クロージャがパニックした                          |
| 4000〜4999 | **アプリのために空けてある。** bengara は使わない |

## 止めるとき

| 場面               | どうなるか                                                         |
|--------------------|--------------------------------------------------------------------|
| 1 回目の停止の合図 | 新しい切り替えを 503 で断る。全接続に 1001 を送る                  |
| 待つ長さ           | `APP_SHUTDOWN_TIMEOUT` と 5 秒の短いほう（無制限の `0` なら 5 秒） |
| 過ぎたら           | 警告を出して、そのまま切る                                         |
| 2 回目の合図       | 待たずに終わる                                                     |

- 1001 を受けたら、少し待ってからつなぎ直すようにクライアントを作ってください。
- **開発中の `cargo artisan serve` は、保存するたびに接続が切れます。** Close も送りません。

## 安全のために

| 危ないこと                             | bengara がすること                                             | アプリがすること                                                       |
|----------------------------------------|----------------------------------------------------------------|------------------------------------------------------------------------|
| 別のサイトのページから接続される       | **何もしない。** 切り替えは GET なので CSRF の確認も掛からない | `req.header("origin")` を確かめる。Cookie のセッションだけで認証しない |
| クエリのチケットがログに残る           | アクセスログで `ticket` の値を伏せる（`?ticket=***`）          | チケットを使い捨て・短命にする                                         |
| 巨大なメッセージでメモリを使い切られる | 受け取る上限を 64 KiB にしている                               | 必要なら絞る                                                           |
| 送る分・受け取った分が溜まる           | 1 MiB で切る。書き込みが 30 秒進まなければ切る                 | 重い処理は別のタスクに回し、`recv` を止めない                          |
| 接続の数で使い切られる                 | 同時接続を 10,000 で断る                                       | 利用者ごとの上限は `local_count` で決める                              |
| 接続中にログアウトしても通り続ける     | 何もしない。**セッションは切り替えた時点の写し**               | `Broadcast::disconnect` で閉じる                                       |
| 巻き戻った操作が配信される             | 何もしない                                                     | コミットの後に `publish` する                                          |

ログに出すのは接続 ID・続いた時間・閉じた番号だけです。メッセージの中身は出しません。

## テスト

### 切り替えの要求を送る

`#[bengara::test]` の `client` で、切り替えのヘッダーを付けて送ります。 **ソケットが無いので、確かめた 101
を返すだけです。クロージャは動きません。**
認証や `Origin` の確かめを、ソケット無しで書けます。

```rust
#[bengara::test]
async fn 切り替えの要求に101を返す() {
    client
        .send(
            "GET",
            "/gateway?ticket=abc",
            Vec::new(),
            &[
                ("upgrade", "websocket"),
                ("connection", "Upgrade"),
                ("sec-websocket-version", "13"),
                ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
            ],
        )
        .await
        .assert_status(101)
        .assert_header("sec-websocket-accept", "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
}
```

### 配信を確かめる

`Broadcast::fake()` を呼ぶと、以後の `publish` を記録だけして送りません。 **記録はプロセスで 1 つです。** 先に
`exclusive()` の札を取ってください。

```rust
use bengara::broadcast::Broadcast;

#[bengara::test]
async fn 投稿すると配信される() {
    let _lock = exclusive().await;
    Broadcast::fake();

    post("/api/posts", "title=hi").await.assert_status(201);

    Broadcast::assert_published("posts", "hi");
    assert_eq!(Broadcast::published("posts").len(), 1);
}
```

| 書き方                                        | すること                           |
|-----------------------------------------------|------------------------------------|
| `Broadcast::fake()`                           | 記録を空にして、記録を始める       |
| `Broadcast::published(名前)`                  | そのチャンネルへの中身（送った順） |
| `Broadcast::assert_published(名前, 含む文字)` | 含む中身が無ければパニック         |
| `Broadcast::assert_nothing_published()`       | 何か記録があればパニック           |

- 札を手放すと、fake は終わり、記録も空になります。
- fake の間は `memory` にも `database` にも送りません。`disconnect` は名前を確かめるだけで、記録もしません。

## 関連

- [deployment.md](deployment.md) — 前段のプロキシの設定と、止め方
- [configuration.md](configuration.md) — 環境変数と機能フラグ
- [testing.md](testing.md) — テストの道具と札
- [migrations.md](migrations.md) — マイグレーションの流し方
