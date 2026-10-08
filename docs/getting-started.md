# 導入手順

空のプロジェクトから、ブラウザで画面が出るところまでの手順です。
つまずきやすい点は、その手順の中に書いてあります。

## 先に知っておくこと

必要なものは Rust 1.85 以上です（edition 2021）。

bengara 自身のコードは edition 2021 で書いています。
ただし依存クレートの hyper-util が edition 2024 を要求します。
そのため Rust 1.82 では依存の解決に失敗し、実際の下限は 1.85 になります。

> **Rust 1.85 でデータベース（`features = ["sqlite"]`）を使うとき**は、依存を 1 つ古い版に固定してください。
> sqlx がたどる `icu_*` が、新しい版では Rust 1.88 以上を要求します。
>
> ```sh
> cargo update idna_adapter --precise 1.2.0
> ```
>
> 1.88 以上の Rust を使うなら、何もしなくて構いません。

## 1. プロジェクトを作る

```sh
cargo new myapp && cd myapp
cargo add bengara --features sqlite
```

**データベースを使うなら `--features sqlite` を忘れないでください。**
後から足すときは `Cargo.toml` を次の形にします。

```toml
bengara = { version = "0.1", features = ["sqlite"] }
```

使わないなら `cargo add bengara` だけで構いません。依存が 74 個少なくなります。

## 2. main.rs を仮の形にする

`cargo new` が作った `src/main.rs` の中身を、次の 1 行だけにします。

```rust
fn main() { bengara::run() }
```

**これを先にやってください。** この状態では `init` と `--version` だけが使えます。
ほかのコマンドを叩くと「先に init を実行してください」と案内が出ます。

`cargo new` が作ったままの `println!` だけの `main` でも `init` は動きます。
ただし `bengara::run()` にしておくと案内が読めるので確実です。

## 3. init を実行する

```sh
cargo run -- init
```

`init` がすることは 3 つです。

- 足りないディレクトリとファイルを作る（既にあるファイルには触りません）
- `Cargo.toml` に `[[bin]]`、`default-run`、`autobins = false`、`[build-dependencies]`、`[profile.release]` を足す
  （`bengara` の依存がまだ無ければ、`features = ["sqlite"]` 付きで書き足します）
- `src/main.rs` を消し、`src/` が空になれば `src/` も消す

何度実行しても壊れません。足りないものだけを作り足します。

### 既にあるファイルへの例外は 3 つ

| ファイル             | すること                       |
|----------------------|--------------------------------|
| `.gitignore`         | 足りない行を足す               |
| `.cargo/config.toml` | `artisan` の別名が無ければ足す |
| `build.rs`           | **書き換えません**（案内だけ） |

- `.gitignore`: `cargo new` は `/target` の 1 行だけの `.gitignore` を必ず作ります。
  そのまま残すと `/.env` が入らず、`APP_KEY` 入りの `.env` が git に入ってしまいます。
- `.cargo/config.toml`: リンカの指定などで自前のものを持っている人は多いです。
  そのまま残すと `cargo artisan` が「そんなサブコマンドは無い」で終わります。
- `build.rs`: 自前の `build.rs` を壊さないよう、書き換えません。
  `bengara_build::discover()` の呼び出しが無いときだけ、足す 1 行を案内します。
  **無いままだと `bengara::app!()` が自動検出の結果を読めません。**

### src/main.rs が消えないとき

`init` が `src/main.rs` を消すのは、中身が次のどちらかのときです。

- `cargo new` の雛形のまま
- `fn main() { bengara::run() }` だけ

自分で書いたコードが入っていると消さず、案内だけを出します。その場合は手で移してください。

## 4. APP_KEY を作る

```sh
cargo artisan key:generate
```

`init` が作る `.env` の `APP_KEY` は**空**です。これを埋めるのが `key:generate` です。

**`APP_KEY` は 64 文字以上が必須です。** 空のままだと、セッション・署名付き URL・暗号化を使う
最初のリクエストでエラーになります。ここで作っておくのが確実です。
くわしくは [configuration.md](configuration.md) の APP_KEY にあります。

## 5. 起動する

```sh
cargo artisan serve
```

→ http://127.0.0.1:8000

### ポートが使われているとき

既定は `.env` の `APP_PORT`（既定 8000）です。変えるには次のどちらかを使います。

```sh
cargo artisan serve --port 8124
```

```ini
# .env
APP_PORT=8124
```

`serve` 中に本体が自分で終了した場合（ポートが使用中など）は、その旨が表示されます。
次にファイルを変更したときに起動し直します。

### cargo artisan が「そんなコマンドはない」と言われるとき

`cargo artisan` は Cargo のエイリアスです。`init` が次のファイルを作ります。

```toml
# .cargo/config.toml
[alias]
artisan = ["run", "-q", "--bin", "artisan", "--"]
```

**このファイルを `.gitignore` に入れないでください。** 消えると使えなくなります。
その場合は `cargo run --bin artisan -- serve` で代用できます。

### プロジェクトのルートで実行する

`cargo artisan` と `cargo run` は、`Cargo.toml` がある場所で実行してください。
`cargo` 経由ならルートは `CARGO_MANIFEST_DIR` から決まるので、サブディレクトリからでも
正しい場所を見ます。`cargo` を通さずに `./target/debug/myapp` と打つときは、ルートで実行してください。

ルートの決め方（全 5 段階）と、決まらないときの動きは [deployment.md](deployment.md) にあります。
**決まらなければ起動しません。**

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

実行ファイルが 2 つできます（本体と `artisan`）。本番に置くのは本体だけで足ります。
`public/` と設定はバイナリに入ります。本体の大きさは**約 6.0 MB**（`sqlite` 機能つき）です。

本番に置くものは 3 つです（[deployment.md](deployment.md)）。

| 置くもの              | なぜ                       |
|-----------------------|----------------------------|
| 実行ファイル（本体）  | `public/` と設定は中にある |
| `.env`                | 環境ごとの値               |
| 書き込める `storage/` | 実行時の書き込み先         |

`storage/` の下は `./myapp storage:init` が作ります。作るのは 7 つです。

```
storage/app  storage/app/public  storage/framework
storage/framework/cache  storage/framework/schedule  storage/framework/sessions
storage/logs
```

## src/ を作り直さない

bengara のプロジェクトに `src/` はありません。入口は直下の `main.rs` と `artisan.rs` です。
**`src/` にファイルを置いても自動検出の対象になりません。**

## 次に読むもの

- [directory-structure.md](directory-structure.md) — どこに何を置くか
- [routing.md](routing.md) — ルートを足す
- [artisan.md](artisan.md) — `serve` の動き
- [testing.md](testing.md) — テストを書く
- [deployment.md](deployment.md) — 本番に置く（**実行ファイル・`.env`・書き込める `storage/`**）
