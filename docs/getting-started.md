# 導入手順

空のプロジェクトから、ブラウザで画面が出るまでの手順です。
必要なものは Rust 1.85 以上です。

## 1. プロジェクトを作る

```sh
cargo new myapp && cd myapp
cargo add bengara --features sqlite
```

**crates.io には公開していません。** 取得したソースを `Cargo.toml` の `[patch.crates-io]` で指します。

`--features`（機能フラグ）は使うものに合わせて選びます。データベースを使わないなら付けません。

| 使うもの            | 機能フラグ  |
|---------------------|-------------|
| SQLite              | `sqlite`    |
| MySQL / MariaDB     | `mysql`     |
| PostgreSQL          | `postgres`  |
| SMTP でのメール送信 | `mail`      |
| WebSocket           | `websocket` |

一覧は [configuration.md](configuration.md#機能フラグ) にあります。

## 2. main.rs を仮の形にする

`src/main.rs` の中身を、次の 1 行だけにします。**`init` より先にやってください。**

```rust
fn main() { bengara::run() }
```

この状態で動くのは `init` と `--version` だけです。

## 3. init を実行する

```sh
cargo run -- init
```

Laravel と同じ構成のディレクトリとファイルができます。
**何度実行しても壊れません。** 足りないものだけを作ります。

| すること                        | 中身                                                    |
|---------------------------------|---------------------------------------------------------|
| ディレクトリとファイルを作る    | 構成は [directory-structure.md](directory-structure.md) |
| 直下に `main.rs` を置く         | 中身は `bengara::app!();` の 1 行                       |
| `src/main.rs` を消す            | `src/` が空になれば `src/` も消す                       |
| `.env` と `.env.example` を作る | `.env` は Unix では所有者だけが読める（0600）           |
| `storage/` を作る               | Unix では所有者だけが使える（0700）                     |
| `Cargo.toml` に足りない行を足す | すでにある行には触らない                                |

`Cargo.toml` に足すのは、入口の `[[bin]]` 2 つ（本体の `main.rs` と `artisan.rs`）、依存、
リリースビルドの設定などです。

### すでにあるファイルの扱い

| ファイル             | すること                                                                  |
|----------------------|---------------------------------------------------------------------------|
| `.gitignore`         | 足りない行を足す（`/.env`・`/.idea`・`/.vscode` など）                    |
| `.cargo/config.toml` | `artisan` の別名が無ければ足す                                            |
| `build.rs`           | **書き換えない。** 足りない 1 行（`bengara_build::discover()`）を案内する |
| `main.rs`            | `cargo new` の雛形か手順 2 の 1 行なら置き換える。ほかは案内だけ          |
| それ以外             | 触らない                                                                  |

**上の 4 つのどれかが読めないときは、何も書き換えずに止まります。**
多いのは UTF-8 でないファイルです。UTF-8 で保存し直してください。

## 4. APP_KEY を作る

```sh
cargo artisan key:generate
```

`init` が作る `.env` の `APP_KEY`（署名と暗号化の鍵）は空です。
空のままだと、セッションや署名付き URL を使うリクエストがエラーになります。

## 5. 起動する

```sh
cargo artisan serve
```

→ http://127.0.0.1:8000

ファイルを保存すると、自動でビルドし直して起動し直します（[artisan.md](artisan.md#serve-の動き)）。
ポートは `--port 8124` か、`.env` の `APP_PORT=8124` で変えられます。

## 6. データベースを用意する

表を作る手順（マイグレーション）を `database/migrations/` に置いて、流します。

```sh
cargo artisan migrate
```

- **表を使う機能は `migrate` が先です。** 認証（`users` 表）やキュー（`jobs` 表）は、流す前は動きません。
- 書き方は [migrations.md](migrations.md) にあります。

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

書き方は [testing.md](testing.md) にあります。

## 8. リリースビルド

```sh
cargo build --release
```

実行ファイルが 2 つできます（本体と `artisan`）。本番に置くのは本体だけです。
手順は [deployment.md](deployment.md) にあります。

## つまずいたとき

| 起きたこと                                   | 直し方                                                                                          |
|----------------------------------------------|-------------------------------------------------------------------------------------------------|
| Rust 1.85 でデータベースの依存が解決できない | `cargo update idna_adapter --precise 1.2.0` で 1 つ古い版に固定する。Rust 1.88 以上なら要らない |
| `cargo artisan` が「そんなコマンドはない」   | 下の「cargo artisan の別名」を見る                                                              |
| `src/main.rs` が消えない                     | 下の「src/main.rs が消えない」を見る                                                            |
| `src/` に置いたファイルが見つからない        | `src/` は読まない。入口は直下の `main.rs` と `artisan.rs`                                       |
| ポートが使われている                         | ポートを空けてから、ファイルを 1 つ保存する。`serve` がもう一度起動する                         |
| 実行ファイルを直接動かすと起動しない         | プロジェクトのルートで動かす（下）                                                              |

### cargo artisan の別名

`cargo artisan` は Cargo の別名です。`init` が次のファイルを作ります。
**このファイルを `.gitignore` に入れないでください。**

```toml
# .cargo/config.toml
[alias]
artisan = ["run", "-q", "--bin", "artisan", "--"]
```

ファイルが無いときは `cargo run --bin artisan -- serve` で代わりになります。

### src/main.rs が消えない

`init` が消すのは、中身が `cargo new` の雛形か手順 2 の 1 行のときだけです。

- 自分のコードが入っているときは、`app/` などへ手で移してから `src/` を消します。
- `Cargo.toml` の `[[bin]]` が `src/main.rs` を指しているときも消しません。その `[[bin]]` を直します。

### 実行ファイルを直接動かすとき

実行ファイルは、`.env` か `storage/` がある場所を基準にします。見つからなければ起動しません。

| 動かし方                      | どこで実行するか             |
|-------------------------------|------------------------------|
| `cargo artisan` / `cargo run` | プロジェクトの中ならどこでも |
| `./target/debug/myapp`        | **プロジェクトのルート**     |

決め方は [deployment.md](deployment.md#基準ディレクトリの決め方) にあります。

## 次に読むもの

- [directory-structure.md](directory-structure.md) — どこに何を置くか
- [routing.md](routing.md) — ルートを足す
- [artisan.md](artisan.md) — コマンドの一覧
