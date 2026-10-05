# Laravel と違う点

bengara は Laravel の構成と書き味に寄せていますが、同じではありません。
ここでは**今あるものの範囲**での違いだけを挙げます。まだ無い機能は [backlog.md](backlog.md) にまとめてあります。

## ファイルの置き場所

| Laravel | bengara |
|---|---|
| `public/index.php` が入口 | 入口はプロジェクト直下の `main.rs`（中身は `bengara::app!();`） |
| `artisan`（PHP スクリプト） | 直下の `artisan.rs`（中身は `fn main() { bengara::artisan() }`） |
| `app/` 以下は Composer のオートロード | `build.rs` の `bengara_build::discover()` が自動検出 |
| （`src/` は無い） | **`src/` は作らない。** Rust の慣習とは違うが、Laravel の構成に合わせた |
| `.php` | `.rs` |

## 参照のしかた

| Laravel | bengara |
|---|---|
| `use App\Http\Controllers\HomeController;` | `use crate::app::http::controllers::HomeController;` |
| 名前空間は PSR-4 | ディレクトリ名は snake_case のモジュールになる（`Http/Controllers` → `http::controllers`） |
| クラス名 = ファイル名 | 大文字始まりのファイルには**同名の型を 1 つ**定義する |

## コマンド

| Laravel | bengara |
|---|---|
| `php artisan ...` | `cargo artisan ...` |
| `make:controller` など生成コマンドが豊富 | **`make:*` は無い**。基本的なコードは AI で生成できるため |
| `route:list` | `route:list`（同じ） |
| `serve` はビルド不要 | `serve` はビルドしてから起動し、変更があれば再ビルド・再起動 |

## 設定

| Laravel | bengara |
|---|---|
| `config/app.php` が配列を返す | `config/app.rs` が `pub fn config() -> AppConfig` を返す |
| `config('app.name')` の文字列キー | `config::<AppConfig>()` の**型キー**。文字列キーは無い |
| `env()` は文字列中心 | `env(key, default)` は**既定値の型**で決まる（`String` `bool` `PathBuf` 整数 浮動小数） |
| 設定ファイルを本番に置く | 設定はコードなのでバイナリに入る。本番に置くファイルは無い |
| `config:cache` | 不要（起動時に組み立てて固定する） |

## ルーティング

| Laravel | bengara |
|---|---|
| `Route::get('/', [HomeController::class, 'index'])` | `Route::get("/", HomeController::index)` |
| `/posts/{post}` | `/posts/{post}`（同じ）。末尾の全取りは `/files/{*path}` |
| `route('posts.show', $post)` | `route_with("posts.show", &[("post", "12")])` — **`Result<String>` を返す**ので `?` が必要 |
| 可変長引数が使える | Rust に可変長引数が無いので、引数はスライスで渡す |
| `Route::group` / `prefix` | `Route::prefix("admin").group(|| { ... })`。`name` の点は自分で書く |
| `Route::middleware(...)` | `Route::get(...).middleware("admin")` と後ろに付ける（[middleware.md](middleware.md)） |
| ルートの重複は実行時に後勝ちなど | **起動時にパニックする**（メソッド＋パスの重複、ルート名の重複） |

## コントローラ

| Laravel | bengara |
|---|---|
| 引数の型からコンテナが解決して注入する | **自動解決は無い。** 受け付ける形は `async fn f() -> Result<Response>` と `async fn f(req: Request) -> Result<Response>` の 2 つだけ |
| `Request` から何でも取れる | `Request` のメソッドは決まった一覧のみ（[requests-and-responses.md](requests-and-responses.md)） |
| `view('welcome')` | テンプレートは無い。`html(...)` に文字列を渡す |

## レスポンス

| Laravel | bengara |
|---|---|
| `response()->json($data)` | `json(&data)` — `Result<Response>` |
| `redirect('/login')` | `redirect().to("/login")` — `Result<Response>` |
| `abort(403)` | `abort(403)` — `Result<T>` を返すので `return abort(403);` と書く |
| 例外を投げる | `Err` を返す。`Error::Http` ならそのステータス、それ以外は 500 |

## 読み込みのしかた

| Laravel | bengara |
|---|---|
| 必要になってから読む（遅延ロード） | **遅延ロードは無い。** ルートと設定は起動時にすべて組み立てて固定する |
| サービスコンテナ・サービスプロバイダ | 無い。設定は型をキーにした保管だけ |
| ファサード | 無い。`use bengara::prelude::*;` の自由関数を使う |

## パスを返す関数

| Laravel | bengara |
|---|---|
| `app_path()` は `app/` を指す | **`app_path(rel)` は基準ディレクトリからの相対パス**。`app/` は指さない。`app_path("app/Models")` と書く |
| `base_path()` はプロジェクトのルート | `base_path()`（同じ） |
| `public_path()` / `storage_path()` | 同じ意味 |

名前は Laravel に合わせましたが、`app_path()` だけは指す場所が違います。
Laravel から移してきたコードは、ここで取り違えやすいので注意してください。

## テスト

| Laravel | bengara |
|---|---|
| `php artisan test` / PHPUnit | `cargo test` |
| `$this->get('/')->assertOk()` | `get("/").await.assert_ok()` |
| トレイトを使う宣言が必要 | `#[bengara::test]` が道具を注入する。`use` は不要 |
| HTTP サーバーを立てる構成もある | ソケットを開かない |

## PHP 固有の機能は作りません

PHP の言語や実行の形に強く結びついた機能は、bengara では作りません。
Rust では原理的に作れないものがあるためです。

| PHP 固有のもの | bengara |
|---|---|
| Blade のテンプレート文法 | 作りません。Rust のテンプレートエンジンを使います（まだ未実装） |
| 実行時に型や名前を調べる仕組み（リフレクション） | ありません。Rust に無いためです |
| サービスコンテナによる依存の自動解決 | ありません。ハンドラの引数は決まった2つの形だけです |
| マジックメソッド（`__get` / `__call`）による遅延ロード | ありません。読み込みはいつも明示的に書きます |
| `eval` 相当・実行時のコード生成 | ありません |
| 緩い型比較（`==` の暗黙の変換） | ありません。Rust の型を使います |

次のものは、**同じ役割を別の形で用意しています。**

| PHP の仕組み | bengara |
|---|---|
| Composer のオートロード | `build.rs` の自動検出。ビルド時にモジュール宣言を作ります |
| 連想配列を前提にした API（`config('app.name')`） | 型をキーにする `config::<AppConfig>()` |
| グローバル名前空間のヘルパ関数 | `use bengara::prelude::*;` で同じ名前の自由関数が使えます |

「作らない」と決める前には、名前と考え方だけ借りられないか、Rust では別の形のほうが自然でないかを
毎回検討しています。借りられるものは、**Laravel と同じ名前のまま中身を Rust の形で作ります。**

## その他

- `GET` のルートは `HEAD` にも応答します（Laravel と同じ考え方）。
- 本文の上限は 2 MiB。超えると 413 を返します。
- ハンドラがパニックしても 500 を返すだけで、プロセスは落ちません。
- `APP_DEBUG=true` のときだけエラーページに詳細が出ます（Laravel と同じ考え方）。
