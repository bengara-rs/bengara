# Laravel と違う点

Laravel から移ってきた人のための対応表です。
bengara は Laravel の構成と書き味に寄せていますが、同じではありません。

- 「同じ」と書いた行は、考え直さなくていいところです。
- くわしい動きは、各行の分野のページにあります。

## 置き場所と参照

| Laravel                                    | bengara                                                          |
|--------------------------------------------|------------------------------------------------------------------|
| `public/index.php` が入口                  | 直下の `main.rs`（中身は `bengara::app!();`）                    |
| `artisan`（PHP スクリプト）                | 直下の `artisan.rs`（中身は `fn main() { bengara::artisan() }`） |
| Composer のオートロード                    | `build.rs` の `bengara_build::discover()` が自動で取り込む       |
| `use App\Http\Controllers\HomeController;` | `use crate::app::http::controllers::HomeController;`             |
| クラス名 = ファイル名                      | 大文字始まりのファイルに**同じ名前の型を 1 つ**書く              |

`src/` は作りません。Laravel の構成に合わせています（[directory-structure.md](directory-structure.md)）。

## コマンド

| Laravel                                | bengara                                                                 |
|----------------------------------------|-------------------------------------------------------------------------|
| `php artisan ...`                      | `cargo artisan ...`（本番は `./myapp ...`）                             |
| `make:controller` などの生成コマンド   | **ありません。** ファイルは手で作ります                                 |
| `serve` はビルド不要                   | ビルドしてから起動します。ソースが変わると作り直します                  |
| 旗の扱いが緩い                         | **旗はコマンドごとに決まっています。** 知らない旗や余分な引数はエラー   |
| 同じ名前の自作コマンドが勝つことがある | **フレームワークのコマンドが必ず先。** 自作のほうは呼ばれず、警告が出る |
| `migrate --force`（本番で必要）        | `migrate` と `db:seed` は本番でも要りません（付けても通ります）         |
| `migrate:fresh` などは本番で確認する   | `--force` が無いと止まります（対象は 5 つ）                             |
| `schedule:run`                         | 同じ名前。**失敗が 1 件でもあれば終了コード 1**                         |

くわしくは [artisan.md](artisan.md) にあります。

## 設定

| Laravel                       | bengara                                           |
|-------------------------------|---------------------------------------------------|
| `config/app.php` が配列を返す | `config/app.rs` の `pub fn config() -> AppConfig` |
| `config('app.name')`          | `config::<AppConfig>().name`。**型で引きます**    |
| `env()` は文字列中心          | `env(key, default)` の型は**既定値の型**で決まる  |
| 設定ファイルを本番に置く      | 設定はコードなので実行ファイルに入る              |
| `config:cache` / `optimize`   | **要りません。** 起動時に組み立てて固定します     |
| サービスコンテナ・プロバイダ  | ありません。設定は型をキーにした保管だけ          |
| ファサード                    | `use bengara::prelude::*;` の関数を使う           |

くわしくは [configuration.md](configuration.md) にあります。

## ルーティングと URL

| Laravel                                             | bengara                                                          |
|-----------------------------------------------------|------------------------------------------------------------------|
| `Route::get('/', [HomeController::class, 'index'])` | `Route::get("/", HomeController::index)`                         |
| `/posts/{post}`                                     | 同じ。末尾の全取りは `/files/{*path}`                            |
| `route('posts.show', $post)`                        | `route_with("posts.show", &[("post", "12")])?`                   |
| パスに無い引数はクエリになる                        | 同じ（`/posts?page=2`）                                          |
| `route()` はパス引数をそのまま置く                  | **パーセント符号化します。** `/` も `%2F` になる                 |
| `Route::prefix(...)->group(...)`                    | `Route::prefix("admin").group(...)`。`name` の点は自分で書く     |
| `Route::middleware(...)`                            | `Route::get(...).middleware("admin")` と後ろに付ける             |
| ルートの重複は後勝ち                                | **起動時にパニック**（パスの重複、ルート名の重複）               |
| `URL::signedRoute(...)`                             | `signed_url("/unsubscribe", &[("user", "12")])?`。**パスを渡す** |
| `$request->hasValidSignature()`                     | `has_valid_signature(&req)?`                                     |
| ミドルウェア `signed` が自動で止める                | **自動では止まりません。** 自分で確かめて返し方を決める          |
| メソッドの大文字小文字を区別しない                  | **区別します。** `get /x` は 405                                 |
| 末尾の `/` は 301 で寄せる                          | **無視します。** `/posts` と `/posts/` は同じルート              |

くわしくは [routing.md](routing.md) にあります。

## コントローラとレスポンス

ハンドラの形は `async fn f() -> Result<Response>` と `async fn f(req: Request) -> Result<Response>` の 2 つだけです。

| Laravel                             | bengara                                                                      |
|-------------------------------------|------------------------------------------------------------------------------|
| 引数の型からコンテナが注入する      | **自動の注入はありません。** 形は上の 2 つだけ                               |
| `$request->route('post')`           | `req.param("post")`。型を変えるなら `req.param_as::<i64>("post")?`           |
| パス引数の型が合わない              | `param_as` は **404**。ルートに無い名前なら **500**                          |
| `view("welcome")`                   | **テンプレートはありません。** `html(...)` か JSON を返す                    |
| `response()->json($data)`           | `json(&data)`                                                                |
| `redirect('/login')`                | `redirect().to("/login")`                                                    |
| `abort(403)`                        | `return abort(403);`                                                         |
| 例外を投げる                        | `Err` を返す。状態コードを持つエラーならそのコードになる                     |
| 本文の上限は PHP の `post_max_size` | `.env` の `APP_MAX_BODY_BYTES`（既定 2 MiB）と、ルートの `.max_body(バイト)` |
| 本文を全部受け取ってから動く        | 同じ。ただし `.stream_body()` のルートは `req.body_stream()?` で少しずつ読む |

くわしくは [requests-and-responses.md](requests-and-responses.md) にあります。

## 入力とバリデーション

| Laravel                                  | bengara                                                |
|------------------------------------------|--------------------------------------------------------|
| `$request->input('title')`               | `req.input("title")`（`Option<String>`）               |
| `$request->validate([...])`              | `req.validate(&[("title", "required\|max:255")])?`     |
| 規則は配列でも文字列でも書ける           | **文字列だけ**                                         |
| `TrimStrings` で全部の入力の空白を落とす | **検査する値だけ**落とす。`req.input()` は落とさない   |
| `nullable` で飛ばした値はそのまま残る    | **空文字が入る**                                       |
| `alpha` 系は空文字を通す                 | **落とす**（`alpha` / `alpha_num` / `alpha_dash`）     |
| 落ちると例外 → リダイレクトか JSON       | 422。JSON を求めた相手には JSON で返す                 |
| `old('title')`                           | `req.session().old("title")`                           |
| `regex:` / `unique:` / `exists:`         | ありません。処理の中でクエリを書いて確かめる           |
| 重なりは `unique:` で止める              | 表の `unique` で止め、`is_unique_violation` で見分ける |
| フォームリクエスト                       | ありません                                             |

くわしくは [validation.md](validation.md) にあります。

## セッションと CSRF

| Laravel                                | bengara                                                     |
|----------------------------------------|-------------------------------------------------------------|
| `session(['k' => 'v'])`                | `req.session().put("k", "v")`                               |
| `session()->flash('k', 'v')`           | `req.session().flash("k", "v")`                             |
| 初めて来た人にもトークンを配る         | **`req.csrf_token()` を呼んだときだけ**作る                 |
| Cookie は暗号化される                  | **署名だけ。** 中身はサーバー側に置く                       |
| ログアウトのあと新しいセッションを配る | **Cookie を消す。** 空のセッションは作らない                |
| `@csrf` が隠しフィールドを出す         | `req.csrf_token()` を自分で埋める                           |
| `_token` をクエリ文字列からも読む      | **本文だけ**から読む（`X-CSRF-TOKEN` ヘッダーは同じく読む） |
| `VerifyCsrfToken::$except`             | `VerifyCsrfToken::new().except("/api/posts")`               |
| （無い）                               | `except_route("api.*")` でルート名で外せる。**勧める形**    |

- 外していいのは、Cookie を使わない API だけです。
- `__` で始まる鍵は内部用です。自分の値には使わないでください。

くわしくは [session.md](session.md) にあります。

## ミドルウェア・エラー・回数の制限

| Laravel                                 | bengara                                                 |
|-----------------------------------------|---------------------------------------------------------|
| `bootstrap/app.php` の `withExceptions` | `with_exceptions(\|e\| e.render(...))`                  |
| `throttle:60,1`                         | `Throttle::from_spec("60,1")?` を `alias` に登録する    |
| 制限はキャッシュで共有される            | **プロセスごとに数える**（2 プロセスなら上限は約 2 倍） |
| 数える単位                              | **相手とルート名の組**。名前が無ければ登録したパス      |
| `TrustProxies` ミドルウェア             | 環境変数 `TRUSTED_PROXIES`。範囲指定（CIDR）は書けない  |

くわしくは [middleware.md](middleware.md) にあります。

## データベース

| Laravel                                            | bengara                                                      |
|----------------------------------------------------|--------------------------------------------------------------|
| `DB::table('posts')->where('status', 'published')` | `DB::table("posts").where_("status", "published")`           |
| `->where('views', '>', 100)`                       | `.where_op("views", ">", 100)`                               |
| `->where(function ($q) {...})`                     | `.where_group(\|q\| ...)`                                    |
| `->orderBy()` に式を書ける                         | **列名だけ。** 式は `order_by_raw`                           |
| `->limit(1)->update([...])`                        | **エラー。** 黙って全件に当てないため                        |
| `->groupBy()` ＋ `->count()`                       | **グループの数**を返す                                       |
| `->increment('views')`                             | `.increment("views", 1)`（量は必ず書く）                     |
| `->paginate(15)` が `?page=` を見る                | `.paginate(15, req.page())`（**引数で渡す**）                |
| `->get()` が Collection を返す                     | `.get().await?` が `Vec<Row>` を返す                         |
| `$row->title`                                      | `row.get::<String>("title")?`                                |
| `DB::transaction(fn () => ...)`                    | `let tx = DB::begin().await?;` … `tx.commit().await?;`       |
| SQL Server にもつながる                            | **SQL Server 以外**（SQLite / MySQL / MariaDB / PostgreSQL） |
| 日時は `Carbon`                                    | **文字列**（`YYYY-MM-DD HH:MM:SS`、UTC）。`now()` で作る     |
| PostgreSQL の `timestamp` 列                       | **`text` 列**になる（日時は文字列のため）                    |
| `charset` / `collation` の指定                     | ありません。サーバーの既定に従う                             |

くわしくは [database.md](database.md) にあります。

## モデルとマイグレーション

| Laravel                                   | bengara                                              |
|-------------------------------------------|------------------------------------------------------|
| `class Post extends Model`                | `#[derive(Model)]` ＋ `#[model(table = "posts")]`    |
| 表の名前はクラス名から推測                | **書く。** 推測しない                                |
| 文字列の主キー（`$keyType = 'string'`）   | `derive` では作れない。`Model` を手で実装する        |
| `$post->comments` で自動的に読む          | `post.comments().get().await?`（遅延ロードはしない） |
| `with('comments')` でまとめ読み           | `where_in` で 2 回に分けて読む                       |
| `Post::create([...])`                     | 構造体を作って `post.save().await?`                  |
| `save()` が 0 件の更新でも成功を返す      | **`Err` になる**                                     |
| `save()` は変わった属性だけを書く         | **`created_at` 以外の全列を書き戻す**（下の注）      |
| `DB::transaction` の中で `$model->save()` | `post.save_using(&tx).await?`                        |
| `$casts` で型を変える                     | 構造体の宣言がそのまま型                             |
| `Post::factory()->count(3)->create()`     | `database/factories/` のただの関数                   |
| `up()` / `down()`                         | 同じ。ただし**同期の関数**                           |
| `$table->dropUnique(['email'])`           | `schema.drop_unique("users", &["email"])`            |
| `$table->string('title')->change()`       | ありません                                           |
| シーダーを `--database=` の接続に流す     | **既定の接続だけ**                                   |

**`save()` の書き戻しに気をつけてください。** 読んだ後に別の場所で増えた列（`views` など）は、
古い値に戻ります。変える列だけを書くなら、クエリビルダを使います（[models.md](models.md)）。

## 認証と認可

| Laravel                               | bengara                                                                    |
|---------------------------------------|----------------------------------------------------------------------------|
| `Hash::make($password)`（bcrypt）     | `Hash::make(password)`（PBKDF2-HMAC-SHA256）                               |
| 非同期の中でも `Hash::make`           | `Hash::make_async` / `Hash::check_async` を使う                            |
| `Hash::check` が自動で再ハッシュ      | `Hash::needs_rehash()` を自分で見る                                        |
| `Auth::attempt([...])`                | `req.auth().attempt::<User>("email", email, password).await?`              |
| `Auth::user()`                        | `req.auth().user::<User>().await?`。どこからでも読む形はない               |
| `Auth::logout()`                      | `req.auth().logout()?`（いまのセッションだけ）                             |
| `Auth::logoutOtherDevices($password)` | `req.auth().logout_other_devices_async().await?`。パスワードの確認は自分で |
| `auth` ミドルウェア                   | `Authenticate::new()` を `alias("auth", ..)` で登録する                    |
| `redirect()->intended('/')`           | `req.session().intended()` を読んで、自分で転送する                        |
| `class User extends Authenticatable`  | `#[derive(Model)]` ＋ `impl Authenticatable`                               |
| remember me の Cookie / 複数の guard  | ありません。セッション 1 本                                                |
| `$this->authorize('update', $post)`   | `authorize(PostPolicy::update(&user, &post))?`（ただの関数）               |
| `encrypt()` / `decrypt()`             | 同じ名前。機能フラグ `encryption` が要る                                   |
| `Password::sendResetLink()`           | `PasswordReset::create()` ＋ `link()`。送信は自分で書く                    |
| `APP_KEY` は base64 の 32 バイト      | **64 文字以上**の文字列                                                    |

くわしくは [authentication.md](authentication.md) と [authorization.md](authorization.md) にあります。

## テスト

| Laravel                       | bengara                                     |
|-------------------------------|---------------------------------------------|
| `php artisan test`            | `cargo test`                                |
| `$this->get('/')->assertOk()` | `get("/").await.assert_ok()`                |
| トレイトを `use` する         | `#[bengara::test]` を付けるだけ             |
| `RefreshDatabase`             | `let db = refresh_database().await;`        |
| `$this->seed()`               | `seed_database(&db).await;`                 |
| `$response->json()`           | `res.json()`。**JSON でない本文はパニック** |

くわしくは [testing.md](testing.md) にあります。

## 周辺の機能

| Laravel                                        | bengara                                                                                                |
|------------------------------------------------|--------------------------------------------------------------------------------------------------------|
| `Cache::remember('k', 60, fn() => ...)`        | `Cache::remember("k", 60, \|\| async { ... }).await?`                                                  |
| `Cache::increment` が期限を引き継ぐ            | **引き継がない。** 期限なしになる                                                                      |
| `Cache::tags([...])`                           | ありません。鍵に頭を付ける                                                                             |
| `Storage::disk('s3')`                          | `storage/` の下だけ                                                                                    |
| `Storage::url()` / `storage:link`              | ありません。`/storage/` ＋ パスの URL を自分で組み立てる（直接配信される）                             |
| `Storage::append('a.log', '行')`               | 同じ名前。挟む改行は**どの OS でも `\n`**                                                              |
| （改行を挟まずに書き足す）                     | `Storage::disk(名前).append_bytes(..)`                                                                 |
| `Storage::writeStream(..)`                     | `Storage::disk(名前).write_stream(パス, req.body_stream()?)`。**最後まで受け取れたときだけ**置き換わる |
| `SendWelcome::dispatch()`                      | `app/Jobs/SendWelcome.rs` の関数 ＋ `Queue::push("SendWelcome", &payload)`                             |
| `->onQueue()` / `->delay()`                    | `Queue::push_on` / `Queue::later` / `Queue::later_on`                                                  |
| `retry_after` を設定ファイルに書く             | `queue:work --retry-after=`（既定 90 秒）                                                              |
| ジョブの `public $timeout = 120;`              | `config/queue.rs` の `QueueConfig::default().timeout("ジョブ名", 120)`。無ければ `--timeout`           |
| `retry_after` を一番長いジョブより長くする     | **要りません。** 打ち切りの時間が長いジョブは、予約を延ばす                                            |
| 時間切れでワーカーごと落とす                   | **そのジョブだけ失敗にする。** ワーカーは次へ進む                                                      |
| Redis のキュー・キャッシュ                     | ありません。キューはデータベース、キャッシュはファイル                                                 |
| `->everyMinute()` / `->cron('0 3 * * 1')`      | `Every::Minute` などの列挙。cron の式は読まない                                                        |
| `schedule:work`                                | ありません。cron から `schedule:run` を 1 分ごとに呼ぶ                                                 |
| イベントクラス ＋ `EventServiceProvider`       | 名前は文字列。登録は `bootstrap/app.rs` の `with_events`                                               |
| `Mail::to(...)->send(new WelcomeMail)`         | `Mail::to(..).subject(..).text(..).send().await?`                                                      |
| 知らない `MAIL_MAILER` は例外                  | 送るときにエラー（`log` には落とさない）                                                               |
| `MAIL_ENCRYPTION=null`                         | `none` か `null`。**空にすると `starttls`**                                                            |
| `Notification`                                 | 作りません。`Mail` を使う                                                                              |
| `Process::run('ls -la')`                       | `Process::run(["ls", "-la"]).await?`。**シェルを通さない**（[processes.md](processes.md)）             |
| `Process::timeout(120)->run(..)`（既定 60 秒） | 同じ（既定 60 秒）。打ち切ると**子プロセスを止める**。待つ側がやめたときも同じ                         |
| `__('messages.welcome')`                       | 同じ（`__`）。`:name` の差し替えは `__with`                                                            |
| `App::setLocale()`                             | `Lang::set`（プロセス全体）。リクエストごとなら `Lang::with`                                           |
| 言語ファイルは PHP の配列                      | `resources/lang/<言語>.toml`。**ビルド時に読む**                                                       |
| `make:command` ＋ `$signature`                 | `app/Console/Commands/*.rs` に `DESCRIPTION` と `handle`。名前はファイル名から                         |

## WebSocket と配信

| Laravel                                                         | bengara                                                              |
|-----------------------------------------------------------------|----------------------------------------------------------------------|
| WebSocket は Reverb（別のプロセス）か Pusher などの外部サービス | **本体が受ける。** `Route::get` ＋ `req.websocket()?.on_upgrade(..)` |
| Pusher 互換のプロトコルと Laravel Echo                          | **ありません。** やり取りの形はアプリが決める                        |
| `routes/channels.php` でチャンネルの認可を書く                  | **ありません。** 切り替える前に確かめ、`socket.join` で入れる        |
| `broadcast(new OrderShipped($order))`（イベントのクラス）       | `Broadcast::publish("orders.1", 文字).await?`                        |
| `BROADCAST_CONNECTION`（`reverb` / `pusher` / `log` など）      | 同じ名前。値は `memory` / `database`。知らない名前は送るときにエラー |
| `ShouldBroadcast` でキューから送る                              | `publish` は直接送る。別のプロセスから送るなら `database`            |

くわしくは [websocket.md](websocket.md) にあります。

## パスを返す関数

| Laravel                                            | bengara                                                      |
|----------------------------------------------------|--------------------------------------------------------------|
| `app_path()` は `app/` を指す                      | **基準ディレクトリを指す。** `app_path("app/Models")` と書く |
| `base_path()` / `public_path()` / `storage_path()` | 同じ意味                                                     |

`app_path()` は名前だけ合わせています。移したコードで取り違えやすいところです。

## 本番で動かす

| Laravel                                  | bengara                                                     |
|------------------------------------------|-------------------------------------------------------------|
| `public/` を Web サーバーの公開先にする  | 置くのは実行ファイル・`.env`・書き込める `storage/` の 3 つ |
| `composer install --no-dev`              | 要りません。実行ファイル 1 つ                               |
| `storage/` を手で作る                    | `./myapp storage:init`（**起動時には作らない**）            |
| `php artisan about`                      | 同じ名前。**`APP_KEY` の値は出さない**                      |
| `php artisan down`（メンテナンスモード） | ありません。前段のプロキシで止める                          |
| php-fpm のプロセス管理                   | 自分で 2 プロセス起動し、前段で振り分ける                   |
| `migrate` の同時実行                     | **後から来たほうが止まる**                                  |
| `php artisan queue:restart`              | ありません。worker に停止の合図を送る                       |

くわしくは [deployment.md](deployment.md) にあります。

## PHP 固有の機能

PHP の言語や実行の形に結びついた機能は作りません。

| PHP 固有のもの                         | bengara                                   |
|----------------------------------------|-------------------------------------------|
| Blade のテンプレート文法               | 作りません                                |
| サービスコンテナによる依存の自動解決   | ありません。ハンドラの引数は 2 つの形だけ |
| マジックメソッド（`__get` / `__call`） | ありません。読み込みはいつも明示する      |
| 実行時に型や名前を調べる仕組み・`eval` | ありません                                |
| 緩い型比較（`==` の暗黙の変換）        | ありません。Rust の型を使う               |

借りられるものは、**Laravel と同じ名前のまま中身を Rust の形で作ります。**
自動検出・型で引く設定・`prelude` の関数がその例です。理由は [decisions.md](decisions.md) にあります。

## その他

- `GET` のルートは `HEAD` にも応答します。
- 本文の上限は既定 2 MiB です。`APP_MAX_BODY_BYTES` とルートの `.max_body(..)` で変えられます。超えると 413 を返します。
- ハンドラがパニックしても 500 を返すだけです。プロセスは落ちません。
- `APP_DEBUG=true` のときだけ、エラーページに詳細が出ます。
