# artisan

コマンドの一覧と、コマンドごとに付けられる指定の説明です。
開発用の `serve`（変更を見張って作り直す）の動きもここにあります。

| 場面 | 打ち方                                  |
|------|-----------------------------------------|
| 開発 | `cargo artisan migrate`                 |
| 本番 | `./myapp migrate`（実行ファイルを直接） |

インストールは要りません。`cargo artisan` は `init` が `.cargo/config.toml` に書く別名です。

## コマンドの一覧

`cargo artisan list`（引数なしの `cargo artisan` も同じ）で、同じ一覧が画面に出ます。

### 開発用（`cargo artisan` だけ）

| コマンド    | 動き                                          | 詳しくは                                 |
|-------------|-----------------------------------------------|------------------------------------------|
| `serve`     | 起動して、変更があれば作り直す                | [serve の動き](#serve-の動き)            |
| `init`      | Laravel と同じ構成のファイルを作る            | [getting-started.md](getting-started.md) |
| `list`      | コマンドの一覧（`help` `--help` `-h` も同じ） |                                          |
| `--version` | bengara の版（`-V` `version` も同じ）         |                                          |

`artisan` はアプリのコードを取り込みません。**アプリがビルドできないときも動きます。**
そのとき `list` は、開発用のコマンドだけを出します。

### 本体

| コマンド       | 動き                                           | 詳しくは                                     |
|----------------|------------------------------------------------|----------------------------------------------|
| `serve`        | HTTP サーバーを起動する（見張りはしない）      | [deployment.md](deployment.md)               |
| `route:list`   | ルートの表（METHOD / URI / NAME / MIDDLEWARE） | [routing.md](routing.md)                     |
| `key:generate` | `APP_KEY` を作って `.env` に書く               | [configuration.md](configuration.md#app_key) |
| `about`        | いまの設定と置き場所を出す                     | [deployment.md](deployment.md)               |
| `storage:init` | `storage/` の下の書き込み先を作る              | [deployment.md](deployment.md)               |
| `session:gc`   | 期限切れのセッションのファイルを消す           | [session.md](session.md)                     |

本体を引数なしで動かすと `serve` です（`./myapp`）。
`--host` か `--port` から始めたときも `serve` です（`./myapp --port 8001`）。

### データベース

| コマンド           | 動き                                   |
|--------------------|----------------------------------------|
| `migrate`          | まだ流していないマイグレーションを流す |
| `migrate:status`   | 流したかどうかの一覧（読むだけ）       |
| `migrate:rollback` | 最後のバッチを巻き戻す                 |
| `migrate:reset`    | 全部巻き戻す                           |
| `migrate:refresh`  | 全部巻き戻してから流し直す             |
| `migrate:fresh`    | 表を全部消してから流し直す             |
| `migrate:unlock`   | 途中で落ちて残った「実行中」の札を外す |
| `db:seed`          | シーダー（初期データ）を流す           |
| `db:wipe`          | 表を全部消す                           |

くわしくは [migrations.md](migrations.md) にあります。

### 周辺機能

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

-

データベースとキューのコマンドには、データベースの機能フラグが要ります（[configuration.md](configuration.md#機能フラグ)）。

- `app/Console/Commands/` に置いた自分のコマンドも、同じ一覧に並びます（[console-commands.md](console-commands.md)）。
- 無い名前を打つと、エラーと一覧を出して終了コード 1 で終わります。
- コードを生成する `make:*` コマンドはありません。ファイルは手で作ります。

## コマンドに付けられる指定

**指定（`--force` などの旗）は、コマンドごとに決まっています。**
受け付けない指定を書くと、何も実行せずにエラーで終わります。

```
$ cargo artisan cache:clear --tries=9
エラー: `cache:clear` は `--tries=9` を受け付けません（このコマンドは旗を取りません）
```

### 本体と周辺機能

| コマンド       | 使える指定                                                      |
|----------------|-----------------------------------------------------------------|
| `serve`        | `--host <ホスト>` / `--port <ポート>`（`--port=8080` の形も可） |
| `key:generate` | `--force`（すでにある鍵を作り直す）                             |
| `queue:work`   | 下の表                                                          |
| `queue:retry`  | `--id=1`（1 件だけ戻す）                                        |
| 上記以外       | **受け付けない。** 余分な引数も断る                             |

`init` / `list` / `--version` だけは、余分な引数を無視します。

| `queue:work` の指定 | 既定      | 意味                                                       |
|---------------------|-----------|------------------------------------------------------------|
| `--queue=名前`      | `default` | 処理するキュー                                             |
| `--tries=3`         | 3         | 諦めるまでに試す回数（1 以上）                             |
| `--sleep=1`         | 1         | ジョブが無いときに待つ秒数                                 |
| `--timeout=60`      | 60        | 1 件のジョブを待つ上限の秒数。`0` は無制限（起動時に警告） |
| `--retry-after=90`  | 90        | 処理中のまま残ったジョブを取り直すまでの秒数（1 以上）     |
| `--once`            | —         | 1 件だけ処理して終わる                                     |

- ジョブごとの打ち切りの時間は `config/queue.rs` で決めます。載っているジョブは `--timeout` より優先します。
- 打ち切りの時間が `--retry-after` より長いジョブは、予約を延ばします。二重には動きません（[queue.md](queue.md)）。

### データベース

| コマンド           | `--database=` | `--seed` | `--class=` | `--step=` | `--force` |
|--------------------|---------------|----------|------------|-----------|-----------|
| `migrate`          | ○             | ○        | ○          | ×         | ○         |
| `migrate:status`   | ○             | ×        | ×          | ×         | ×         |
| `migrate:rollback` | ○             | ×        | ×          | ○         | ○         |
| `migrate:reset`    | ○             | ×        | ×          | ×         | ○         |
| `migrate:refresh`  | ○             | ○        | ○          | ×         | ○         |
| `migrate:fresh`    | ○             | ○        | ○          | ×         | ○         |
| `migrate:unlock`   | ○             | ×        | ×          | ×         | ×         |
| `db:seed`          | ○             | ×        | ○          | ×         | ○         |
| `db:wipe`          | ○             | ×        | ×          | ×         | ○         |

| 指定                     | 意味                                             |
|--------------------------|--------------------------------------------------|
| `--database=接続の名前`  | 既定ではない接続に流す                           |
| `--seed`                 | 流した後にシーダーも流す                         |
| `--class=シーダーの名前` | そのシーダーだけ流す（`--class=DatabaseSeeder`） |
| `--step=2`               | 巻き戻すバッチの数（既定 1）                     |
| `--force`                | 本番で、中身が消えるコマンドを通す               |

- `--class=` は、`db:seed` 以外では `--seed` と一緒に使います。
- **シーダーは既定の接続にだけ流せます。** `db:seed` と `--seed` に別の `--database=` を付けるとエラーです。
- `migrate` と `db:seed` の `--force` は、付けても付けなくても同じ動きです。
- `migrate:status` と `migrate:unlock` は、記録の表や札の表が無くても**作りません。**

## 本番で `--force` が要るコマンド

`APP_ENV=production` のとき、**中身が消える次の 5 つ**は `--force` が無いと止まります。

`migrate:rollback` / `migrate:reset` / `migrate:refresh` / `migrate:fresh` / `db:wipe`

**表を変えるコマンドは同時に流せません。** 2
つ目は「実行中です」で止まります（[deployment.md](deployment.md#migrate-の同時実行)）。

## serve の動き

```sh
cargo artisan serve
cargo artisan serve --host 0.0.0.0 --port 8124
```

ホストとポートの既定は、`.env` の `APP_HOST`（`127.0.0.1`）と `APP_PORT`（`8000`）です。

1. アプリをビルドする。
2. できた実行ファイルを `storage/framework/serve/` に写して起動する。
3. ファイルの変更を見張る。
4. 変更があれば、ビルドし直して起動し直す。

同じプロジェクトで `serve` を 2 つ動かしても、写した実行ファイルはぶつかりません。
前回までの写しは、起動のときに片付けます。

### 見張るもの

**見張るのは、ビルドに影響するファイルだけです。**

- `app/` `bootstrap/` `config/` `database/` `routes/` `resources/` の下の `.rs`
- `resources/lang/<言語>.toml` と `resources/lang/<言語>/*.toml`
- `.env` `Cargo.toml` `Cargo.lock` `build.rs` `main.rs` `artisan.rs` `.cargo/config.toml` `rust-toolchain.toml`

- `public/` は見張りません。直せばすぐ反映されます（デバッグビルドはディスクから読むため）。
- `tests/` は見張りません。テストは `cargo test` で走らせます。
- `storage/` `target/` `node_modules/` `.git/`、隠しファイル、データベースのファイルも見張りません。

- 0.4 秒ごとに確かめます。保存が続いている間は待って、1 回のビルドにまとめます。
- 比べるのは**ファイルの数**と**いちばん新しい更新時刻**です。
  どちらも変わらない書き換えは見落とします。そのときはファイルを 1 つ保存し直してください。
- ビルドが自分で書き換えた `Cargo.lock` では作り直しません。
  ビルドの最中に `cargo update` をしたときは、ファイルを 1 つ保存し直してください。

### 失敗したとき

| 起きたこと                   | 動き                                           |
|------------------------------|------------------------------------------------|
| ビルドに失敗した             | エラーを出し、**前の版をそのまま動かし続ける** |
| 本体が自分で終わった         | その旨を出し、次の変更でもう一度起動する       |
| 初回のビルドや起動に失敗した | `serve` は止まらない。直せばもう一度試す       |

本体が自分で終わるのは、ポートが使われているときや、`serve` に知らない指定を付けたときです。
画面に「本体が異常終了しました（終了コード 1）」と出ます。

### 止め方

Ctrl+C で止めます。

- **`serve` は本体をすぐに止めます。** 処理中のリクエストは待ちません。
- 待ってから止まる動き（`APP_SHUTDOWN_TIMEOUT`）を確かめるときは、`cargo run -- serve`
  で動かします（[deployment.md](deployment.md#止める)）。
- `serve` を強制終了したとき（`kill -9`、タスクマネージャ）だけは、本体が残ります。手で止めてください。

## 関連

- [getting-started.md](getting-started.md) — 最初の手順
- [console-commands.md](console-commands.md) — 自分のコマンドを足す
- [deployment.md](deployment.md) — 本番での動かし方と止め方
