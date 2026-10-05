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
| `list`（`help` / `--help` / `-h`） | **全部のコマンドの一覧**                       |
| `--version`                        | bengara の版                                   |
| 上記以外                           | そのまま本体へ渡す                             |

**`list` は 1 画面に全部を出します。** 開発用のコマンドを出したあと、本体を呼び直して
`migrate` / `cache:clear` と自作コマンドまで続けて出します。Laravel の `artisan list` と同じです。

アプリがビルドできないときは、開発用のコマンドだけを出して終わります。
アプリが壊れていても `artisan` が動くようにするためです。

`make:*` コマンドはありません。意図的な判断です（[decisions.md](decisions.md)）。

## 本体のコマンド

`cargo run -- ...`、本番では `./myapp ...` で実行します。

| コマンド               | 動き                                                               |
|------------------------|--------------------------------------------------------------------|
| 引数なし / `serve`     | HTTP サーバーを起動。`--host` / `--port`（`--port=8080` の形も可） |
| `route:list`           | 登録されているルートの表（METHOD / URI / NAME / MIDDLEWARE）       |
| `key:generate`         | `APP_KEY` を作って `.env` に書き込む。`--force` で上書き           |
| `session:gc`           | 期限切れのセッションのファイルを消す                               |
| `storage:init`         | `storage/` の下の書き込み先を作る（デプロイ時に1回）               |
| `about`                | いまの設定と置き場所を出す。**`APP_KEY` の値は出しません**         |
| `init`                 | Laravel と同じ構成のファイルを作る                                 |
| `--version` / `--help` | 版・ヘルプ                                                         |

`storage:init` と `about` は [deployment.md](deployment.md) で詳しく説明しています。

データベースのコマンド（`features = ["sqlite"]` のとき）。詳しくは
[migrations.md](migrations.md) にあります。

| コマンド           | 動き                                                            |
|--------------------|-----------------------------------------------------------------|
| `migrate`          | まだ流していないマイグレーションを流す（`--seed` でシーダーも） |
| `migrate:status`   | 流したかどうかを一覧にする                                      |
| `migrate:rollback` | 最後のバッチを巻き戻す（`--step=2` で2バッチ分）                |
| `migrate:reset`    | 全部巻き戻す                                                    |
| `migrate:refresh`  | 全部巻き戻してから流し直す（`--seed` でシーダーも）             |
| `migrate:fresh`    | 表を全部消してから流し直す（`--seed` でシーダーも）             |
| `db:seed`          | シーダーを流す（`--class=DatabaseSeeder` で1本だけ）            |
| `db:wipe`          | 表を全部消す                                                    |
| `migrate:unlock`   | 途中で落ちて残った実行中の札を外す                              |

どれにも `--database=接続の名前` を付けられます。
`APP_ENV=production` のとき、表を消すコマンド（`migrate:fresh` / `db:wipe`）は
`--force` を付けないと止まります。

**表を変えるコマンドは同時に流せません。** 2つ目は「実行中です」で止まります
（[deployment.md](deployment.md)）。`migrate:status` は読むだけなので、いつでも流せます。

周辺機能のコマンド。

| コマンド        | 動き                           | 詳しくは                           |
|-----------------|--------------------------------|------------------------------------|
| `cache:clear`   | キャッシュを全部消す           | [cache.md](cache.md)               |
| `cache:prune`   | 期限切れのキャッシュだけ消す   | [cache.md](cache.md)               |
| `queue:work`    | ジョブを処理する               | [queue.md](queue.md)               |
| `queue:failed`  | 諦めたジョブの一覧             | [queue.md](queue.md)               |
| `queue:retry`   | 諦めたジョブをキューへ戻す     | [queue.md](queue.md)               |
| `queue:flush`   | 諦めたジョブの記録を捨てる     | [queue.md](queue.md)               |
| `schedule:run`  | いま動かすべき定期処理を動かす | [scheduling.md](scheduling.md)     |
| `schedule:list` | 定期処理の一覧                 | [scheduling.md](scheduling.md)     |
| `lang:list`     | 読み込まれている言語の一覧     | [localization.md](localization.md) |

`queue:*` は `features = ["sqlite"]` のときだけ動きます。

`cache:prune` は、置き場所に掃除の仕組みが無いとき（`CACHE_DRIVER=memory`）に、
そう分かる文を出して終わります。消した数は出ません。

## コマンドの旗

**旗はコマンドごとに分かれています。** 受け付けない旗を書くと、その場でエラーになります。

| コマンド      | 使える旗                                                                   |
|---------------|----------------------------------------------------------------------------|
| `queue:work`  | `--queue=名前` / `--tries=3` / `--sleep=1` / `--retry-after=90` / `--once` |
| `queue:retry` | `--id=1`                                                                   |
| `migrate` 系  | `--database=` / `--class=` / `--step=` / `--seed` / `--force`              |
| 上記以外      | **旗を受け付けません**                                                     |

旗を受け付けないのは `cache:clear` / `cache:prune` / `queue:failed` / `queue:flush` /
`schedule:run` / `schedule:list` / `lang:list` です。

```
$ cargo artisan cache:clear --tries=9
エラー: `cache:clear` は `--tries=9` を受け付けません（このコマンドは旗を取りません）
```

以前はどのコマンドでも旗を黙って読み飛ばしていました。
`queue:work` のつもりで `cache:clear --tries=9` と書いた間違いに気づけるようにしました。

`queue:work` の `--retry-after=` は、予約されたまま残ったジョブを取り直すまでの秒数です
（既定 90 秒、[queue.md](queue.md)）。

## 自分のコマンド

`app/Console/Commands/*.rs` に置いたものが、同じ一覧に並びます。
書き方は [console-commands.md](console-commands.md) にあります。

```sh
cargo artisan greet アリス
```

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

**見張るのはビルドに影響するファイルだけです。**

| 種類           | 対象                                                       |
|----------------|------------------------------------------------------------|
| ファイルの種類 | `.rs` と、`resources/lang/` 直下の `.toml`                 |
| ディレクトリ   | `app` `bootstrap` `config` `database` `resources` `routes` |
| ファイル       | `.env` `Cargo.toml` `build.rs` `main.rs` `artisan.rs`      |

次のものは **見張りません。**

| 見張らないもの                                   | 理由                                                     |
|--------------------------------------------------|----------------------------------------------------------|
| `public/`                                        | デバッグビルドはディスクから読むので、再ビルドが要らない |
| データベースのファイル（`-wal` / `-shm` を含む） | 本体がつなぐだけで更新時刻が変わり、毎回再起動していた   |
| `storage/` `target` `node_modules` `.git`        | 重いか、ビルドに関係ない                                 |
| 隠しファイル                                     | エディタの一時ファイルを拾わないため                     |

### 見張り方

- 更新時刻を 0.4 秒ごとに確認します。
- 変更が続いている間は 0.25 秒待って、まとめて 1 回ビルドします。
- OS ごとの通知の仕組みに頼らないので、依存クレートが増えません。

### 本体の止め方

**`serve` は本体を即座に止めます。** 処理中のリクエストは待ちません。
`APP_SHUTDOWN_TIMEOUT` の猶予は `serve` では通りません。
停止の猶予を確かめたいときは、本体を直接動かしてください
（`cargo run -- serve`、[deployment.md](deployment.md)）。

### 失敗したとき

- ビルドに失敗したら、直前に動いていた版をそのまま動かし続け、エラーを表示します。
- 本体が自分で終了した場合（ポートが使用中など）はその旨を表示し、次の変更で起動し直します。

### 既知の割り切り

Ctrl+C で `artisan` が止まるとき、まれに本体が残ることがあります。
通常は同じコンソールに Ctrl+C が届くので一緒に終わります。

## 終了のしかた

本体は Ctrl+C と（Unix では）SIGTERM を受けたら、処理中のリクエストを終えてから止まります。

`queue:work` も同じで、 **いま動かしている 1 件を終わらせてから**止まります。

`cargo artisan serve` から動かした本体だけは別です。上の「本体の止め方」を見てください。

## 関連

- [getting-started.md](getting-started.md)
- [routing.md](routing.md)
- [console-commands.md](console-commands.md) — 自分のコマンドを足す
