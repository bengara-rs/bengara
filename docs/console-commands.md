# 自分のコマンド

`cargo artisan <名前>` で動かす処理を、自分で足す方法です。
`app/Console/Commands/` にファイルを置くだけで動きます。登録は要りません。

## 書く

1 つのファイルに、次の 2 つを書きます。どちらかが無いとビルドが通りません。

| 書くもの                                             | 役割                            |
|------------------------------------------------------|---------------------------------|
| `pub const DESCRIPTION: &str`                        | `cargo artisan list` に出す説明 |
| `pub async fn handle(args: &[String]) -> Result<()>` | 中身                            |

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

- `handle` が返ったら、コマンドは終わります（Laravel と同じ）。
- 待たずに始めた裏の処理（`tokio::spawn` や `spawn_blocking`）は、最大 1 秒だけ待ってから打ち切ります。
  最後まで済ませたい処理は、`handle` の中で `.await` してください。

## 動かす

```sh
cargo artisan greet アリス     # 開発
./myapp greet アリス           # 本番
```

`cargo artisan list` の末尾に、説明つきで並びます。

```
アプリの自作コマンド（app/Console/Commands/）

  greet  名前を言って挨拶する
```

## 名前の付き方

ファイル名がコマンド名になります。**大文字の前に `-` が入ります。** `_` も `-` になります。

| ファイル名       | コマンド名                               |
|------------------|------------------------------------------|
| `Greet.rs`       | `greet`                                  |
| `SendReport.rs`  | `send-report`                            |
| `Send_Report.rs` | `send-report`                            |
| `send_report.rs` | **一覧に入らない**（ビルドで警告が出る） |

| 決まり                                     | 守らないと                             |
|--------------------------------------------|----------------------------------------|
| ファイル名を大文字で始める                 | コマンドにならない（警告）             |
| 使う文字は英数字と `_` `-` だけ            | `.` や空白が入ると取り込まない（警告） |
| 名前を全体で重ねない                       | **ビルドが止まる**                     |
| フレームワークのコマンドと同じ名前にしない | **自分のほうは呼ばれない**（警告）     |

- 下にディレクトリを作って分けても、名前は 1 つの一覧で見ます。
  `SendReport.rs` と `Sub/Send_Report.rs` は、どちらも `send-report` なので重なります。
- **名前に `:` は入れられません。** `report:send` ではなく `report-send` のように `-` で区切ります。

### 使えないファイル名

次のファイル名は、フレームワークのコマンドと重なります。**使わないでください。**

`About.rs` / `Help.rs` / `Init.rs` / `List.rs` / `Migrate.rs` / `Serve.rs` / `Version.rs`

置くと、ビルドのときと、コマンドを動かすたびに警告が出ます。

## 引数

`args` には、コマンド名より後ろがそのまま入ります。**解析はされません。** 自分で読みます。

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

知らない指定も自分で断ってください。フレームワークのコマンドと違い、指定の確かめは掛かりません。

## 中でできること

アプリの機能を全部使えます。データベース・キュー・メール・イベントも動きます。

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

- 画面への出力は `println!` を使います。
- 決まった時刻に動かす処理は、[scheduling.md](scheduling.md) の定期処理に書きます。

## 関連

- [artisan.md](artisan.md) — もとからあるコマンド
- [directory-structure.md](directory-structure.md) — ファイルの自動検出の決まり
