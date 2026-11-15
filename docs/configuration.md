# 設定

設定ファイルの書き方と、`.env` に書ける項目の一覧です。
機能フラグ（`Cargo.toml` の `features`）の一覧もここにあります。

設定は Rust のコードです。`config/` 直下にファイルを置くと、起動時に自動で登録されます。
設定は実行ファイルに入るので、本番に設定ファイルを置く必要はありません。

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

| 決まり                                         | 守らないとどうなるか                 |
|------------------------------------------------|--------------------------------------|
| `config/` の**直下**に置く                     | 設定として登録されません             |
| ファイル名を**小文字**で始める                 | 設定として登録されません             |
| `pub fn config() -> 何かの型` を書く           | ビルドが通りません                   |
| 1 つの型は 1 つのファイルからだけ返す          | どちらか 1 つしか残りません          |

### 読み出す

型を指定して取り出します。

```rust
let app = config::<AppConfig>();          // 登録されていなければパニック
let app = try_config::<AppConfig>();      // 登録されていなければ None
```

- `config("app.name")` のような文字列での読み出しはありません。
- 取れるのは、`config/` のファイルが返した型だけです。
  `config/cache.rs` を置いていなければ、`config::<CacheConfig>()` はパニックします。

### 自分の設定を増やす

1. `config/<名前>.rs` を作る。
2. 好きな型を定義する。
3. `pub fn config() -> その型` を書く。

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

### フレームワークの設定を上書きする

次の型を返すファイルを置くと、フレームワークはその値を使います。
置かなければ、下の「環境変数の一覧」のとおりに環境変数から読みます。

| 返す型           | 何の設定か             | 詳しくは                               | `init` が作るか |
|------------------|------------------------|----------------------------------------|-----------------|
| `AppConfig`      | 基本設定               | このページ                             | 作る            |
| `DatabaseConfig` | データベースの接続     | [database.md](database.md)             | 作る            |
| `CacheConfig`    | キャッシュの置き場所   | [cache.md](cache.md)                   | 作らない        |
| `MailConfig`     | メールの送り先         | [mail.md](mail.md)                     | 作らない        |
| `StorageConfig`  | ファイルの置き場所     | [storage.md](storage.md)               | 作らない        |
| `HashConfig`     | パスワードの変換の回数 | [authentication.md](authentication.md) | 作らない        |

ファイル名は自由です。見分けるのは返す型です。

### 今の環境を調べる

```rust
if config::<AppConfig>().is_local() {
    // 手元の開発環境のときだけ
}
```

| 関数              | 真になる条件                     |
|-------------------|----------------------------------|
| `is_production()` | `APP_ENV` が `production` のとき |
| `is_local()`      | `APP_ENV` が `local` のとき      |
| `is_testing()`    | `APP_ENV` が `testing` のとき    |

- `staging` のような他の値は、`config::<AppConfig>().env` を自分で比べてください。
- `#[bengara::test]` のテストは、`APP_ENV` がどこにも無ければ `testing` で動きます。

## env() の使い方

`env(名前, 既定値)` で環境変数を読みます。返す型は**既定値の型**で決まります。

```rust
let port: u16 = env("APP_PORT", 8000u16);
let debug: bool = env("APP_DEBUG", false);
let name: String = env("APP_NAME", "myapp");
let dir: std::path::PathBuf = env("CACHE_DIR", "storage/cache");
```

数の既定値には型を付けて書きます（`8000u16`、`10u32`）。
型の無い数字は `i32` と読まれ、`u16` などに入れられずにコンパイルエラーになるためです。

| 読める型                    | 補足                                                                     |
|-----------------------------|--------------------------------------------------------------------------|
| `String`                    |                                                                          |
| `bool`                      | 真: `true` `1` `yes` `on`。偽: `false` `0` `no` `off` `null` `none`      |
| `std::path::PathBuf`        |                                                                          |
| 整数（`i8`〜`i128`、`u8`〜`u128`、`isize`、`usize`） |                                                 |
| 小数（`f32`、`f64`）        |                                                                          |

### 既定値になるとき

| 環境変数の値                  | `env()` が返すもの           |
|-------------------------------|------------------------------|
| 設定されていない              | 既定値                       |
| 空（`MAIL_FROM_NAME=`）       | 既定値                       |
| 空白だけ（`A="   "`）         | 既定値                       |
| 型に合わない（`APP_PORT=abc`）| 既定値。警告が出ます         |
| 前後に空白がある（`A=" x "`） | `x`（前後の空白は落ちます）  |

**`.env` の書き方では、値を空や空白にできません。**
空にしたいときは、読む側の既定値を空にしてください（`env("MAIL_FROM_NAME", "")`）。

## .env の書き方

```ini
APP_NAME=myapp
APP_ENV=local
APP_DEBUG=true

# コメント
DB_PASSWORD="p#ss"
```

| 決まり                   | 内容                                                                 |
|--------------------------|----------------------------------------------------------------------|
| 優先                     | 本物の環境変数が勝ちます。`.env` は設定されていないものだけを埋めます |
| 同じ名前が 2 回          | **先に出てくるほう**が勝ちます                                       |
| コメント                 | `#` から行末まで。前に空白は要りません（`APP_PORT=8000#dev` は `8000`） |
| `#` を含む値             | **必ず引用符で囲みます。** 囲まないと `#` より前までが値になります   |
| `"..."`                  | `\n` `\r` `\t` `\\` `\"` `\'` を解釈します。ほかの `\` はそのまま残ります |
| `'...'`                  | すべてそのままの文字です                                             |
| 行頭の `export`          | 無視します                                                           |
| 先頭の BOM               | 外して読みます（メモ帳で保存しても読めます）                         |

- Windows のパスは `"..."` の中にそのまま書けます（`DB_DATABASE="C:\data\app.sqlite"`）。
- `DB_PASSWORD=p#ss` の値は `p` になります。警告も出ません。`DB_PASSWORD="p#ss"` と書きます。

読めない行は飛ばします。起動時のログに、行番号と理由が警告で出ます。

| 警告が出る行                        | 例              |
|-------------------------------------|-----------------|
| `=` が無い                          | `APP_PORT 8000` |
| `=` の前に名前が無い                | `=8000`         |
| 名前に英数字と `_` 以外が入っている | `APP-PORT=8000` |
| 引用符が閉じていない                | `A="abc`        |

## 環境変数の一覧

「設定ファイル」の列は、その値を読む `config/` の型です。
型が書いてある項目は、設定ファイルを書けば別の読み方に変えられます。
「—」の項目は、フレームワークが環境変数を直接読みます。

### アプリ

| 名前        | 既定                    | 何に効くか                                         | 設定ファイル |
|-------------|-------------------------|----------------------------------------------------|--------------|
| `APP_NAME`  | `init` 時のパッケージ名 | アプリ名                                           | `AppConfig`  |
| `APP_ENV`   | `production`            | 環境名（下の注）                                   | `AppConfig`  |
| `APP_DEBUG` | `false`                 | エラーページの詳細表示と、ログの細かさ             | `AppConfig`  |
| `APP_URL`   | `http://localhost:8000` | `url()` が作る URL の頭                            | `AppConfig`  |
| `APP_KEY`   | （空）                  | 署名と暗号化に使う鍵（[APP_KEY](#app_key)）        | `AppConfig`  |
| `RUST_LOG`  | （なし）                | ログの細かさ（[ログ](#ログ)）                      | —            |

`APP_ENV=production` のときに変わる動きは 2 つです。

- 中身が消えるコマンド（`migrate:fresh` など）が `--force` を求めます（[artisan.md](artisan.md)）。
- セッションの Cookie に `Secure` が付きます。HTTPS でないと Cookie が送られません。

### 待ち受けと停止

| 名前                   | 既定        | 何に効くか                                        |
|------------------------|-------------|---------------------------------------------------|
| `APP_HOST`             | `127.0.0.1` | 待ち受けるホスト                                  |
| `APP_PORT`             | `8000`      | 待ち受けるポート                                  |
| `APP_SHUTDOWN_TIMEOUT` | `30`        | 止めるときに待つ上限の秒数。**`0` は無制限**（[deployment.md](deployment.md)） |
| `TRUSTED_PROXIES`      | （空）      | 信頼する前段のアドレス（[TRUSTED_PROXIES](#trusted_proxies)） |

どれもフレームワークが直接読みます。

### 置き場所

| 名前               | 既定     | 何に効くか                               | 設定ファイル    |
|--------------------|----------|------------------------------------------|-----------------|
| `APP_BASE_PATH`    | （なし） | 基準ディレクトリ。**絶対パスのみ**       | —               |
| `APP_STORAGE_PATH` | （なし） | `storage/` の場所。**絶対パスのみ**      | —               |
| `STORAGE_DISK`     | `local`  | 既定のファイル置き場                     | `StorageConfig` |

- **`APP_BASE_PATH` は `.env` に書いても効きません。** `.env` の場所を決める値だからです。
  本物の環境変数で渡してください。
- `APP_BASE_PATH` が相対パスのときと、指した場所が無いときは起動しません。
- `APP_STORAGE_PATH` は `.env` でも効きます。相対パスのときは起動しません。
- 基準ディレクトリの決め方は [deployment.md](deployment.md) にあります。

### セッションと認証

| 名前               | 既定     | 何に効くか                                | 設定ファイル |
|--------------------|----------|-------------------------------------------|--------------|
| `SESSION_DRIVER`   | `file`   | セッションの置き場所（`file` / `memory`） | —            |
| `SESSION_LIFETIME` | `120`    | セッションが消えるまでの分                | —            |
| `HASH_ITERATIONS`  | `120000` | パスワードの変換の繰り返し回数            | `HashConfig` |

- セッションの 2 つは `StartSession::from_env()` が読みます（[session.md](session.md)）。
- `SESSION_DRIVER` の `array` は `memory` の別名です。知らない名前は、警告を出して `file` にします。
- `SESSION_LIFETIME` は 1 分より短くなりません。`0` と書いても 1 分です。
- `HASH_ITERATIONS` は [authentication.md](authentication.md) で説明しています。

### データベース

| 名前                 | 既定                              | 何に効くか                                         |
|----------------------|-----------------------------------|----------------------------------------------------|
| `DB_CONNECTION`      | `sqlite`                          | 使う接続（`sqlite` / `mysql` / `mariadb` / `pgsql`） |
| `DB_DATABASE`        | SQLite は `database/database.sqlite`、ほかは `bengara` | SQLite はファイル、ほかはデータベース名 |
| `DB_HOST`            | `127.0.0.1`                       | つなぎ先                                           |
| `DB_PORT`            | 3306（MySQL）/ 5432（PostgreSQL） | ポート。空と `0` は左の値になります                |
| `DB_USERNAME`        | `root`（MySQL）/ `postgres`（PostgreSQL） | 利用者名。空は左の値になります             |
| `DB_PASSWORD`        | （空）                            | パスワード                                         |
| `DB_URL`             | （空）                            | 接続文字列。上の 4 つより優先します                |
| `DB_MAX_CONNECTIONS` | `5`                               | 同時に張る接続の数の上限                           |
| `DB_TEST_DATABASE`   | SQLite は `:memory:`              | テストで使うデータベース                           |

- `DB_TEST_DATABASE` 以外は `DatabaseConfig`（`init` が作る `config/database.rs`）が読みます。
- `DB_HOST` から `DB_PASSWORD` までは、MySQL と PostgreSQL のときだけ使います。
- **`DB_URL` が効くのは、`DB_CONNECTION` で選んだ接続だけです。**
- `DB_MAX_CONNECTIONS` は、全部の接続に同じ値が入ります。
- **MySQL と PostgreSQL では `DB_TEST_DATABASE` が必須です。**
  テストは表を全部消すので、書かないとテストが始まりません。
- `#` を含む接続文字列は引用符で囲みます（`DB_URL="postgres://u:p#ss@host/db"`）。
- 選んだデータベースの機能フラグが要ります（[機能フラグ](#機能フラグ)）。

### キャッシュ・メール・言語

| 名前                  | 既定                  | 何に効くか                                 | 設定ファイル  |
|-----------------------|-----------------------|--------------------------------------------|---------------|
| `CACHE_DRIVER`        | `file`                | キャッシュの置き場所（`file` / `memory`）  | `CacheConfig` |
| `MAIL_DRIVER`         | `log`                 | メールの送り先（`log` / `array` / `smtp`） | `MailConfig`  |
| `MAIL_FROM`           | `noreply@example.com` | 差出人                                     | `MailConfig`  |
| `MAIL_FROM_NAME`      | `bengara`             | 差出人の名前                               | `MailConfig`  |
| `MAIL_HOST`           | `127.0.0.1`           | SMTP サーバ                                | `MailConfig`  |
| `MAIL_PORT`           | 暗号化で決まる        | `tls` は 465、`starttls` は 587、`none` は 25 | `MailConfig` |
| `MAIL_ENCRYPTION`     | `starttls`            | `tls` / `starttls` / `none`                | `MailConfig`  |
| `MAIL_USERNAME`       | （空）                | SMTP の利用者名。空なら認証しません        | `MailConfig`  |
| `MAIL_PASSWORD`       | （空）                | SMTP のパスワード                          | `MailConfig`  |
| `MAIL_EHLO_NAME`      | （空）                | SMTP で名乗る名前。空なら `[127.0.0.1]`    | `MailConfig`  |
| `MAIL_TIMEOUT`        | `10`                  | 返事を待つ上限の秒数                       | `MailConfig`  |
| `APP_LOCALE`          | `ja`                  | 画面の言語                                 | —             |
| `APP_FALLBACK_LOCALE` | `ja`                  | 鍵が無いときに見る言語                     | —             |

- `MAIL_HOST` から下の `MAIL_` は、`MAIL_DRIVER=smtp` のときだけ使います。
- `init` が作る `.env` は、`MAIL_FROM_NAME` にパッケージ名を入れます。
- `[127.0.0.1]` と名乗ると断るサーバがあります。外へ送るときは、
  `MAIL_EHLO_NAME` に自分のホスト名（`mail.example.com` など）を書いてください。
- くわしくは [cache.md](cache.md) / [mail.md](mail.md) / [localization.md](localization.md) にあります。

## APP_KEY

署名と暗号化に使う鍵です。セッション・署名付き URL・暗号化（`encrypt` / `decrypt`）が使います。

```sh
cargo artisan key:generate
```

- **64 文字以上が必須です。** 空のときと短いときは、鍵を使うリクエストがエラーになります。
- `key:generate` は 64 文字の鍵を作って `.env` に書きます。先に `.env` が要ります。
- すでに値があるときは書き換えません。作り直すときは `--force` を付けます。
- **作り直すと、いまのセッション・暗号化した値・配った署名付き URL が無効になります。**
  全員がログアウトします。
- `.env` の権限（Unix）は、書き込んだ後も変わりません。読み取り専用のときは書かずにエラーにします。

## TRUSTED_PROXIES

前段にプロキシ（nginx など）を置くときに、そのアドレスを書きます。
回数の制限（`Throttle`）が、相手を見分けるのに使います（[middleware.md](middleware.md)）。

```ini
TRUSTED_PROXIES=10.0.0.1,10.0.0.2
```

| 書き方       | 動き                                                           |
|--------------|----------------------------------------------------------------|
| 空（既定）   | 転送元を表すヘッダーを見ません。つないできた相手で数えます     |
| アドレスの列 | カンマ区切り。載っている相手から来たヘッダーだけを見ます       |
| `*`          | すべての相手を信頼します                                       |

見るヘッダーは `X-Forwarded-For` と `X-Real-IP` です。

- **範囲の書き方（`10.0.0.0/8`）は使えません。** アドレスを 1 つずつ並べます。
- 読めない値は警告を出して無視します。
- IPv4 のアドレスはそのまま書きます（`10.0.0.1`）。`::ffff:10.0.0.1` の形で届いても合います。
- `Request::ip()` は、この値に関係なく**つないできた相手**を返します。

### どのアドレスを採るか

`X-Forwarded-For` は `送ってきた人, プロキシ1, プロキシ2` のように、左から右へ積まれます。
**左端は、送ってきた人が好きに書ける値です。**

| `TRUSTED_PROXIES` | 採るアドレス                               |
|-------------------|--------------------------------------------|
| アドレスの列      | **右端から見て、一覧に無い最初のアドレス** |
| `*`               | 左端                                       |

- アドレスとして読めない値は捨てます。1 つも読めなければ、つないできた相手で数えます。
- **`*` は、前段がヘッダーを必ず上書きする構成でだけ使ってください。**
  追記する構成で `*` にすると、左端を書き換えるだけで回数の制限をすり抜けられます。

## ログ

ログは標準出力に出ます。細かさは `RUST_LOG` で決めます。

| `RUST_LOG`                     | 動き                                              |
|--------------------------------|---------------------------------------------------|
| 無い                           | `APP_DEBUG` が真なら `debug`、偽なら `info`       |
| `trace` `debug` `info` `warn` `error` | その細かさで出す                           |
| `off`                          | 何も出さない                                      |
| `info,sqlx=debug`              | 流した SQL も出す（既定では出しません）           |
| `myapp=debug`                  | 自分のアプリだけ細かく出す                        |

- 下の 2 つの書き方には、機能フラグ `log-filter` が要ります（既定で入っています）。
- ログの細かさを決める `APP_DEBUG` は、環境変数を直接読みます。
  `config/app.rs` で `debug` を別の値にしても、ログの細かさは変わりません。

## 機能フラグ

使わない機能を実行ファイルに入れないための仕組みです。`Cargo.toml` に書きます。

```toml
[dependencies]
bengara = { version = "0.1", features = ["sqlite", "mail"] }
```

| フラグ       | 既定             | 入れると使えるもの                                     |
|--------------|------------------|--------------------------------------------------------|
| `sqlite`     | 入っていない     | SQLite（[database.md](database.md)）                   |
| `mysql`      | 入っていない     | MySQL と MariaDB                                       |
| `mariadb`    | 入っていない     | `mysql` と同じものです                                 |
| `postgres`   | 入っていない     | PostgreSQL                                             |
| `mail`       | 入っていない     | SMTP でのメール送信（[mail.md](mail.md)）              |
| `encryption` | 入っていない     | `encrypt` / `decrypt`（[authentication.md](authentication.md)） |
| `log-filter` | **入っている**   | `RUST_LOG=myapp=debug` のような絞り込み                |

- `init` で作ったプロジェクトには `features = ["sqlite"]` が入ります。
  **別のデータベースを使うなら書き換えてください。** 使わないなら消してください。
- **キューとマイグレーションにもデータベースのフラグが要ります**（[queue.md](queue.md)）。
- 認証・キャッシュ・ファイル・多言語・イベント・定期処理は、フラグなしで使えます。
- メールは、送り先が `log` と `array` ならフラグなしで使えます。
- `mysql` / `postgres` / `mail` は、通信の暗号化のためにビルドに C コンパイラが要ります。
- `Cargo.toml` に見える `database` と `tls` は内部用です。自分では書きません。

### log-filter を外す

依存を減らしたいときに外せます。

```toml
[dependencies]
bengara = { version = "0.1", default-features = false, features = ["sqlite"] }
```

外すと、`RUST_LOG` に書けるのは `trace` `debug` `info` `warn` `error` `off` の 1 語だけになります。

## パスを作る関数

| 関数                | 返すもの                                  |
|---------------------|-------------------------------------------|
| `base_path()`       | 基準ディレクトリ                          |
| `app_path(rel)`     | 基準ディレクトリからの相対パスを絶対パスに |
| `public_path(rel)`  | `public/` の下のパス                      |
| `storage_path(rel)` | `storage/` の下のパス                     |

`app_path()` は **Laravel と意味が違います。** Laravel は `app/` を指しますが、
bengara は基準ディレクトリを指します（[laravel-differences.md](laravel-differences.md)）。

## 関連

- [getting-started.md](getting-started.md) — 最初の `.env` を作るまで
- [directory-structure.md](directory-structure.md) — `config/` の置き方
- [deployment.md](deployment.md) — 基準ディレクトリと本番の `.env`
