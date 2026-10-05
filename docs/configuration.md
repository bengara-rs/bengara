# 設定

設定は Rust のコードです。`config/` 直下にファイルを置くと、起動時に自動で登録されます。
設定がバイナリに入るので、本番に設定ファイルを置く必要はありません。

## config/app.rs

```rust
use bengara::prelude::*;

pub fn config() -> AppConfig {
    AppConfig {
        name: env("APP_NAME", "myapp"),
        env: env("APP_ENV", "production"),
        debug: env("APP_DEBUG", false),
        url: env("APP_URL", "http://localhost:8000"),
    }
}
```

**`config/` 直下のファイルには `pub fn config() -> 何らかの型` を必ず定義します。** 無いとビルドが通りません。

## 読み出す

```rust
let app = config::<AppConfig>();          // 無ければパニック
let app = try_config::<AppConfig>();      // Option 相当。無ければ取れないだけ
```

型をキーにして取り出します。`config("app.name")` のような文字列キーは用意していません（[backlog.md](backlog.md)）。

### 今の環境を調べる

`AppConfig` には、`env` を見るだけの短い関数が 2 つあります。

```rust
if config::<AppConfig>().is_local() {
// 手元の開発環境のときだけ
}
```

| 関数              | true になる条件                  |
|-------------------|----------------------------------|
| `is_production()` | `APP_ENV` が `production` のとき |
| `is_local()`      | `APP_ENV` が `local` のとき      |

比較するのは **この 2 つの文字列だけ**です。`staging` のような他の値を使うときは、
`config::<AppConfig>().env` を自分で比べてください。

## env () の型

```rust
let port: u16 = env("APP_PORT", 8000);
let debug: bool = env("APP_DEBUG", false);
let name: String = env("APP_NAME", "myapp");
let dir: std::path::PathBuf = env("CACHE_DIR", "storage/cache");
```

| 対応する型                                          |
|-----------------------------------------------------|
| `String`                                            |
| `bool`                                              |
| `std::path::PathBuf`                                |
| 整数（`i8`〜`i128` `isize` / `u8`〜`u128` `usize`） |
| 浮動小数（`f32` `f64`）                             |

変換できない値が入っていた場合は、警告を出して既定値を使います。止まりません。

## .env の書き方

```ini
APP_NAME=myapp
APP_ENV=local
APP_DEBUG=true
APP_URL=http://localhost:8000

DB_CONNECTION=sqlite
DB_DATABASE=database/database.sqlite
DB_TEST_DATABASE=:memory:

APP_HOST=127.0.0.1
APP_PORT=8000

# 信頼する前段のプロキシ（カンマ区切り。`*` で全部）
# TRUSTED_PROXIES=10.0.0.1

# ログの細かさ（trace / debug / info / warn / error）
# RUST_LOG=info
```

- 実際の環境変数が優先されます。`.env` は「まだ設定されていないものだけ」を埋めます。
- `#` から行末まではコメントです。
- 行頭の `export ` は無視します。
- `"..."` は `\n` `\t` などを解釈します。`'...'` はそのままの文字です。

## APP_* の一覧

| 名前               | 既定                       | 用途                                                                         |
|--------------------|----------------------------|------------------------------------------------------------------------------|
| `APP_NAME`         | `init` 時のパッケージ名    | アプリ名。`AppConfig.name`                                                   |
| `APP_ENV`          | `production`               | 環境名。`AppConfig.env`                                                      |
| `APP_DEBUG`        | `false`                    | エラーページの詳細表示、ログの既定の細かさ                                   |
| `APP_URL`          | `http://localhost:8000`    | アプリの URL。`AppConfig.url` と `url()` の土台                              |
| `APP_KEY`          | （なし）                   | 署名と暗号化に使う鍵。**64 文字以上**。`cargo artisan key:generate` で作る   |
| `SESSION_DRIVER`   | `file`                     | セッションの置き場所（`file` / `memory`）                                    |
| `SESSION_LIFETIME` | `120`                      | セッションが消えるまでの分                                                   |
| `DB_CONNECTION`    | `sqlite`                   | 既定で使う接続の名前                                                         |
| `DB_DATABASE`      | `database/database.sqlite` | SQLite のファイル。相対パスはプロジェクト直下から                            |
| `DB_TEST_DATABASE` | `:memory:`                 | テストで使うデータベース                                                     |
| `HASH_ITERATIONS`  | `120000`                   | パスワードの変換の繰り返し回数（[authentication.md](authentication.md)）     |
| `APP_HOST`         | `127.0.0.1`                | サーバーが待ち受けるホスト                                                   |
| `APP_PORT`         | `8000`                     | サーバーが待ち受けるポート                                                   |
| `TRUSTED_PROXIES`  | （空）                     | 信頼する前段のアドレス。下の表を参照                                         |
| `APP_BASE_PATH`    | （なし）                   | 基準ディレクトリ。**絶対パスのみ**（[deployment.md](deployment.md)）         |
| `APP_STORAGE_PATH` | （なし）                   | `storage/` の場所。**絶対パスのみ**                                          |
| `APP_SHUTDOWN_TIMEOUT` | `30`                   | 停止の上限（秒）。**`0` は無制限**                                           |
| `RUST_LOG`         | （なし）                   | ログの細かさ。無ければ `APP_DEBUG` から決まります                            |
| `CACHE_DRIVER`     | `file`                     | キャッシュの置き場所（`file` / `memory`）（[cache.md](cache.md)）            |
| `STORAGE_DISK`     | `local`                    | 既定のファイル置き場（[storage.md](storage.md)）                             |
| `MAIL_DRIVER`      | `log`                      | メールの送り先（`log` / `array`）（[mail.md](mail.md)）                      |
| `MAIL_FROM`        | `noreply@example.com`      | 差出人                                                                       |
| `MAIL_FROM_NAME`   | `bengara`                  | 差出人の名前。既定はこの値。普通はアプリ名を書く（[mail.md](mail.md)）       |
| `APP_LOCALE`       | `ja`                       | 画面の言語（[localization.md](localization.md)）                             |
| `APP_FALLBACK_LOCALE` | `ja`                    | 鍵が無いときに見る言語                                                       |

`APP_NAME` `APP_ENV` `APP_DEBUG` `APP_URL` は `config/app.rs` が読んでいるだけです。読み方を変えるのは自由です。
`APP_HOST` `APP_PORT` `APP_BASE_PATH` `APP_STORAGE_PATH` `APP_SHUTDOWN_TIMEOUT`
`RUST_LOG` `SESSION_*` `DB_TEST_DATABASE` `TRUSTED_PROXIES` はフレームワークが直接読みます。
**`APP_BASE_PATH` は `.env` に書いても効きません。** `.env` の場所そのものを決める値なので、
読む前に必要になります。本物の環境変数で渡してください。
`APP_STORAGE_PATH` と `APP_SHUTDOWN_TIMEOUT` は `.env` でも効きます。
`DB_CONNECTION` と `DB_DATABASE` は `config/database.rs` が読んでいるだけです（[database.md](database.md)）。
`CACHE_DRIVER` `STORAGE_DISK` `MAIL_*` `APP_LOCALE` `APP_FALLBACK_LOCALE` は、
対応する `config/*.rs` が無ければフレームワークが直接読みます。
`config/cache.rs`・`config/filesystems.rs`・`config/mail.rs` を置けば、そちらが優先します。

## APP_KEY

署名と暗号化に使う鍵です。`cargo artisan key:generate` が 64 文字を作って `.env` に書きます。

```sh
cargo artisan key:generate
```

- **64 文字以上が必須です。** 短いと、セッション・署名付き URL・暗号化を使う
  **最初のリクエストでエラーになります**。
- 空のままでも同じです。これらを使うリクエストが失敗します。
- 鍵は用途ごとに作り分けます。`APP_KEY` をそのまま使いません。

| 用途           | 作り分けの名前         |
|----------------|------------------------|
| セッション     | `bengara:session`      |
| 署名付き URL   | `bengara:signed-url`   |
| 暗号化         | `bengara:encryption`   |

**鍵の作り方を変えたので、前の版で作ったセッション・暗号文・配布済みの署名付き URL は
無効になります。** `APP_KEY` を作り直したときと同じです。

くわしくは [session.md](session.md) と [authentication.md](authentication.md) にあります。

## TRUSTED_PROXIES

前段にプロキシを置くときに、そのアドレスを書きます。
`X-Forwarded-For` と `X-Real-IP` を見るかどうかがこれで決まります。

```ini
TRUSTED_PROXIES=10.0.0.1,10.0.0.2
```

| 書き方       | 動き                                                  |
|--------------|-------------------------------------------------------|
| 空（既定）   | どちらのヘッダーも見ない。つないできた相手だけを見る  |
| アドレスの列 | カンマ区切り。載っている相手からのヘッダーだけ見る    |
| `*`          | すべて信頼する                                        |

- **範囲の書き方（`10.0.0.0/8` のような CIDR）は読めません。** アドレスを 1 つずつ並べます。
- 読めない値は警告を出して無視します。止まりません。
- 効くのは回数の制限（`Throttle`）です（[middleware.md](middleware.md)）。
  `Request::ip()` は、この値に関係なく **つないできた相手**を返します。

## 機能フラグ

bengara は使わない機能をバイナリに入れないようにしています。今あるフラグは 3 つです。

| フラグ       | 既定             | 中身                                                              |
|--------------|------------------|-------------------------------------------------------------------|
| `log-filter` | 入っている       | `RUST_LOG=myapp=debug` のような細かい絞り込みを使えるようにします |
| `sqlite`     | **入っていない** | SQLite につながるようにします（[database.md](database.md)）       |
| `encryption` | **入っていない** | `encrypt` / `decrypt` が使えます（+14 クレート）                  |

認証（`Hash` / `Auth` / `authorize` / `PasswordReset`）は **フラグが要りません。**
依存クレートを増やさずに作っているためです（[authentication.md](authentication.md)）。

`log-filter` は、依存クレートを減らしたいときに外せます。外すと 3 つ減り
（`matchers` / `regex-automata` / `regex-syntax`）、`RUST_LOG` は
`trace` / `debug` / `info` / `warn` / `error` の 1 語だけになります。

```toml
[dependencies]
bengara = { version = "0.1", default-features = false }
```

`sqlite` を入れると **依存クレートが大きく増えます**（依存の木が 59 → 133）。
データベースを使わないアプリには入れないでください。 **キューも `sqlite` が要ります**
（[queue.md](queue.md)）。キャッシュ・ファイル・メール・多言語・イベント・定期処理は
フラグなしで使えます。
`cargo run -- init` で作ったプロジェクトには最初から入っています。

```toml
[dependencies]
bengara = { version = "0.1", features = ["sqlite"] }
```

## 自分の設定型を増やす

1. `config/<名前>.rs` を作る
2. 好きな型を定義する（`Send + Sync + 'static` であること）
3. `pub fn config() -> その型` を書く

```rust
// config/mail.rs
use bengara::prelude::*;

pub struct MailConfig {
    pub from: String,
    pub port: u16,
}

pub fn config() -> MailConfig {
    MailConfig {
        from: env("MAIL_FROM", "noreply@example.com"),
        port: env("MAIL_PORT", 587),
    }
}
```

```rust
let mail = config::< crate::config::mail::MailConfig>();
```

型ごとに 1 つだけ保管されます。同じ型を 2 つのファイルから返さないでください。

## パス

| 関数                | 返すもの                                                                             |
|---------------------|--------------------------------------------------------------------------------------|
| `base_path()`       | 基準ディレクトリ（決め方は [deployment.md](deployment.md)。**決まらなければ起動しません**） |
| `app_path(rel)`     | ルートからの相対パスを絶対パスにする                                                 |
| `public_path(rel)`  | `public/` 以下のパス                                                                 |
| `storage_path(rel)` | `storage/` 以下のパス                                                                |

`app_path()` は **Laravel と意味が違います**。Laravel は `app/` を指しますが、bengara は
基準ディレクトリからの相対パスです（[laravel-differences.md](laravel-differences.md)）。

## 関連

- [getting-started.md](getting-started.md)
- [directory-structure.md](directory-structure.md)
- [cache.md](cache.md) / [storage.md](storage.md) / [mail.md](mail.md) / [localization.md](localization.md) — 周辺機能の設定
