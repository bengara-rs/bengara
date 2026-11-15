# bengara

Laravel と同じディレクトリ構成で、Web アプリを Rust で書けるフレームワークです。

- `routes/web.rs` や `app/Http/Controllers/HomeController.rs` にファイルを置くと、そのまま動きます。
- `cargo build --release` で実行ファイルが 1 つできます。ランタイムも言語処理系も要りません。
- 必要なものは Rust 1.85 以上だけです。

> bengara は Laravel 公式とは無関係の、独立したプロジェクトです。Laravel のロゴや商標は使っていません。

## できること

| 分野       | 中身                                                                                  |
|------------|---------------------------------------------------------------------------------------|
| 入口       | ルーティング、ミドルウェア、入力の検査、リクエストとレスポンス                        |
| 画面の状態 | セッション、Cookie、CSRF、認証、認可、多言語                                          |
| データ     | SQLite / MySQL / MariaDB / PostgreSQL、マイグレーション、モデル、シーダー             |
| 裏の処理   | キャッシュ、ファイルの置き場所、キュー、定期処理、イベント、メール                    |
| 道具       | `cargo artisan` のコマンド、自分のコマンド、テスト、本番への配置                      |

ページごとの案内は [docs/README.md](docs/README.md) にあります。

## 5 分で動かす

1. プロジェクトを作る。

   ```sh
   cargo new myapp && cd myapp
   cargo add bengara --features sqlite
   ```

2. `src/main.rs` の中身を、次の 1 行だけにする。

   ```rust
   fn main() { bengara::run() }
   ```

3. 骨組みを作る。

   ```sh
   cargo run -- init
   ```

4. 起動する。→ http://127.0.0.1:8000

   ```sh
   cargo artisan serve
   ```

- **crates.io にはまだ公開していません。** 公開までは、手順 1 の `cargo add` では取れません。
  取得したソースを `Cargo.toml` の `[patch.crates-io]` で指します。
- データベースを使わないなら、手順 1 の `--features` は要りません。
- 手順 2 は `init` より先にやります。`init` が `src/` を片付け、入口を直下の `main.rs` に移します。
- つまずきやすい点つきの手順は [docs/getting-started.md](docs/getting-started.md) にあります。

## 書き味

```rust
// routes/web.rs
use crate::app::http::controllers::HomeController;
use bengara::prelude::*;

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

`mod` の書き並べはありません。`src/` もありません。
置き場所の全体は [docs/directory-structure.md](docs/directory-structure.md) にあります。

## 本番に置くもの

| 置くもの              | なぜ                                   |
|-----------------------|----------------------------------------|
| 実行ファイル          | 本体。`public/` と設定は中に入っている |
| `.env`                | 環境ごとの値                           |
| 書き込める `storage/` | セッション・キャッシュ・ログの置き場所 |

`storage/` の下は `./myapp storage:init` が作ります。
くわしくは [docs/deployment.md](docs/deployment.md) にあります。

## まだ無いもの

まだ無いものと分かっている制限は、[docs/backlog.md](docs/backlog.md) の 1 か所にまとめてあります。
使うときに影響する 3 つだけ、先に書いておきます。

- テンプレート（ビュー）はありません。`html(...)` に文字列を渡すか、JSON を返します。
- macOS / Linux では**未確認です**。手で確かめているのは Windows 10 だけです。
- `make:*` コマンドは**作りません**。意図した判断です
  （[docs/decisions.md](docs/decisions.md) の判断 2）。

## 必要な Rust の版

Rust 1.85 以上です。
bengara 自身のコードは edition 2021 ですが、依存クレートの hyper-util が edition 2024 を要求します。

## ライセンス

MIT OR Apache-2.0
