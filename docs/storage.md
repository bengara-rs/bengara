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

`Storage::put` のような静的メソッドは、**既定のディスク**に対して動きます。

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

気をつけることは 4 つです。

- **書き込みは原子的ではありません。** 途中でプロセスが落ちると、途中まで書かれた
  ファイルが残ります（Laravel の `put` も同じです）。中途半端な中身を読まれたくない
  ときは、別の名前に書いてから自分で置き換えてください。
- **`storage/` そのものが無いと、書き込みはエラーになります。** 途中のディレクトリは
  作りますが、`storage/` は作りません。`cargo artisan storage:init` で先に作ってください。
- **空のパス（`""` や `"."`）はファイルとして扱いません。** 一覧（`files("")`）の
  起点としてだけ使えます。
- **既定のディスクは最初の 1 回だけ組み立てて覚えます。** 実行中に設定を差し替えても
  変わりません。別の場所を使うときは `Storage::disk("名前")` を呼んでください。

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

**`Storage::disk("名前")` は、設定に無い名前だとパニックします。** 設定の書き間違いに
読み書きの前で気づけるようにするためです。名前が決まっていないときは
`Storage::default_disk()?` を使ってください。

### 一覧を取る

`files` が返すのは**ディスクからの相対パス**です。下のディレクトリまで全部見ます。

```rust
Storage::default_disk()?.files("notes").await?;
// => ["notes/memo.txt", "notes/2026/01.txt"]
```

| 渡したもの                          | 返るもの     |
|-------------------------------------|--------------|
| `""`                                | ディスク全部 |
| 無いディレクトリ                    | 空の一覧     |
| ファイル（ディレクトリでないもの）  | 空の一覧     |
| `..` を含む・絶対パス・使えない名前 | エラー       |

次のものは一覧から抜けます。**黙って減らしません。警告がログに出ます。**

| 抜けるもの                             | 理由                                     |
|----------------------------------------|------------------------------------------|
| シンボリックリンク（ジャンクション）   | たどると再帰が止まらなくなるため         |
| 深さ 32 より下（数えはじめは `prefix`） | 打ち切ります                            |
| 名前が UTF-8 でない項目                | `read` や `delete` に渡しても見つからない |
| 読めないサブディレクトリ               | その枝だけが抜け、全体の一覧は返ります   |

## 置き場所の外には出られません

**利用者が決めたパスをそのまま渡して構いません。** 次のものはエラーになります。

| 断るもの                    | 例                              |
|-----------------------------|---------------------------------|
| `..` を含む                 | `../../.env`                    |
| 絶対パス（先頭が `/` `\`）  | `/etc/passwd`                   |
| `:` を含む                  | `C:\Windows`                    |
| Windows の装置の名前        | `nul`、`con`、`com1`、`nul.txt` |
| 末尾が空白か `.` のパス要素 | `note.txt `、`notes./a.txt`     |
| `< > " \| ? *` を含む       | `notes/a?b.txt`                 |
| 制御文字を含む              | 改行やタブが混ざった名前        |
| `~` の直後が数字            | `avatars/alicep~1.jpg`          |

**どれも全 OS でエラーです。** 理由は下の「断る名前のこと」にあります。

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

`public` ディスクに置いたものは、**`/storage/...` で配信されます。**

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

**`storage:link` はありません。** `/storage/...` をフレームワークが直接配信します。

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
storage_path("app")        // <プロジェクト>/storage/app
base_path("public")        // <プロジェクト>/public
public_path("robots.txt")  // <プロジェクト>/public/robots.txt
app_path("app/Models")     // <プロジェクト>/app/Models（基準ディレクトリからの相対）
```

## 置き場所を動かす

`storage/` そのものを別の場所に移せます。

```
APP_STORAGE_PATH=/var/lib/myapp/storage
```

**絶対パスのみです。** 相対パスを指定すると起動しません。
プロセスを 2 つ以上動かすときは、全プロセスで同じ場所を指してください
（[deployment.md](deployment.md)）。

`storage/` の中のディレクトリは、`cargo artisan storage:init` が作ります。
**起動時には作りません。**

## 断る名前のこと

上の「置き場所の外には出られません」で断る名前の、くわしい理由です。
**ふだんは気にしなくて構いません。**

| 断るもの                 | Windows で起きること                                   |
|--------------------------|--------------------------------------------------------|
| 装置の名前               | `nul` は書いた中身が捨てられ、`con` は読み戻しで止まる |
| 末尾の空白と `.`         | 落とされるので `a.txt ` と `a.txt` が同じファイルになる |
| `< > " \| ? *`・制御文字 | ファイル名に使えないのでエラーになる                   |
| `~` の直後が数字         | 長い名前に自動で付く「短い名前」と区別できない         |

- 装置の名前は、拡張子を付けても装置のままです（`nul.txt` も `nul`）。
  最初の `.` より前だけを見て判定します。
- `~1` の形を断るのは、NTFS が長い名前に付ける短い名前（`<先頭 6 文字>~<数字>`）と
  区別できないためです。別の名前のつもりが、同じ中身を指します。
- **`#[cfg(windows)]` で分けていません。** OS で挙動を変えると、Linux で通った
  コードが Windows で失敗したり、黙ってデータを捨てたりします。
  キャッシュの鍵（[cache.md](cache.md)）も同じ判定を通ります。

## 無いもの

S3 などの外部の置き場所と、アップロードの受け取り（multipart）はありません。
`Storage::url(path)` と `storage:link` もありません。`/storage/` ＋ パスを自分で
組み立ててください。範囲リクエスト（`Range`）は前段のプロキシに任せます。
`ETag` と 304 は内蔵しています（[requests-and-responses.md](requests-and-responses.md)）。

ほかにまだ無いものは [backlog.md](backlog.md) にまとめてあります。

## 関連

- [deployment.md](deployment.md) — `storage:init`、置き場所の決め方
- [requests-and-responses.md](requests-and-responses.md) — 応答の作り方、静的ファイル
- [configuration.md](configuration.md) — `config/*.rs` と `.env`
