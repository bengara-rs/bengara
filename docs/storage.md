# ファイルの置き場所

アップロードされたファイルや、アプリが作ったファイルを置く仕組みです。
このページで、読み書きの仕方・公開の仕方・断られるパスが分かります。

## ディスク

置き場所を「ディスク」という名前で分けて扱います。初めから 2 つあります。

| ディスク        | 置き場所              | 用途                           |
|-----------------|-----------------------|--------------------------------|
| `local`（既定） | `storage/app/`        | 外から直接は見えない           |
| `public`        | `storage/app/public/` | `/storage/...` で公開するもの  |

増やすときは `config/filesystems.rs` を置きます。無くても動きます。

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

- `DiskConfig::new` の 2 つめは、**`storage/` からの相対パス**です。
- 既定のディスクは、`.env` の `STORAGE_DISK` でも変えられます（既定は `local`）。
  `init` が作る `.env` には、`# STORAGE_DISK=local` の行が入っています。

## 既定のディスクに読み書きする

`Storage::put` のように `Storage::` から直接呼ぶと、**既定のディスク**に対して動きます。

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

気をつけることは 4 つです。

| こと                           | 内容                                                               |
|--------------------------------|--------------------------------------------------------------------|
| 書き込みの途中で落ちたとき     | 途中まで書かれたファイルが残る（Laravel の `put` と同じ）          |
| `storage/` そのものが無いとき  | 書き込みがエラーになる。先に `storage:init` で作る                 |
| 空のパス（`""` や `"."`）      | ファイルとしては扱わず、エラーになる                               |
| 文字として読めない中身         | `get` はエラーになる。`get_bytes` で読む                           |

途中までの中身を読まれたくないときは、別の名前に書いてから自分で置き換えてください。

## ディスクを選ぶ

`Storage::disk("名前")` で選びます。メソッドの名前が、上と少し違います。

```rust
let disk = Storage::disk("public");

disk.write("logo.png", bytes).await?;
let size: Option<u64> = disk.size("logo.png").await?;
let files: Vec<String> = disk.files("").await?;     // 下まで全部
let real: PathBuf = disk.path("logo.png")?;         // 実際の場所
```

| メソッド               | 返るもの                  | 既定のディスクでの名前     |
|------------------------|---------------------------|----------------------------|
| `write(パス, Vec<u8>)` | `Result<()>`              | `put_bytes`                |
| `read(パス)`           | `Result<Option<String>>`  | `get`                      |
| `read_bytes(パス)`     | `Result<Option<Vec<u8>>>` | `get_bytes`                |
| `has(パス)`            | `Result<bool>`            | `exists`                   |
| `remove(パス)`         | `Result<()>`              | `delete`                   |
| `size(パス)`           | `Result<Option<u64>>`     | –                          |
| `files(頭のパス)`      | `Result<Vec<String>>`     | –                          |
| `path(パス)`           | `Result<PathBuf>`         | –                          |

- **`Storage::disk("名前")` は、設定に無い名前だとパニックします。** 設定の書き間違いに、読み書きの前で気づけます。
- 既定のディスクそのものは `Storage::default_disk()?` で取れます。`size` や `files` を使うときに呼びます。

### 一覧を取る

`files` は、下のディレクトリまで全部見ます。返るのは**ディスクからの相対パス**です。

```rust
Storage::default_disk()?.files("notes").await?;
// => ["notes/2026/01.txt", "notes/memo.txt"]
```

| 渡したもの                          | 返るもの     |
|-------------------------------------|--------------|
| `""`                                | ディスク全部 |
| 無いディレクトリ                    | 空の一覧     |
| ファイル（ディレクトリでないもの）  | 空の一覧     |
| `..` を含む・絶対パス・使えない名前 | エラー       |

次のものは一覧に入りません。リンク以外は、警告がログに出ます。

| 入らないもの                         | 補足                                         |
|--------------------------------------|----------------------------------------------|
| シンボリックリンク（ジャンクション） | たどりません                                 |
| 深さ 32 より下                       | 渡した頭のパスから数えます                   |
| 名前が UTF-8 でないもの              | 一覧に出しても、読み書きに渡せないためです   |
| 読めないサブディレクトリ             | その枝だけが抜け、残りは返ります             |

## 断るパス

**置き場所の外には出られません。** 利用者が決めたパスを、そのまま渡して構いません。
次のものはエラーになります。

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

**どれも全 OS でエラーです。** Windows だけで困る名前も、Linux で断ります。
OS で動きを変えると、開発機で通ったコードが本番で失敗するためです。

```rust
pub async fn write_note(req: Request) -> Result<Response> {
    let input = req.validate(&[("name", "required|max:30"), ("body", "required")])?;
    // name に ../ が入っていても、put がエラーを返します。
    Storage::put(&format!("notes/{}.txt", input.get("name")), input.get("body")).await?;
    json(&bengara::serde_json::json!({ "ok": true }))
}
```

名前に使える文字は、検査規則でも絞ってください（`alpha_dash` など）。
断られたときに、利用者へ 422 で理由を返せます。

### Windows で困る名前

下の 4 つを断る理由です。**ふだんは気にしなくて構いません。**

| 断るもの                 | Windows で起きること                                    |
|--------------------------|---------------------------------------------------------|
| 装置の名前               | `nul` は書いた中身が捨てられ、`con` は読み戻しで止まる  |
| 末尾の空白と `.`         | 落とされるので `a.txt ` と `a.txt` が同じファイルになる |
| `< > " \| ? *`・制御文字 | ファイル名に使えない                                    |
| `~` の直後が数字         | 長い名前に自動で付く「短い名前」と区別できない          |

装置の名前は、拡張子を付けても装置のままです（`nul.txt` も断ります）。

## 公開する

`public` ディスクに置いたものは、**`/storage/...` で配信されます。**

```
storage/app/public/avatars/1.png   →   GET /storage/avatars/1.png
```

```rust
Storage::disk("public").write("avatars/1.png", bytes).await?;
// これで http://localhost:8000/storage/avatars/1.png が返る
```

| 決まり                 | 内容                                                  |
|------------------------|-------------------------------------------------------|
| メソッド               | `GET` と `HEAD` だけ                                  |
| 無いファイル           | 404                                                   |
| ディレクトリ           | 中の `index.html` を返す                              |
| `/storage/` だけ       | **404**                                               |
| 大文字の `/STORAGE/...` | **404。** 配るのは小文字の `/storage/...` だけ       |
| `public/storage/` の中身 | **配りません。** ファイルは `storage/app/public/` に置く |
| 頭のパス               | `/storage/` で固定。変えられない                      |

- リンクを張る操作（Laravel の `storage:link`）は要りません。フレームワークが直接配信します。
- URL は `/storage/` にパスをつなげて、自分で組み立てます。
- 応答に付くヘッダーと `ETag` は、[requests-and-responses.md](requests-and-responses.md) の静的ファイルの節にあります。

### 見せる相手を絞る

**同じパスに自分のルートを書くと、そちらが先に当たります。** ログインの確認などを挟めます。

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

## パスを作る関数

```rust
storage_path("app")        // <storage の場所>/app
public_path("robots.txt")  // <プロジェクト>/public/robots.txt
app_path("app/Models")     // <プロジェクト>/app/Models
base_path()                // <プロジェクト>
```

## 置き場所を動かす

`storage/` そのものを、別の場所に移せます。

```
APP_STORAGE_PATH=/var/lib/myapp/storage
```

- **絶対パスだけです。** 相対パスを書くと起動しません。
- プロセスを 2 つ以上動かすときは、全プロセスで同じ場所を指します（[deployment.md](deployment.md)）。

`storage/` とその中のディレクトリは、`storage:init` が作ります。**起動時には作りません。**

| 場面                 | 呼び方                          |
|----------------------|---------------------------------|
| 開発                 | `cargo artisan storage:init`    |
| 本番（cargo が無い） | `./<実行ファイル> storage:init` |

`storage/` が無いときのエラーは次の形です。キャッシュなども同じ文で知らせます。

```
<場所>/storage がありません。storage:init（開発では `cargo artisan storage:init`、本番では `./<実行ファイル> storage:init`） を実行してください
```

## できないこと

| できないこと                              | 代わりにすること                                  |
|-------------------------------------------|---------------------------------------------------|
| S3 などの外部の置き場所                   | `storage/` を共有のディスクに置く                 |
| フォームからのアップロード（multipart）の受け取り | 本文そのもの（`req.body()`）を `put_bytes` に渡す |
| `Storage::url(パス)`                      | `/storage/` ＋ パスを自分で組み立てる             |
| 途中から読む配信（`Range`）・大きなファイル | 前段のプロキシで配る                            |
| 実行中に既定のディスクを切り替える        | `Storage::disk("名前")` で選ぶ                    |

まだ無い機能の一覧は [backlog.md](backlog.md) にあります。

## 関連

- [deployment.md](deployment.md) — `storage:init`、置き場所の決め方
- [requests-and-responses.md](requests-and-responses.md) — 応答の作り方、静的ファイル
- [configuration.md](configuration.md) — `config/*.rs` と `.env`
- [cache.md](cache.md) — キャッシュ。鍵にも同じ名前の判定が掛かります
