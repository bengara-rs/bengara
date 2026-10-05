# ファイルの置き場所

アップロードされたファイルや、アプリが作ったファイルを置く場所です。

## ディスク

| ディスク        | 置き場所              | 用途                           |
|-----------------|-----------------------|--------------------------------|
| `local`（既定） | `storage/app/`        | 外から直接は見えない           |
| `public`        | `storage/app/public/` | 公開してよいもの               |

`config/filesystems.rs` を置けば増やせます。無くても動きます。

```rust
// config/filesystems.rs
use bengara::prelude::*;

pub fn config() -> StorageConfig {
    StorageConfig {
        default: "local".to_string(),
        disks: vec![
            DiskConfig::new("local", "app"),
            DiskConfig::new("public", "app/public"),
            DiskConfig::new("invoices", "app/invoices"),
        ],
    }
}
```

`DiskConfig::new` の 2 つめは **`storage/` からの相対パス**です。

## 読み書き

```rust
use bengara::prelude::*;

Storage::put("notes/memo.txt", "本文").await?;
Storage::put_bytes("images/a.png", bytes).await?;

let body: Option<String> = Storage::get("notes/memo.txt").await?;
let bytes: Option<Vec<u8>> = Storage::get_bytes("images/a.png").await?;

let there = Storage::exists("notes/memo.txt").await?;
Storage::delete("notes/memo.txt").await?;
```

関連関数（`Storage::put` など）は **既定のディスク**に対して動きます。

| メソッド                       | 返るもの                   | 覚えること                      |
|--------------------------------|----------------------------|---------------------------------|
| `put(パス, 文字)`              | `Result<()>`               | 途中のディレクトリも作る        |
| `put_bytes(パス, Vec<u8>)`     | `Result<()>`               |                                 |
| `get(パス)`                    | `Result<Option<String>>`   | 無ければ `None`                 |
| `get_bytes(パス)`              | `Result<Option<Vec<u8>>>`  |                                 |
| `exists(パス)` / `missing(パス)` | `Result<bool>`           |                                 |
| `delete(パス)`                 | `Result<()>`               | 無くてもエラーにしない          |

## ディスクを選ぶ

```rust
let disk = Storage::disk("public");

disk.write("logo.png", bytes).await?;
let size: Option<u64> = disk.size("logo.png").await?;
let files: Vec<String> = disk.files("").await?;      // 下まで全部
let real: PathBuf = disk.path("logo.png")?;          // 実際の場所
```

| メソッド                   | 返るもの                  |
|----------------------------|---------------------------|
| `write(パス, Vec<u8>)`     | `Result<()>`              |
| `read(パス)`               | `Result<Option<String>>`  |
| `read_bytes(パス)`         | `Result<Option<Vec<u8>>>` |
| `has(パス)`                | `Result<bool>`            |
| `remove(パス)`             | `Result<()>`              |
| `size(パス)`               | `Result<Option<u64>>`     |
| `files(頭のパス)`          | `Result<Vec<String>>`     |
| `path(パス)`               | `Result<PathBuf>`         |

`files` が返すのは **ディスクからの相対パス**です。下のディレクトリまで全部見ます。

```rust
Storage::default_disk()?.files("notes").await?;
// => ["notes/memo.txt", "notes/2026/01.txt"]
```

## 置き場所の外には出られません

**利用者が決めたパスをそのまま渡して構いません。** 次のものはエラーになります。

| 断るもの          | 例                  |
|-------------------|---------------------|
| `..` を含む       | `../../.env`        |
| 絶対パス          | `/etc/passwd`       |
| `:` を含む        | `C:\Windows`        |

```rust
pub async fn write_note(req: Request) -> Result<Response> {
    let input = req.validate(&[("name", "required|max:30"), ("body", "required")])?;
    // name に ../ が入っていても、put がエラーを返します。
    Storage::put(&format!("notes/{}.txt", input.get("name")), input.get("body")).await?;
    json(&bengara::serde_json::json!({ "ok": true }))
}
```

名前に使える文字を絞りたいときは、検査規則で縛るほうが親切です
（エラーの出方が分かりやすくなります）。

## 公開するもの

`public` ディスクに置いたものは、**まだ自動では配信されません。**
いまは読み出す画面を自分で書いてください。

```rust
pub async fn logo() -> Result<Response> {
    match Storage::disk("public").read_bytes("logo.png").await? {
        Some(bytes) => Ok(Response::bytes("image/png", bytes)),
        None => abort(404),
    }
}
```

## パスの作り方

```rust
storage_path("app")       // <プロジェクト>/storage/app
base_path("public")       // <プロジェクト>/public
public_path("robots.txt") // <プロジェクト>/public/robots.txt
app_path("Models")        // <プロジェクト>/app/Models
```

## 無いもの

| 項目                       | 代わりにすること                   |
|----------------------------|------------------------------------|
| S3 などの外部の置き場所    | ありません                         |
| `storage:link`             | 配信する画面を自分で書く           |
| アップロードの受け取り（multipart） | ありません。本文をそのまま受け取る |

## 関連

- [requests-and-responses.md](requests-and-responses.md) — 応答の作り方、静的ファイル
- [configuration.md](configuration.md) — `config/*.rs` と `.env`
