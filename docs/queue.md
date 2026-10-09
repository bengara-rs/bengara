# キューとジョブ

時間のかかる処理を後回しにして、画面はすぐ返す仕組みです。
積んだ処理（ジョブ）は、別のプロセス（worker）が取り出して動かします。

**データベースの機能フラグが要ります**（`sqlite` / `mysql` / `mariadb` / `postgres` の
どれか）。ジョブは DB の `jobs` 表に入ります。

使うまでの手順は 4 つです。

| 手順            | すること                           |
|-----------------|------------------------------------|
| 1. 表を作る     | `Queue::define` のマイグレーション |
| 2. ジョブを書く | `app/Jobs/` に `handle` を置く     |
| 3. 積む         | `Queue::push` を呼ぶ               |
| 4. 動かす       | `cargo artisan queue:work`         |

## 表を作る

```rust
// database/migrations/2026_10_05_000500_create_jobs_table.rs
use bengara::prelude::*;

pub fn up(schema: &mut Schema) {
    // jobs と failed_jobs の2つをまとめて作ります。
    Queue::define(schema);
}

pub fn down(schema: &mut Schema) {
    schema.drop_if_exists(bengara::queue::TABLE);
    schema.drop_if_exists(bengara::queue::FAILED_TABLE);
}
```

```sh
cargo artisan migrate
```

## ジョブを書く

`app/Jobs/` に置きます。 **ファイル名がジョブの名前になります。**

```rust
// app/Jobs/SendWelcome.rs
use bengara::prelude::*;

use crate::app::models::User;

#[derive(bengara::serde::Serialize, bengara::serde::Deserialize)]
#[serde(crate = "bengara::serde")]
pub struct Payload {
    pub user_id: i64,
}

pub async fn handle(payload: String) -> Result<()> {
    let payload: Payload = bengara::serde_json::from_str(&payload)?;

    let Some(user) = User::find(payload.user_id).await? else {
        // もう消えている。やることが無いので成功で終わる。
        return Ok(());
    };

    Mail::to(&user.email)
        .subject("ようこそ")
        .text(format!("{} さん、登録ありがとうございます。", user.name))
        .send()
        .await
}
```

形は 1 つだけです。

```rust
pub async fn handle(payload: String) -> Result<()>
```

| 決まり              | 内容                                           |
|---------------------|------------------------------------------------|
| 受け取るもの        | JSON の文字列。中身は自分で読む                |
| 名前                | ファイル名（`SendWelcome.rs` → `SendWelcome`） |
| `Ok(())` を返したら | 成功。行は消える                               |
| `Err` を返したら    | 失敗。待ってから再挑戦                         |
| パニックしたら      | 失敗と同じ扱い。**worker は止まらない**        |

**引数の型は効きません。** 中身の形が違っていても、実行して初めて分かります
（読めなければエラーになり、`failed_jobs` に残ります）。

## ジョブを入れる

```rust
use bengara::prelude::*;

Queue::push("SendWelcome", &Payload { user_id: 1 }).await?;             // すぐ
Queue::later("SendWelcome", &Payload { user_id: 1 }, 300).await?;       // 5 分後
Queue::push_on("mail", "SendWelcome", &Payload { user_id: 1 }).await?;  // 列を分ける
Queue::push_raw("SendWelcome", r#"{"user_id":1}"#).await?;              // すでに JSON のとき
```

返るのは入れた行の id です。

## 数を見る

| メソッド                             | 返るもの      | 数えるもの                     |
|--------------------------------------|---------------|--------------------------------|
| `size()` / `size_on(列)`             | `Result<i64>` | 表に残っている**全部**         |
| `ready_size()` / `ready_size_on(列)` | `Result<i64>` | **いますぐ処理できるもの**だけ |
| `failed_size()`                      | `Result<i64>` | 諦めたもの（`failed_jobs` 表） |
| `clear()`                            | `Result<u64>` | 待っているものを全部捨てる     |

`size()` は、次の 3 つも数に入ります。

- 処理中のもの（worker が予約している）
- 遅延待ちのもの（`Queue::later` で入れた、まだ時刻が来ていない）
- 再挑戦待ちのもの（失敗して待ち時間を伸ばした）

**「あと何件すぐ捌けるか」を知りたいときは `ready_size()` です。**

## 動かす

```sh
cargo artisan queue:work
```

```
キュー `default` のジョブを処理します（Ctrl+C で終了）
  SendWelcome #1 成功（6 ms）
```

| 指定               | 既定      | 内容                                 |
|--------------------|-----------|--------------------------------------|
| `--queue=名前`     | `default` | どの列を見るか                       |
| `--once`           | –         | 1 件だけ処理して終わる               |
| `--tries=回数`     | 3         | 何回まで試すか                       |
| `--sleep=秒`       | 1         | 空のときに待つ秒数                   |
| `--retry-after=秒` | 90        | 処理中のまま残ったものを取り直す秒数 |

**Ctrl+C は、いま動かしている 1 件を終わらせてから止まります。** 途中で切りません。
1 回目を押すと、そう伝える案内が出ます。 **待ちたくないときは、もう一度 Ctrl+C を押すと
強制終了します**（終了コードは 130）。

### 複数の worker

**同じジョブを 2 つの worker が処理することはありません。**
取り出しは `select` で候補を 1 件取り、続けて「まだ予約されていなければ」という条件を
付けた `update` を投げます。この 2 文の比較交換で、更新が通るのは 1 つの worker だけです。

トランザクションは使いません。読んでから書くと、SQLite の WAL では
`SQLITE_BUSY_SNAPSHOT` になります。これは待ち時間を伸ばしても再試行されないので、
worker が丸ごと落ちていました。

### 取り残しを拾う

worker が強制終了すると、予約の目印（`reserved_at`）が立ったままの行が残ります。
そのままでは誰も処理しません。

`retry_after` 秒より古い予約は、別の worker が取り直します。

| 決まり         | 内容                                          |
|----------------|-----------------------------------------------|
| 既定の秒数     | 90 秒                                         |
| 変え方         | `queue:work --retry-after=秒`                 |
| 取り直したとき | **失敗 1 回として数える**（`--tries` に効く） |

**`retry_after` は、ジョブ 1 本にかかる最長の時間より長くしてください。**
短いと、まだ動いているジョブを別の worker が二重に処理します。

処理を終えた worker は、`id` だけでなく自分が書いた `reserved_at` も条件に見ます。
取り直されて自分の予約でなくなっていたときは、 **成功も失敗も記録せず、何もしません。**
代わりに警告が出て、`--retry-after` を長くするよう案内します。
これが無いと、成功したときに別の worker が動かしている行を消し、失敗したときに
予約を外して 3 つ目の worker に取らせていました。

## 失敗したとき

待ってから再挑戦します。待ち時間は倍々に増えます。

| 失敗の回数 | 次の挑戦まで |
|------------|--------------|
| 1 回目     | 10 秒        |
| 2 回目     | 20 秒        |
| 3 回目     | 40 秒        |
| …          | …            |
| 6 回目以降 | 320 秒       |

`--tries` を超えたら `failed_jobs` 表に移します。
**移すのと元の行を消すのは 1 つのトランザクションで行います。** 戻すときも同じです。
途中で落ちても、ジョブが消えたり二重になったりしません。

```sh
cargo artisan queue:failed          # 一覧
cargo artisan queue:retry           # 全部キューに戻す
cargo artisan queue:retry --id=1    # 1 件だけ
cargo artisan queue:flush           # 記録を捨てる
```

```
ID   ジョブ               失敗した時刻          理由
1    AlwaysFails          2026-10-05 10:21:05  このジョブはいつも失敗します

1 件
```

## 本番で動かす

`queue:work` は常駐します。落ちたら上げ直す仕組みを外に用意してください。

| OS      | 使うもの                               |
|---------|----------------------------------------|
| Linux   | systemd のサービス（`Restart=always`） |
| Windows | タスクスケジューラ、サービス化のツール |

**`queue:work` と `serve` は別のプロセスです。** どちらも同じ `.env` と
同じ DB を見るようにしてください。

## テスト

`#[bengara::test]` の中では worker は動きません。 **入ったことだけを確かめます。**

```rust
#[bengara::test]
async fn 登録するとジョブが積まれる() {
    let _db = refresh_database().await;

    client.post("/api/register", "name=x&email=a@example.com&password=password123").await;

    assert_eq!(Queue::size().await.unwrap(), 1);
}
```

処理の中身を確かめたいときは、`handle` をそのまま呼びます。

```rust
crate::app::jobs::send_welcome::handle(r#"{"user_id":1}"#.to_string())
    .await
    .unwrap();
assert_eq!(Mail::sent().len(), 1);
```

## `queue:retry` を 2 つ同時に流しても二重に入りません

失敗したジョブを戻すときは、**先に `failed_jobs` から消して、消せた分だけ**
`jobs` に入れます。消せるのは 1 つだけなので、同じジョブが 2 本入りません。

## 無いもの

| 項目                             | 代わりにすること              |
|----------------------------------|-------------------------------|
| Redis のキュー                   | DB を使う                     |
| ファイルのキュー                 | DB を使う                     |
| ジョブのクラス（`dispatch`）     | 関数 1 つ。名前は文字列で渡す |
| バッチ（`Bus::batch`）           | ありません                    |
| 一意なジョブ（`ShouldBeUnique`） | 入れる前に自分で確かめる      |
| `queue:work` の自動復帰          | systemd などに任せる          |

## 関連

- [scheduling.md](scheduling.md) — 定期処理。`schedule:run` とは別の仕組みです
- [events.md](events.md) — イベント。聞く側からジョブを積むのがよくある形です
- [mail.md](mail.md) — メール
- [migrations.md](migrations.md) — 表を作る
