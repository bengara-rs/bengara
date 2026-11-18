# キューとジョブ

時間のかかる処理を後回しにして、画面をすぐ返す仕組みです。
積んだ処理（ジョブ）は、別のプロセス（worker）が取り出して動かします。

**データベースの機能フラグが要ります**（[database.md](database.md)）。ジョブは DB の `jobs` 表に入ります。

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
    // jobs と failed_jobs の 2 つをまとめて作ります。
    Queue::define(schema);
}

pub fn down(schema: &mut Schema) {
    schema.drop_if_exists(bengara::queue::TABLE);
    schema.drop_if_exists(bengara::queue::FAILED_TABLE);
}
```

置いたら `cargo artisan migrate` を流します。

## ジョブを書く

`app/Jobs/` に置きます。**ファイル名がジョブの名前になります**（`SendWelcome.rs` → `SendWelcome`）。
書くのは `handle` という関数 1 つです。

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

- `handle` は JSON の文字列を受け取ります。中身は自分で読みます。
- `Ok(())` を返すと成功です。行は消えます。
- `Err` を返すと失敗です。待ってから再挑戦します。
- パニックも失敗と同じです。worker は止まりません。
- **ジョブは、2 回動いても困らない作りにしてください。** 時間切れなどで、もう一度動くことがあります。

## ジョブを積む

```rust
use bengara::prelude::*;

Queue::push("SendWelcome", &Payload { user_id: 1 }).await?;                   // すぐ
Queue::later("SendWelcome", &Payload { user_id: 1 }, 300).await?;             // 5 分後
Queue::push_on("mail", "SendWelcome", &Payload { user_id: 1 }).await?;        // 列を分ける
Queue::later_on("mail", "SendWelcome", &Payload { user_id: 1 }, 300).await?;  // 列を分けて 5 分後
Queue::push_raw("SendWelcome", r#"{"user_id":1}"#).await?;                    // すでに JSON のとき
```

- 返るのは、積んだ行の id（`i64`）です。
- 列（キューの名前）を書かなければ `default` に入ります。
- 名前が空のジョブはエラーです。
- `jobs` 表の `attempts` 列を自分で書き換えないでください。試す回数がずれます。

## 数を見る・捨てる

| メソッド                             | すること                     | 見る列             |
|--------------------------------------|------------------------------|--------------------|
| `size()` / `size_on(列)`             | 表に残っている数             | `default` / その列 |
| `ready_size()` / `ready_size_on(列)` | いますぐ処理できる数         | `default` / その列 |
| `failed_size()`                      | 諦めた数（`failed_jobs` 表） | 全部の列           |
| `clear()`                            | 待っているものを捨てる       | **全部の列**       |
| `clear_on(列)`                       | 待っているものを捨てる       | その列             |

- 数える 3 つは `i64`、捨てる 2 つは消した件数（`u64`）を返します。
- `size()` には、処理中や遅らせて待っているものも入ります。
- **`clear()` は処理中の行も消します。** 動いているジョブは止まりませんが、失敗しても再挑戦されません。
- 諦めたもの（`failed_jobs` 表）は消しません。

## 動かす

```sh
cargo artisan queue:work
```

```
キュー `default` のジョブを処理します（Ctrl+C で終了）
  SendWelcome #1 成功（6 ms）
```

| 指定               | 既定      | 意味                                                                              |
|--------------------|-----------|-----------------------------------------------------------------------------------|
| `--queue=名前`     | `default` | どの列を見るか                                                                    |
| `--once`           | –         | 1 件だけ処理して終わる                                                            |
| `--tries=回数`     | 3         | 何回まで試すか（1 以上）                                                          |
| `--sleep=秒`       | 1         | 空のときに待つ秒数                                                                |
| `--timeout=秒`     | 60        | ジョブ 1 本を待つ上限。`0` は無制限。`config/queue.rs` に載ったジョブには効かない |
| `--retry-after=秒` | 90        | 処理中のまま残ったものを取り直すまでの秒数（1 以上）                              |

- 値は `=` でつなぎます（`--tries=5`）。
- `--timeout` が `--retry-after` より長くても構いません。予約をその分だけ延ばします（下の「同じジョブが 2 回動くとき」）。

### 止める

Ctrl+C を押すと、いま動かしている 1 件を終わらせてから止まります。

- もう 1 回押すと、待たずに強制終了します（終了コード 130）。
- `systemctl stop` や `docker stop` の合図（SIGTERM）でも同じです。
- 待つのは長くても、そのジョブの打ち切りの時間（`config/queue.rs` か `--timeout`）までです。
- 本番での止め方は [deployment.md](deployment.md) にあります。

### 時間切れ（`--timeout`）

ジョブが `--timeout` 秒で終わらなければ、打ち切って失敗として扱います。
ジョブごとに時間を決めたいときは、下の「ジョブごとの打ち切りの時間」を見てください。
ほかの失敗と同じく、待ってから再挑戦します。

```
  SendWelcome #1 失敗（1 回目、10 秒後に再挑戦）: 60 秒で終わらなかったので打ち切りました（--timeout）
```

- 打ち切れるのは、`.await` で待っている所です（返事の来ない通信など）。
- `.await` を挟まない長い計算は、打ち切れません。このときは **worker をエラーで終えます**。同じジョブが 2 本同時に動くのを防ぐためです。
- 長い計算は小さく区切って、間に `.await` を挟んでください。
- 打ち切ったジョブは、途中まで進んでいます。再挑戦は最初からやり直します。
- **systemd などで、落ちたら起動し直す設定にしてください**（[deployment.md](deployment.md)）。

### ジョブごとの打ち切りの時間

`config/queue.rs` に `QueueConfig` を返す関数を書くと、ジョブごとに打ち切りの時間を決められます。
Laravel のジョブの `$timeout` に当たります。

```rust
// config/queue.rs
use bengara::queue::QueueConfig;

pub fn config() -> QueueConfig {
    QueueConfig::default()
        .timeout("TranscodeVideo", 30 * 60) // 30 分
        .timeout("SendWelcome", 30)
}
```

- ジョブの名前は `app/Jobs/` のファイル名です。秒で書きます。
- `QueueConfig` は `bengara::QueueConfig` でも使えます（`prelude` には入っていません）。
- 中身は `pub timeouts: Vec<(String, u64)>` です。`Default` は空です。
- 載っているジョブは、`--timeout` より `config/queue.rs` の値を使います。
- 同じ名前が 2 回あれば、最初の 1 つを使います。
- `0` は打ち切りません。
- ファイルを置かなければ、どのジョブも `--timeout` で打ち切ります。

打ち切ったときの失敗の文には、どこで決めた時間かが入ります。

```
  TranscodeVideo #3 失敗（1 回目、10 秒後に再挑戦）: 1800 秒で終わらなかったので打ち切りました（config/queue.rs）
```

`queue:work` は、起動時に次のものを警告します。起動は止めません。

| 見つけたもの           | 警告する理由                                               |
|------------------------|------------------------------------------------------------|
| `--timeout 0`          | 予約を延ばせないので、`--retry-after` を過ぎると二重に動く |
| `app/Jobs/` に無い名前 | 書き間違いだと、そのジョブは `--timeout` のまま動く        |
| 同じ名前が 2 回        | 使うのは最初の 1 つだけ                                    |
| `0`（打ち切らない）    | 予約を延ばせないので、`--retry-after` を過ぎると二重に動く |

### 同じジョブが 2 回動くとき

2 つの worker が、同じジョブを同時に取り出すことはありません。
2 回動くのは、次のときだけです。

- 打ち切らない（`0`）ジョブが `--retry-after` より長くかかった
- worker が途中で強制終了した
- 時間切れで打ち切ったジョブが、途中まで進んでいた
- 成功したのに、その記録を DB に書けなかった

worker は、取り出したジョブに「処理中」の印（`jobs.reserved_at`）を付けます。
印が `--retry-after` 秒より古くなると、別の worker が取り直します。取り直しは失敗 1 回として数えます。

打ち切りの時間が長いジョブは、印に先の時刻を書いて、予約を延ばします。
動いている途中に取り直されることはありません。`config/queue.rs` の値にも `--timeout` にも効きます。

| 打ち切りの時間            | 印に書く時刻                           | 取り直されるのは                     |
|---------------------------|----------------------------------------|--------------------------------------|
| `retry_after - 30` 秒以下 | いま                                   | `--retry-after` 秒後（今までどおり） |
| それより長い              | いま + (打ち切り + 30 - `retry_after`) | 打ち切り + 30 秒後                   |
| 0（打ち切らない）         | いま                                   | `--retry-after` 秒後（延ばせない）   |

- 取り直されるのは「打ち切りの時間 + 30 秒」と `--retry-after` の遅いほうです。
- 既定（`--timeout=60`・`--retry-after=90`）では、今までと何も変わりません。
- **worker が落ちたとき、長いジョブが取り直されるまでの待ちも「打ち切り + 30 秒」になります。**
  30 分のジョブなら、取り直しは 30 分 30 秒後です。
- 表の形は変わりません。マイグレーションは要りません。
- Laravel では「`retry_after` を一番長いジョブより長くする」必要があります。bengara では要りません。

- **どんな落ち方でも、動くのは `--tries` の回数までです。**
- 複数の台で動かすときは、台どうしの時計をそろえてください（NTP など）。

## 失敗したとき

待ってから再挑戦します。`--tries` に達したら `failed_jobs` 表に移します。

| 失敗の回数 | 次に試すまで                |
|------------|-----------------------------|
| 1 回目     | 10 秒                       |
| 2 回目     | 20 秒                       |
| 3 回目以降 | 倍々に伸びる（上限 320 秒） |

```sh
cargo artisan queue:failed          # 一覧
cargo artisan queue:retry           # 全部キューに戻す
cargo artisan queue:retry --id=1    # 1 件だけ
cargo artisan queue:flush           # 記録を捨てる
```

```
ID   ジョブ       失敗した時刻         理由
1    AlwaysFails  2026-10-05 10:21:05  このジョブはいつも失敗します

1 件
```

- コードからは `Queue::retry(Some(1))`・`Queue::retry(None)`・`Queue::flush_failed()` で同じことができます。
- `app/Jobs/` に無い名前のジョブは、再挑戦せず `failed_jobs` へ移します。ファイルを置いてから `queue:retry` で戻せます。

## 本番で動かす

`queue:work` は常駐します。**落ちたら上げ直す仕組みを、外に用意してください**（Linux なら systemd の `Restart=always`）。

- `queue:work` と `serve` は別のプロセスです。同じ `.env` と同じ DB を見るようにします。
- 設定の例は [deployment.md](deployment.md) にあります。

DB に一時的につながらないときは、待ってやり直します。直らなければエラーで終わります。

| 失敗したところ   | やり直し                           |
|------------------|------------------------------------|
| ジョブの取り出し | 5 回まで。待ちは 1・2・4・8・10 秒 |
| 成功・失敗の記録 | 3 回まで。待ちは 1・2・4 秒        |

成功の記録を書けずに終わったジョブは、予約が切れた後（ふつうは `--retry-after` の後）にもう 1 回動きます。

## テスト

`#[bengara::test]` の中では worker は動きません。積まれたことだけを確かめます。

```rust
#[bengara::test]
async fn 登録するとジョブが積まれる() {
    let _db = refresh_database().await;

    // Cookie でログインする口なので、CSRF のトークンを付けて送る
    client
        .post_with_csrf(
            "/api/register",
            "name=x&email=a@example.com&password=password123",
            "/csrf-token",
        )
        .await
        .assert_status(201);

    assert_eq!(Queue::size().await.unwrap(), 1);
}
```

**応答も確かめてください。** 419 や 422 で断られていると、ジョブは積まれません。

処理の中身は、`handle` を直接呼んで確かめます。

```rust
#[bengara::test]
async fn ジョブがメールを送る() {
    let _db = refresh_database().await;
    let _lock = exclusive().await;      // Mail::sent() はプロセスで 1 つ
    Mail::clear_sent();

    // 利用者がいないと、このジョブは何もせずに終わる。先に作る
    let mut user = User::register("アリス", "alice@example.com", "password123")
        .await
        .unwrap();
    user.save().await.unwrap();

    let payload = bengara::serde_json::json!({ "user_id": user.id }).to_string();
    crate::app::jobs::send_welcome::handle(payload).await.unwrap();

    assert_eq!(Mail::sent().len(), 1);
}
```

- `User::register` はアプリの側の関数です（[authentication.md](authentication.md)）。
- 札を取る順は `refresh_database()` → `exclusive()` です（[testing.md](testing.md)）。

ジョブは DB の `jobs` 表にだけ置けます。まだ無い機能は [backlog.md](backlog.md) にまとめています。

## 関連

- [scheduling.md](scheduling.md) — 定期処理。`schedule:run` とは別の仕組みです
- [events.md](events.md) — イベント。聞く側からジョブを積むのがよくある形です
- [mail.md](mail.md) — メール
- [deployment.md](deployment.md) — 本番での常駐と止め方
