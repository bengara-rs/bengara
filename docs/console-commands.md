# 自分のコマンド

`cargo artisan <名前>` で動かす処理を、自分で足す方法です。
`app/Console/Commands/` にファイルを置くだけで、登録は要りません。

## 書く

1 つのファイルに、次の 2 つを書きます。どちらかが無いとビルドが通りません。

| 書くもの                                             | 役割                             |
|------------------------------------------------------|----------------------------------|
| `pub const DESCRIPTION: &str`                        | `cargo artisan list` に出す説明  |
| `pub async fn handle(args: &[String]) -> Result<()>` | 中身                             |

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

## 動かす

```sh
cargo artisan greet アリス
```

```
こんにちは、アリス さん
```

本番では実行ファイルを直接動かします（`./myapp greet アリス`）。

`cargo artisan list` の末尾に、説明つきで並びます。

```
アプリの自作コマンド（app/Console/Commands/）

  greet  名前を言って挨拶する
  stats  記事とコメントの数を出す
```

## 名前の付き方

ファイル名がコマンド名になります。**大文字の前に `-` が入ります。**

| ファイル名         | コマンド名                                    |
|--------------------|-----------------------------------------------|
| `Greet.rs`         | `greet`                                       |
| `SendReport.rs`    | `send-report`                                 |
| `ClearOldPosts.rs` | `clear-old-posts`                             |
| `Send_Report.rs`   | `send-report`（`_` も `-` になります）        |
| `send_report.rs`   | **一覧に入りません**（ビルドで警告が出ます）  |

| 決まり                                   | 守らないとどうなるか                            |
|------------------------------------------|-------------------------------------------------|
| ファイル名を大文字で始める               | コマンドになりません（警告）                    |
| 使う文字は英数字と `_` `-` だけ          | `.` や空白が入ると取り込みません（警告）        |
| 名前を全体で重ねない                     | **ビルドが止まります**                          |
| フレームワークのコマンドと同じ名前にしない | **自分のほうは呼ばれません**（警告）          |

- 下にディレクトリを作って分けても、名前は 1 つの一覧で見ます。
  `SendReport.rs` と `Sub/Send_Report.rs` は、どちらも `send-report` なので重なります。
- **名前に `:` は入れられません。** `report:send` のような形は作れません。`-` で区切ります。

### フレームワークと同じ名前

次のファイル名は、フレームワークのコマンドと重なります。**使わないでください。**

`About.rs` / `Help.rs` / `Init.rs` / `List.rs` / `Migrate.rs` / `Serve.rs` / `Version.rs`

置くと、ビルドのときと、コマンドを動かすたびに警告が出ます。

```
WARN 自作コマンド `list` は呼ばれません。フレームワークに同じ名前のコマンドがあります（app/Console/Commands/ のファイル名を変えてください）
```

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

- **引数の解析はしません。** 必要な形で自分で読んでください。
- **知らない指定も自分で断ってください。** フレームワークのコマンドのような
  指定の確かめ（[artisan.md](artisan.md)）は、自分のコマンドには掛かりません。

## 中でできること

アプリの機能を全部使えます。データベースもキューもメールもイベントも動きます。

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

決まった時刻に動かしたい処理は、[scheduling.md](scheduling.md) の定期処理に書きます。

## できないこと

- ファイルを作るコマンド（`make:command`）はありません。手で作ります。
- 途中で質問する・進み具合の棒を出す、といった道具はありません。`println!` で出します。

まだ無い機能の一覧は [backlog.md](backlog.md) にあります。

## 関連

- [artisan.md](artisan.md) — もとからあるコマンドと、`cargo artisan` の仕組み
- [scheduling.md](scheduling.md) — 定期処理
- [directory-structure.md](directory-structure.md) — ファイルの自動検出の決まり
