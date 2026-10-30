# artisan

`cargo artisan` は開発用コマンドの入口です。グローバルなインストールは要りません。
本番では実行ファイルを直接叩きます（`./myapp migrate` など）。

## コマンドの一覧

`cargo artisan list` で、下の表と同じものが 1 画面に出ます。

| コマンド           | 動き                                           | 条件・詳しくは                     |
|--------------------|------------------------------------------------|------------------------------------|
| `serve`            | 起動して、変更を見張って作り直す               | [serve の動き](#serve-の動き)      |
| `init`             | Laravel と同じ構成のファイルを作る             |                                    |
| `list`             | 全部のコマンドの一覧                           | `help` / `--help` / `-h` も同じ    |
| `--version`        | bengara の版                                   |                                    |
| `route:list`       | ルートの表（METHOD / URI / NAME / MIDDLEWARE） | [routing.md](routing.md)           |
| `key:generate`     | `APP_KEY` を作って `.env` に書く               | `--force` で上書き                 |
| `session:gc`       | 期限切れのセッションのファイルを消す           | [session.md](session.md)           |
| `storage:init`     | `storage/` の下の書き込み先を作る              | [deployment.md](deployment.md)     |
| `about`            | いまの設定と置き場所を出す                     | `APP_KEY` の値は出しません         |
| `migrate`          | まだ流していないマイグレーションを流す         | `--seed` でシーダーも              |
| `migrate:status`   | 流したかどうかを一覧にする                     | 読むだけ。いつでも流せます         |
| `migrate:rollback` | 最後のバッチを巻き戻す                         | **本番は `--force`**、`--step=2` で 2 バッチ分 |
| `migrate:reset`    | 全部巻き戻す                                   | **本番は `--force`**               |
| `migrate:refresh`  | 全部巻き戻してから流し直す                     | **本番は `--force`**、`--seed` 可  |
| `migrate:fresh`    | 表を全部消してから流し直す                     | **本番は `--force`**、`--seed` 可  |
| `migrate:unlock`   | 途中で落ちて残った実行中の札を外す             | [deployment.md](deployment.md)     |
| `db:seed`          | シーダーを流す                                 | `--class=DatabaseSeeder` で 1 本   |
| `db:wipe`          | 表を全部消す                                   | **本番は `--force`**               |
| `cache:clear`      | キャッシュを全部消す                           | [cache.md](cache.md)               |
| `cache:prune`      | 期限切れのキャッシュだけ消す                   | [cache.md](cache.md)               |
| `queue:work`       | ジョブを処理する                               | [queue.md](queue.md)               |
| `queue:failed`     | 諦めたジョブの一覧                             | [queue.md](queue.md)               |
| `queue:retry`      | 諦めたジョブをキューへ戻す                     | [queue.md](queue.md)               |
| `queue:flush`      | 諦めたジョブの記録を捨てる                     | [queue.md](queue.md)               |
| `schedule:run`     | いま動かすべき定期処理を動かす                 | [scheduling.md](scheduling.md)     |
| `schedule:list`    | 定期処理の一覧                                 | [scheduling.md](scheduling.md)     |
| `lang:list`        | 読み込まれている言語の一覧                     | [localization.md](localization.md) |

- `migrate*` / `db:*` / `queue:*` はデータベースの機能フラグ（`sqlite` / `mysql` /
  `mariadb` / `postgres` のどれか）が要ります（[configuration.md](configuration.md)）。
- 引数なしの `cargo artisan` は `list` と同じです。引数なしで本体を動かすと `serve` になります。
- `make:*` コマンドはありません。意図的な判断です（[decisions.md](decisions.md)）。
- `app/Console/Commands/*.rs` に置いた自作コマンドも、同じ一覧に並びます。

## 仕組み

`cargo artisan` は 3 段になっています。

| 段                    | 何か                                                                        |
|-----------------------|-----------------------------------------------------------------------------|
| 1. Cargo のエイリアス | `.cargo/config.toml` の `artisan = ["run", "-q", "--bin", "artisan", "--"]` |
| 2. `artisan` バイナリ | `artisan.rs`。`serve` `init` `list` `--version` を自分で処理する             |
| 3. 本体への委譲       | 知らないコマンドは本体を呼び直して実行する                                  |

- `cargo artisan --version` は受け付けます（`-V` も同じ）。`list` には載せていません。
- `list` は開発用のコマンドを出したあと、本体を呼び直して残りを続けて出します。
  **アプリがビルドできないときは、開発用のコマンドだけを出して終わります。**
- `init` 前の `main.rs`（`fn main() { bengara::run() }`）は `init` と `--version`
  だけを受け付けます。それ以外は「先に init を実行してください」と案内します。

## 本番で `--force` が要るコマンド

`APP_ENV=production` のとき、 **中身が消える次の 5 つ**は `--force` を付けないと止まります。

| コマンド           | 中身が消える理由                      |
|--------------------|---------------------------------------|
| `migrate:rollback` | `down` を流すので、巻き戻した分の中身が消える |
| `migrate:reset`    | `down` を流すので、表ごと中身が消える |
| `migrate:refresh`  | 流し直す前に `down` を流す            |
| `migrate:fresh`    | 表を全部消す                          |
| `db:wipe`          | 表を全部消す                          |

**表を変えるコマンドは同時に流せません。** 2 つ目は「実行中です」で止まります
（[deployment.md](deployment.md)）。

## コマンドの旗

**旗はコマンドごとに分かれています。** 受け付けない旗を書くと、その場でエラーになります。

| コマンド       | 使える旗                                                                   |
|----------------|----------------------------------------------------------------------------|
| `serve`        | `--host H` / `--port P`（`--port=8080` の形も可）。下の注                  |
| `key:generate` | `--force`                                                                  |
| `queue:work`   | `--queue=名前` / `--tries=3` / `--sleep=1` / `--retry-after=90` / `--once` |
| `queue:retry`  | `--id=1`                                                                   |
| `migrate` 系   | `--database=` / `--class=` / `--step=` / `--seed` / `--force`              |
| 上記以外       | **旗を受け付けません**                                                     |

旗を受け付けないのは `cache:clear` / `cache:prune` / `queue:failed` / `queue:flush` /
`schedule:run` / `schedule:list` / `lang:list` です。

```
$ cargo artisan cache:clear --tries=9
エラー: `cache:clear` は `--tries=9` を受け付けません（このコマンドは旗を取りません）
```

**`key:generate` も同じです。** `--forse` のような打ち間違いはエラーになります。

**`cargo artisan serve` は旗を確かめません。** 書いた旗はそのまま本体に渡します。
本体が受け付けない旗だと本体がその場で終わり、`serve` の画面に
「本体が異常終了しました（終了コード 1）」と出ます。
`serve` は止まらず、次の変更でもう一度起動します。

- `migrate` 系の `--database=接続の名前` はどのコマンドにも付けられます。
- `queue:work` の `--retry-after=` は、予約されたまま残ったジョブを取り直すまでの
  秒数です（既定 90 秒、[queue.md](queue.md)）。
- `cache:prune` は、置き場所に掃除の仕組みが無いとき（`CACHE_DRIVER=memory`）に、
  そう分かる文を出して終わります。消した数は出ません。

## serve の動き

ホストとポートの既定は `.env` の `APP_HOST`（既定 `127.0.0.1`）と `APP_PORT`（既定 `8000`）です。

1. アプリをビルドする。
2. `storage/framework/serve/` に実行ファイルをコピーしてから起動する。
   同じプロジェクトで `serve` を 2 つ動かしても衝突しません。
   **起動のときに、前回までの控えもまとめて片付けます。**
3. 変更を見張る。
4. 変更があれば再ビルドして再起動する。

### 見張る対象

**見張るのはビルドに影響するファイルだけです。**

| 種類           | 対象                                                       |
|----------------|------------------------------------------------------------|
| ファイルの種類 | `.rs` と、`resources/lang/` の `.toml`                     |
| ディレクトリ   | `app` `bootstrap` `config` `database` `resources` `routes` |
| ファイル       | `.env` `Cargo.toml` `Cargo.lock` `.cargo/config.toml` `rust-toolchain.toml` `build.rs` `main.rs` `artisan.rs` |

言語ファイルは **2 つの置き方のどちらも見張ります。**

| 見張る                            | 見張らない                              |
|-----------------------------------|-----------------------------------------|
| `resources/lang/ja.toml`          | `resources/lang/ja/admin/messages.toml` |
| `resources/lang/ja/messages.toml` | （`<言語>/` より下のディレクトリ）      |

次のものは **見張りません。**

| 見張らないもの                                   | 直したときにすること              |
|--------------------------------------------------|-----------------------------------|
| `public/`                                        | 何も要りません（すぐ反映されます） |
| データベースのファイル（`-wal` / `-shm` を含む） | —                                 |
| `storage/` `target` `node_modules` `.git`        | —                                 |
| 隠しファイル                                     | —                                 |

### 見張り方

更新時刻を 0.4 秒ごとに確認し、変更が続いている間は 0.25 秒待って、
まとめて 1 回ビルドします。

比べるのは **ファイルの数と、更新時刻のいちばん新しいもの**の 2 つだけです。
**数も最大の更新時刻も同じまま中身だけ入れ替わった場合は、取りこぼします。**
そのときはファイルを 1 つ触るか、`serve` を入れ直してください。

### 失敗したとき

- ビルドに失敗したら、直前に動いていた版をそのまま動かし続け、エラーを表示します。
- 本体が自分で終了した場合（ポートが使用中など）はその旨を表示し、次の変更で起動し直します。
- **初回のビルドや起動が失敗しても、`serve` は止まりません。** エラーを出してから
  見張りを続けます。ファイルを直せば、もう一度ビルドして起動します。

## 終了のしかた

本体は Ctrl+C と（Unix では）SIGTERM を受けたら、処理中のリクエストを終えてから止まります。
`queue:work` も同じで、 **いま動かしている 1 件を終わらせてから**止まります。

`serve` から動かした本体だけは別です。

- **`serve` は本体を即座に止めます。** 処理中のリクエストは待ちません。
- `APP_SHUTDOWN_TIMEOUT` の猶予は `serve` では通りません。
  停止の猶予を確かめたいときは、本体を直接動かしてください（`cargo run -- serve`）。
- **`artisan` が落ちたときも本体を止めます。** パニックやエラーで終わっても、
  本体がポートを握ったままにはなりません。次の `serve` が「address in use」で
  失敗することはありません。
- 例外は `artisan` を強制終了したときです（`kill -9`、タスクマネージャからの終了）。
  片付けの処理ごと消えるので、本体が残ります。残ったら手で止めてください。

止め方のくわしい話は [deployment.md](deployment.md) にあります。

## 自分のコマンド

`app/Console/Commands/*.rs` に置いたものが、同じ一覧に並びます。

```sh
cargo artisan greet アリス
```

書き方は [console-commands.md](console-commands.md) にあります。

## 関連

- [getting-started.md](getting-started.md)
- [routing.md](routing.md)
- [console-commands.md](console-commands.md) — 自分のコマンドを足す
