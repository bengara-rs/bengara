# 設定

設定ファイルの書き方、`.env` に書ける項目、機能フラグの一覧です。

- 設定は Rust のコードです。`config/` 直下に置くと、起動時に自動で読まれます。
- 設定は実行ファイルに入ります。本番に設定ファイルは置きません。
- 値は `.env`（環境変数）から読みます。

## 設定ファイルを書く

```rust
// config/app.rs
use bengara::prelude::*;

pub fn config() -> AppConfig {
    AppConfig {
        name: env("APP_NAME", "myapp"),
        env: env("APP_ENV", "production"),
        debug: env("APP_DEBUG", false),
        url: env("APP_URL", "http://localhost:8000"),
        key: env("APP_KEY", ""),
    }
}
```

| 決まり                                | 守らないと                |
|---------------------------------------|---------------------------|
| `config/` の**直下**に置く            | 設定として読まれない      |
| ファイル名を**小文字**で始める        | 設定として読まれない      |
| `pub fn config() -> 何かの型` を書く  | ビルドが通らない          |
| 1 つの型は 1 つのファイルからだけ返す | どちらか 1 つしか残らない |

### 読み出す

型を指定して取り出します。文字列のキー（`config("app.name")`）では読めません。

```rust
let app = config::<AppConfig>();          // 無ければパニック
let app = try_config::<AppConfig>();      // 無ければ None
```

取れるのは `config/` のファイルが返した型だけです。
`config/cache.rs` が無いのに `config::<CacheConfig>()` を呼ぶと、パニックします。

### 自分の設定を足す

`config/<名前>.rs` に好きな型を書き、それを返す `config()` を書きます。

```rust
// config/shop.rs
use bengara::prelude::*;

pub struct ShopConfig {
    pub name: String,
    pub tax_percent: u32,
}

pub fn config() -> ShopConfig {
    ShopConfig {
        name: env("SHOP_NAME", "my shop"),
        tax_percent: env("SHOP_TAX_PERCENT", 10u32),
    }
}
```

```rust
let shop = config::<crate::config::shop::ShopConfig>();
```

### フレームワークの設定を差し替える

次の型を返すファイルを置くと、フレームワークはその値を使います。
置かなければ、下の「環境変数の一覧」のとおりに読みます。ファイル名は自由です。

| 返す型            | 何の設定か                 | 詳しくは                               | `init` が作るか |
|-------------------|----------------------------|----------------------------------------|-----------------|
| `AppConfig`       | 基本設定                   | このページ                             | 作る            |
| `DatabaseConfig`  | データベースの接続         | [database.md](database.md)             | 作る            |
| `CacheConfig`     | キャッシュの置き場所       | [cache.md](cache.md)                   | 作らない        |
| `MailConfig`      | メールの送り先             | [mail.md](mail.md)                     | 作らない        |
| `StorageConfig`   | ファイルの置き場所         | [storage.md](storage.md)               | 作らない        |
| `HashConfig`      | パスワードの変換の回数     | [authentication.md](authentication.md) | 作らない        |
| `ArgonConfig`     | Argon2id の強さ            | [authentication.md](authentication.md) | 作らない        |
| `BroadcastConfig` | 配信の送り方               | [websocket.md](websocket.md)           | 作らない        |
| `QueueConfig`     | ジョブごとの打ち切りの時間 | [queue.md](queue.md)                   | 作らない        |

- 次の 3 つは `prelude` に入っていません。`use` で取り込みます。

  | 型                | 取り込む場所                          |
  |-------------------|---------------------------------------|
  | `ArgonConfig`     | `bengara::ArgonConfig`                |
  | `QueueConfig`     | `bengara::queue::QueueConfig`         |
  | `BroadcastConfig` | `bengara::broadcast::BroadcastConfig` |

- `QueueConfig` は環境変数から読みません。置かなければ、どのジョブも `queue:work --timeout` で打ち切ります。

### いまの環境を調べる

| 書き方                                  | 真になるとき         |
|-----------------------------------------|----------------------|
| `config::<AppConfig>().is_production()` | `APP_ENV=production` |
| `config::<AppConfig>().is_local()`      | `APP_ENV=local`      |
| `config::<AppConfig>().is_testing()`    | `APP_ENV=testing`    |

`staging` などは、`config::<AppConfig>().env` を自分で比べます。

## env() の使い方

`env(名前, 既定値)` で環境変数を読みます。返す型は**既定値の型**で決まります。

```rust
let port: u16 = env("APP_PORT", 8000u16);
let debug: bool = env("APP_DEBUG", false);
let name: String = env("APP_NAME", "myapp");
let dir: std::path::PathBuf = env("CACHE_DIR", "storage/cache");
```

- **数の既定値には型を付けます**（`8000u16`）。付けないと `i32` になり、コンパイルが通りません。
- 読める型は、文字列・パス（`PathBuf`）・`bool`・整数・小数です。
- `bool` は `true` `1` `yes` `on` が真です。`false` `0` `no` `off` `null` `none` が偽です。

| 環境変数の値                   | `env()` が返すもの  |
|--------------------------------|---------------------|
| 無い・空・空白だけ             | 既定値              |
| 型に合わない（`APP_PORT=abc`） | 既定値。警告が出る  |
| 前後に空白がある（`A=" x "`）  | `x`（空白は落とす） |

**`.env` では値を空にできません。** 空にしたいときは、既定値を空にします（`env("MAIL_FROM_NAME", "")`）。

## .env の書き方

```ini
APP_NAME=myapp
APP_ENV=local

# コメント
DB_PASSWORD="p#ss"
MAIL_FROM_NAME="${APP_NAME}"
```

| 決まり          | 内容                                                                   |
|-----------------|------------------------------------------------------------------------|
| 優先            | 本物の環境変数が勝つ。`.env` は設定されていないものだけを埋める        |
| 空の環境変数    | 空と空白だけの環境変数は、設定されていない扱い。`.env` の値で埋める    |
| 同じ名前が 2 回 | **先に出てくるほう**が勝つ                                             |
| コメント        | `#` から行末まで。前に空白は要らない（`APP_PORT=8000#dev` は `8000`）  |
| `#` を含む値    | **必ず引用符で囲む。** 囲まないと `#` の前までが値になる。警告は出ない |
| `"..."`         | `\n` `\r` `\t` `\\` `\"` `\'` を解釈する。ほかの `\` はそのまま        |
| `'...'`         | すべてそのままの文字                                                   |
| 行頭の `export` | 無視する                                                               |
| 先頭の BOM      | 外して読む（メモ帳で保存しても読める）                                 |

- Windows のパスは `"..."` の中にそのまま書けます（`DB_DATABASE="C:\data\app.sqlite"`）。
- 読めない行は、起動時のログに行番号つきで警告が出ます。
  `=` が無い行と、名前に英数字と `_` 以外が入った行（`APP-PORT=8000`）です。

### `${VAR}` で他の値を使う

値の中の `${名前}` は、その変数の値に置き換わります（Laravel と同じ）。

| 決まり            | 内容                                                               |
|-------------------|--------------------------------------------------------------------|
| 置き換える所      | 引用符なしの値と `"..."` の中。`'...'` の中は置き換えない          |
| 探す順            | 本物の環境変数 → `.env` の**前の行**。後の行に書いた変数は使えない |
| 見つからないとき  | 空になる（警告は出ない）                                           |
| `$VAR`            | 波かっこが無いものは置き換えない                                   |
| 閉じていない `${` | そのまま残る                                                       |

既定値つきの `${VAR:-default}` と、`\$` での逃がしは使えません。

### テストのときの値

`#[bengara::test]` のテストは、次の値で動きます。

| 名前             | 値        |
|------------------|-----------|
| `APP_ENV`        | `testing` |
| `SESSION_DRIVER` | `memory`  |
| `CACHE_DRIVER`   | `memory`  |

- 強さは「本物の環境変数 > この表 > `.env`」です（Laravel の `phpunit.xml` と同じ）。
- `.env` に `APP_ENV=local` と書いても、テストは `testing` で走ります。
- くわしくは [testing.md](testing.md) にあります。

## 環境変数の一覧

「設定ファイル」の列は、その値を読む型です。
型がある項目は、設定ファイルを書けば読み方を変えられます。「—」はフレームワークが直接読みます。

### アプリ

| 名前        | 既定                    | 何に効くか                              | 設定ファイル |
|-------------|-------------------------|-----------------------------------------|--------------|
| `APP_NAME`  | `init` 時のパッケージ名 | アプリ名                                | `AppConfig`  |
| `APP_ENV`   | `production`            | 環境名                                  | `AppConfig`  |
| `APP_DEBUG` | `false`                 | エラーページの詳細表示と、ログの細かさ  | `AppConfig`  |
| `APP_URL`   | `http://localhost:8000` | `url()` が作る URL の頭                 | `AppConfig`  |
| `APP_KEY`   | （空）                  | 署名と暗号化の鍵（[APP_KEY](#app_key)） | `AppConfig`  |
| `RUST_LOG`  | （なし）                | ログの細かさ（[ログ](#ログ)）           | —            |

`APP_ENV=production` にすると、次の 2 つが変わります。

- 中身が消えるコマンド（`migrate:fresh` など）が `--force` を求めます（[artisan.md](artisan.md)）。
- セッションの Cookie に `Secure` が付きます。HTTPS でないと Cookie が送られません。

### 待ち受けと停止

| 名前                   | 既定        | 何に効くか                                                     |
|------------------------|-------------|----------------------------------------------------------------|
| `APP_HOST`             | `127.0.0.1` | 待ち受けるホスト                                               |
| `APP_PORT`             | `8000`      | 待ち受けるポート                                               |
| `APP_SHUTDOWN_TIMEOUT` | `30`        | 止めるときに待つ上限の秒数。**`0` は無制限**                   |
| `APP_MAX_BODY_BYTES`   | `2097152`   | 受け取る本文の上限（2 MiB）。ルートの `.max_body` のほうが優先 |
| `TRUSTED_PROXIES`      | （空）      | 信頼する前段のアドレス（[TRUSTED_PROXIES](#trusted_proxies)）  |

- 止め方は [deployment.md](deployment.md#止める) にあります。
- 本文の上限は [requests-and-responses.md](requests-and-responses.md) にあります。
  `0` や数でない値なら、警告を出して既定にします。

### 置き場所

| 名前               | 既定     | 何に効くか                          | 設定ファイル    |
|--------------------|----------|-------------------------------------|-----------------|
| `APP_BASE_PATH`    | （なし） | 基準ディレクトリ。**絶対パスのみ**  | —               |
| `APP_STORAGE_PATH` | （なし） | `storage/` の場所。**絶対パスのみ** | —               |
| `STORAGE_DISK`     | `local`  | 既定のファイル置き場                | `StorageConfig` |

- **`APP_BASE_PATH` は `.env` に書いても効きません。** `.env` の場所を決める値なので、本物の環境変数で渡します。
- `APP_STORAGE_PATH` は `.env` でも効きます。
- どちらも、相対パスなら起動しません。`APP_BASE_PATH` は、指した場所が無いときも起動しません。
- 基準ディレクトリの決め方は [deployment.md](deployment.md#基準ディレクトリの決め方) にあります。

### セッションとパスワード

| 名前                   | 既定     | 何に効くか                                | 設定ファイル  |
|------------------------|----------|-------------------------------------------|---------------|
| `SESSION_DRIVER`       | `file`   | セッションの置き場所（`file` / `memory`） | —             |
| `SESSION_LIFETIME`     | `120`    | 最後に使ってから消えるまでの分            | —             |
| `HASH_DRIVER`          | `pbkdf2` | パスワードの方式（`pbkdf2` / `argon2id`） | —             |
| `HASH_ITERATIONS`      | `240000` | PBKDF2 の繰り返し回数                     | `HashConfig`  |
| `ARGON_MEMORY`         | `65536`  | Argon2id のメモリ（KiB）                  | `ArgonConfig` |
| `ARGON_TIME`           | `4`      | Argon2id の繰り返し                       | `ArgonConfig` |
| `ARGON_THREADS`        | `1`      | Argon2id の並列の数                       | `ArgonConfig` |
| `HASH_MAX_CONCURRENCY` | CPU の数 | パスワードの計算を同時に走らせる数の上限  | —             |

- `SESSION_DRIVER` は大文字小文字を見ません。`array` は `memory` と同じです。
- 知らない名前は、警告を出して `file` にします。
- 本番で `memory` にすると、起動時に警告が出ます。起動し直すと全員がログアウトするためです。
- `SESSION_LIFETIME` は 1 分より短くなりません。最大 60 秒早く切れることがあります（[session.md](session.md)）。
- `HASH_DRIVER=argon2id` には機能フラグ `argon2` が要ります。
  フラグが無いときと知らない名前のときは、ログに出して `pbkdf2` にします。
- パスワードの項目は [authentication.md](authentication.md) にくわしくあります。

### データベース

| 名前                 | 既定                                                   | 何に効くか                                           |
|----------------------|--------------------------------------------------------|------------------------------------------------------|
| `DB_CONNECTION`      | `sqlite`                                               | 使う接続（`sqlite` / `mysql` / `mariadb` / `pgsql`） |
| `DB_DATABASE`        | SQLite は `database/database.sqlite`、ほかは `bengara` | SQLite はファイル、ほかはデータベース名              |
| `DB_HOST`            | `127.0.0.1`                                            | つなぎ先                                             |
| `DB_PORT`            | 3306（MySQL）/ 5432（PostgreSQL）                      | ポート。空と `0` は左の値                            |
| `DB_USERNAME`        | `root`（MySQL）/ `postgres`（PostgreSQL）              | 利用者名。空は左の値                                 |
| `DB_PASSWORD`        | （空）                                                 | パスワード                                           |
| `DB_URL`             | （空）                                                 | 接続文字列。上の 4 つより優先                        |
| `DB_MAX_CONNECTIONS` | `5`                                                    | 同時に張る接続の数の上限（どの接続も同じ値）         |
| `DB_TEST_DATABASE`   | SQLite は `:memory:`                                   | テストで使うデータベース                             |

- `DB_HOST` から `DB_PASSWORD` までは、MySQL と PostgreSQL のときだけ使います。
- **`DB_URL` が効くのは、`DB_CONNECTION` で選んだ接続だけです。**
- **MySQL と PostgreSQL では `DB_TEST_DATABASE` が必須です。** テストは表を全部消すので、無いとテストが始まりません。
- 選んだデータベースの機能フラグが要ります（[機能フラグ](#機能フラグ)）。

### キャッシュ・メール・言語

| 名前                  | 既定                     | 何に効くか                                 | 設定ファイル  |
|-----------------------|--------------------------|--------------------------------------------|---------------|
| `CACHE_DRIVER`        | `file`                   | キャッシュの置き場所（`file` / `memory`）  | `CacheConfig` |
| `MAIL_MAILER`         | （空なら `MAIL_DRIVER`） | メールの送り先（`log` / `array` / `smtp`） | `MailConfig`  |
| `MAIL_DRIVER`         | `log`                    | 古い名前。`MAIL_MAILER` が空のときだけ見る | `MailConfig`  |
| `MAIL_FROM_ADDRESS`   | （空なら `MAIL_FROM`）   | 差出人のアドレス                           | `MailConfig`  |
| `MAIL_FROM`           | `noreply@example.com`    | 古い名前。`MAIL_FROM_ADDRESS` が空のとき   | `MailConfig`  |
| `MAIL_FROM_NAME`      | `bengara`                | 差出人の名前                               | `MailConfig`  |
| `APP_LOCALE`          | `ja`                     | 画面の言語                                 | —             |
| `APP_FALLBACK_LOCALE` | `ja`                     | 鍵が無いときに見る言語                     | —             |

- `MAIL_FROM_ADDRESS` と `MAIL_FROM` の `null` は、空と同じです。
- `init` が作る `.env` では、`MAIL_FROM_NAME="${APP_NAME}"` です。
- SMTP の項目（`MAIL_HOST` など）は [mail.md](mail.md) にあります。

### WebSocket と配信

| 名前                        | 既定                          | 何に効くか                                          | 設定ファイル      |
|-----------------------------|-------------------------------|-----------------------------------------------------|-------------------|
| `BROADCAST_CONNECTION`      | （空なら `BROADCAST_DRIVER`） | 配信の送り方（`memory` / `database`）               | `BroadcastConfig` |
| `BROADCAST_DRIVER`          | `memory`                      | 古い名前。`BROADCAST_CONNECTION` が空のときだけ見る | `BroadcastConfig` |
| `WEBSOCKET_MAX_CONNECTIONS` | `10000`                       | 1 プロセスの同時接続の上限。`0` は既定と同じ        | —                 |

`database` のときの項目（`BROADCAST_POLL_MS` など）は [websocket.md](websocket.md) にあります。

## APP_KEY

署名と暗号化に使う鍵です。セッション・署名付き URL・暗号化（`encrypt` / `decrypt`）が使います。

```sh
cargo artisan key:generate
```

- **64 文字以上が必須です。** 空や短いと、鍵を使うリクエストがエラーになります。
- `key:generate` は 64 文字の鍵を作って `.env` に書きます。先に `.env` が要ります。
- 値があるときは書き換えません。作り直すときは `--force` を付けます。
- **作り直すと、全員がログアウトします。** 暗号化した値と、配った署名付き URL も無効になります。

## TRUSTED_PROXIES

前段にプロキシ（nginx など）を置くときに、そのアドレスを書きます。
回数の制限（`Throttle`）が、相手を見分けるのに使います（[middleware.md](middleware.md)）。

```ini
TRUSTED_PROXIES=10.0.0.1,10.0.0.2
```

| 書き方       | 動き                                           | 採るアドレス                                 |
|--------------|------------------------------------------------|----------------------------------------------|
| 空（既定）   | 転送元のヘッダーを見ない                       | つないできた相手                             |
| アドレスの列 | カンマ区切り。載っている相手のヘッダーだけ見る | **右端から見て、一覧に無い最初のアドレス**   |
| `*`          | 接続元を信頼する（Laravel の `'*'` と同じ）    | **右端から見て、接続元と違う最初のアドレス** |

- 見るヘッダーは `X-Forwarded-For` と `X-Real-IP` です。
- アドレスは 1 つずつ並べます。範囲の書き方（`10.0.0.0/8`）は使えません。
- `Request::ip()` は、この値に関係なく**つないできた相手**を返します。
- `*` でも左端は採りません。左端は送り手が好きに書けるためです。
- `*` で外すのは接続元の 1 段だけです。プロキシが 2 段以上なら、アドレスを全部並べます。
- **プロキシを置かないのに書くと、** 偽のヘッダーで回数の制限をすり抜けられます。

## ログ

ログは標準出力に出ます。細かさは `RUST_LOG` で決めます。

| `RUST_LOG`                            | 動き                                        |
|---------------------------------------|---------------------------------------------|
| 無い                                  | `APP_DEBUG` が真なら `debug`、偽なら `info` |
| `trace` `debug` `info` `warn` `error` | その細かさで出す                            |
| `off`                                 | 何も出さない                                |
| `info,sqlx=debug`                     | 流した SQL も出す（既定では出さない）       |
| `myapp=debug`                         | 自分のアプリだけ細かく出す                  |

- 下の 2 つの書き方には、機能フラグ `log-filter` が要ります（既定で入っています）。
- ログの細かさは、**環境変数**の `APP_DEBUG` で決まります。`config/app.rs` の `debug` を変えても変わりません。
  ログの準備を、`config/` を読む前に済ませるためです。

## 機能フラグ

使わない機能を実行ファイルに入れないための仕組みです。`Cargo.toml` に書きます。

```toml
[dependencies]
bengara = { version = "0.1", features = ["sqlite", "mail"] }
```

| フラグ       | 既定           | 入れると使えるもの                                              |
|--------------|----------------|-----------------------------------------------------------------|
| `sqlite`     | 入っていない   | SQLite（[database.md](database.md)）                            |
| `mysql`      | 入っていない   | MySQL と MariaDB                                                |
| `mariadb`    | 入っていない   | `mysql` と同じ                                                  |
| `postgres`   | 入っていない   | PostgreSQL                                                      |
| `mail`       | 入っていない   | SMTP でのメール送信（[mail.md](mail.md)）                       |
| `websocket`  | 入っていない   | WebSocket の受け口（[websocket.md](websocket.md)）              |
| `encryption` | 入っていない   | `encrypt` / `decrypt`（[authentication.md](authentication.md)） |
| `argon2`     | 入っていない   | パスワードの Argon2id（`HASH_DRIVER=argon2id`）                 |
| `view`       | 入っていない   | テンプレート（[views.md](views.md)）                            |
| `log-filter` | **入っている** | `RUST_LOG=myapp=debug` のような絞り込み                         |

- `init` で作ると `features = ["sqlite"]` が入ります。**別のデータベースなら書き換えてください。**
- **キューとマイグレーションにも、データベースのフラグが要ります。**
- 次はフラグなしで使えます: 認証・キャッシュ・ファイル・多言語・イベント・定期処理。
- メールは、送り先が `log` と `array` ならフラグなしで使えます。
- 配信（`Broadcast::publish`）はフラグなしで呼べます。受け口（`req.websocket()`）には `websocket` が要ります。
- `mysql` / `postgres` / `mail` のビルドには C コンパイラが要ります（通信の暗号化のため）。
- `Cargo.toml` に見える `database` と `tls` は内部用です。自分では書きません。

`log-filter` は外せます。外すと、`RUST_LOG` に書けるのは `trace` `debug` `info` `warn` `error` `off` の 1 語だけです。

```toml
[dependencies]
bengara = { version = "0.1", default-features = false, features = ["sqlite"] }
```

## パスを作る関数

| 関数                | 返すもの                                     |
|---------------------|----------------------------------------------|
| `base_path()`       | 基準ディレクトリ                             |
| `app_path(rel)`     | 基準ディレクトリからの相対パスを、絶対パスに |
| `public_path(rel)`  | `public/` の下のパス                         |
| `storage_path(rel)` | `storage/` の下のパス                        |

**`app_path()` は Laravel と意味が違います。** Laravel は `app/` を指しますが、bengara は基準ディレクトリを指します。

## 関連

- [directory-structure.md](directory-structure.md) — `config/` の置き方
- [deployment.md](deployment.md) — 基準ディレクトリと本番の `.env`
