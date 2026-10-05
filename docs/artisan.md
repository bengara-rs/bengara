# artisan

`cargo artisan` は開発用コマンドの入口です。グローバルなインストールは要りません。

## 仕組み

3 段になっています。

| 段                    | 何か                                                                                                          |
|-----------------------|---------------------------------------------------------------------------------------------------------------|
| 1. Cargo のエイリアス | `.cargo/config.toml` の `artisan = ["run", "-q", "--bin", "artisan", "--"]`                                   |
| 2. `artisan` バイナリ | `artisan.rs`（中身は `fn main() { bengara::artisan() }`）。`serve` `init` `list` `--version` を自分で処理する |
| 3. 本体への委譲       | 知らないコマンドは `cargo run -q --manifest-path <root>/Cargo.toml -- ...` で本体に渡す                       |

つまり `cargo artisan route:list` は、`artisan` が本体を呼び直して実行しています。

## cargo artisan のコマンド

| コマンド                           | 動き                                           |
|------------------------------------|------------------------------------------------|
| `serve [--host H] [--port P]`      | ビルド → 起動 → 変更を見張って再ビルド・再起動 |
| `init`                             | Laravel と同じ構成のファイルを作る             |
| `list`（`help` / `--help` / `-h`） | ヘルプ                                         |
| `--version`                        | bengara の版                                   |
| 上記以外                           | そのまま本体へ渡す                             |

`make:*` コマンドはありません。意図的な判断です（[decisions.md](decisions.md)）。

## 本体のコマンド

`cargo run -- ...`、本番では `./myapp ...` で実行します。

| コマンド               | 動き                                                               |
|------------------------|--------------------------------------------------------------------|
| 引数なし / `serve`     | HTTP サーバーを起動。`--host` / `--port`（`--port=8080` の形も可） |
| `route:list`           | 登録されているルートの表（METHOD / URI / NAME / MIDDLEWARE）       |
| `key:generate`         | `APP_KEY` を作って `.env` に書き込む。`--force` で上書き           |
| `session:gc`           | 期限切れのセッションのファイルを消す                               |
| `init`                 | Laravel と同じ構成のファイルを作る                                 |
| `--version` / `--help` | 版・ヘルプ                                                         |

データベースのコマンド（`features = ["sqlite"]` のとき）。詳しくは
[migrations.md](migrations.md) にあります。

| コマンド           | 動き                                                            |
|--------------------|-----------------------------------------------------------------|
| `migrate`          | まだ流していないマイグレーションを流す（`--seed` でシーダーも） |
| `migrate:status`   | 流したかどうかを一覧にする                                      |
| `migrate:rollback` | 最後のバッチを巻き戻す（`--step=2` で2バッチ分）                |
| `migrate:reset`    | 全部巻き戻す                                                    |
| `migrate:refresh`  | 全部巻き戻してから流し直す                                      |
| `migrate:fresh`    | 表を全部消してから流し直す（`--force` が要る場合あり）          |
| `db:seed`          | シーダーを流す（`--class=DatabaseSeeder`）                      |
| `db:wipe`          | 表を全部消す                                                    |

どれにも `--database=接続の名前` を付けられます。

ホストとポートの既定は `.env` の `APP_HOST`（既定 `127.0.0.1`）と `APP_PORT`（既定 `8000`）です。

`init` 前の `main.rs`（`fn main() { bengara::run() }`）は `init` と `--version` だけを受け付けます。
それ以外は「先に init を実行してください」と案内します。

## serve の動き

1. `cargo build --bin <pkg>` を実行し、JSON 出力から実行ファイルのパスを拾う。
2. `storage/framework/serve/<pkg>-<連番>` へコピーしてから起動する。
   （Windows は動いている実行ファイルを上書きできないため）
   起動時に `APP_BASE_PATH` を渡します。
3. 変更を見張る。
4. 変更があれば再ビルドして再起動する。

### 見張る対象

| 種類         | 対象                                                                |
|--------------|---------------------------------------------------------------------|
| ディレクトリ | `app` `bootstrap` `config` `database` `public` `resources` `routes` |
| ファイル     | `.env` `Cargo.toml` `build.rs` `main.rs` `artisan.rs`               |
| 除外         | 隠しファイル、`target`                                              |

### 見張り方

- 更新時刻を 0.4 秒ごとに確認します。
- 変更が続いている間は 0.25 秒待って、まとめて 1 回ビルドします。
- OS ごとの通知の仕組みに頼らないので、依存クレートが増えません。

### 失敗したとき

- ビルドに失敗したら、直前に動いていた版をそのまま動かし続け、エラーを表示します。
- 本体が自分で終了した場合（ポートが使用中など）はその旨を表示し、次の変更で起動し直します。

### 既知の割り切り

Ctrl+C で `artisan` が止まるとき、まれに本体が残ることがあります。
通常は同じコンソールに Ctrl+C が届くので一緒に終わります。

## 終了のしかた

本体は Ctrl+C と（Unix では）SIGTERM を受けたら、処理中のリクエストを終えてから止まります。

## 関連

- [getting-started.md](getting-started.md)
- [routing.md](routing.md)
