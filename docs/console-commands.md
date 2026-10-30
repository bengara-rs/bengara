# 自分のコマンド

`cargo artisan <名前>` で動かす処理を、自分で足せます。
置き場所は `app/Console/Commands/` で、ファイル名がコマンド名になります。

## 書く

1 つのファイルに **2 つとも必要です。**

| 書くもの                                             | 役割                        |
|------------------------------------------------------|-----------------------------|
| `pub const DESCRIPTION: &str`                        | `list` に出す説明。**必須** |
| `pub async fn handle(args: &[String]) -> Result<()>` | 中身。**必須**              |

どちらかが無いと **コンパイルエラー**になります。

```rust
//! app/Console/Commands/Greet.rs
use bengara::prelude::*;

pub const DESCRIPTION: &str = "名前を言って挨拶する";

pub async fn handle(args: &[String]) -> Result<()> {
    let name = args.first().map(String::as_str).unwrap_or("世界");
    println!("{}", __with("messages.greeting", &[("name", name)]));
    Ok(())
}
```

`Err` を返すと、理由を出して終了コード 1 で終わります。

## 名前の付き方

ファイル名からコマンド名を作ります。 **大文字の前で `-` を入れます。**

| ファイル名           | コマンド名                                       |
|----------------------|--------------------------------------------------|
| `Greet.rs`           | `greet`                                          |
| `SendReport.rs`      | `send-report`                                    |
| `ClearOldPosts.rs`   | `clear-old-posts`                                |
| `Send_Report.rs`     | `send-report`（`_` も `-` になります）           |
| `send_report.rs`     | **一覧に入りません。** 大文字で始めてください    |

**ファイル名は大文字で始めてください。** 小文字始まりのファイルはコマンドの一覧に
入りません。黙って無視せず、ビルドのときに警告を出します。

**使える文字は英数字と `_` `-` だけです。** `My Report.rs` や `Send.Report.rs` は
控えのファイルと見なして取り込みません（警告が出ます。
[directory-structure.md](directory-structure.md)）。

**名前に `:` は使えません。** `-` で区切ってください。
Laravel の `make:command` が作る `app:send-report` に近い形です（頭の `app:` は付けません）。

**名前がぶつかったらビルドが止まります。** ディレクトリを分けても同じです。
`SendReport.rs` と `Sub/Send_Report.rs` はどちらも `send-report` になります。

**フレームワークのコマンドが先です。** `migrate` や `queue:work` と同じ名前を付けると、
自分のほうは呼ばれません。`list` で確かめてください。

## 動かす

```sh
cargo artisan greet アリス
```

```
こんにちは、アリス さん
```

`cargo artisan list` の末尾に、説明つきで並びます。

```
アプリの自作コマンド（app/Console/Commands/）

  greet  名前を言って挨拶する
  stats  記事とコメントの数を出す
```

`list` は本体を呼び直すので、 **フレームワークのコマンドと自作コマンドが 1 画面に並びます。**

## 引数

`args` には、コマンド名より後ろがそのまま入ります。

```sh
cargo artisan send-report --to=alice@example.com --dry-run
```

```rust
pub async fn handle(args: &[String]) -> Result<()> {
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let to = args
        .iter()
        .find_map(|a| a.strip_prefix("--to="))
        .ok_or_else(|| Error::msg("--to= を指定してください"))?;
    // ...
    Ok(())
}
```

**解析はしません。** Laravel の `$signature` に当たるものは無いので、
必要な形で自分で読んでください。
**知らない旗も自分で断ってください。** フレームワークのコマンドがする旗の照合
（[artisan.md](artisan.md)）は、自作コマンドには掛かりません。

## 中で何をするか

アプリの中身を全部使えます。DB もキューもメールも動きます。

```rust
//! app/Console/Commands/Stats.rs
use bengara::prelude::*;

use crate::app::models::{Comment, Post};

pub const DESCRIPTION: &str = "記事とコメントの数を出す";

pub async fn handle(_args: &[String]) -> Result<()> {
    println!("記事:     {}", Post::count().await?);
    println!("コメント: {}", Comment::count().await?);
    println!("利用者:   {}", DB::table("users").count().await?);
    println!("待ちジョブ: {}", Queue::size().await?);
    Ok(())
}
```

## まだ無いもの

まだ無いものは [backlog.md](backlog.md) にまとめてあります。
**ファイルは手で作ります**（`make:command` はありません）。

## 関連

- [artisan.md](artisan.md) — `cargo artisan` の仕組みと、もとからあるコマンド
- [scheduling.md](scheduling.md) — 定期処理
- [directory-structure.md](directory-structure.md) — ファイルの自動検出の規則
