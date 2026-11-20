# Laravel と違う点

Laravel から来た人がつまずきやすい違いを、分野ごとに並べます。
左が Laravel、右が bengara です。くわしくは各節の最後のページを見てください。

## まず知っておくこと

| Laravel                                    | bengara                                                      |
|--------------------------------------------|--------------------------------------------------------------|
| `public/index.php` と `artisan` が入口     | 直下の `main.rs` と `artisan.rs`。`src/` は作らない          |
| Composer のオートロード                    | `build.rs` が置いたファイルを自動で取り込む                  |
| `use App\Http\Controllers\HomeController;` | `use crate::app::http::controllers::HomeController;`         |
| クラス名 = ファイル名                      | 大文字始まりのファイルに**同じ名前の型を 1 つ**書く          |
| ファサードとサービスコンテナ               | `use bengara::prelude::*;` の関数と型を使う                  |
| 引数の型から自動で注入する                 | **しない。** ハンドラの形は `()` と `(req: Request)` の 2 つ |
| 例外を投げる                               | `Err` を返す。状態コードを持つエラーならそのコードになる     |
| `make:*` でファイルを作る                  | **無い。** ファイルは手で作る                                |

置き場所は [directory-structure.md](directory-structure.md) にあります。

## コマンド

| Laravel                                | bengara                                                   |
|----------------------------------------|-----------------------------------------------------------|
| `php artisan ...`                      | `cargo artisan ...`。本番は `./myapp ...`                 |
| `serve` はすぐ動く                     | ビルドしてから起動する。ソースが変わると作り直す          |
| 旗の扱いが緩い                         | **知らない旗や余分な引数はエラー**                        |
| 同じ名前の自作コマンドが勝つ           | **フレームワークのコマンドが先。** 自作のほうは呼ばれない |
| 本番の `migrate` には `--force` が要る | 要らない（付けても通る）                                  |
| `migrate:fresh` などは本番で確認する   | 本番では `--force` が無いと止まる                         |
| `schedule:run`                         | 同じ。**失敗が 1 件でもあれば終了コード 1**               |

くわしくは [artisan.md](artisan.md) にあります。

## 設定

| Laravel                     | bengara                                                     |
|-----------------------------|-------------------------------------------------------------|
| `config('app.name')`        | `config::<AppConfig>().name`。**型で引く**                  |
| `config/app.php` が配列     | `config/app.rs` の `pub fn config() -> AppConfig`           |
| `env()` は文字列中心        | `env(名前, 既定値)` の型は**既定値の型**で決まる            |
| `config:cache` / `optimize` | **要らない。** 設定は起動時に組み立てて固定する             |
| `app_path()` は `app/`      | **基準ディレクトリ**を指す。`app_path("app/Models")` と書く |

くわしくは [configuration.md](configuration.md) にあります。

## ルーティングと URL

| Laravel                                             | bengara                                                     |
|-----------------------------------------------------|-------------------------------------------------------------|
| `Route::get('/', [HomeController::class, 'index'])` | `Route::get("/", HomeController::index)`                    |
| `route('posts.show', $post)`                        | `route_with("posts.show", &[("post", "12")])?`              |
| `route()` はパス引数をそのまま置く                  | **`%xx` に符号化する。** `/` も `%2F` になる                |
| `Route::middleware(...)`                            | `Route::get(...).middleware("admin")` と後ろに付ける        |
| `Route::name('admin.')`                             | 同じ形。**点は自分で書く**（忘れると `adminindex`）         |
| ルートの重複は後勝ち                                | **起動時にパニック**（パスの重複、ルート名の重複）          |
| `URL::signedRoute('name', ...)`                     | `signed_url("/unsubscribe", ...)?`。**パスを渡す**          |
| ミドルウェア `signed` が自動で止める                | **自動では止まらない。** `has_valid_signature` で確かめる   |
| メソッドの大文字小文字を区別しない                  | **区別する。** `get /x` は 405                              |
| 末尾の `/` は 301 で寄せる                          | **転送しない。** `/posts` と `/posts/` は同じルートに当たる |

くわしくは [routing.md](routing.md) にあります。

## リクエストとレスポンス

| Laravel                             | bengara                                                            |
|-------------------------------------|--------------------------------------------------------------------|
| `$request->route('post')`           | `req.param("post")`。数にするなら `req.param_as::<i64>("post")?`   |
| パス引数の型が合わない              | `param_as` は **404**。ルートに無い名前なら **500**                |
| `abort(403)`                        | `return abort(403);`                                               |
| 本文の上限は PHP の `post_max_size` | `.env` の `APP_MAX_BODY_BYTES`（既定 2 MiB）とルートの `.max_body` |

くわしくは [requests-and-responses.md](requests-and-responses.md) にあります。

## テンプレート

機能フラグ `view` で使います。書き方は **Jinja 系**です。**Blade の構文は使えません。**

| Laravel                            | bengara                                             |
|------------------------------------|-----------------------------------------------------|
| `welcome.blade.php`                | `welcome.html`                                      |
| `view("welcome")`                  | `view("welcome", ())`。値が無くても第 2 引数を書く  |
| `{{ $title }}`                     | `{{ title }}`（`$` は付けない）                     |
| `{!! $html !!}`                    | `{{ html\|safe }}`                                  |
| `@extends` / `@section` / `@yield` | `{% extends %}` / `{% block %}`                     |
| `@csrf`                            | `{{ csrf_field() }}`。`req.view(..)` で描くときだけ |
| エラーページを Blade で作る        | エラーページはテンプレートにならない                |

くわしくは [views.md](views.md) にあります。

## 入力とバリデーション

| Laravel                                  | bengara                                                              |
|------------------------------------------|----------------------------------------------------------------------|
| 落ちると前の画面に戻す（`$errors`）      | **422 を返す。** JSON を求めた相手には JSON                          |
| 規則は配列でも文字列でも書ける           | **文字列だけ**（`"required\|max:255"`）                              |
| 規則の書き間違いは実行して分かる         | `rules!` で書けば**コンパイルが止まる**                              |
| FormRequest のクラス                     | トレイト `FormRequest` と `const RULES`。`req.validate_as()?` で読む |
| メッセージは言語ファイルで変える         | **日本語で固定**                                                     |
| `TrimStrings` で全部の入力の空白を落とす | **検査する値だけ**落とす。`req.input()` は落とさない                 |
| `nullable` で飛ばした値はそのまま        | **空文字が入る**                                                     |
| `alpha` 系は空文字を通す                 | **落とす**                                                           |
| `date` はいろいろな形を通す              | **`YYYY-MM-DD` と `YYYY-MM-DD HH:MM(:SS)` だけ**。時差付きは落とす   |
| 重なりは `unique:` 規則で止める          | 表の `unique` で止め、`is_unique_violation` で見分ける               |

くわしくは [validation.md](validation.md) にあります。

## セッションと CSRF

CSRF は、別のサイトからの なりすまし送信のことです。

| Laravel                                   | bengara                                                           |
|-------------------------------------------|-------------------------------------------------------------------|
| 初めて来た人にもトークンを配る            | **`req.csrf_token()` を呼んだときだけ**作る                       |
| Cookie は暗号化される                     | **署名だけ。** 中身はサーバー側に置く                             |
| ログアウトのあと新しいセッションを配る    | **Cookie を消す。** 空のセッションは作らない                      |
| `_token` をクエリ文字列からも読む         | **本文だけ**から読む（`X-CSRF-TOKEN` ヘッダーも読む）             |
| `VerifyCsrfToken::$except`                | `except("/webhook")`。ルート名の `except_route("api.*")` を勧める |
| 1 本ずつにするのは `block()` のルートだけ | **同じセッションのリクエストはいつも 1 本ずつ**                   |

- `__` で始まるセッションの鍵は内部用です。自分の値には使わないでください。
- くわしくは [session.md](session.md) にあります。

## ミドルウェアと回数の制限

| Laravel                           | bengara                                                    |
|-----------------------------------|------------------------------------------------------------|
| `throttle:60,1`                   | `Throttle::from_spec("60,1")?` を `alias` に登録する       |
| 制限はキャッシュで共有される      | **プロセスごとに数える**（2 プロセスなら上限は約 2 倍）    |
| 名前の無い `throttle` は 1 つの枠 | **ルートごとに別の枠**                                     |
| `TrustProxies` ミドルウェア       | 環境変数 `TRUSTED_PROXIES`。範囲（`10.0.0.0/8`）は書けない |

くわしくは [middleware.md](middleware.md) にあります。

## データベース

| Laravel                                      | bengara                                                      |
|----------------------------------------------|--------------------------------------------------------------|
| `->where('status', 'published')`             | `.where_("status", "published")`（`where` は Rust の予約語） |
| `->where('views', '>', 100)`                 | `.where_op("views", ">", 100)`                               |
| `->orderBy()` に式を書ける                   | **列名だけ。** 式は `order_by_raw`                           |
| `->limit(1)->update([...])`                  | **エラー。** 黙って全件に当てないため                        |
| `->groupBy()` ＋ `->count()`                 | **グループの数**を返す                                       |
| `->increment('views')`                       | `.increment("views", 1)`。量は必ず書く                       |
| `->paginate(15)` が `?page=` を見る          | `.paginate(15, req.page())`。**ページは引数で渡す**          |
| `$row->title`                                | `row.get::<String>("title")?`                                |
| `DB::transaction(fn () => ...)`              | `DB::begin().await?` で始めて `tx.commit().await?`           |
| トランザクションの中の `truncate()`（MySQL） | **エラー。** `delete()` を使う                               |
| `ilike` や `<=>` をどこでも書ける            | `ilike` は PostgreSQL だけ、`<=>` は MySQL だけ              |

### 日時（Carbon）

| Laravel                         | bengara                                              |
|---------------------------------|------------------------------------------------------|
| タイムゾーン付き                | **UTC だけ。** データベースには文字列で入る          |
| `Carbon::parse()` は何でも読む  | **`YYYY-MM-DD` と `HH:MM(:SS)` だけ。** 桁はちょうど |
| `addMonth()` は月末で繰り上がる | `add_months(1)` は**月末で止まる**（1/31 → 2/28）    |
| `diffInDays()`                  | `diff_in_days(&other)` は**符号付き**                |
| `format('Y/m/d')`               | 決まった形だけ（`to_date_string()` など）            |
| PostgreSQL の `timestamp` 列    | **`text` 列**になる                                  |

くわしくは [database.md](database.md) にあります。

## モデルとマイグレーション

| Laravel                               | bengara                                                   |
|---------------------------------------|-----------------------------------------------------------|
| `class Post extends Model`            | `#[derive(Model)]` ＋ `#[model(table = "posts")]`         |
| 表の名前はクラス名から推測            | **書く。** 推測しない                                     |
| `$post->comments` で自動的に読む      | `comments()` メソッドを自分で書き、`.get().await?` で読む |
| `with('comments')` でまとめ読み       | `where_in` で読み、`bengara::database::group_by` で束ねる |
| `Post::create([...])`                 | 構造体を作って `post.save().await?`                       |
| `save()` は変わった属性だけを書く     | **主キーと `created_at` 以外の全列を書き戻す**            |
| `save()` は 0 件の更新でも成功        | **`Err` になる**                                          |
| トランザクションの中の `save()`       | `post.save_using(&tx).await?`                             |
| `Post::factory()->count(3)->create()` | `database/factories/` にただの関数を書く                  |
| `up()` / `down()`                     | 同じ。ただし**同期の関数**                                |

**`save()` は全列を書き戻します。** 読んだ後に別の場所で増えた列（`views` など）は、古い値に戻ります。
変える列だけを書くなら `save_only(&["title"])` を使います（[models.md](models.md)）。

## 認証と認可

| Laravel                               | bengara                                                             |
|---------------------------------------|---------------------------------------------------------------------|
| `Hash::make()` の既定は bcrypt        | 既定は PBKDF2。`HASH_DRIVER=argon2id` も使える（フラグ `argon2`）   |
| bcrypt（`$2y$`）の値で照合する        | **照合できない。** Argon2id（`$argon2id$`）の値は照合できる         |
| 非同期の中でも `Hash::make`           | `Hash::make_async` / `Hash::check_async` を使う                     |
| `Auth::user()`                        | `req.auth().user::<User>().await?`。**`req` から読む**              |
| `Auth::logoutOtherDevices($password)` | `req.auth().logout_other_devices_async()`。パスワードの確認は自分で |
| `redirect()->intended('/')`           | `req.session().intended()` を読んで、自分で転送する                 |
| `$this->authorize('update', $post)`   | `authorize(PostPolicy::update(&user, &post))?`（ただの関数）        |
| `encrypt()` / `decrypt()`             | 同じ名前。機能フラグ `encryption` が要る                            |
| `Password::sendResetLink()`           | `PasswordReset::create()` ＋ `link()`。送信は自分で書く             |
| `APP_KEY` は base64 の 32 バイト      | **64 文字以上**の文字列                                             |

くわしくは [authentication.md](authentication.md) と [authorization.md](authorization.md) にあります。

## テスト

| Laravel                  | bengara                                              |
|--------------------------|------------------------------------------------------|
| `php artisan test`       | `cargo test`                                         |
| トレイトを `use` する    | `#[bengara::test]` を付けるだけ                      |
| `RefreshDatabase`        | `let db = refresh_database().await;`                 |
| `$response->json()`      | `res.json()`。**JSON でない本文はパニック**          |
| `phpunit.xml` の `<env>` | `#[bengara::test]` が `APP_ENV=testing` などを入れる |

くわしくは [testing.md](testing.md) にあります。

## キャッシュ・ファイル・キュー・イベント

| Laravel                                  | bengara                                                    |
|------------------------------------------|------------------------------------------------------------|
| `Cache::increment` は期限を引き継ぐ      | **引き継がない。** 期限なしになる                          |
| `Storage::append` が挟む改行             | **どの OS でも `\n`**。挟まないなら `append_bytes`         |
| `SendWelcome::dispatch()`                | `Queue::push("SendWelcome", &payload)`。ジョブは名前で呼ぶ |
| ジョブの `public $timeout = 120;`        | `config/queue.rs` の `QueueConfig`。無ければ `--timeout`   |
| `retry_after` は設定ファイル             | `queue:work --retry-after=`（既定 90 秒）                  |
| イベントクラス ＋ `EventServiceProvider` | 名前は文字列。登録は `bootstrap/app.rs` の `with_events`   |
| `->everyMinute()`                        | `Every::Minute` などの列挙                                 |

くわしくは [cache.md](cache.md) / [storage.md](storage.md) / [queue.md](queue.md) / [events.md](events.md) にあります。

## メール・外部コマンド・多言語

| Laravel                                     | bengara                                                         |
|---------------------------------------------|-----------------------------------------------------------------|
| `MAIL_ENCRYPTION=null` で暗号化しない       | `none` か `null`。**空にすると `starttls`**                     |
| `MAIL_SCHEME=smtp` は STARTTLS をあれば使う | **STARTTLS を必ず求める**                                       |
| `Process::run('ls -la')`                    | `Process::run(["ls", "-la"])`。**シェルを通さない**             |
| `App::setLocale()`                          | `Lang::set` は**プロセス全体**。リクエストごとなら `Lang::with` |
| 言語ファイルは PHP の配列                   | `resources/lang/<言語>.toml`。**ビルド時に読む**                |
| `$signature` でコマンド名を決める           | **ファイル名**から決まる。名前に `:` は入れられない             |

くわしくは [mail.md](mail.md) / [processes.md](processes.md) / [localization.md](localization.md) / [console-commands.md](console-commands.md)
にあります。

## WebSocket と配信

| Laravel                                      | bengara                                              |
|----------------------------------------------|------------------------------------------------------|
| Reverb（別のプロセス）や Pusher を使う       | **本体が受ける。** `req.websocket()?.on_upgrade(..)` |
| `routes/channels.php` でチャンネルを認可する | 切り替える前に確かめ、`socket.join` で入れる         |
| `broadcast(new OrderShipped($order))`        | `Broadcast::publish("orders.1", 文字).await?`        |
| `ShouldBroadcast` でキューから送る           | 直接送る。別のプロセスから送るなら `database` にする |

くわしくは [websocket.md](websocket.md) にあります。

## 本番で動かす

| Laravel                                 | bengara                                                     |
|-----------------------------------------|-------------------------------------------------------------|
| `public/` を Web サーバーの公開先にする | 置くのは実行ファイル・`.env`・書き込める `storage/` の 3 つ |
| `composer install --no-dev`             | 要らない                                                    |
| `storage/` を手で作る                   | `./myapp storage:init` で作る（**起動時には作らない**）     |
| `storage:link`                          | 要らない。`/storage/...` は本体が配信する                   |
| php-fpm がプロセスを管理する            | 自分でプロセスを起動し、前段で振り分ける                    |

くわしくは [deployment.md](deployment.md) にあります。

## PHP 固有の機能

PHP の言語や実行の形に結びついた機能は作りません。代わりに次のように書きます。

| PHP 固有のもの                         | bengara での書き方                            |
|----------------------------------------|-----------------------------------------------|
| Blade のテンプレート文法               | 機能フラグ `view` の Jinja 系テンプレート     |
| サービスコンテナ・プロバイダ           | ハンドラの引数は 2 つの形だけ。設定は型で引く |
| マジックメソッド（`__get` / `__call`） | 読み込みはいつも明示する                      |
| 緩い型比較（`==` の暗黙の変換）        | Rust の型を使う                               |

借りられるものは、**Laravel と同じ名前のまま中身を Rust の形で作ります。** 理由は [decisions.md](decisions.md) にあります。
