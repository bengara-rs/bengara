# bengara

bengara は、Laravel と同じディレクトリ構成・同じ書き味で Web アプリケーションを書ける Rust 製のフレームワークです。
`app/Http/Controllers/HomeController.rs`、`routes/web.rs`、`config/app.rs` といった見慣れた場所にファイルを置くと、そのまま動きます。
`cargo build --release` で実行ファイルが 1 つできるので、置けば動きます。

> bengara は Laravel 公式とは無関係の、独立したプロジェクトです。Laravel のロゴや商標は使っていません。

## 特徴

- **Laravel と同じ構成** — ディレクトリ名・ファイル名・書き味をそろえています。
- **シングルバイナリ** — リリースビルドの成果物は実行ファイル 1 つです。`public/` も設定も中に入ります。
  大きさは **約 6.3 MB**（`sqlite` 機能つき）、`sqlite` 無しなら 2.1 MB 程度です。
- **Cargo だけで完結** — `cargo artisan ...` で開発用コマンドが使えます。グローバルなツールのインストールは不要です。
- **環境に依存しない** — ランタイムも言語処理系も要りません。ビルドした実行ファイルを置くだけです。
- **いま書けるもの** — ルーティング、ミドルウェア、入力の検査、セッションと CSRF、
  データベース（SQLite）、マイグレーション、モデル、認証と認可、キャッシュ、ファイルの置き場所、
  キュー、定期処理、イベント、メール（ログのみ）、多言語、自分のコマンド、テスト、
  本番への配置（`storage:init`・停止の合図と上限時間・プロセス2つでの無停止更新）。

## 本番に置くもの

| 置くもの              | なぜ                                   |
|-----------------------|----------------------------------------|
| 実行ファイル          | 本体。`public/` と設定は中に入っている |
| `.env`                | 環境ごとの値                           |
| 書き込める `storage/` | セッション・キャッシュ・ログの置き場所 |

`storage/` の下は `./myapp storage:init` が作ります。
くわしくは [docs/deployment.md](docs/deployment.md) を見てください。

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
│   ├── Console/Commands/        自分のコマンド
│   ├── Http/Controllers/HomeController.rs
│   ├── Http/Middleware/         ミドルウェア
│   ├── Jobs/                    キューのジョブ
│   ├── Listeners/               イベントを聞く側
│   ├── Models/                  モデル
│   └── Policies/                誰に何を許すか
├── bootstrap/app.rs             アプリの組み立て
├── config/app.rs                設定
├── config/database.rs           データベースの設定
├── database/
│   ├── migrations/              表を作る手順
│   ├── seeders/DatabaseSeeder.rs 初期データ
│   └── factories/               テスト用のデータを作る関数
├── public/robots.txt            静的ファイル
├── resources/
│   ├── lang/ja.toml             言語ごとの文字
│   └── views/                   （まだ使いません）
├── routes/
│   ├── web.rs                   ルート定義
│   └── console.rs               定期処理
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

| コマンド                                          | 動き                                           |
|---------------------------------------------------|------------------------------------------------|
| `cargo artisan serve [--host H] [--port P]`       | ビルド → 起動 → 変更を見張って再ビルド・再起動 |
| `cargo artisan init`                              | Laravel と同じ構成のファイルを作る             |
| `cargo artisan list` （`help` / `--help` / `-h`） | ヘルプ。実行時のコマンドと自作のコマンドも出す |
| `cargo artisan --version`                         | bengara の版                                   |
| 上記以外                                          | そのまま本体へ渡す                             |

### 本体（`cargo run -- ...` / 本番は `./myapp ...`）

| コマンド                                                                                                  | 動き                                                         |
|-----------------------------------------------------------------------------------------------------------|--------------------------------------------------------------|
| 引数なし / `serve`                                                                                        | HTTP サーバーを起動（`--host` / `--port`）                   |
| `route:list`                                                                                              | 登録されているルートの表（METHOD / URI / NAME / MIDDLEWARE） |
| `key:generate`                                                                                            | `APP_KEY` を作って `.env` に書き込む                         |
| `session:gc`                                                                                              | 期限切れのセッションを消す                                   |
| `storage:init` / `about`                                                                                  | 書き込み先を作る / いまの設定と置き場所を出す                |
| `migrate` / `migrate:status` / `migrate:rollback` / `migrate:reset` / `migrate:refresh` / `migrate:fresh` | マイグレーション                                             |
| `db:seed` / `db:wipe`                                                                                     | 初期データ / 表を全部消す                                    |
| `cache:clear` / `cache:prune`                                                                             | キャッシュを消す                                             |
| `queue:work` / `queue:failed` / `queue:retry` / `queue:flush`                                             | キューのジョブ                                               |
| `schedule:run` / `schedule:list`                                                                          | 定期処理                                                     |
| `lang:list`                                                                                               | 読み込まれている言語の一覧                                   |
| `init`                                                                                                    | Laravel と同じ構成のファイルを作る                           |
| `--version` / `--help`                                                                                    | 版・ヘルプ                                                   |

`app/Console/Commands/*.rs` に置いたものも、同じ一覧に並びます。

`make:*` コマンドはありません（意図的な判断です。[docs/decisions.md](docs/decisions.md) を参照）。

## まだ無いもの

| 無いもの                   | いまの形                                             |
|----------------------------|------------------------------------------------------|
| テンプレート（ビュー）     | ありません                                           |
| メールの送信               | ログに書き出すだけ。SMTP はありません                |
| MySQL / PostgreSQL         | SQLite だけです                                      |
| Redis                      | キャッシュはファイル、キューは DB です               |
| Argon2 / bcrypt            | パスワードの変換は PBKDF2 です                       |
| Windows のサービス停止要求 | Ctrl+C とコンソールを閉じる操作には応えます          |
| macOS / Linux での確認     | 確かめているのは Windows 10 だけです                 |

一覧は [docs/backlog.md](docs/backlog.md) にまとめてあります。

## 必要なもの

- Rust 1.85 以上
- edition 2021

bengara 自身のコードは edition 2021 で書いていますが、依存クレートの hyper-util が edition 2024 を要求するため、実際に必要な
Rust は 1.85 以上です。

## ドキュメント

[docs/README.md](docs/README.md) が目次です。導入手順・ルーティング・設定・テストなどを置いています。

## ライセンス

MIT OR Apache-2.0
