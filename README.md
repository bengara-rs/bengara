# bengara

Laravel と同じディレクトリ構成で Web アプリを書ける、Rust 製のフレームワークです。
`routes/web.rs`・`app/Http/Controllers/HomeController.rs`・`config/app.rs` といった見慣れた場所に
ファイルを置くと、そのまま動きます。`mod` の書き並べはありません。

`cargo build --release` で実行ファイルが 1 つできます。ランタイムも言語処理系も要りません。

> bengara は Laravel 公式とは無関係の、独立したプロジェクトです。Laravel のロゴや商標は使っていません。

## できること

| 分野       | 中身                                                                           |
|------------|--------------------------------------------------------------------------------|
| 入口       | ルーティング、ミドルウェア、入力の検査、リクエストとレスポンス                 |
| 画面の状態 | セッション、Cookie、CSRF、認証、認可、多言語                                   |
| データ     | SQLite / MySQL / MariaDB / PostgreSQL、マイグレーション、モデル、シーダー、ファクトリ |
| 裏の処理   | キャッシュ、ファイルの置き場所、キュー、定期処理、イベント、メール（SMTP / ログ） |
| 道具       | `cargo artisan` のコマンド、自分のコマンド、テスト、本番への配置               |

ページごとの案内は [docs/README.md](docs/README.md) にあります。
コマンドの一覧は [docs/artisan.md](docs/artisan.md) にあります。

## 5 分で動かす

```sh
cargo new myapp && cd myapp
cargo add bengara --features sqlite   # データベースを使わないなら --features は不要
# src/main.rs の中身を fn main() { bengara::run() } に書き換える
cargo run -- init
cargo artisan serve        # → http://127.0.0.1:8000
```

要点は、`init` の前に `src/main.rs` を書き換えておくことです。
`init` が足りないファイルを作り、`src/` を片付けます。
つまずきやすい点つきの手順は [docs/getting-started.md](docs/getting-started.md) にあります。

## 書き味

```rust
// routes/web.rs
use bengara::prelude::*;
use crate::app::http::controllers::HomeController;

pub fn routes() {
    Route::get("/", HomeController::index).name("home");
}
```

```rust
// app/Http/Controllers/HomeController.rs
use bengara::prelude::*;

pub struct HomeController;

impl HomeController {
    pub async fn index() -> Result<Response> {
        let app = config::<AppConfig>();
        html(format!("<h1>{} が動いています</h1>", app.name))
    }
}
```

`src/` はありません。入口はプロジェクト直下の `main.rs` と `artisan.rs` です。
置き場所の全体は [docs/directory-structure.md](docs/directory-structure.md) にあります。

## 本番に置くもの

| 置くもの              | なぜ                                   |
|-----------------------|----------------------------------------|
| 実行ファイル          | 本体。`public/` と設定は中に入っている |
| `.env`                | 環境ごとの値                           |
| 書き込める `storage/` | セッション・キャッシュ・ログの置き場所 |

`storage/` の下は `./myapp storage:init` が作ります。
実行ファイルの大きさは約 6.0 MB（`sqlite` 機能つき）です。`sqlite` 無しなら 2.1 MB 程度です。
`mysql` / `postgres` / `mail` を足すと、暗号化（rustls）が入るぶん大きくなります。
くわしくは [docs/deployment.md](docs/deployment.md) にあります。

## まだ無いもの

| 無いもの                   | いまの形                                    |
|----------------------------|---------------------------------------------|
| テンプレート（ビュー）     | ありません                                  |
| メールの添付ファイル       | ありません。本文（文字と HTML）だけです      |
| SQL Server / Oracle        | SQLite / MySQL / MariaDB / PostgreSQL です  |
| Redis                      | キャッシュはファイル、キューは DB です      |
| Argon2 / bcrypt            | パスワードの変換は PBKDF2 です              |
| Windows のサービス停止要求 | Ctrl+C とコンソールを閉じる操作には応えます |
| ファイルのアップロード     | ありません（multipart は未実装）            |
| macOS / Linux での確認     | **未確認です。** 確かめているのは Windows 10 だけです |

一覧は [docs/backlog.md](docs/backlog.md) にまとめてあります。

`make:*` コマンドは**作りません**。意図した判断です（[docs/decisions.md](docs/decisions.md)）。

## 必要なもの

- Rust 1.85 以上
- edition 2021

bengara 自身のコードは edition 2021 で書いています。
ただし依存クレートの hyper-util が edition 2024 を要求するので、実際の下限は 1.85 です。

## ライセンス

MIT OR Apache-2.0
