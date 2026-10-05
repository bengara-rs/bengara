# Laravel と違う点

bengara は Laravel の構成と書き味に寄せていますが、同じではありません。
ここでは **今あるものの範囲**での違いだけを挙げます。まだ無い機能は [backlog.md](backlog.md) にまとめてあります。

## ファイルの置き場所

| Laravel                               | bengara                                                                 |
|---------------------------------------|-------------------------------------------------------------------------|
| `public/index.php` が入口             | 入口はプロジェクト直下の `main.rs`（中身は `bengara::app!();`）         |
| `artisan`（PHP スクリプト）           | 直下の `artisan.rs`（中身は `fn main() { bengara::artisan() }`）        |
| `app/` 以下は Composer のオートロード | `build.rs` の `bengara_build::discover()` が自動検出                    |
| （`src/` は無い）                     | **`src/` は作らない。** Rust の慣習とは違うが、Laravel の構成に合わせた |
| `.php`                                | `.rs`                                                                   |

## 参照のしかた

| Laravel                                    | bengara                                                                                    |
|--------------------------------------------|--------------------------------------------------------------------------------------------|
| `use App\Http\Controllers\HomeController;` | `use crate::app::http::controllers::HomeController;`                                       |
| 名前空間は PSR-4                           | ディレクトリ名は snake_case のモジュールになる（`Http/Controllers` → `http::controllers`） |
| クラス名 = ファイル名                      | 大文字始まりのファイルには**同名の型を 1 つ**定義する                                      |

## コマンド

| Laravel                                  | bengara                                                      |
|------------------------------------------|--------------------------------------------------------------|
| `php artisan ...`                        | `cargo artisan ...`                                          |
| `make:controller` など生成コマンドが豊富 | **`make:*` は無い**。基本的なコードは AI で生成できるため    |
| `route:list`                             | `route:list`（同じ）                                         |
| `serve` はビルド不要                     | `serve` はビルドしてから起動し、変更があれば再ビルド・再起動 |

## 設定

| Laravel                           | bengara                                                                                 |
|-----------------------------------|-----------------------------------------------------------------------------------------|
| `config/app.php` が配列を返す     | `config/app.rs` が `pub fn config() -> AppConfig` を返す                                |
| `config('app.name')` の文字列キー | `config::<AppConfig>()` の**型キー**。文字列キーは無い                                  |
| `env()` は文字列中心              | `env(key, default)` は**既定値の型**で決まる（`String` `bool` `PathBuf` 整数 浮動小数） |
| 設定ファイルを本番に置く          | 設定はコードなのでバイナリに入る。本番に置くファイルは無い                              |
| `config:cache`                    | 不要（起動時に組み立てて固定する）                                                      |

## ルーティング

| Laravel                                             | bengara                                                                                    |
|-----------------------------------------------------|--------------------------------------------------------------------------------------------|
| `Route::get('/', [HomeController::class, 'index'])` | `Route::get("/", HomeController::index)`                                                   |
| `/posts/{post}`                                     | `/posts/{post}`（同じ）。末尾の全取りは `/files/{*path}`                                   |
| `route('posts.show', $post)`                        | `route_with("posts.show", &[("post", "12")])` — **`Result<String>` を返す**ので `?` が必要 |
| 可変長引数が使える                                  | Rust に可変長引数が無いので、引数はスライスで渡す                                          |
| `Route::group` / `prefix`                           | `Route::prefix("admin").group(                                                             || { ... })`。`name` の点は自分で書く |
| `Route::middleware(...)`                            | `Route::get(...).middleware("admin")` と後ろに付ける（[middleware.md](middleware.md)）     |
| ルートの重複は実行時に後勝ちなど                    | **起動時にパニックする**（メソッド＋パスの重複、ルート名の重複）                           |

## コントローラ

| Laravel                                | bengara                                                                                                                              |
|----------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------|
| 引数の型からコンテナが解決して注入する | **自動解決は無い。** 受け付ける形は `async fn f() -> Result<Response>` と `async fn f(req: Request) -> Result<Response>` の 2 つだけ |
| `Request` から何でも取れる             | `Request` のメソッドは決まった一覧のみ（[requests-and-responses.md](requests-and-responses.md)）                                     |
| `view("welcome")`                      | **テンプレートは作りません。** `html(...)` に文字列を渡すか、JSON を返して画面は別で作る                                             |

## レスポンス

| Laravel                   | bengara                                                           |
|---------------------------|-------------------------------------------------------------------|
| `response()->json($data)` | `json(&data)` — `Result<Response>`                                |
| `redirect('/login')`      | `redirect().to("/login")` — `Result<Response>`                    |
| `abort(403)`              | `abort(403)` — `Result<T>` を返すので `return abort(403);` と書く |
| 例外を投げる              | `Err` を返す。`Error::Http` ならそのステータス、それ以外は 500    |

## 入力とバリデーション

| Laravel                                  | bengara                                                      |
|------------------------------------------|--------------------------------------------------------------|
| `$request->input('title')`               | `req.input("title")` — `Option<String>` が返る               |
| `$request->validate([...])`              | `req.validate(&[("title", "required                          |max:255")])?` |
| 規則は配列でも文字列でも書ける           | **文字列だけ**（`"required\|max:255"`）                      |
| 落ちると例外 → リダイレクトか JSON       | 落ちると `Error::Validation` → 422。`Accept` で JSON か HTML |
| `old('title')`                           | `req.session().old("title")`                                 |
| `regex:` / `unique:` / `exists:`         | まだ無い                                                     |
| フォームリクエスト（`StorePostRequest`） | まだ無い                                                     |

## セッションと CSRF

| Laravel                        | bengara                                                       |
|--------------------------------|---------------------------------------------------------------|
| `session(['k' => 'v'])`        | `req.session().put("k", "v")`                                 |
| `session()->flash('k', 'v')`   | `req.session().flash("k", "v")`                               |
| `SESSION_DRIVER=file`（既定）  | 同じ。`memory` も選べる（テスト用）                           |
| Cookie は暗号化される          | **署名だけ。** 中身はサーバー側に置くので、ブラウザには出ない |
| `@csrf` が隠しフィールドを出す | テンプレートが無いので、`req.csrf_token()` を自分で埋める     |
| `X-CSRF-TOKEN` ヘッダー        | 同じ                                                          |
| 失敗すると 419                 | 同じ                                                          |
| `VerifyCsrfToken::$except`     | `VerifyCsrfToken::new().except("/api/*")`                     |

## URL と署名

| Laravel                              | bengara                                                                           |
|--------------------------------------|-----------------------------------------------------------------------------------|
| `url('/posts')`                      | `url("/posts")`（同じ）                                                           |
| `URL::signedRoute(...)`              | `signed_url("/unsubscribe", &[("user", "12")])?` — **ルート名ではなくパス**を渡す |
| `URL::temporarySignedRoute(...)`     | `temporary_signed_url(path, params, secs)?`                                       |
| `$request->hasValidSignature()`      | `has_valid_signature(&req)?`                                                      |
| ミドルウェア `signed` が自動で止める | **自動では止まらない。** 自分で確かめて返し方を決める                             |

## エラーと制限

| Laravel                                 | bengara                                                 |
|-----------------------------------------|---------------------------------------------------------|
| `bootstrap/app.php` の `withExceptions` | `with_exceptions(\|e\| e.render(...))`                  |
| `throttle:60,1`                         | `Throttle::from_spec("60,1")?` を `alias` に登録        |
| 制限はキャッシュで共有される            | **プロセスごとに数える**（2 プロセスなら上限は約 2 倍） |

## データベース

| Laravel                                            | bengara                                                        |
|----------------------------------------------------|----------------------------------------------------------------|
| `DB::table('posts')->where('status', 'published')` | `DB::table("posts").where_("status", "published")`             |
| `->where('views', '>', 100)`                       | `.where_op("views", ">", 100)`（**引数の数では分けられない**） |
| `->where(function ($q) {...})`                     | `.where_group(\|q\| ...)`                                      |
| `->paginate(15)` が `?page=` を見る                | `.paginate(15, req.page())`（**引数で渡す**）                  |
| `->get()` が Collection を返す                     | `.get().await?` が `Vec<Row>` を返す                           |
| `$row->title`                                      | `row.get::<String>("title")?`                                  |
| MySQL / PostgreSQL / SQLite / SQL Server           | **SQLite だけ**（機能フラグ `sqlite`）                         |
| `DB::transaction(fn () => ...)`                    | `let tx = DB::begin().await?;` … `tx.commit().await?;`         |
| 日時は `Carbon`                                    | **文字列**（`YYYY-MM-DD HH:MM:SS`、UTC）。`now()` で作る       |

## モデルとマイグレーション

| Laravel                                    | bengara                                                |
|--------------------------------------------|--------------------------------------------------------|
| `class Post extends Model`                 | `#[derive(Model)]` ＋ `#[model(table = "posts")]`      |
| 表の名前はクラス名から推測                 | **書く。** 推測しません                                |
| `$post->comments` で自動的に読む           | `post.comments().get().await?`（**遅延ロードは無い**） |
| `with('comments')` でまとめ読み            | `where_in` ＋ `group_by` で2回に分ける                 |
| `Post::create([...])`                      | 構造体を作って `post.save().await?`                    |
| `$casts` で型を変える                      | 構造体の宣言がそのまま型                               |
| `Post::factory()->count(3)->create()`      | `database/factories/` のただの関数                     |
| `up()` / `down()` は `Schema::create(...)` | 同じ。ただし**同期の関数**                             |
| `$table->string('title')->change()`        | **無い**（SQLite が苦手なため）                        |
| `unique:posts,title` のバリデーション規則  | **無い。** `exists()` をクエリで書く                   |
| `php artisan migrate`                      | `cargo artisan migrate`（本番は `./myapp migrate`）    |
| `RefreshDatabase` トレイト                 | `let _db = refresh_database().await;`                  |

## 認証と認可

| Laravel                                            | bengara                                                                       |
|----------------------------------------------------|-------------------------------------------------------------------------------|
| `Hash::make($password)`                            | `Hash::make(password)`（中身は PBKDF2-HMAC-SHA256。bcrypt ではない）          |
| `Auth::attempt(['email' => .., 'password' => ..])` | `req.auth().attempt::<User>("email", email, password).await?`                 |
| `Auth::user()`                                     | `req.auth().user::<User>().await?`（**どこからでも読める形は無い**）          |
| `Auth::id()` / `Auth::check()`                     | `req.auth().id()` / `req.auth().check()`                                      |
| `Auth::logout()`                                   | `req.auth().logout()?`                                                        |
| `auth` ミドルウェア                                | `Authenticate::new()` を `alias("auth", ..)` で登録                           |
| `class User extends Authenticatable`               | `#[derive(Model)]` ＋ `impl Authenticatable`（`password_hash()` の 1 つだけ） |
| remember me の Cookie                              | **無い**                                                                      |
| 複数の guard                                       | **無い。** セッション 1 本                                                    |
| `Gate::allows('update', $post)`                    | `PostPolicy::update(&user, &post)`（ただの関数）                              |
| `$this->authorize('update', $post)`                | `authorize(PostPolicy::update(&user, &post))?`                                |
| ポリシーの自動対応づけ                             | **無い。** 呼ぶ側で関数を指定する                                             |
| `encrypt()` / `decrypt()`                          | 同名。ただし機能フラグ `encryption` が要る。中身は ChaCha20-Poly1305          |
| `Password::sendResetLink()`                        | `PasswordReset::create()` ＋ `link()`。**メールの送信はまだ無い**             |
| `Hash::check` が自動で再ハッシュ                   | `Hash::needs_rehash()` を自分で見る                                           |

## 読み込みのしかた

| Laravel                              | bengara                                                               |
|--------------------------------------|-----------------------------------------------------------------------|
| 必要になってから読む（遅延ロード）   | **遅延ロードは無い。** ルートと設定は起動時にすべて組み立てて固定する |
| モデルのリレーションも遅延ロード     | **しない。** `await` を書いたときだけ問い合わせが走る                 |
| サービスコンテナ・サービスプロバイダ | 無い。設定は型をキーにした保管だけ                                    |
| ファサード                           | 無い。`use bengara::prelude::*;` の自由関数を使う                     |

## パスを返す関数

| Laravel                              | bengara                                                                                                  |
|--------------------------------------|----------------------------------------------------------------------------------------------------------|
| `app_path()` は `app/` を指す        | **`app_path(rel)` は基準ディレクトリからの相対パス**。`app/` は指さない。`app_path("app/Models")` と書く |
| `base_path()` はプロジェクトのルート | `base_path()`（同じ）                                                                                    |
| `public_path()` / `storage_path()`   | 同じ意味                                                                                                 |

名前は Laravel に合わせましたが、`app_path()` だけは指す場所が違います。
Laravel から移してきたコードは、ここで取り違えやすいので注意してください。

## テスト

| Laravel                         | bengara                                           |
|---------------------------------|---------------------------------------------------|
| `php artisan test` / PHPUnit    | `cargo test`                                      |
| `$this->get('/')->assertOk()`   | `get("/").await.assert_ok()`                      |
| トレイトを使う宣言が必要        | `#[bengara::test]` が道具を注入する。`use` は不要 |
| HTTP サーバーを立てる構成もある | ソケットを開かない                                |

## 周辺の機能

| Laravel                                   | bengara                                                      |
|-------------------------------------------|--------------------------------------------------------------|
| `Cache::remember('k', 60, fn() => ...)`   | `Cache::remember("k", 60, \|\| async { ... }).await?`         |
| `Cache::tags([...])`                      | ありません。鍵に頭を付けます                                 |
| `Storage::disk('s3')`                     | `storage/` の下だけ。S3 はありません                          |
| `Storage::url($path)` / `storage:link`    | ありません。配信する画面を自分で書きます                      |
| ジョブのクラス＋`SendWelcome::dispatch()` | `app/Jobs/SendWelcome.rs` の関数＋`Queue::push("SendWelcome", &payload)` |
| `$job->handle()` に型つきの引数           | `handle(payload: String)` の 1 つだけ。中身は自分で読みます   |
| `Redis` のキュー・キャッシュ              | ありません。キューは DB、キャッシュはファイル                 |
| `->everyMinute()` / `->cron('0 3 * * 1')` | `Every::Minute` などの列挙。cron の式は読みません             |
| `schedule:work`                           | ありません。cron から `schedule:run` を 1 分ごとに呼びます     |
| イベントクラス＋`EventServiceProvider`    | 名前は文字列。登録は `bootstrap/app.rs` の `with_events`      |
| `Mail::to(...)->send(new WelcomeMail)`    | `Mail::to(..).subject(..).text(..).send().await?`             |
| SMTP での送信                             | **ありません。** `log` と `array` だけです                     |
| `Notification`                            | 作りません。`Mail` を直接使います                             |
| `__('messages.welcome')`                  | 同じ名前（`__`）。`:name` の差し替えは `__with`                |
| 言語ファイルは PHP の配列                 | `resources/lang/*.toml`。**ビルド時に読みます**               |
| `trans_choice`（複数形）                  | ありません。鍵を分けます                                      |
| `make:command` ＋ `$signature`            | `app/Console/Commands/*.rs` に `DESCRIPTION` と `handle`      |

## PHP 固有の機能は作りません

PHP の言語や実行の形に強く結びついた機能は、bengara では作りません。
Rust では原理的に作れないものがあるためです。

| PHP 固有のもの                                         | bengara                                                                                |
|--------------------------------------------------------|----------------------------------------------------------------------------------------|
| Blade のテンプレート文法                               | 作りません。Rust のエンジンを選ぶのは Laravel に似せる話とは別なので、当面は入れません |
| 実行時に型や名前を調べる仕組み（リフレクション）       | ありません。Rust に無いためです                                                        |
| サービスコンテナによる依存の自動解決                   | ありません。ハンドラの引数は決まった2つの形だけです                                    |
| マジックメソッド（`__get` / `__call`）による遅延ロード | ありません。読み込みはいつも明示的に書きます                                           |
| `eval` 相当・実行時のコード生成                        | ありません                                                                             |
| 緩い型比較（`==` の暗黙の変換）                        | ありません。Rust の型を使います                                                        |

次のものは、 **同じ役割を別の形で用意しています。**

| PHP の仕組み                                     | bengara                                                   |
|--------------------------------------------------|-----------------------------------------------------------|
| Composer のオートロード                          | `build.rs` の自動検出。ビルド時にモジュール宣言を作ります |
| 連想配列を前提にした API（`config('app.name')`） | 型をキーにする `config::<AppConfig>()`                    |
| グローバル名前空間のヘルパ関数                   | `use bengara::prelude::*;` で同じ名前の自由関数が使えます |

「作らない」と決める前には、名前と考え方だけ借りられないか、Rust では別の形のほうが自然でないかを
毎回検討しています。借りられるものは、 **Laravel と同じ名前のまま中身を Rust の形で作ります。**

## その他

- `GET` のルートは `HEAD` にも応答します（Laravel と同じ考え方）。
- 本文の上限は 2 MiB。超えると 413 を返します。
- ハンドラがパニックしても 500 を返すだけで、プロセスは落ちません。
- `APP_DEBUG=true` のときだけエラーページに詳細が出ます（Laravel と同じ考え方）。
