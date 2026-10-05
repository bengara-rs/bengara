# 定期処理

決まった間隔で動かしたい処理を書く場所です。

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

**1 分ごとに外から呼んでください。** Laravel と同じ形です。

```text
* * * * * cd /path/to/app && ./myapp schedule:run >> /dev/null 2>&1
```

Windows ならタスクスケジューラで、1 分おきに `myapp.exe schedule:run` を登録します。

## 一覧を見る

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

## 間隔の数え方

最後に動かした時刻は **キャッシュ**に置きます（鍵は `schedule:<名前>`）。

| 決まり                                   | 理由                                             |
|------------------------------------------|--------------------------------------------------|
| 動かす **前**に時刻を書く                 | 長くかかる処理が二重に走らないようにするため     |
| 失敗しても次の間隔まで待つ                | 失敗を理由にすぐ繰り返さない                     |
| `cache:clear` すると「まだ」に戻る         | 置き場所がキャッシュなので                       |

**名前を変えると、別の処理として数え直します。** 名前は鍵の一部です。

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

| 項目                            | 代わりにすること                     |
|---------------------------------|--------------------------------------|
| cron の式（`0 3 * * 1`）        | `Every` を使う                       |
| `schedule:work`（常駐）          | cron やタスクスケジューラから呼ぶ    |
| 重なりの防止（`withoutOverlapping`） | キューに積む。または自分でキャッシュに印を置く |
| 時間帯の指定（`at('3:00')`）     | ありません                           |

## 関連

- [queue.md](queue.md) — キュー。時間のかかる処理はこちらへ
- [cache.md](cache.md) — キャッシュ。最後に動かした時刻の置き場所
- [console-commands.md](console-commands.md) — 自分のコマンド
