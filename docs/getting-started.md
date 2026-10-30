# 導入手順

空のプロジェクトから、ブラウザで画面が出るところまでの手順です。
つまずいたときは、最後の「つまずいたとき」を見てください。

必要なものは Rust 1.85 以上です。

## 1. プロジェクトを作る

```sh
cargo new myapp && cd myapp
cargo add bengara --features sqlite
```

データベースを使うなら機能フラグを付けます。SQLite なら `sqlite`、
MySQL / MariaDB なら `mysql`、PostgreSQL なら `postgres` です。
使わないなら `cargo add bengara` だけで構いません。
SMTP でメールを送るなら `mail` も足します（[mail.md](mail.md)）。

後から足すときは `Cargo.toml` を次の形にします。

```toml
bengara = { version = "0.1", features = ["sqlite"] }
```

## 2. main.rs を仮の形にする

`cargo new` が作った `src/main.rs` の中身を、次の 1 行だけにします。

```rust
fn main() { bengara::run() }
```

この状態では `init` と `--version` だけが使えます。
ほかのコマンドを叩くと「先に init を実行してください」と案内が出ます。

## 3. init を実行する

```sh
cargo run -- init
```

| すること                                | 中身                                                        |
|-----------------------------------------|-------------------------------------------------------------|
| ディレクトリとファイルを作る            | 入口の `main.rs` は `bengara::app!();` の 1 行              |
| `Cargo.toml` に項目を足す               | 下の表                                                      |
| `src/main.rs` を消す                    | `src/` が空になれば `src/` も消す                           |

| 足す項目            | 何のため                                                     |
|---------------------|--------------------------------------------------------------|
| `rust-version`      | bengara が動く Rust の下限（1.85）                           |
| `autobins = false`  | 入口は直下の `main.rs` と `artisan.rs`。`src/` は使いません  |
| `autotests = false` | `tests/` 直下の `.rs` を別のテストとして拾わせない（[testing.md](testing.md)） |
| `default-run`       | `cargo run` で動かす bin。該当する `[[bin]]` があるときだけ   |
| `[build-dependencies]` / `[profile.release]` | ビルドの設定                         |

何度実行しても壊れません。足りないものだけを作り足します。
`bengara` の依存がまだ無ければ、`features = ["sqlite"]` 付きで書き足します。

既にあるファイルは次のように扱います。

| ファイル             | すること                       |
|----------------------|--------------------------------|
| `.gitignore`         | 足りない行を足す               |
| `.cargo/config.toml` | `artisan` の別名が無ければ足す |
| `build.rs`           | **書き換えません**（案内だけ） |

**`build.rs` に `bengara_build::discover()` が無いと、`bengara::app!()` が
自動検出の結果を読めません。** 無いときは足す 1 行を案内します。

## 4. APP_KEY を作る

```sh
cargo artisan key:generate
```

`init` が作る `.env` の `APP_KEY` は空です。これを埋めます。
**64 文字以上が必須です。** 空のままだと、セッション・署名付き URL・暗号化を使う
最初のリクエストでエラーになります（[configuration.md](configuration.md)）。

## 5. 起動する

```sh
cargo artisan serve
```

→ http://127.0.0.1:8000

ポートを変えるには、次のどちらかを使います。

```sh
cargo artisan serve --port 8124
```

```ini
# .env
APP_PORT=8124
```

## 6. データベースを用意する

`init` が `config/database.rs` と `database/` を作っています。
表を作る手順を `database/migrations/` に置いて、流します。

```sh
cargo artisan migrate
```

**表を使う機能は `migrate` が先です。** 認証（`users` 表）やキュー（`jobs` 表）は、
流す前は動きません。書き方は [migrations.md](migrations.md) にあります。
データベースを使わないなら、この手順は飛ばして構いません。

## 7. テストを書く

```sh
cargo test
```

`#[bengara::test]` を付けると、テストの中で使える道具が入ります。

| 名前                                        | 中身                                      |
|---------------------------------------------|-------------------------------------------|
| `client`                                    | リクエストを送る口                        |
| `get` / `post` / `put` / `patch` / `delete` | その方法で 1 本送る                       |
| `refresh_database()`                        | DB を作り直す。戻り値は順番を守るための札 |
| `seed_database(&db)`                        | 初期データを入れる。札を渡す              |
| `exclusive()`                               | DB 以外の共通のもの（メール・言語）の札   |

```rust
#[bengara::test]
async fn 記事が作れる() {
    let db = refresh_database().await;
    seed_database(&db).await;
    post("/posts", "title=hello").await.assert_redirect("/posts");
}
```

くわしくは [testing.md](testing.md) にあります。

## 8. リリースビルド

```sh
cargo build --release
```

実行ファイルが 2 つできます（本体と `artisan`）。本番に置くのは本体だけです。
`public/` と設定はバイナリに入ります。本体の大きさは約 6.0 MB（`sqlite` 機能つき）です。

本番に置くものは 3 つです（[deployment.md](deployment.md)）。

| 置くもの              | なぜ                       |
|-----------------------|----------------------------|
| 実行ファイル（本体）  | `public/` と設定は中にある |
| `.env`                | 環境ごとの値               |
| 書き込める `storage/` | 実行時の書き込み先         |

`storage/` の下は `./myapp storage:init` が作ります。

## つまずいたとき

### Rust 1.85 でデータベースを使うと依存が解決できない

依存を 1 つ古い版に固定してください。1.88 以上の Rust なら何もしなくて構いません。

```sh
cargo update idna_adapter --precise 1.2.0
```

### `cargo artisan` が「そんなコマンドはない」と言われる

`cargo artisan` は Cargo のエイリアスです。`init` が次のファイルを作ります。
**`.gitignore` に入れないでください。** 消えると使えなくなります。
その場合は `cargo run --bin artisan -- serve` で代用できます。

```toml
# .cargo/config.toml
[alias]
artisan = ["run", "-q", "--bin", "artisan", "--"]
```

### `src/main.rs` が消えない

`init` が消すのは、中身が `cargo new` の雛形のままか、
`fn main() { bengara::run() }` だけのときです。
自分で書いたコードが入っていると、案内だけを出します。手で移してください。

### ポートが使われている

`serve` 中に本体が自分で終了した場合は、その旨が表示されます。
次にファイルを変更したときに起動し直します。

### どこで実行するか

| 動かし方                      | どこで実行するか                             |
|-------------------------------|----------------------------------------------|
| `cargo artisan` / `cargo run` | プロジェクトの中ならどこでも                 |
| `./target/debug/myapp`        | **プロジェクトのルート**                     |

基準ディレクトリの決め方は [deployment.md](deployment.md) にあります。
**決まらなければ起動しません。**

### `src/` に置いたファイルが見つからない

bengara のプロジェクトに `src/` はありません。入口は直下の `main.rs` と `artisan.rs` です。
**`src/` は自動検出の対象になりません**（[directory-structure.md](directory-structure.md)）。

## 次に読むもの

- [directory-structure.md](directory-structure.md) — どこに何を置くか
- [routing.md](routing.md) — ルートを足す
- [artisan.md](artisan.md) — `serve` の動き
- [testing.md](testing.md) — テストを書く
- [deployment.md](deployment.md) — 本番に置く
