# キューとジョブ

時間のかかる処理を後回しにして、画面をすぐ返す仕組みです。
積んだ処理（ジョブ）は、別のプロセス（worker）が取り出して動かします。
このページで、ジョブの書き方・積み方・worker の動かし方が分かります。

**データベースの機能フラグが要ります**（`sqlite` / `mysql` / `mariadb` / `postgres` のどれか）。
ジョブは DB の `jobs` 表に入ります。

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

```sh
cargo artisan migrate
```

## ジョブを書く

`app/Jobs/` に置きます。**ファイル名がジョブの名前になります。**
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

| 決まり                 | 内容                                           |
|------------------------|------------------------------------------------|
| 名前                   | ファイル名（`SendWelcome.rs` → `SendWelcome`） |
| 受け取るもの           | JSON の文字列。中身は自分で読む                |
| `Ok(())` を返したら    | 成功。行は消える                               |
| `Err` を返したら       | 失敗。待ってから再挑戦                         |
| パニックしたら         | 失敗と同じ扱い。**worker は止まらない**        |
| `--timeout` を超えたら | 打ち切って、失敗と同じ扱い                     |

- **フレームワークが呼ぶのは `handle` だけです。** `Payload` は、自分で JSON を読むために置いた構造体です。
- **中身の形は、実行して初めて確かめられます。** 読めなければ失敗になり、最後は `failed_jobs` に残ります。
- **ジョブは、2 回動いても困らない作りにしてください。** 時間切れや worker の停止で、同じジョブがもう一度動くことがあります。

## ジョブを入れる

```rust
use bengara::prelude::*;

Queue::push("SendWelcome", &Payload { user_id: 1 }).await?;                   // すぐ
Queue::later("SendWelcome", &Payload { user_id: 1 }, 300).await?;             // 5 分後
Queue::push_on("mail", "SendWelcome", &Payload { user_id: 1 }).await?;        // 列を分ける
Queue::later_on("mail", "SendWelcome", &Payload { user_id: 1 }, 300).await?;  // 列を分けて 5 分後
Queue::push_raw("SendWelcome", r#"{"user_id":1}"#).await?;                    // すでに JSON のとき
```

- 返るのは、入れた行の id（`i64`）です。
- 列（キューの名前）を書かなければ `default` に入ります。
- 名前が空のジョブは、入れるときにエラーになります。
- 遅らせる秒数には上限があります。`9999-12-31 23:59:59` を超える分は、そこで止めます。
- **`jobs` 表の `attempts` を自分で書き換えないでください。** 試す回数がずれます。

## 数を見る・捨てる

| メソッド                             | すること                             | 見る列             |
|--------------------------------------|--------------------------------------|--------------------|
| `size()` / `size_on(列)`             | 表に残っている数                     | `default` / その列 |
| `ready_size()` / `ready_size_on(列)` | **いますぐ処理できる数**             | `default` / その列 |
| `failed_size()`                      | 諦めた数（`failed_jobs` 表）         | 全部の列           |
| `clear()`                            | 待っているものを捨てる               | **全部の列**       |
| `clear_on(列)`                       | 待っているものを捨てる               | その列             |

数える 3 つは `Result<i64>` を、捨てる 2 つは消した件数（`Result<u64>`）を返します。

- `size()` には、処理中・遅延待ち・再挑戦待ちも入ります。すぐ捌ける数は `ready_size()` です。
- **`size()` は `default` の列だけ、`clear()` は全部の列です。** 範囲が違います。
- **`clear()` と `clear_on()` は、処理中の行も消します。** 動いているジョブは止まりません。
  そのジョブが失敗しても再挑戦されず、`failed_jobs` にも残りません。
- 諦めたもの（`failed_jobs` 表）は消しません。

## 動かす

```sh
cargo artisan queue:work
```

```
キュー `default` のジョブを処理します（Ctrl+C で終了）
  SendWelcome #1 成功（6 ms）
```

| 旗                 | 既定      | 内容                                      |
|--------------------|-----------|-------------------------------------------|
| `--queue=名前`     | `default` | どの列を見るか                            |
| `--once`           | –         | 1 件だけ処理して終わる                    |
| `--tries=回数`     | 3         | 何回まで試すか。**必ずこの回数で止まる**  |
| `--sleep=秒`       | 1         | 空のときに待つ秒数                        |
| `--timeout=秒`     | 60        | ジョブ 1 本を待つ上限。`0` は無制限       |
| `--retry-after=秒` | 90        | 処理中のまま残ったものを取り直すまでの秒数 |

- 値は `=` でつなぎます（`--tries=5`）。
- `--tries` と `--retry-after` は 1 以上です。
- **`--timeout` は `--retry-after` より数秒短くしてください。** 同じか長いと、起動時に警告が出ます。
  理由は下の「同じジョブが 2 回動くとき」にあります。

### 止める

**Ctrl+C を押すと、いま動かしている 1 件を終わらせてから止まります。** 途中で切りません。

| 合図       | 動き                                                   |
|------------|--------------------------------------------------------|
| 1 回目     | いまの 1 件が終わるのを待って止まる                    |
| 2 回目     | 待たずに強制終了する（終了コードは 130）               |

- `systemctl stop` や `docker stop` が送る合図（SIGTERM）でも同じです。
- 待つのは長くても `--timeout` 秒です（`--timeout=0` のときは、ジョブが終わるまで待ちます）。
- 本番での止め方は [deployment.md](deployment.md) の「queue:work を止める」にあります。

### 時間切れ（`--timeout`）

ジョブが `--timeout` 秒で終わらなければ、打ち切って**失敗として扱います。**
ほかの失敗と同じで、待ってから再挑戦します。

```
  SendWelcome #1 失敗（1 回目、10 秒後に再挑戦）: 60 秒で終わらなかったので打ち切りました（--timeout）
```

| 知っておくこと   | 内容                                                                   |
|------------------|------------------------------------------------------------------------|
| 止められるもの   | **待ち（`.await`）で固まったジョブ**。返事の来ない通信、長い `sleep` など |
| 止められないもの | `.await` を挟まずに計算し続ける処理。同期の入出力で止まっている処理    |
| 止められないとき | 失敗として記録し、worker は次のジョブへ進む。その処理は裏で動き続ける  |
| `--timeout=0`    | 打ち切らない。固まったジョブ 1 本で worker が止まり続ける              |

- 打ち切ったジョブは、**途中まで進んでいます。**
- **止められない処理は、スレッドを 1 本ふさいだまま残ります。** たまると後のジョブが動けません。
  そのときは worker を上げ直してください。長い計算は、ジョブの中で小さく区切ります。

### 同じジョブが 2 回動くとき

**待っているジョブを、2 つの worker が同時に取り出すことはありません。**
2 回動くのは、次の場合だけです。

| 場合                                               | 防ぎ方                                         |
|----------------------------------------------------|------------------------------------------------|
| ジョブ 1 本が `--retry-after` より長くかかった     | `--timeout` を `--retry-after` より短くする    |
| worker が処理の途中で強制終了した                  | 防げません。ジョブを 2 回動いてもよい作りにする |
| 成功したのに、その記録を DB に書けなかった         | 同上（下の「本番で動かす」）                   |

worker は、取り出したジョブに「処理中」の目印を付けます。
目印が `--retry-after` 秒より古くなると、worker が落ちたと見なして別の worker が取り直します。

| とき                                   | どうなるか                                    |
|----------------------------------------|-----------------------------------------------|
| 取り残しを取り直した                   | **失敗 1 回として数える**（`--tries` に効く） |
| `--tries` に達していた                 | 取り直さず `failed_jobs` へ移す               |
| 処理の間に別の worker が取り直していた | 成功も失敗も記録せず、警告を出す              |

- **落ち方によらず、動くのは `--tries` の回数までです。** `Err`・パニック・プロセスごと落ちた、のどれでも同じです。
- **複数の台で動かすときは、台どうしの時計をそろえてください**（NTP など）。
  時刻は worker の側で作るためです。

## 失敗したとき

待ってから再挑戦します。`--tries` に達したら `failed_jobs` 表に移します。

| 失敗の回数 | 次に試すまで             |
|------------|--------------------------|
| 1 回目     | 10 秒                    |
| 2 回目     | 20 秒                    |
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

コードからは `Queue::retry(Some(1))`・`Queue::retry(None)`・`Queue::flush_failed()` で同じことができます。

**`app/Jobs/` に無い名前のジョブは、再挑戦しません。** 1 回目でそのまま `failed_jobs` へ移します。
ファイルを置いたあとに `queue:retry` で戻せます。

## 本番で動かす

`queue:work` は常駐します。**落ちたら上げ直す仕組みを、外に用意してください。**

| OS      | 使うもの                               |
|---------|----------------------------------------|
| Linux   | systemd のサービス（`Restart=always`） |
| Windows | タスクスケジューラ、サービス化のツール |

- **`queue:work` と `serve` は別のプロセスです。** どちらも同じ `.env` と同じ DB を見るようにします。
- 設定の例は [deployment.md](deployment.md) にあります。

### DB につながらないとき

**一時的なエラーでは、すぐには終わりません。** 短い待ちを挟んでやり直します。
直らなければ、エラーを出して終わります（終了コードは 0 以外）。

| 失敗したところ   | やり直し                           |
|------------------|------------------------------------|
| ジョブの取り出し | 5 回まで。待ちは 1・2・4・8・10 秒 |
| 成功・失敗の記録 | 3 回まで。待ちは 1・2・4 秒        |

**成功の記録を書けないまま終わると、そのジョブは `--retry-after` の後にもう 1 回動きます。**
このときは、その旨を出してから終わります。

## テスト

`#[bengara::test]` の中では worker は動きません。**積まれたことだけを確かめます。**

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
CSRF を確かめないルートなら、`client.post(..)` のままで構いません。

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

## できないこと

| できないこと                           | 代わりにすること                             |
|----------------------------------------|----------------------------------------------|
| DB 以外にジョブを置く（Redis など）    | DB の `jobs` 表を使う                        |
| ジョブごとに時間切れを変える           | `--queue` で列を分け、worker ごとに `--timeout` を決める |
| 同じジョブを 1 つだけに絞る（`ShouldBeUnique`） | 入れる前に自分で確かめる            |
| worker が自分で上がり直す              | systemd などに任せる                         |

まだ無い機能の一覧は [backlog.md](backlog.md) にあります。

## 関連

- [scheduling.md](scheduling.md) — 定期処理。`schedule:run` とは別の仕組みです
- [events.md](events.md) — イベント。聞く側からジョブを積むのがよくある形です
- [mail.md](mail.md) — メール
- [migrations.md](migrations.md) — 表を作る
- [deployment.md](deployment.md) — 本番での常駐と止め方
