# ファイルの置き場所

アップロードされたファイルや、アプリが作ったファイルを置く仕組みです（Laravel の `Storage`）。

## 読み書きする

`Storage::` から直接呼ぶと、既定のディスク（`storage/app/`）に読み書きします。

```rust
use bengara::prelude::*;

Storage::put("notes/memo.txt", "本文").await?;
Storage::put_bytes("images/a.png", bytes).await?;

let body: Option<String> = Storage::get("notes/memo.txt").await?;
let bytes: Option<Vec<u8>> = Storage::get_bytes("images/a.png").await?;

let there = Storage::exists("notes/memo.txt").await?;
Storage::delete("notes/memo.txt").await?;
```

| メソッド                         | 返るもの                  | 覚えること               |
|----------------------------------|---------------------------|--------------------------|
| `put(パス, 文字)`                | `Result<()>`              | 途中のディレクトリも作る |
| `put_bytes(パス, Vec<u8>)`       | `Result<()>`              | 同上                     |
| `get(パス)`                      | `Result<Option<String>>`  | 無ければ `None`          |
| `get_bytes(パス)`                | `Result<Option<Vec<u8>>>` | 無ければ `None`          |
| `exists(パス)` / `missing(パス)` | `Result<bool>`            |                          |
| `delete(パス)`                   | `Result<()>`              | 無くてもエラーにしない   |

- 書く途中で落ちると、途中までのファイルが残ります（Laravel と同じ）。
- `storage/` そのものが無いと、書き込みはエラーです。先に `storage:init` で作ります（下の「置き場所を動かす」）。
- 空のパス（`""` や `"."`）はエラーです。
- 文字として読めない中身を `get` するとエラーです。`get_bytes` で読みます。

## ディスク

ディスクは、置き場所に付けた名前です。初めから 2 つあります。

| ディスク        | 置き場所              | 用途                          |
|-----------------|-----------------------|-------------------------------|
| `local`（既定） | `storage/app/`        | 外から直接は見えない          |
| `public`        | `storage/app/public/` | `/storage/...` で公開するもの |

`Storage::disk("名前")` で選びます。メソッドの名前が少し違います。

```rust
let disk = Storage::disk("public");

disk.write("logo.png", bytes).await?;
let size: Option<u64> = disk.size("logo.png").await?;
let files: Vec<String> = disk.files("").await?;     // 下まで全部
let real: PathBuf = disk.path("logo.png")?;         // 実際の場所
```

| メソッド               | 返るもの                  | 既定のディスクでの名前 |
|------------------------|---------------------------|------------------------|
| `write(パス, Vec<u8>)` | `Result<()>`              | `put_bytes`            |
| `read(パス)`           | `Result<Option<String>>`  | `get`                  |
| `read_bytes(パス)`     | `Result<Option<Vec<u8>>>` | `get_bytes`            |
| `has(パス)`            | `Result<bool>`            | `exists`               |
| `remove(パス)`         | `Result<()>`              | `delete`               |
| `size(パス)`           | `Result<Option<u64>>`     | –                      |
| `files(頭のパス)`      | `Result<Vec<String>>`     | –                      |
| `path(パス)`           | `Result<PathBuf>`         | –                      |

- **設定に無い名前を `Storage::disk` に渡すと、パニックします。** 書き間違いにすぐ気づけるようにするためです。
- 既定のディスクそのものは `Storage::default_disk()?` で取れます。`size` や `files` を使うときに呼びます。

### ディスクを増やす

`config/filesystems.rs` を置きます。無くても動きます。

```rust
// config/filesystems.rs
use bengara::prelude::*;

pub fn config() -> StorageConfig {
    StorageConfig {
        default: env("STORAGE_DISK", "local"),
        disks: vec![
            DiskConfig::new("local", "app"),
            DiskConfig::new("public", "app/public"),
            DiskConfig::new("invoices", "app/invoices"),
        ],
    }
}
```

- `DiskConfig::new` の 2 つめは、`storage/` からの相対パスです。
- 既定のディスクは `.env` の `STORAGE_DISK` でも変えられます（既定は `local`）。

### 一覧を取る

`files` は下のディレクトリまで全部見て、ディスクからの相対パスを返します。

```rust
Storage::default_disk()?.files("notes").await?;
// => ["notes/2026/01.txt", "notes/memo.txt"]
```

- `""` を渡すと、ディスク全部です。
- 無いディレクトリやファイルを渡すと、空の一覧です。
- シンボリックリンクはたどりません。
- 深さ 32 より下、名前が UTF-8 でないもの、読めないディレクトリは飛ばします（警告がログに出ます）。

## 断るパス

**置き場所の外には出られません。** 利用者が決めたパスを、そのまま渡して構いません。
次のものはエラーになります。

| 断るもの                    | 例                                |
|-----------------------------|-----------------------------------|
| `..` を含む                 | `../../.env`                      |
| 絶対パス（先頭が `/` `\`）  | `/etc/passwd`                     |
| `:` を含む                  | `C:\Windows`                      |
| Windows の装置の名前        | `nul`、`con`、`com1`、`nul.txt`   |
| 末尾が空白か `.` のパス要素 | `note.txt `、`notes./a.txt`       |
| `< > " \| ? *`・制御文字    | `notes/a?b.txt`                   |
| `~` の直後が数字            | `avatars/alicep~1.jpg`            |

- **どの OS でもエラーにします。** 開発機で通ったコードが、本番で失敗しないようにするためです。
- 装置の名前は `CON` `PRN` `AUX` `NUL` `COM0`〜`COM9` `LPT0`〜`LPT9` などです。大文字小文字は見ません。拡張子を付けても断ります。

```rust
pub async fn write_note(req: Request) -> Result<Response> {
    let input = req.validate(&[("name", "required|max:30"), ("body", "required")])?;
    // name に ../ が入っていても、put がエラーを返します。
    Storage::put(&format!("notes/{}.txt", input.get("name")), input.get("body")).await?;
    json(&bengara::serde_json::json!({ "ok": true }))
}
```

名前に使える文字は、検査規則（`alpha_dash` など）でも絞ってください。
断ったときに、利用者へ 422 で理由を返せます。

## 公開する

`public` ディスクに置いたものは、`/storage/...` で配信されます。

```
storage/app/public/avatars/1.png   →   GET /storage/avatars/1.png
```

```rust
Storage::disk("public").write("avatars/1.png", bytes).await?;
// これで http://localhost:8000/storage/avatars/1.png が返る
```

- 配るのは `GET` と `HEAD` だけです。無いファイルは 404 です。
- ディレクトリは、中の `index.html` を返します。`/storage/` だけなら 404 です。
- 配るのは小文字の `/storage/...` だけです。頭のパスは変えられません。
- `public/storage/` に置いたファイルは配りません。`storage/app/public/` に置きます。
- Laravel の `storage:link` は要りません。フレームワークが直接配ります。
- URL は `/storage/` にパスをつなげて、自分で組み立てます。
- 付くヘッダーは [requests-and-responses.md](requests-and-responses.md) の静的ファイルの節にあります。

### 見せる相手を絞る

**`public` ディスクのファイルは、だれでも読めます。** ログインを確かめません。
見せる相手を絞るファイルは、配信されない `local` ディスクに置きます。
自分でルートを書き、確かめてから中身を返します。

```rust
// 置くとき（利用者ごとに分ける）
let path = format!("private/{}/{name}", user.id);
Storage::disk("local").write(&path, bytes).await?;
```

```rust
// routes/web.rs
Route::get("/files/{name}", FileController::show).middleware("auth");
```

```rust
pub async fn show(req: Request) -> Result<Response> {
    let name: String = req.param_as("name")?;
    let user = req.auth().user_or_fail::<User>().await?;
    let path = format!("private/{}/{name}", user.id);

    match Storage::disk("local").read_bytes(&path).await? {
        Some(bytes) => Ok(Response::bytes("image/png", bytes)),
        None => abort(404),
    }
}
```

**`public` に置いて、`/storage/private/{name}` のルートで守る形は使わないでください。**
このルートは 1 段のパスにしか当たりません。`/storage/private/1/a.png` は配信がそのまま返してしまいます。

## パスを作る関数

```rust
storage_path("app")        // <storage の場所>/app
public_path("robots.txt")  // <プロジェクト>/public/robots.txt
app_path("app/Models")     // <プロジェクト>/app/Models
base_path()                // <プロジェクト>
```

## 置き場所を動かす

`storage/` そのものを別の場所に移せます。

```
APP_STORAGE_PATH=/var/lib/myapp/storage
```

- **絶対パスだけです。** 相対パスを書くと起動しません。
- プロセスを 2 つ以上動かすときは、全部で同じ場所を指します（[deployment.md](deployment.md)）。

`storage/` とその中は、`storage:init` が作ります。起動時には作りません。

| 場面                 | 呼び方                          |
|----------------------|---------------------------------|
| 開発                 | `cargo artisan storage:init`    |
| 本番（cargo が無い） | `./<実行ファイル> storage:init` |

無いときは、次のエラーになります。キャッシュなども同じ文です。

```
<場所>/storage がありません。storage:init（開発では `cargo artisan storage:init`、本番では `./<実行ファイル> storage:init`） を実行してください
```

置き場所はローカルのディスクだけです。まだ無い機能は [backlog.md](backlog.md) にまとめています。

## 関連

- [deployment.md](deployment.md) — `storage:init`、置き場所の決め方
- [requests-and-responses.md](requests-and-responses.md) — 応答の作り方、静的ファイル
- [configuration.md](configuration.md) — `config/*.rs` と `.env`
