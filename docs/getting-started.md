# 導入手順

空のプロジェクトから、ブラウザで画面が出るところまでの手順です。
うまくいかないときは、最後の「つまずいたとき」を見てください。

必要なものは Rust 1.85 以上です。

## 1. プロジェクトを作る

```sh
cargo new myapp && cd myapp
cargo add bengara --features sqlite
```

**crates.io にはまだ公開していません。** 公開までは、上の `cargo add` では取れません。
取得したソースを `Cargo.toml` の `[patch.crates-io]` で指します。

`--features` は使うデータベースで選びます。使わないなら付けません。

| 使うもの             | 付ける機能フラグ |
|----------------------|------------------|
| SQLite               | `sqlite`         |
| MySQL / MariaDB      | `mysql`          |
| PostgreSQL           | `postgres`       |
| SMTP でのメール送信  | `mail`           |

機能フラグの一覧は [configuration.md](configuration.md) にあります。

## 2. main.rs を仮の形にする

`cargo new` が作った `src/main.rs` の中身を、次の 1 行だけにします。

```rust
fn main() { bengara::run() }
```

この状態で使えるのは `init` と `--version` だけです。
ほかのコマンドは「先に init を実行してください」と案内します。

## 3. init を実行する

```sh
cargo run -- init
```

Laravel と同じ構成のディレクトリとファイルができます。
**何度実行しても壊れません。** 足りないものだけを作ります。

| すること                     | 中身                                                 |
|------------------------------|------------------------------------------------------|
| ディレクトリとファイルを作る | 構成は [directory-structure.md](directory-structure.md) |
| 直下に `main.rs` を置く      | 中身は `bengara::app!();` の 1 行                    |
| `src/main.rs` を消す         | `src/` が空になれば `src/` も消す                    |
| `.env` と `.env.example` を作る | `.env` は Unix では所有者だけが読める権限（0600） |
| `storage/` を作る            | Unix では所有者だけが使える権限（0700）              |
| `Cargo.toml` に項目を足す    | 下の表。すでにある行には触りません                   |

| `Cargo.toml` に足す項目                   | 何のため                                                 |
|-------------------------------------------|----------------------------------------------------------|
| `rust-version = "1.85"`                   | bengara が動く Rust の下限                               |
| `autobins = false`                        | 入口は直下の `main.rs` と `artisan.rs`。`src/` は使いません |
| `autotests = false`                       | `tests/` を cargo が別のテストとして拾わないようにする   |
| `default-run`                             | `cargo run` で動かす実行ファイル（下の注）               |
| `bengara`（`features = ["sqlite"]`）      | 依存がまだ無いときだけ                                   |
| `[build-dependencies]` の `bengara-build` | ファイルの自動検出                                       |
| `[profile.release]`                       | 実行ファイルを小さくする設定                             |
| `[[bin]]` を 2 つ                         | 本体（`main.rs`）と `artisan`（`artisan.rs`）            |

- 本体の実行ファイル名は、パッケージ名と違っていても動きます
  （`[[bin]] name = "server" path = "main.rs"`）。
- そのときは `default-run` を書きません。`cargo run` で動かす名前を自分で書いてください。
  `cargo artisan` はそのままで動きます。

### すでにあるファイルの扱い

| ファイル             | すること                                                         |
|----------------------|------------------------------------------------------------------|
| `.gitignore`         | 足りない行を足す（`/.env` など）                                 |
| `.cargo/config.toml` | `artisan` の別名が無ければ足す                                   |
| `build.rs`           | **書き換えません。** 足りない 1 行を案内します                   |
| `main.rs`            | `cargo new` の雛形か手順 2 の 1 行だけなら置き換える。ほかは案内だけ |
| それ以外             | 触りません                                                       |

- `build.rs` には `bengara_build::discover()` の呼び出しが必要です。
- **上の 4 つのどれかが読めないときは、何も書き換えずに止まります。**
  UTF-8 でないファイルが当たります。エラーに出たファイルを UTF-8 で保存し直してください。

## 4. APP_KEY を作る

```sh
cargo artisan key:generate
```

`init` が作る `.env` の `APP_KEY` は空です。これを埋めます。
空のままだと、セッション・署名付き URL・暗号化を使うリクエストがエラーになります
（[configuration.md](configuration.md)）。

## 5. 起動する

```sh
cargo artisan serve
```

→ http://127.0.0.1:8000

ファイルを保存すると、自動で作り直して起動し直します（[artisan.md](artisan.md)）。
ポートを変えるには、次のどちらかを使います。

```sh
cargo artisan serve --port 8124
```

```ini
# .env
APP_PORT=8124
```

## 6. データベースを用意する

表を作る手順（マイグレーション）を `database/migrations/` に置いて、流します。

```sh
cargo artisan migrate
```

- **表を使う機能は `migrate` が先です。** 認証（`users` 表）やキュー（`jobs` 表）は、
  流す前は動きません。
- 書き方は [migrations.md](migrations.md) にあります。
- データベースを使わないなら、この手順は飛ばして構いません。

## 7. テストを走らせる

```sh
cargo test
```

`init` が `tests/Feature/HomeTest.rs` を作っています。

```rust
#[bengara::test]
async fn 生存確認が応答する() {
    get("/up").await.assert_ok().assert_see("ok");
}
```

`#[bengara::test]` を付けると、`get` や `post` でリクエストを送れます。
書き方は [testing.md](testing.md) にあります。

## 8. リリースビルド

```sh
cargo build --release
```

実行ファイルが 2 つできます（本体と `artisan`）。本番に置くのは本体だけです。

| 本番に置くもの        | なぜ                                |
|-----------------------|-------------------------------------|
| 実行ファイル（本体）  | `public/` と設定は中に入っています  |
| `.env`                | 環境ごとの値                        |
| 書き込める `storage/` | 実行時の書き込み先                  |

手順は [deployment.md](deployment.md) にあります。

## つまずいたとき

### Rust 1.85 でデータベースを使うと依存が解決できない

依存を 1 つ古い版に固定してください。Rust 1.88 以上なら何もしなくて構いません。

```sh
cargo update idna_adapter --precise 1.2.0
```

### `cargo artisan` が「そんなコマンドはない」と言われる

`cargo artisan` は Cargo の別名（エイリアス）です。`init` が次のファイルを作ります。
**`.gitignore` に入れないでください。** 消えると使えなくなります。

```toml
# .cargo/config.toml
[alias]
artisan = ["run", "-q", "--bin", "artisan", "--"]
```

ファイルが無いときは `cargo run --bin artisan -- serve` で代用できます。

### `src/main.rs` が消えない

`init` が消すのは、中身が `cargo new` の雛形のままか、手順 2 の 1 行だけのときです。
自分で書いたコードが入っていると、消さずに案内だけを出します。
要るコードを `app/` などへ手で移してから、`src/` を消してください。

### `src/` に置いたファイルが見つからない

bengara のプロジェクトに `src/` はありません。入口は直下の `main.rs` と `artisan.rs` です。
**`src/` は自動検出の対象になりません**（[directory-structure.md](directory-structure.md)）。

### ポートが使われている

本体は起動できずに終わります。`serve` はその旨を表示して、見張りを続けます。
ポートを空けてからファイルを 1 つ保存すると、もう一度起動します。

### 実行ファイルを直接動かすと起動しない

| 動かし方                      | どこで実行するか                 |
|-------------------------------|----------------------------------|
| `cargo artisan` / `cargo run` | プロジェクトの中ならどこでも     |
| `./target/debug/myapp`        | **プロジェクトのルート**         |

実行ファイルは、`.env` か `storage/` がある場所を基準にします。
見つからなければ起動しません（[deployment.md](deployment.md)）。

## 次に読むもの

- [directory-structure.md](directory-structure.md) — どこに何を置くか
- [routing.md](routing.md) — ルートを足す
- [artisan.md](artisan.md) — コマンドの一覧と `serve` の動き
- [testing.md](testing.md) — テストを書く
- [deployment.md](deployment.md) — 本番に置く
