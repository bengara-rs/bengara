# ファイルの置き場所

アップロードされたファイルや、アプリが作ったファイルを置く場所です。
置き場所を「ディスク」という名前で分けて扱います。

## ディスク

| ディスク        | 置き場所              | 用途                 |
|-----------------|-----------------------|----------------------|
| `local`（既定） | `storage/app/`        | 外から直接は見えない |
| `public`        | `storage/app/public/` | 公開してよいもの     |

`config/filesystems.rs` を置くと増やせます。無くても動きます。

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

## 既定のディスクに読み書きする

`Storage::put` のような静的メソッドは、 **既定のディスク**に対して動きます。

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
| `put_bytes(パス, Vec<u8>)`       | `Result<()>`              |                          |
| `get(パス)`                      | `Result<Option<String>>`  | 無ければ `None`          |
| `get_bytes(パス)`                | `Result<Option<Vec<u8>>>` |                          |
| `exists(パス)` / `missing(パス)` | `Result<bool>`            |                          |
| `delete(パス)`                   | `Result<()>`              | 無くてもエラーにしない   |

**書き込みは原子的ではありません。** `put` / `put_bytes` / `write` は、途中で
プロセスが落ちると**途中まで書かれたファイル**を残します（Laravel の `put` も同じです）。
中途半端な中身を読まれたくないときは、別の名前に書いてから自分で置き換えてください。

既定のディスク（`Storage::default_disk()`）の置き場所は **最初の 1 回だけ組み立てて覚えます。**
静的メソッドは毎回ここを通るので、設定の走査とパスの組み立てを繰り返さないためです。
設定は起動時に決まって変わらない前提です。

**実行中に設定を差し替えても、既定のディスクは変わりません。**
別の場所を使いたいときは `Storage::disk("名前")` を呼んでください。

## ディスクを選ぶ

```rust
let disk = Storage::disk("public");

disk.write("logo.png", bytes).await?;
let size: Option<u64> = disk.size("logo.png").await?;
let files: Vec<String> = disk.files("").await?;     // 下まで全部
let real: PathBuf = disk.path("logo.png")?;         // 実際の場所
```

| メソッド               | 返るもの                  |
|------------------------|---------------------------|
| `write(パス, Vec<u8>)` | `Result<()>`              |
| `read(パス)`           | `Result<Option<String>>`  |
| `read_bytes(パス)`     | `Result<Option<Vec<u8>>>` |
| `has(パス)`            | `Result<bool>`            |
| `remove(パス)`         | `Result<()>`              |
| `size(パス)`           | `Result<Option<u64>>`     |
| `files(頭のパス)`      | `Result<Vec<String>>`     |
| `path(パス)`           | `Result<PathBuf>`         |

`files` が返すのは **ディスクからの相対パス**です。下のディレクトリまで全部見ます。

- **シンボリックリンク（Windows のジャンクション）はたどりません。** 飛ばします。
  たどると、親を指すリンクが 1 つあるだけで再帰が止まらなくなります。
- **深さの上限は 32 です。** そこで打ち切るときは警告がログに出ます。
- **名前が UTF-8 でない項目は、警告を出して飛ばします。** Linux ではそういう名前が
  作れます。一覧に入れても、その文字列を `read` や `delete` に渡しても見つかりません。

一覧に出ないファイルがあるときは、警告を見てください。黙って減らしません。

```rust
Storage::default_disk()?.files("notes").await?;
// => ["notes/memo.txt", "notes/2026/01.txt"]
```

## 置き場所の外には出られません

**利用者が決めたパスをそのまま渡して構いません。** 次のものはエラーになります。

| 断るもの                    | 例                              |
|-----------------------------|---------------------------------|
| `..` を含む                 | `../../.env`                    |
| 絶対パス                    | `/etc/passwd`                   |
| `:` を含む                  | `C:\Windows`                    |
| Windows の装置の名前        | `nul`、`con`、`com1`、`nul.txt` |
| 末尾が空白か `.` のパス要素 | `note.txt `、`notes./a.txt`     |
| `< > " \| ? *` を含む       | `notes/a?b.txt`                 |
| 制御文字を含む              | 改行やタブが混ざった名前        |

**どれも全 OS でエラーです。** `#[cfg(windows)]` で分けていません。
OS で挙動を変えると、Linux で通ったコードが Windows で失敗したり、
黙ってデータを捨てたりします。開発と本番で挙動が変わるほうが困るので、
どの OS でも同じ名前を断ります。

断る理由は次のとおりです。

| 断るもの               | Windows で起きること                                  |
|------------------------|-------------------------------------------------------|
| 装置の名前             | `nul` は書いた中身が捨てられ、`con` は読み戻しで止まる |
| 末尾の空白と `.`       | 落とされるので `a.txt ` と `a.txt` が同じファイルになる |
| `< > " \| ? *`・制御文字 | ファイル名に使えないのでエラーになる                 |

装置の名前は、拡張子を付けても装置のままです（`nul.txt` も `nul`）。最初の `.` より
前だけを見て判定します。

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

`public` ディスクに置いたものは、 **`/storage/...` で配信されます。**

```
storage/app/public/avatars/1.png   →   GET /storage/avatars/1.png
```

```rust
Storage::disk("public").write("avatars/1.png", bytes).await?;
// これで http://localhost:8000/storage/avatars/1.png が返る
```

| 決めごと         | 内容                                  |
|------------------|---------------------------------------|
| メソッド         | `GET` と `HEAD` だけ。ほかは 404      |
| 無いファイル     | 404                                   |
| ディレクトリ     | 中の `index.html` を探す              |
| `/storage/` だけ | **404。** `index.html` は配信しません |
| 置き場所の外     | **404**（エラーの中身は見せません）   |
| 頭のパス         | `/storage/` で固定。変えられません    |

`/storage/` だけで来たときに `index.html` を返さないのは、 **置き場所の入口を見せないため**です。
「中身が空なら `index.html`」は `public/`（`GET /`）のための規則です。

**`storage:link` はありません。** Windows でシンボリックリンクを作るには管理者権限が
要るので、フレームワークが直接配信する形にしました。

### 認可を挟みたいとき

**ルートのほうが先に当たります。** 同じパスに自分のルートを書けば、そちらが勝ちます。

```rust
// routes/web.rs
Route::get("/storage/private/{name}", FileController::show).middleware("auth");
```

```rust
pub async fn show(req: Request) -> Result<Response> {
    let name: String = req.param_as("name")?;
    let user = req.auth().user_or_fail::<User>().await?;
    let path = format!("private/{}/{name}", user.id);

    match Storage::disk("public").read_bytes(&path).await? {
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

## 置き場所を動かす

`storage/` そのものを別の場所に移せます。

```
APP_STORAGE_PATH=/var/lib/myapp/storage
```

**絶対パスのみです。** 相対パスを指定すると起動しません。
プロセスを 2 つ以上動かすときは、全プロセスで同じ場所を指してください
（[deployment.md](deployment.md)）。

`storage/` の中のディレクトリは、`cargo artisan storage:init` が作ります。 **起動時には作りません。**

## 無いもの

| 項目                                | 代わりにすること                      |
|-------------------------------------|---------------------------------------|
| S3 などの外部の置き場所             | ありません                            |
| `storage:link`                      | `/storage/...` で直接配信します       |
| `Storage::url(path)`                | `/storage/` ＋ パスを自分で組み立てる |
| アップロードの受け取り（multipart） | ありません。本文をそのまま受け取る    |
| 範囲リクエスト（`Range`）           | 前段のプロキシに任せる                |

`ETag` と 304 は内蔵しています（[requests-and-responses.md](requests-and-responses.md)）。

## 関連

- [deployment.md](deployment.md) — `storage:init`、置き場所の決め方
- [requests-and-responses.md](requests-and-responses.md) — 応答の作り方、静的ファイル
- [configuration.md](configuration.md) — `config/*.rs` と `.env`
