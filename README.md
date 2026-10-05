# bengara

bengara は、Laravel と同じディレクトリ構成・同じ書き味で Web アプリケーションを書ける Rust 製のフレームワークです。
`app/Http/Controllers/HomeController.rs`、`routes/web.rs`、`config/app.rs` といった見慣れた場所にファイルを置くと、そのまま動きます。
`cargo build --release` で実行ファイルが 1 つできるので、置けば動きます。

> bengara は Laravel 公式とは無関係の、独立したプロジェクトです。Laravel のロゴや商標は使っていません。

## 特徴

- **Laravel と同じ構成** — ディレクトリ名・ファイル名・書き味をそろえています。
- **シングルバイナリ** — リリースビルドの成果物は実行ファイル 1 つ。設定ファイルを本番に置かなくても動きます。
- **Cargo だけで完結** — `cargo artisan ...` で開発用コマンドが使えます。グローバルなツールのインストールは不要です。
- **環境に依存しない** — ランタイムも言語処理系も要りません。ビルドした実行ファイルを置くだけです。
- **いま書けるもの** — ルーティング、ミドルウェア、入力の検査、セッションと CSRF、
  データベース（SQLite）、マイグレーション、モデル、テスト。

## 5分で動かす

```sh
cargo new myapp && cd myapp
cargo add bengara --features sqlite   # データベースを使わないなら --features は不要
# main.rs を一時的に fn main() { bengara::run() } にする（init 前はこれだけ動く）
cargo run -- init
cargo artisan serve        # → http://127.0.0.1:8000
cargo test
cargo build --release      # 実行ファイル2つ（本体 と artisan）
```

`cargo new` が作る `src/main.rs` の中身を `fn main() { bengara::run() }` に書き換えてから `cargo run -- init` を実行します。
`init` が必要なファイルを作り、`src/` を片付けます。詳しくは [docs/getting-started.md](docs/getting-started.md) を参照してください。

## できるプロジェクトの構成

`src/` はありません。入口はプロジェクト直下の `main.rs` と `artisan.rs` です。

```
myapp/
├── .cargo/config.toml           cargo artisan のエイリアス
├── .env / .env.example
├── Cargo.toml
├── build.rs                     自動検出
├── main.rs                      bengara::app!();
├── artisan.rs                   fn main() { bengara::artisan() }
├── app/
│   ├── Http/Controllers/HomeController.rs
│   ├── Http/Middleware/         ミドルウェア
│   └── Models/                  モデル
├── bootstrap/app.rs             アプリの組み立て
├── config/app.rs                設定
├── config/database.rs           データベースの設定
├── database/
│   ├── migrations/              表を作る手順
│   ├── seeders/DatabaseSeeder.rs 初期データ
│   └── factories/               テスト用のデータを作る関数
├── public/robots.txt            静的ファイル
├── resources/views/             （まだ使いません）
├── routes/web.rs                ルート定義
├── storage/                     実行時の書き込み先
└── tests/
    ├── Feature/HomeTest.rs
    └── Unit/
```

## 最小のコード例

### routes/web.rs

```rust
use bengara::prelude::*;
use crate::app::http::controllers::HomeController;

pub fn routes() {
    Route::get("/", HomeController::index).name("home");
}
```

### app/Http/Controllers/HomeController.rs

```rust
use bengara::prelude::*;

pub struct HomeController;

impl HomeController {
    pub async fn index() -> Result<Response> {
        let app = config::<AppConfig>();
        html(format!("<h1>{} が動いています</h1>", app.name))
    }
}
```

### bootstrap/app.rs

```rust
use bengara::prelude::*;

pub fn app() -> Application {
    Application::configure()
        .with_routing(|r| r.web(crate::routes::web::routes).health("/up"))
        .create()
}
```

### config/app.rs

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

### tests/Feature/HomeTest.rs

```rust
#[bengara::test]
async fn トップページが表示される() {
    get("/").await.assert_ok().assert_see("myapp");
}
```

## コマンド一覧

### cargo artisan（開発用）

| コマンド | 動き |
|---|---|
| `cargo artisan serve [--host H] [--port P]` | ビルド → 起動 → 変更を見張って再ビルド・再起動 |
| `cargo artisan init` | Laravel と同じ構成のファイルを作る |
| `cargo artisan list` （`help` / `--help` / `-h`） | ヘルプ |
| `cargo artisan --version` | bengara の版 |
| 上記以外 | そのまま本体へ渡す |

### 本体（`cargo run -- ...` / 本番は `./myapp ...`）

| コマンド | 動き |
|---|---|
| 引数なし / `serve` | HTTP サーバーを起動（`--host` / `--port`） |
| `route:list` | 登録されているルートの表（METHOD / URI / NAME / MIDDLEWARE） |
| `key:generate` | `APP_KEY` を作って `.env` に書き込む |
| `session:gc` | 期限切れのセッションを消す |
| `migrate` / `migrate:status` / `migrate:rollback` / `migrate:reset` / `migrate:refresh` / `migrate:fresh` | マイグレーション |
| `db:seed` / `db:wipe` | 初期データ / 表を全部消す |
| `init` | Laravel と同じ構成のファイルを作る |
| `--version` / `--help` | 版・ヘルプ |

`make:*` コマンドはありません（意図的な判断です。[docs/decisions.md](docs/decisions.md) を参照）。

## まだ無いもの

テンプレート（ビュー）、認証、キャッシュ、キュー、メールはまだありません。
データベースは **SQLite だけ**です（MySQL と PostgreSQL は未実装）。
一覧は [docs/backlog.md](docs/backlog.md) にまとめてあります。

## 必要なもの

- Rust 1.85 以上
- edition 2021

bengara 自身のコードは edition 2021 で書いていますが、依存クレートの hyper-util が edition 2024 を要求するため、実際に必要な Rust は 1.85 以上です。

## ドキュメント

[docs/README.md](docs/README.md) が目次です。導入手順・ルーティング・設定・テストなどを置いています。

## ライセンス

MIT OR Apache-2.0
