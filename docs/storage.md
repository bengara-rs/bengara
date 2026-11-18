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

| メソッド                         | 返るもの                  | 覚えること                         |
|----------------------------------|---------------------------|------------------------------------|
| `put(パス, 文字)`                | `Result<()>`              | 途中のディレクトリも作る           |
| `put_bytes(パス, Vec<u8>)`       | `Result<()>`              | 同上                               |
| `get(パス)`                      | `Result<Option<String>>`  | 無ければ `None`                    |
| `get_bytes(パス)`                | `Result<Option<Vec<u8>>>` | 無ければ `None`                    |
| `exists(パス)` / `missing(パス)` | `Result<bool>`            |                                    |
| `delete(パス)`                   | `Result<()>`              | 無くてもエラーにしない             |
| `append(パス, 文字)`             | `Result<()>`              | 末尾に書き足す（下の「書き足す」） |

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

| メソッド                         | 返るもの                  | 既定のディスクでの名前         |
|----------------------------------|---------------------------|--------------------------------|
| `write(パス, Vec<u8>)`           | `Result<()>`              | `put_bytes`                    |
| `read(パス)`                     | `Result<Option<String>>`  | `get`                          |
| `read_bytes(パス)`               | `Result<Option<Vec<u8>>>` | `get_bytes`                    |
| `has(パス)`                      | `Result<bool>`            | `exists`                       |
| `remove(パス)`                   | `Result<()>`              | `delete`                       |
| `size(パス)`                     | `Result<Option<u64>>`     | –                              |
| `files(頭のパス)`                | `Result<Vec<String>>`     | –                              |
| `path(パス)`                     | `Result<PathBuf>`         | –                              |
| `append_bytes(パス, Vec<u8>)`    | `Result<()>`              | –（`append` は改行を挟む別物） |
| `write_stream(パス, BodyStream)` | `Result<u64>`             | –                              |

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

## 書き足す

```rust
Storage::append("logs/notes.txt", "1 行目").await?;
Storage::append("logs/notes.txt", "2 行目").await?;   // 中身は "1 行目\n2 行目"

Storage::disk("local").append_bytes("chunks/a.bin", bytes).await?;
```

| メソッド                           | 動き                                                                     |
|------------------------------------|--------------------------------------------------------------------------|
| `Storage::append(パス, 文字)`      | ファイルがあれば、間に改行（`\n`）を挟んで足す。無ければ作る（挟まない） |
| `disk.append_bytes(パス, Vec<u8>)` | 何も挟まずに末尾へ足す。無ければ作る                                     |

- `Storage::append` は Laravel の `Storage::append` と同じ決まりです。
- Laravel の改行は OS ごとに違います。bengara は**どの OS でも `\n`** です。
- **同時に `append` すると、改行の有無がずれることがあります。**
  ファイルがあるかを見てから書くまでの間に、別の書き足しが入るためです。
  何本ものリクエストから 1 行ずつ足すときは、自分で `\n` を付けて `append_bytes` を使います。
- `append_bytes` は OS の追記モードで書きます。ファイル全体を読み直しません。途中のディレクトリも作ります。
- `append_bytes` の 1 回の書き足しが途中で切れることはあります。前の中身は壊しません。
- 既定のディスクに `append_bytes` するときは `Storage::default_disk()?.append_bytes(..)` と書きます。

### 名前が違う理由

Rust では、同じ型に同じ名前の関数を 2 つ置けません。
そのため、`Storage::` の静的メソッドとディスクのメソッドで名前を分けています（`put` と `write`、`get` と `read` と同じ理由）。

- 静的メソッドは Laravel の名前どおり `append`（改行を挟む）だけです。
- ディスクのほうは、`write` と同じくバイト列を受ける `append_bytes` と `write_stream` です。

## 本文を少しずつ書く

`.stream_body()` のルートで受けた本文（[requests-and-responses.md](requests-and-responses.md)）を、
全部メモリに載せずにファイルへ書きます。Laravel の `writeStream` に当たります。

```rust
pub async fn upload(req: Request) -> Result<Response> {
    let name: String = req.param_as("name")?;
    let body = req.body_stream()?;
    let bytes = Storage::disk("local").write_stream(&format!("uploads/{name}"), body).await?;
    text(format!("{bytes} バイト保存しました"))
}
```

- 返すのは書いたバイト数です。
- **最後まで受け取れたときだけ、その名前のファイルになります。**
  書いている間は、同じディレクトリの `.名前.<16進の乱数>.part` に書きます。終わったら置き換えます。
- Windows でファイルが「使用中」のときは、少し待ってやり直します。
- 途中で失敗したら、書きかけを消します。**前からあったファイルはそのまま残ります。**
- 失敗は、本文の上限超え（413）・時間切れ（408）・切断（400）と、書き込みの失敗です。
  本文のエラーは同じ状態コードで返るので、`?` で返せばその応答になります。
- 待つのをやめたとき（`tokio::time::timeout` で包んだ、キューの打ち切りで止められた、など）も、書きかけを消します。
  ただし、ちょうど裏のスレッドで書いている最中だと、Windows では消せません。ログに警告が出て、`.part` が残ります。
- 書いている間の `.part` のファイルは、`files()` の一覧にも見えます。
- `fsync`（ディスクへの書き出しを待つこと）はしません。ほかの書き込みと同じです。

**書き口（`Storage::writer` のようなもの）はありません。** 閉じ忘れなど、使い方を誤りやすいためです。
少しずつ届くデータは、かたまりごとに `append_bytes` するか、本文なら `write_stream` を使います。

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
| `< > " \| ? *`・制御文字    | `notes/a?b.txt`                 |
| `~` の直後が数字            | `avatars/alicep~1.jpg`          |

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
