# 定期処理

決まった間隔で動かしたい処理を書く場所です。

> **自動では動きません。** `cargo artisan schedule:run` を **1 分ごとに外から呼ぶ前提**です。
> cron やタスクスケジューラに登録してください。常駐する仕組みはありません。
> Laravel と同じ形です。

```text
* * * * * cd /path/to/app && ./myapp schedule:run >> /dev/null 2>&1
```

Windows ならタスクスケジューラで、1 分おきに `myapp.exe schedule:run` を登録します。

## 書く

`routes/console.rs` に書きます。`init` が雛形を作ります。

```rust
//! routes/console.rs
use bengara::prelude::*;

pub fn schedule(s: &mut Schedule) {
    s.job("再設定トークンの掃除", Every::Hour, || async {
        PasswordReset::sweep_expired().await.map(|_| ())
    });

    s.job("記事数の集計", Every::Minutes(5), || async {
        let total = crate::app::models::Post::count().await?;
        Cache::forever("stats.posts", total.to_string()).await
    });
}
```

クロージャは `Result<()>` を返します。登録は書いた順です。

## 間隔

| `Every`      | 間隔                                       |
|--------------|--------------------------------------------|
| `Minute`     | 呼ばれるたびに動かす（1 分ごとに呼ぶ前提） |
| `Minutes(n)` | n 分ごと                                   |
| `Hour`       | 1 時間ごと                                 |
| `Day`        | 1 日ごと                                   |
| `Seconds(n)` | n 秒ごと                                   |

**「毎週月曜の 3 時」のような指定はできません。** cron の式は読みません。

## 動かす

```sh
cargo artisan schedule:run
```

```
  再設定トークンの掃除 成功（9 ms）
  記事数の集計 成功（2 ms）
2 件動かしました。
```

一覧だけ見たいときは `schedule:list` です。

```sh
cargo artisan schedule:list
```

```
名前                  間隔        前回
再設定トークンの掃除  1 時間ごと  2026-10-05 10:17:21
記事数の集計          5 分ごと    2026-10-05 10:17:21
生存の記録            毎分        2026-10-05 10:17:21

3 件
```

### 二重に走らない

**タスクごとに錠ファイルを取ります。** 前の `schedule:run` が終わる前に次が始まっても、
同じタスクが 2 回走ることはありません。2 台が同じ `storage/` を共有しているときも同じです。

| 決まり           | 内容                                                       |
|------------------|------------------------------------------------------------|
| 錠の置き場所     | `storage/framework/schedule/+locks/`                       |
| 錠を握る長さ     | 「前回の時刻を読む → 判定する → 書く」の間だけ             |
| 錠が取れないとき | `名前 は飛ばします（錠を取れませんでした）` と出て動かない |
| ほかのタスク     | 1 つ飛ばしても、残りは動かす                               |

処理そのものは錠を手放してから動かします。長い処理の間ずっと握ると、古い錠と
見なされて外されてしまうためです。動かすと決める前に時刻を書くので、
同じ時刻に動かすのは 1 つだけになります。

## 間隔の数え方

最後に動かした時刻は **`storage/framework/schedule/` のファイル**に置きます。
1 つの処理が 1 つのファイルです。ファイル名は名前をハッシュにしたものです。

| 決まり                     | 理由                                         |
|----------------------------|----------------------------------------------|
| 動かす **前**に時刻を書く  | 長くかかる処理が二重に走らないようにするため |
| 失敗しても次の間隔まで待つ | 失敗を理由にすぐ繰り返さない                 |
| キャッシュには置かない     | `CACHE_DRIVER=memory` でも正しく数えるため   |

**`CACHE_DRIVER` が何であっても間隔は正しく効きます。** `cache:clear` でも消えません。
やり直したいときは `storage/framework/schedule/` の中を消してください。

`storage/framework/schedule/` は `cargo artisan storage:init` が作ります。
無いときは `schedule:run` が、作ってくださいと言って止まります。

**名前を変えると、別の処理として数え直します。** 名前はファイル名のもとです。

## 中で何をするか

`schedule:run` は本体のバイナリなので、アプリの中身を全部使えます。

```rust
s.job("古い記事を消す", Every::Day, || async {
    DB::table("posts").where_op("created_at", "<", "2020-01-01 00:00:00").delete().await?;
    Ok(())
});

s.job("お知らせを積む", Every::Hour, || async {
    Queue::push_raw("SendReport", "{}").await?;
    Ok(())
});
```

時間のかかるものは、ここで直接やらずに **キューに積む**ほうがよいです。
`schedule:run` は 1 分ごとに呼ばれるので、前の実行が終わっていないと重なります。

## 無いもの

| 項目                                 | 代わりにすること                               |
|--------------------------------------|------------------------------------------------|
| cron の式（`0 3 * * 1`）             | `Every` を使う                                 |
| `schedule:work`（常駐）              | cron やタスクスケジューラから呼ぶ              |
| 重なりの防止（`withoutOverlapping`） | キューに積む。または自分でキャッシュに印を置く |
| 時間帯の指定（`at('3:00')`）         | ありません                                     |

## 関連

- [queue.md](queue.md) — キュー。時間のかかる処理はこちらへ
- [deployment.md](deployment.md) — `storage:init` と `storage/` の置き場所
- [console-commands.md](console-commands.md) — 自分のコマンド
