# bengara

Laravel と同じディレクトリ構成で、Web アプリを Rust で書けるフレームワークです。

- `routes/web.rs` や `app/Http/Controllers/` に置いたファイルが、そのまま動きます。
- `cargo build --release` で実行ファイルが 1 つできます。PHP のような実行環境は要りません。
- 必要なものは Rust 1.85 以上だけです。

> bengara は Laravel 公式とは無関係の、独立したプロジェクトです。

## できること

| 分野       | 中身                                                                             |
|------------|----------------------------------------------------------------------------------|
| 入口       | ルーティング、ミドルウェア、入力の検査、リクエストとレスポンス                   |
| 画面の状態 | セッション、Cookie、CSRF（なりすまし送信の防止）、認証、認可、多言語             |
| データ     | SQLite / MySQL / MariaDB / PostgreSQL、マイグレーション、モデル、シーダー        |
| 裏の処理   | キャッシュ、ファイルの置き場所、キュー、定期処理、イベント、メール、外部コマンド |
| 双方向     | WebSocket、チャンネルへの配信（機能フラグ `websocket`）                          |
| 道具       | `cargo artisan` のコマンド、自分のコマンド、テスト、本番への配置                 |

ページごとの案内は [docs/README.md](docs/README.md) にあります。

## 5 分で動かす

```sh
cargo new myapp && cd myapp
cargo add bengara --features sqlite
```

`src/main.rs` の中身を、次の 1 行だけにします。

```rust
fn main() { bengara::run() }
```

```sh
cargo run -- init          # 骨組みを作る
cargo artisan serve        # 起動する → http://127.0.0.1:8000
```

- **crates.io には公開していません。** 取得したソースを `Cargo.toml` の `[patch.crates-io]` で指します。
- データベースを使わないなら、`--features` は要りません。
- くわしい手順は [docs/getting-started.md](docs/getting-started.md) にあります。

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

`mod` を書き並べる必要はありません。`src/` もありません。
置き場所は [docs/directory-structure.md](docs/directory-structure.md) にあります。

## 本番に置くもの

置くのは、実行ファイル・`.env`・書き込める `storage/` の 3 つです。
`public/` と設定は実行ファイルに入っています。
手順は [docs/deployment.md](docs/deployment.md) にあります。

## 知っておくこと

- テンプレート（ビュー）はありません。`html(...)` に文字列を渡すか、JSON を返します。
- ファイルを生成する `make:*` コマンドはありません。ファイルは手で作ります。
- 手で動きを確かめているのは Windows 10 だけです。

まだ無い機能は [docs/backlog.md](docs/backlog.md) にあります。

## ライセンス

MIT OR Apache-2.0
