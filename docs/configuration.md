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

| 関数 | true になる条件 |
|---|---|
| `is_production()` | `APP_ENV` が `production` のとき |
| `is_local()` | `APP_ENV` が `local` のとき |

比較するのは**この 2 つの文字列だけ**です。`staging` のような他の値を使うときは、
`config::<AppConfig>().env` を自分で比べてください。

## env() の型

```rust
let port: u16 = env("APP_PORT", 8000);
let debug: bool = env("APP_DEBUG", false);
let name: String = env("APP_NAME", "myapp");
let dir: std::path::PathBuf = env("CACHE_DIR", "storage/cache");
```

| 対応する型 |
|---|
| `String` |
| `bool` |
| `std::path::PathBuf` |
| 整数（`i8`〜`i128` `isize` / `u8`〜`u128` `usize`） |
| 浮動小数（`f32` `f64`） |

変換できない値が入っていた場合は、警告を出して既定値を使います。止まりません。

## .env の書き方

```ini
APP_NAME=myapp
APP_ENV=local
APP_DEBUG=true
APP_URL=http://localhost:8000

APP_HOST=127.0.0.1
APP_PORT=8000

# ログの細かさ（trace / debug / info / warn / error）
# RUST_LOG=info
```

- 実際の環境変数が優先されます。`.env` は「まだ設定されていないものだけ」を埋めます。
- `#` から行末まではコメントです。
- 行頭の `export ` は無視します。
- `"..."` は `\n` `\t` などを解釈します。`'...'` はそのままの文字です。

## APP_* の一覧

| 名前 | 既定 | 用途 |
|---|---|---|
| `APP_NAME` | `init` 時のパッケージ名 | アプリ名。`AppConfig.name` |
| `APP_ENV` | `production` | 環境名。`AppConfig.env` |
| `APP_DEBUG` | `false` | エラーページの詳細表示、ログの既定の細かさ |
| `APP_URL` | `http://localhost:8000` | アプリの URL。`AppConfig.url` と `url()` の土台 |
| `APP_KEY` | （なし） | セッションと署名付き URL の署名に使う鍵。`cargo artisan key:generate` で作る |
| `SESSION_DRIVER` | `file` | セッションの置き場所（`file` / `memory`） |
| `SESSION_LIFETIME` | `120` | セッションが消えるまでの分 |
| `APP_HOST` | `127.0.0.1` | サーバーが待ち受けるホスト |
| `APP_PORT` | `8000` | サーバーが待ち受けるポート |
| `APP_BASE_PATH` | （なし） | プロジェクトのルート。`serve` が起動時に渡します |
| `RUST_LOG` | （なし） | ログの細かさ。無ければ `APP_DEBUG` から決まります |

`APP_NAME` `APP_ENV` `APP_DEBUG` `APP_URL` は `config/app.rs` が読んでいるだけです。読み方を変えるのは自由です。
`APP_HOST` `APP_PORT` `APP_BASE_PATH` `RUST_LOG` `SESSION_*` はフレームワークが直接読みます。

**`APP_KEY` が空のままだと、セッションと署名付き URL を使うリクエストが 500 になります。**
`cargo artisan key:generate` で作ってください（[session.md](session.md)）。

## 機能フラグ

bengara は使わない機能をバイナリに入れないようにしています。今あるフラグは 1 つです。

| フラグ | 既定 | 中身 |
|---|---|---|
| `log-filter` | 入っている | `RUST_LOG=myapp=debug` のような細かい絞り込みを使えるようにします |

依存クレートを減らしたいときは外せます。外すと 3 つ減り（`matchers` / `regex-automata` / `regex-syntax`）、`RUST_LOG` は
`trace` / `debug` / `info` / `warn` / `error` の 1 語だけになります。

```toml
[dependencies]
bengara = { version = "0.1", default-features = false }
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
let mail = config::<crate::config::mail::MailConfig>();
```

型ごとに 1 つだけ保管されます。同じ型を 2 つのファイルから返さないでください。

## パス

| 関数 | 返すもの |
|---|---|
| `base_path()` | プロジェクトのルート（`APP_BASE_PATH` → `CARGO_MANIFEST_DIR` → カレント の順で決定） |
| `app_path(rel)` | ルートからの相対パスを絶対パスにする |
| `public_path(rel)` | `public/` 以下のパス |
| `storage_path(rel)` | `storage/` 以下のパス |

`app_path()` は **Laravel と意味が違います**。Laravel は `app/` を指しますが、bengara は
基準ディレクトリからの相対パスです（[laravel-differences.md](laravel-differences.md)）。

## 関連

- [getting-started.md](getting-started.md)
- [directory-structure.md](directory-structure.md)
