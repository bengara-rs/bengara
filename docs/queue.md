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

`app/Jobs/` に置きます。**ファイル名がジョブの名前になります。**
形は 1 つだけで、`pub async fn handle(payload: String) -> Result<()>` です。

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

| 決まり              | 内容                                           |
|---------------------|------------------------------------------------|
| 受け取るもの        | JSON の文字列。中身は自分で読む                |
| 名前                | ファイル名（`SendWelcome.rs` → `SendWelcome`） |
| `Ok(())` を返したら | 成功。行は消える                               |
| `Err` を返したら    | 失敗。待ってから再挑戦                         |
| パニックしたら      | 失敗と同じ扱い。**worker は止まらない**        |

**引数の型は効きません。** 中身の形が違っていても、実行して初めて分かります
（読めなければ `failed_jobs` に残ります）。

**フレームワークが呼ぶのは `handle` だけです。** 上の例の `Payload` のような構造体は、
自分で `serde_json::from_str` に渡すために置いているだけです。
Laravel のジョブクラスに当たるものはありません。

## ジョブを入れる

```rust
use bengara::prelude::*;

Queue::push("SendWelcome", &Payload { user_id: 1 }).await?;             // すぐ
Queue::later("SendWelcome", &Payload { user_id: 1 }, 300).await?;       // 5 分後
Queue::push_on("mail", "SendWelcome", &Payload { user_id: 1 }).await?;  // 列を分ける
Queue::push_raw("SendWelcome", r#"{"user_id":1}"#).await?;              // すでに JSON のとき
```

返るのは入れた行の id です。

- **`Queue::later` の遅延には上限があります。** 足した時刻が `9999-12-31 23:59:59` を
  超えるときは、そこで止めます。
- **`jobs` 表の `attempts` を自分で書き換えないでください。** `--tries` に達する回数が
  ずれます。

## 数を見る

| メソッド                             | 返るもの      | 数えるもの                     |
|--------------------------------------|---------------|--------------------------------|
| `size()` / `size_on(列)`             | `Result<i64>` | 表に残っている**全部**         |
| `ready_size()` / `ready_size_on(列)` | `Result<i64>` | **いますぐ処理できるもの**だけ |
| `failed_size()`                      | `Result<i64>` | 諦めたもの（`failed_jobs` 表） |
| `clear()`                            | `Result<u64>` | 待っているものを全部捨てる     |

`size()` には、処理中のもの・遅延待ちのもの・再挑戦待ちのものも入ります。
**「あと何件すぐ捌けるか」を知りたいときは `ready_size()` です。**

## 動かす

```sh
cargo artisan queue:work
```

```
キュー `default` のジョブを処理します（Ctrl+C で終了）
  SendWelcome #1 成功（6 ms）
```

運用で触る数字は、この旗だけです。

| 旗                 | 既定      | 内容                                     |
|--------------------|-----------|------------------------------------------|
| `--queue=名前`     | `default` | どの列を見るか                           |
| `--once`           | –         | 1 件だけ処理して終わる                   |
| `--tries=回数`     | 3         | 何回まで試すか。**必ずこの回数で止まる** |
| `--sleep=秒`       | 1         | 空のときに待つ秒数                       |
| `--retry-after=秒` | 90        | 処理中のまま残ったものを取り直す秒数     |

**`retry_after` は、ジョブ 1 本にかかる最長の時間より長くしてください。**
短いと、まだ動いているジョブを別の worker が二重に処理します。

**Ctrl+C は、いま動かしている 1 件を終わらせてから止まります。** 途中で切りません。
待ちたくないときは、もう一度 Ctrl+C を押すと強制終了します（終了コードは 130）。

### 取り残しと複数の worker

**同じジョブを 2 つの worker が処理することはありません。**
worker が強制終了すると予約の目印（`reserved_at`）が立ったままの行が残りますが、
`retry_after` 秒より古い予約は別の worker が取り直します。

| とき                                     | どうなるか                                    |
|------------------------------------------|-----------------------------------------------|
| 取り残しを取り直した                     | **失敗 1 回として数える**（`--tries` に効く） |
| `--tries` に達した                       | 取り直さず `failed_jobs` へ移す               |
| 処理の間に別の worker が取り直していた   | 成功も失敗も記録せず、警告を出して何もしない  |
| worker のプロセスごと落ちた（OOM など）  | 取り残しとして拾われ、`--tries` まで試される  |

**落ち方によらず、動くのは `--tries` の回数までです。** `Err` を返した・パニックした・
プロセスごと落ちた、のどれでも同じです。**そのジョブ 1 件でキュー全体が止まることは
ありません。**

**複数の台で worker を動かすときは、台どうしの時計をそろえてください**（NTP など）。
「取れる時刻」も「予約した時刻」も、worker の側で作るためです。

## 失敗したとき

待ってから再挑戦します。`--tries` を超えたら `failed_jobs` 表に移します。

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

**名前の無いジョブは待ちません。** `app/Jobs/` に無い名前のジョブは、再挑戦しても
動くようにならないので、1 回目でそのまま `failed_jobs` へ移します。ファイルを
置いたあとに `queue:retry` で戻せます。

## 本番で動かす

`queue:work` は常駐します。落ちたら上げ直す仕組みを外に用意してください。

| OS      | 使うもの                               |
|---------|----------------------------------------|
| Linux   | systemd のサービス（`Restart=always`） |
| Windows | タスクスケジューラ、サービス化のツール |

**`queue:work` と `serve` は別のプロセスです。** どちらも同じ `.env` と
同じ DB を見るようにしてください。

## テスト

`#[bengara::test]` の中では worker は動きません。**入ったことだけを確かめます。**

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

## worker の中の数字（触りません）

旗では変えられない、内部の決まりです。**ふだんは気にしなくて構いません。**

| こと                       | 決まり                                                            |
|----------------------------|-------------------------------------------------------------------|
| 失敗したあとの待ち時間     | 倍々に増えます。10 秒 → 20 秒 → 40 秒 …（6 回目以降は 320 秒で頭打ち） |
| 取り合いに負けたとき       | 待たずに取り直します。続けて 10 回負けたら `--sleep` の待ちに落ちます |
| ジョブの取り出し           | `select` で候補を 1 件取り、見たとおりの行かを条件に付けた `update` を投げます（比較交換） |
| 取り出しのトランザクション | 使いません。SQLite の WAL で worker が落ちるためです              |
| `failed_jobs` への出し入れ | 移すのと元の行を消すのは 1 つのトランザクションです。戻すときも同じで、二重に入りません |

## 無いもの

キューの置き場所は DB だけです（Redis とファイルはありません）。
ジョブのバッチ（`Bus::batch`）と一意なジョブ（`ShouldBeUnique`）はありません。
入れる前に自分で確かめてください。
ほかにまだ無いものは [backlog.md](backlog.md) にまとめてあります。

## 関連

- [scheduling.md](scheduling.md) — 定期処理。`schedule:run` とは別の仕組みです
- [events.md](events.md) — イベント。聞く側からジョブを積むのがよくある形です
- [mail.md](mail.md) — メール
- [migrations.md](migrations.md) — 表を作る
