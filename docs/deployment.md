# 本番に置く

実行ファイル 1 つを置いて動かす手順です。
止め方、止めずに更新する方法、常駐させる設定の例もここにあります。

## 手順の全体

初回は 1〜7、2 回目からは 1・2・6・7 です。起動したら `/up` が 200 を返すのを確かめます（[下](#準備完了の確認)）。

```sh
# 1. 作る
cargo build --release

# 2. 置く。動いている実行ファイルには上書きできないので、名前を変えてから置く
mv /opt/myapp/myapp /opt/myapp/myapp.old
cp target/release/myapp /opt/myapp/myapp

# 3〜5. 初回だけ。.env を先に置く
cd /opt/myapp
cp /path/to/.env.example .env   # 中身を本番用に直す
./myapp storage:init
./myapp key:generate

# 6. 表を作る（更新のたびに 1 回）
./myapp migrate

# 7. 起動
./myapp serve --port 8001
```

**初回は `.env` を先に置いてください。** `.env` も `storage/` も無い場所では、
`storage:init` も `key:generate` も「基準ディレクトリが決まりません」で止まります。

## 置くもの

置くのは、実行ファイル・`.env`・書き込める `storage/` の 3 つだけです。

```
/opt/myapp/
├── myapp          実行ファイル
├── .env           設定
└── storage/       書き込み先
```

- `public/`・`config/`・`resources/`・`routes/` は**置きません。** 全部が実行ファイルに入っています。
- 入れ替えても、動いているプロセスは古い版のままです。新しい版にするには、起動し直します。

## 基準ディレクトリの決め方

起動したときに「基準ディレクトリ」を 1 つ決めます。`.env`・`storage/`・`public/` はここから探します。

| 順 | 見るもの                       | 条件                                       |
|----|--------------------------------|--------------------------------------------|
| 1  | 環境変数 `APP_BASE_PATH`       | **絶対パスのみ。** その場所があること      |
| 2  | cargo のプロジェクト           | `cargo run` で動かしたとき                 |
| 3  | 実行ファイルのあるディレクトリ | **そこに `.env` か `storage/` があるとき** |
| 4  | カレントディレクトリ           | **そこに `.env` か `storage/` があるとき** |

**決まらなければ起動しません。** 探した場所と直し方を書いたエラーが出ます。

```
エラー: 基準ディレクトリが決まりません。
探した場所:
  実行ファイルの隣: /opt/myapp
  カレントディレクトリ: /home/alice
どちらにも .env と storage/ がありません。
```

- `APP_BASE_PATH` は `.env` に書いても効きません。本物の環境変数で渡します。
- `--version` と `--help` は、基準ディレクトリが無くても動きます。

### いまの設定を確かめる

`./myapp about` で、いまの設定と置き場所を出します。

```
bengara           0.1.0
環境              production
APP_KEY           あり
基準ディレクトリ  /opt/myapp（実行ファイルの隣）
storage           /opt/myapp/storage
...
```

- `APP_KEY` の値は出しません。設定されているかどうかだけです。
- ほかに、言語・セッション・キャッシュ・メールの設定と、マイグレーションなどの数を出します。
- `serve` も、起動のときに基準ディレクトリと `storage` の絶対パスをログに出します。

## storage を作る

```sh
./myapp storage:init
```

`storage/` と、その下の書き込み先を作ります（一覧は [directory-structure.md](directory-structure.md#storage)）。

- 何度でも実行できます。すでにあるものには触りません。
- Unix では、作ったものを **0700**（所有者だけ）にします。
- **起動時には作りません。** 無いまま起動すると、ログに警告が出ます。
- `storage/` の所有者は、アプリを動かすユーザーにしてください。
- 場所を変えるときは、`APP_STORAGE_PATH` に絶対パスを書きます（`APP_STORAGE_PATH=/var/lib/myapp/storage`）。
- Laravel の `storage:link` は要りません。`/storage/...` は本体が直接配信します（[storage.md](storage.md)）。

## 本番の .env

| 項目              | すること                                                            |
|-------------------|---------------------------------------------------------------------|
| `APP_ENV`         | `production` にする                                                 |
| `APP_DEBUG`       | `false` にする。エラー画面に詳細を出さない                          |
| `APP_KEY`         | `./myapp key:generate` で作る。**64 文字以上が必須**                |
| `APP_URL`         | 公開する URL にする                                                 |
| `HASH_ITERATIONS` | 開発用に下げた値を残さない。既定（240,000）以上にする               |
| `TRUSTED_PROXIES` | 前段にプロキシを置くときだけ書く（[下](#接続元のアドレスを伝える)） |

- `APP_ENV=production` にすると、セッションの Cookie に `Secure` が付きます。**利用者には HTTPS で見せてください。**
- **鍵を作り直す（`key:generate --force`）と、全員がログアウトします。** 暗号化した値と、配った署名付き URL も無効になります。
- 項目の一覧は [configuration.md](configuration.md) にあります。ログは標準出力に出ます。

## 止める

| OS      | 受け付ける合図                                                       |
|---------|----------------------------------------------------------------------|
| Unix    | SIGTERM、SIGINT（Ctrl+C）                                            |
| Windows | Ctrl+C、Ctrl+Break、コンソールを閉じる操作、ログオフ、シャットダウン |

合図を受け取った後の動きは、どの OS でも同じです。

1. 待ち受けを閉じる。新しい接続は受け付けない。
2. 処理中のリクエストが終わるのを待つ。
3. 上限の時間を過ぎたら、残りを打ち切って終わる。

| 場面                        | どうなるか                                                |
|-----------------------------|-----------------------------------------------------------|
| 上限の時間                  | `APP_SHUTDOWN_TIMEOUT` の秒数。既定 30。**`0` は無制限**  |
| 上限を過ぎた                | 警告を出して打ち切る。裏の処理を **3 秒だけ**待って終わる |
| 待っている間に 2 回目の合図 | **待たずに終わる**（終了コード 130）。無制限のときも効く  |

- **上限より長くかかるリクエストは切られます。** 長い処理は [キュー](queue.md) に回してください。
- WebSocket の接続には 1001（サーバーが止まる）を送ります。待つのは上限と 5
  秒の短いほうです（[websocket.md](websocket.md#止めるとき)）。
- **Windows のサービス停止要求は受け取れません。** `nssm` などでサービスにすると、止めるときに強制終了になります。

### queue:work を止める

`queue:work` も同じ合図を受けます。

| 合図   | 動き                                                              |
|--------|-------------------------------------------------------------------|
| 1 回目 | **いま動かしている 1 件を終えてから**止まる。次のジョブは取らない |
| 2 回目 | 待たずに終わる（終了コード 130）                                  |

- `APP_SHUTDOWN_TIMEOUT` は効きません。待つのは「いまの 1 件」が終わるまでです。
- 途中で強制終了されたジョブは、`--retry-after`（既定 90 秒）の後に取り直されます（[queue.md](queue.md)）。
- データベースの一時的なエラーが直らなければ、エラーで終わります。起動し直す設定（`Restart=always` など）を入れてください。

## プロセスを 2 つ動かす

更新の間も止めないために、同じ実行ファイルを**別のポートで 2 つ**動かします。
前段のプロキシ（nginx など）で振り分けます。

```sh
./myapp serve --port 8001 &
./myapp serve --port 8002 &
```

| 守ること                        | 理由                                                                      |
|---------------------------------|---------------------------------------------------------------------------|
| **`storage/` は同じ場所を指す** | セッションとキャッシュを共有するため                                      |
| `SESSION_DRIVER=file`           | `memory` はプロセスをまたげない                                           |
| `CACHE_DRIVER=file`             | 同じ理由                                                                  |
| `migrate` は更新のたびに 1 回   | プロセスごとの起動時には流さない                                          |
| `schedule:run` は 1 か所だけ    | 同じ処理を二重に走らせないため                                            |
| `BROADCAST_CONNECTION=database` | WebSocket の配信をプロセスをまたいで届けるため（[下](#websocket-を通す)） |

回数の制限（`Throttle`）はプロセスごとに数えます。2 つ動かすと、上限は約 2 倍になります。

### 更新の手順

1. 新しい実行ファイルに入れ替える。
2. 必要なら `migrate` を実行する（**古い版でも動く変更だけ**）。
3. 1 つ目のプロセスを止めて新しい版で起動し、`/up` が 200 を返すまで待つ。
4. 2 つ目も同じように入れ替える。

更新の間は、古い版と新しい版が同時に動きます。
表の形を変えるときは、**列や表の追加を先に**、**削除は全部のプロセスを更新した後に**行います。

### 前段の設定（nginx の例）

```nginx
upstream myapp {
    server 127.0.0.1:8001;
    server 127.0.0.1:8002;
}

server {
    listen 80;
    location / {
        proxy_pass http://myapp;
        # 止めたプロセスにつながらなければ、もう一方へ送る
        proxy_next_upstream error timeout;
    }
}
```

**`POST` は送り直されません。** 止めたプロセスへの `POST` は失敗します。
`non_idempotent` を足せば送り直しますが、処理が二重に走る危険があります。

### WebSocket を通す

WebSocket（[websocket.md](websocket.md)）を使うなら、その URL に切り替えのヘッダーを渡す設定を足します。

```nginx
location /gateway {
    proxy_pass http://127.0.0.1:8000;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_set_header Host $host;
    proxy_read_timeout 120s;   # アプリのハートビートの間隔より長く
}
```

- `proxy_read_timeout` の間に何も流れないと、nginx が接続を切ります。クライアントが定期的に送るか、`ping_interval` を使います。
- HTTPS は nginx で受けます。ブラウザからは `wss://` でつなぎます。
- 更新のときは、止めたプロセスの接続が 1001 で切れます。クライアントはつなぎ直すように作ります。

次のどれかに当たるなら、配信の送り方を `database` にします（`BROADCAST_CONNECTION=database`）。

- 定期処理（`schedule:run`）・キュー（`queue:work`）・コマンドから送る（別のプロセスなので `memory` では届かない）
- サーバーを 2 つ以上動かす

中継の表を作るマイグレーションが要ります（[websocket.md](websocket.md#database-を使う)）。

### 接続元のアドレスを伝える

前段にプロキシを置くと、本体から見える接続元はプロキシのアドレスになります。
回数の制限は、ログインしていない人を接続元のアドレスで数えます。
そのままでは全員が同じ枠を使うので、`.env` にプロキシのアドレスを書きます。

```ini
TRUSTED_PROXIES=127.0.0.1
```

- **前段にプロキシを置くときだけ書きます。** 置かないのに書くと、偽のヘッダーで回数の制限をすり抜けられます。
- 書き方は [configuration.md](configuration.md#trusted_proxies) にあります。

## migrate の同時実行

表を変えるコマンドは、実行の前に「実行中」の札を置きます。**2 つ目は待たずに止まります**（誰が取ったかを表示します）。
札は `migration_locks` 表に置きます。この表は自動で作ります。

| 札を取る                                                                                                       | 札を取らない                        |
|----------------------------------------------------------------------------------------------------------------|-------------------------------------|
| `migrate` / `migrate:rollback` / `migrate:reset` / `migrate:refresh` / `migrate:fresh` / `db:seed` / `db:wipe` | `migrate:status` / `migrate:unlock` |

途中でプロセスが落ちると、札が残ります。**時間がたっても自動では外れません。**
`./myapp migrate:unlock` で外してください（[migrations.md](migrations.md)）。

## 準備完了の確認

`/up` は、起動の処理が全部終わってから 200 を返します。
`init` が書く `bootstrap/app.rs` の `health("/up")` が登録しています。

```sh
curl -sf http://127.0.0.1:8001/up || echo "まだ"
```

本文は `{"status":"ok","name":"<アプリ名>"}` です。

## 前段のプロキシで守るもの

本体が自分で守るのは、次の 3 つです。

| 守るもの                                                  | 上限                                                                 | 超えたとき |
|-----------------------------------------------------------|----------------------------------------------------------------------|------------|
| リクエストの本文の大きさ                                  | 既定 2 MiB。`APP_MAX_BODY_BYTES` とルートの `.max_body` で変えられる | 413        |
| 本文を読み終えるまでの時間（全部読むルート）              | 合計 30 秒                                                           | 408        |
| 次のかたまりが届くまでの時間（`.stream_body()` のルート） | 30 秒。全体の時間は数えない                                          | 408        |

くわしくは [requests-and-responses.md](requests-and-responses.md) の「本文の上限」にあります。

次のものは、前段のプロキシで掛けてください。**前段を置かずに直接公開するのは勧めません。**

| 前段で掛けるもの                         | nginx での例            |
|------------------------------------------|-------------------------|
| ヘッダーが来るまでの待ち時間             | `client_header_timeout` |
| 使っていない接続を切るまでの時間         | `keepalive_timeout`     |
| 同時接続数の上限                         | `limit_conn`            |
| 相手ごとの回数の制限（プロセスをまたぐ） | `limit_req`             |
| HTTPS                                    | `listen 443 ssl`        |

### 本文の上限を前段とそろえる

- nginx にも本文の上限があります（`client_max_body_size`、既定 1 MB）。
- `APP_MAX_BODY_BYTES` や `.max_body` を上げたら、前段の上限もそれ以上にします。前段のほうが小さいと、本体に届く前に断られます。
- `.stream_body()` のルートは全体の時間を数えません。少しずつ送り続ける相手は、前段の同時接続数の上限などで守ります。
- nginx はふだん本文を全部受け取ってから本体へ渡します。少しずつ渡したいなら `proxy_request_buffering off` を確かめてください。

## 常駐させる

systemd の例です。

```ini
[Unit]
Description=myapp (port 8001)
After=network.target

[Service]
WorkingDirectory=/opt/myapp
ExecStart=/opt/myapp/myapp serve --port 8001
Restart=always
# SIGTERM を送ってから、この秒数まで待つ
TimeoutStopSec=40
User=myapp

[Install]
WantedBy=multi-user.target
```

`TimeoutStopSec` は、SIGTERM から強制終了（SIGKILL）までの秒数です。短いと、処理の途中で切られます。

| 動かすもの   | `TimeoutStopSec` の目安                                             |
|--------------|---------------------------------------------------------------------|
| `serve`      | `APP_SHUTDOWN_TIMEOUT` ＋ 10 秒（既定の 30 秒なら 40）              |
| `queue:work` | ジョブ 1 件にかかる時間（`config/queue.rs` か `--timeout`）より長く |

### キューと定期処理

Web とは別に動かします。

```ini
# キューの worker（1 つ以上）
ExecStart=/opt/myapp/myapp queue:work
Restart=always
TimeoutStopSec=120
```

```
# 定期処理（cron。1 分ごとに、1 か所だけ）
* * * * * cd /opt/myapp && ./myapp schedule:run >> /dev/null 2>&1
```

- 長いジョブは、`config/queue.rs` でジョブごとに打ち切りの時間を決めます（[queue.md](queue.md)）。
- **`schedule:run` は 1 分ごとに呼んでください**（[scheduling.md](scheduling.md)）。

### セッションの掃除

期限切れのセッションのファイルは、ふだんの書き込みのついでに少しずつ消えます（1 回に 500 件まで）。
利用者が多いところでは、全部を見る `session:gc` を 1 日 1 回ほど流してください（[session.md](session.md)）。

```
0 4 * * * cd /opt/myapp && ./myapp session:gc >> /dev/null 2>&1
```

## 関連

- [configuration.md](configuration.md) — `.env` の項目
- [artisan.md](artisan.md) — コマンドの一覧
- [queue.md](queue.md) / [scheduling.md](scheduling.md) — 別のプロセスで動かすもの
