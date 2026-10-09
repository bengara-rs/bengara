# Laravel と違う点

bengara は Laravel の構成と書き味に寄せています。ただし同じではありません。
このページは対応表です。 **今あるものの範囲**での違いを挙げます。
まだ無い機能は [backlog.md](backlog.md) にまとめてあります。

表の「同じ」は、Laravel から移してきたときに考え直さなくていいところです。

## 置き場所と参照

| Laravel                                    | bengara                                                          |
|--------------------------------------------|------------------------------------------------------------------|
| `public/index.php` が入口                  | 直下の `main.rs`（中身は `bengara::app!();`）                    |
| `artisan`（PHP スクリプト）                | 直下の `artisan.rs`（中身は `fn main() { bengara::artisan() }`） |
| `src/` は無い                              | **`src/` は作りません。** Laravel の構成に合わせました           |
| Composer のオートロード                    | `build.rs` の `bengara_build::discover()` が自動検出             |
| `use App\Http\Controllers\HomeController;` | `use crate::app::http::controllers::HomeController;`             |
| 名前空間は PSR-4                           | ディレクトリ名は snake_case のモジュールになります               |
| クラス名 = ファイル名                      | 大文字始まりのファイルには**同名の型を 1 つ**定義します          |

## コマンド

| Laravel                              | bengara                                                       |
|--------------------------------------|---------------------------------------------------------------|
| `php artisan ...`                    | `cargo artisan ...`                                           |
| `route:list`                         | 同じ                                                          |
| `make:controller` などの生成コマンド | **ありません。** 基本的なコードは AI で生成できます            |
| `serve` はビルド不要                 | ビルドしてから起動します。変更があれば作り直します            |
| `serve` が見張る対象は広い           | `.rs` と `resources/lang/` の `.toml` だけです                |
| コマンドの旗は共通で緩い             | **旗はコマンドごとに分かれています。** 知らない旗はエラーです  |

## 設定

| Laravel                           | bengara                                                      |
|-----------------------------------|--------------------------------------------------------------|
| `config/app.php` が配列を返す     | `config/app.rs` が `pub fn config() -> AppConfig` を返す     |
| `config('app.name')` の文字列キー | `config::<AppConfig>()` の**型キー**。文字列キーはありません |
| `env()` は文字列中心              | `env(key, default)` は**既定値の型**で決まります             |
| 設定ファイルを本番に置く          | 設定はコードなので実行ファイルに入ります                     |
| `config:cache` / `optimize`       | **要りません。** 起動時に組み立てて固定します                |
| サービスコンテナ・プロバイダ      | ありません。設定は型をキーにした保管だけです                 |
| ファサード                        | ありません。`use bengara::prelude::*;` の自由関数を使います  |

## ルーティングと URL

| Laravel                                             | bengara                                                                               |
|-----------------------------------------------------|---------------------------------------------------------------------------------------|
| `Route::get('/', [HomeController::class, 'index'])` | `Route::get("/", HomeController::index)`                                              |
| `/posts/{post}`                                     | 同じ。末尾の全取りは `/files/{*path}`                                                 |
| `route('posts.show', $post)`                        | `route_with("posts.show", &[("post", "12")])?` を使います                             |
| 可変長引数が使える                                  | ありません。引数はスライスで渡します                                                  |
| `route()` はパス引数をそのまま置く                  | **パーセント符号化します。** `/` も `%2F` になります                                  |
| `Route::group` / `prefix`                           | `Route::prefix("admin").group(...)`。`name` の点は自分で書きます                      |
| `Route::middleware(...)`                            | `Route::get(...).middleware("admin")` と後ろに付けます                                |
| ルートの重複は後勝ち                                | **起動時にパニックします**（パスの重複、ルート名の重複）                              |
| `URL::signedRoute(...)`                             | `signed_url("/unsubscribe", &[("user", "12")])?` — **ルート名ではなくパス**を渡します |
| `$request->hasValidSignature()`                     | `has_valid_signature(&req)?`                                                          |
| ミドルウェア `signed` が自動で止める                | **止まりません。** 自分で確かめて返し方を決めます                                     |

`signed_url()` は **URL にそのまま書けない文字**を含むパスを断ります（空白・`?`・`#`・
`<`・`>`・`\`・非 ASCII など）。符号化済みのパスを渡してください。

**末尾の `/` は無視します。** `/posts` と `/posts/` は同じルートに当たります。
Laravel は 301 で寄せますが、bengara は転送せずそのまま処理します。

## コントローラとレスポンス

受け付けるハンドラの形は `async fn f() -> Result<Response>` と
`async fn f(req: Request) -> Result<Response>` の 2 つだけです。

| Laravel                        | bengara                                                                                  |
|--------------------------------|------------------------------------------------------------------------------------------|
| 引数の型からコンテナが注入する | **自動解決はありません。** 受け付ける形は上の 2 つだけです                               |
| `Request` から何でも取れる     | メソッドは決まった一覧だけです（[requests-and-responses.md](requests-and-responses.md)） |
| `view("welcome")`              | **テンプレートはありません。** `html(...)` か JSON を返します                            |
| `response()->json($data)`      | `json(&data)` — `Result<Response>`                                                       |
| `redirect('/login')`           | `redirect().to("/login")` — `Result<Response>`                                           |
| `abort(403)`                   | `return abort(403);` と書きます                                                          |
| 例外を投げる                   | `Err` を返します。`Error::Http` ならそのステータスです                                   |

## 入力とバリデーション

| Laravel                                  | bengara                                                   |
|------------------------------------------|-----------------------------------------------------------|
| `$request->input('title')`               | `req.input("title")` — `Option<String>`                   |
| `$request->validate([...])`              | `req.validate(&[("title", "required\|max:255")])?`        |
| 規則は配列でも文字列でも書ける           | **文字列だけ**です                                        |
| `min` / `max` / `between` の決め方       | 同じ。`numeric` か `integer` が付いた項目だけ値で比べます |
| 空文字の扱い                             | 同じ。飛ばすのは未送信のときと `nullable` のときだけです  |
| 落ちると例外 → リダイレクトか JSON       | 落ちると `Error::Validation` → 422。`Accept` で分かれます |
| `old('title')`                           | `req.session().old("title")`                              |
| `regex:` / `unique:` / `exists:`         | ありません。`exists()` をクエリで書きます                 |
| 重なりは `unique:` で止める              | 表の `unique` が砦です。`is_unique_violation` で見分けます |
| フォームリクエスト（`StorePostRequest`） | ありません                                                |

## セッションと CSRF

| Laravel                                   | bengara                                                    |
|-------------------------------------------|------------------------------------------------------------|
| `session(['k' => 'v'])`                   | `req.session().put("k", "v")`                              |
| `session()->flash('k', 'v')`              | `req.session().flash("k", "v")`                            |
| `SESSION_DRIVER=file`（既定）             | 同じ。`memory` も選べます（テスト用）                      |
| 毎リクエスト保存して期限を延ばす          | **残りが半分を切ったときだけ**書き直します                 |
| 同じ鍵の `put` と `flash` は後勝ち        | 同じ。`flash` を先に見ます                                 |
| Cookie は暗号化される                     | **署名だけ。** 中身はサーバー側に置きます                  |
| `regenerate()` が CSRF トークンも作り直す | 同じ。ログイン前のトークンはログイン後に使えません         |
| `@csrf` が隠しフィールドを出す            | テンプレートが無いので `req.csrf_token()` を自分で埋めます |
| `X-CSRF-TOKEN` ヘッダー                   | 同じ                                                       |
| 失敗すると 419                            | 同じ                                                       |
| `VerifyCsrfToken::$except`                | `VerifyCsrfToken::new().except("/api/posts")`              |
| （Laravel には無い）                      | `except_route("api.*")` でルート名で外せます。**勧める形** |

**`except` に `/api/*` のようなまとめ書きをしないでください。**
セッションは全ルートに掛かるので、`/api/*` も Cookie で認証されます。
外していいのは Cookie を使わない API だけです。

## ミドルウェア・エラー・回数の制限

| Laravel                                 | bengara                                                   |
|-----------------------------------------|-----------------------------------------------------------|
| 共通のミドルウェアが 404 にも掛かる     | 同じ。ただしルートが無いときは CSRF を確かめません        |
| `bootstrap/app.php` の `withExceptions` | `with_exceptions(\|e\| e.render(...))`                    |
| `throttle:60,1`                         | `Throttle::from_spec("60,1")?` を `alias` に登録します    |
| 制限はキャッシュで共有される            | **プロセスごとに数えます**（2 プロセスなら上限は約 2 倍） |
| 数えるのは「人かアドレス」               | 同じ。ログイン中はその人、していなければ接続元のアドレス  |
| 数える単位はルート                      | **ルート名**と組にします。名前が無いときだけパス           |
| `X-Forwarded-For` を見る                | **下の行の相手からのものだけ**見ます                      |
| `TrustProxies` ミドルウェア             | 環境変数 `TRUSTED_PROXIES`。範囲指定（CIDR）は書けません  |
| 転送元は右端から探す                    | 同じ。読めない値は捨てます                                |

## データベース

| Laravel                                            | bengara                                                      |
|----------------------------------------------------|--------------------------------------------------------------|
| `DB::table('posts')->where('status', 'published')` | `DB::table("posts").where_("status", "published")`           |
| `->where('views', '>', 100)`                       | `.where_op("views", ">", 100)`（引数の数では分けられません） |
| `->where(function ($q) {...})`                     | `.where_group(\|q\| ...)`                                    |
| `->orderBy()` に式を書ける                         | **列名は英数字と `_` `.` だけ。** 式は `order_by_raw` です   |
| `->limit(1)->update([...])` が効く                 | **エラーになります。** 黙って全件に当てないためです          |
| `->groupBy()` ＋ `->count()`                       | **グループの数**を返します                                   |
| `->groupBy()` ＋ `->sum()` など                    | **併用できません**（エラー）                                 |
| `->increment('views')`                             | `.increment("views", 1)`（量は必ず書きます）                 |
| `->paginate(15)` が `?page=` を見る                | `.paginate(15, req.page())`（**引数で渡します**）            |
| `->get()` が Collection を返す                     | `.get().await?` が `Vec<Row>` を返します                     |
| `$row->title`                                      | `row.get::<String>("title")?`                                |
| `DB::transaction(fn () => ...)`                    | `let tx = DB::begin().await?;` … `tx.commit().await?;`       |
| MySQL / PostgreSQL / SQLite / SQL Server           | **SQLite だけ**（機能フラグ `sqlite`）                       |
| 日時は `Carbon`                                    | **文字列**（`YYYY-MM-DD HH:MM:SS`、UTC）。`now()` で作ります |

`update` / `delete` / `truncate` に `join` / `limit` / `offset` / `group_by` / `having` /
`distinct` を付けるとエラーです。`order_by` だけは黙って無視します。
`truncate` に条件（`where_`）を付けるのもエラーです（絞って消すなら `delete()`）。

## モデルとマイグレーション

| Laravel                                    | bengara                                                           |
|--------------------------------------------|-------------------------------------------------------------------|
| `class Post extends Model`                 | `#[derive(Model)]` ＋ `#[model(table = "posts")]`                 |
| 表の名前はクラス名から推測                 | **書きます。** 推測しません                                       |
| `$keyType = 'string'`（文字列の主キー）    | **使えません**（コンパイルエラー）。`Model` を手で実装します      |
| `$post->comments` で自動的に読む           | `post.comments().get().await?`（遅延ロードはありません）          |
| `with('comments')` でまとめ読み            | `where_in` で 2 回に分けます                                      |
| `Post::create([...])`                      | 構造体を作って `post.save().await?`                               |
| `DB::transaction` の中で `$model->save()`  | `post.save_using(&tx).await?` を使います                          |
| `$casts` で型を変える                      | 構造体の宣言がそのまま型です                                      |
| `Post::factory()->count(3)->create()`      | `database/factories/` のただの関数                                |
| `up()` / `down()` は `Schema::create(...)` | 同じ。ただし**同期の関数**です                                    |
| `$table->string('title')->change()`        | **ありません**（SQLite が苦手なため）                             |
| `php artisan migrate`                      | `cargo artisan migrate`（本番は `./myapp migrate`）               |

`#[derive(Model)]` が扱えるのは **自動採番の整数の主キー**だけです。

## 認証と認可

| Laravel                                            | bengara                                                                   |
|----------------------------------------------------|---------------------------------------------------------------------------|
| `Hash::make($password)`                            | `Hash::make(password)`（PBKDF2-HMAC-SHA256。bcrypt ではありません）       |
| `Hash::make` をそのまま呼ぶ                        | 非同期の中では `Hash::make_async` / `Hash::check_async` を使います        |
| `Hash::check` が自動で再ハッシュ                   | `Hash::needs_rehash()` を自分で見ます                                     |
| `Auth::attempt(['email' => .., 'password' => ..])` | `req.auth().attempt::<User>("email", email, password).await?`             |
| `Auth::user()` / `Auth::id()` / `Auth::check()`    | `req.auth().user::<User>().await?` など。どこからでも読める形はありません |
| `Auth::logout()`                                   | `req.auth().logout()?`（いまのセッションだけ）                            |
| `Auth::logoutOtherDevices($password)`              | `req.auth().logout_other_devices_async().await?`。パスワードの確認は自分で行います |
| （利用者を指定して切る口は無い）                   | `logout_all_devices()` / `logout_user(id)` があります                     |
| `auth` ミドルウェア                                | `Authenticate::new()` を `alias("auth", ..)` で登録します                 |
| `class User extends Authenticatable`               | `#[derive(Model)]` ＋ `impl Authenticatable`                              |
| remember me の Cookie                              | **ありません**                                                            |
| 複数の guard                                       | **ありません。** セッション 1 本です                                      |
| `Gate::allows('update', $post)`                    | `PostPolicy::update(&user, &post)`（ただの関数）                          |
| `$this->authorize('update', $post)`                | `authorize(PostPolicy::update(&user, &post))?`                            |
| ポリシーの自動対応づけ                             | **ありません。** 呼ぶ側で関数を指定します                                 |
| `encrypt()` / `decrypt()`                          | 同名。機能フラグ `encryption` が要ります（ChaCha20-Poly1305）             |
| `Password::sendResetLink()`                        | `PasswordReset::create()` ＋ `link()`。送信は自分で書きます               |
| トークンの照合                                     | `PasswordReset::consume_if_valid(email, token)`。**1 回しか使えません**   |
| `APP_KEY` は base64 の 32 バイト                   | **64 文字以上**の文字列。用途ごとに別の鍵を作ります                       |

## テスト

| Laravel                         | bengara                                                              |
|---------------------------------|----------------------------------------------------------------------|
| `php artisan test` / PHPUnit    | `cargo test`                                                         |
| `$this->get('/')->assertOk()`   | `get("/").await.assert_ok()`                                         |
| トレイトを使う宣言が必要        | `#[bengara::test]` が道具を注入します。`use` は不要です              |
| `RefreshDatabase` トレイト      | `let db = refresh_database().await;`                                 |
| `$this->seed()`                 | `seed_database(&db).await;`（`refresh_database()` の戻りを渡します） |
| HTTP サーバーを立てる構成もある | ソケットを開きません。接続元は `127.0.0.1` 扱いです                  |

## 周辺の機能

| Laravel                                   | bengara                                                                  |
|-------------------------------------------|--------------------------------------------------------------------------|
| `Cache::remember('k', 60, fn() => ...)`   | `Cache::remember("k", 60, \|\| async { ... }).await?`                    |
| `Cache::increment` が期限を引き継ぐ       | **引き継ぎません。** 期限なしで書き直します                              |
| `Cache::tags([...])`                      | ありません。鍵に頭を付けます                                             |
| （期限切れの掃除コマンドは無い）          | `Cache::prune()` と `cargo artisan cache:prune`                          |
| `Storage::disk('s3')`                     | `storage/` の下だけです。S3 はありません                                 |
| `Storage::url($path)` / `storage:link`    | ありません。`/storage/` ＋ パスを組み立てます                            |
| ジョブのクラス＋`SendWelcome::dispatch()` | `app/Jobs/SendWelcome.rs` の関数＋`Queue::push("SendWelcome", &payload)` |
| `$job->handle()` に型つきの引数           | `handle(payload: String)` の 1 つだけです                                |
| `config/queue.php` の `retry_after`       | `WorkerOptions` と `queue:work --retry-after=`（既定 90 秒）             |
| `Redis` のキュー・キャッシュ              | ありません。キューは DB、キャッシュはファイルです                        |
| `->everyMinute()` / `->cron('0 3 * * 1')` | `Every::Minute` などの列挙。cron の式は読みません                        |
| 間隔は分の頭で判定する                    | 前回から数えます。**30 秒の遅れは許します**（間隔が 1 分以上のとき）     |
| `schedule:work`                           | ありません。cron から `schedule:run` を 1 分ごとに呼びます               |
| 前回の実行時刻はキャッシュに置く          | `storage/framework/schedule/` のファイルに置きます                       |
| イベントクラス＋`EventServiceProvider`    | 名前は文字列。登録は `bootstrap/app.rs` の `with_events`                 |
| `Mail::to(...)->send(new WelcomeMail)`    | `Mail::to(..).subject(..).text(..).send().await?`                        |
| SMTP での送信                             | **ありません。** `log` と `array` だけです                               |
| `Notification`                            | 作りません。`Mail` を直接使います                                        |
| `__('messages.welcome')`                  | 同じ名前（`__`）。`:name` の差し替えは `__with`                          |
| `App::setLocale()`                        | `Lang::set`（プロセス全体）。リクエストごとに変えるなら `Lang::with`     |
| 言語ファイルは PHP の配列                 | `resources/lang/<言語>.toml` か `<言語>/<群>.toml`。**ビルド時に読む**   |
| `trans_choice`（複数形）                  | ありません。鍵を分けます                                                 |
| `make:command` ＋ `$signature`            | `app/Console/Commands/*.rs` に `DESCRIPTION` と `handle`                 |
| コマンド名は自分で書く                    | ファイル名から作ります。使える文字は英数字と `_` `-` だけ                |

## パスを返す関数

| Laravel                              | bengara                                                                 |
|--------------------------------------|-------------------------------------------------------------------------|
| `app_path()` は `app/` を指す        | **基準ディレクトリからの相対パス**。`app_path("app/Models")` と書きます |
| `base_path()` はプロジェクトのルート | 同じ                                                                    |
| `public_path()` / `storage_path()`   | 同じ意味                                                                |

`app_path()` は名前だけ合わせてあります。指す場所が違うので、移したコードでは取り違えやすいところです。

## 本番で動かす

| Laravel                                  | bengara                                                         |
|------------------------------------------|-----------------------------------------------------------------|
| `public/` を Web サーバーの公開先にする  | 置くのは実行ファイル・`.env`・書き込める `storage/` の 3 つです |
| `public/` のファイルを配信する           | リリースビルドでは**実行ファイルの中**に入ります                |
| `php artisan storage:link`               | **ありません。** `/storage/...` を直接配信します                |
| `php artisan about`                      | 同じ名前。**`APP_KEY` の値は出しません**                        |
| `php artisan down`（メンテナンスモード） | ありません。前段のプロキシで止めてください                      |
| `php artisan optimize`                   | **要りません。** 設定は起動時に固定されます                     |
| php-fpm のプロセス管理                   | 自分で 2 プロセス起動し、前段で振り分けます                     |
| `composer install --no-dev`              | 要りません。実行ファイル 1 つです                               |
| `storage/` を手で `mkdir`                | `cargo artisan storage:init`（**起動時には作りません**）        |
| `migrate` の同時実行                     | **後から来たほうが止まります**（`migration_locks` 表の札）      |
| `php artisan queue:restart`              | ありません。worker に停止の合図を送ってください                 |

## PHP 固有の機能は作らない

PHP の言語や実行の形に強く結びついた機能は作りません。
Rust では原理的に作れないものがあるためです。

| PHP 固有のもの                                     | bengara                                       |
|----------------------------------------------------|-----------------------------------------------|
| Blade のテンプレート文法                           | 作りません。Rust のエンジンは当面入れません   |
| 実行時に型や名前を調べる仕組み                     | ありません。Rust に無いためです               |
| サービスコンテナによる依存の自動解決               | ありません。ハンドラの引数は 2 つの形だけです |
| マジックメソッド（`__get` / `__call`）の遅延ロード | ありません。読み込みはいつも明示します         |
| `eval` 相当・実行時のコード生成                    | ありません                                    |
| 緩い型比較（`==` の暗黙の変換）                    | ありません。Rust の型を使います               |

次のものは **同じ役割を別の形で用意しています。**

| PHP の仕組み                              | bengara                                         |
|-------------------------------------------|-------------------------------------------------|
| Composer のオートロード                   | `build.rs` の自動検出。ビルド時に宣言を作ります |
| 連想配列を前提にした API（`config(...)`） | 型をキーにする `config::<AppConfig>()`          |
| グローバル名前空間のヘルパ関数            | `use bengara::prelude::*;` の自由関数           |

「作らない」と決める前に、名前と考え方だけ借りられないか、
Rust では別の形のほうが自然でないかを毎回検討しています。
借りられるものは **Laravel と同じ名前のまま中身を Rust の形で作ります。**

## その他

- `GET` のルートは `HEAD` にも応答します（Laravel と同じ考え方）。
- 405 の `Allow` には、`GET` があるとき `HEAD` も入ります。
- 本文の上限は 2 MiB です。超えると 413 を返します。
- ハンドラがパニックしても 500 を返すだけです。プロセスは落ちません。
- `APP_DEBUG=true` のときだけエラーページに詳細が出ます（Laravel と同じ考え方）。
