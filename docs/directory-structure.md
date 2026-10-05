# ディレクトリ構成と自動検出

bengara には `mod` の書き並べがありません。`build.rs` がファイルを見つけて、モジュール宣言を自動で作ります。

```rust
// build.rs
fn main() { bengara_build::discover() }
```

```rust
// main.rs
bengara::app!();
```

`bengara::app!()` が、生成されたモジュール宣言を取り込みます。

## 構成

```
myapp/
├── main.rs                      bengara::app!();
├── artisan.rs                   fn main() { bengara::artisan() }
├── build.rs                     fn main() { bengara_build::discover() }
├── app/Console/Commands/        自分のコマンド
├── app/Http/Controllers/        コントローラ
├── app/Jobs/                    キューのジョブ
├── app/Listeners/               イベントを聞く側
├── app/Models/                  モデル
├── app/Policies/                誰に何を許すか
├── bootstrap/app.rs             アプリの組み立て
├── config/app.rs                設定
├── config/database.rs           データベースの設定
├── database/migrations/         表を作る手順
├── database/seeders/            初期データ
├── database/factories/          テスト用のデータを作る関数
├── public/                      静的ファイル
├── resources/lang/              言語ごとの文字（*.toml）
├── resources/views/             （まだ使いません）
├── routes/web.rs                ルート定義
├── routes/console.rs            定期処理
├── storage/                     実行時の書き込み先
└── tests/Feature/ tests/Unit/   テスト
```

`src/` はありません。入口はプロジェクト直下です。

## 覚えておく 3 つの約束

> ### 1. ほかのファイルは `crate::` から参照する
> `super::` は使いません。ファイルの実体はクレート直下に平らに置かれ、名前空間は再エクスポートで組み立てられているためです。
>
> ```rust
> use crate::app::http::controllers::HomeController;
> ```
>
> ### 2. 大文字始まりのファイルには、同名の型を 1 つ定義する
> `app/Models/User.rs` には `pub struct User`（または `enum` など）を 1 つ書きます。
> その型が `crate::app::models::User` として使えるようになります。
>
> ### 3. `config/` 直下のファイルには `pub fn config()` を書く
> `config/` 直下の小文字のファイルは設定として自動登録されます。`pub fn config() -> 何らかの型` が必ず必要です。
> 無いとビルドが通りません。

## 名前の付き方

| 対象                                           | どうなるか                                                                                    |
|------------------------------------------------|-----------------------------------------------------------------------------------------------|
| `app` `bootstrap` `config` `database` `routes` | モジュールとして公開される                                                                    |
| `tests`                                        | `#[cfg(test)]` 付きで取り込まれる（型の再エクスポートはしない）                               |
| ディレクトリ                                   | snake_case のモジュールへ。`app/Http/Controllers/` → `crate::app::http::controllers`          |
| 大文字始まりのファイル                         | `app/Models/User.rs` → 型 `crate::app::models::User` と モジュール `crate::app::models::user` |
| 小文字始まりのファイル                         | `routes/web.rs` → `crate::routes::web`                                                        |
| 日付で始まるファイル                           | 先頭に `m` を付けたモジュール名になる（`2026_…` → `m2026_…`）                                 |

### `database/` の約束

| 場所                                            | 書くもの                                                               |
|-------------------------------------------------|------------------------------------------------------------------------|
| `database/migrations/` の、日付で始まるファイル | `pub fn up(schema: &mut Schema)` と `pub fn down(schema: &mut Schema)` |
| `database/seeders/` の、大文字始まりのファイル  | `pub async fn run() -> Result<()>`                                     |
| `database/factories/`                           | ただの関数（決まりはありません）                                       |

- `migrations/` と `seeders/` は、名前順の一覧が自動で作られて本体に渡ります。 **登録の作業はありません。**
- `seeders/` と `factories/` では、 **大文字始まりのファイルに同名の型は要りません。**
  関数を置く場所として扱います。
- 詳しくは [migrations.md](migrations.md) と [models.md](models.md) にあります。

### `app/Jobs/` `app/Listeners/` `app/Console/Commands/` の約束

| 場所                    | 書くもの                                                                              |
|-------------------------|---------------------------------------------------------------------------------------|
| `app/Jobs/`             | `pub async fn handle(payload: String) -> Result<()>`                                  |
| `app/Listeners/`        | 同じ形                                                                                |
| `app/Console/Commands/` | `pub const DESCRIPTION: &str` と `pub async fn handle(args: &[String]) -> Result<()>` |

- この 3 つでは、 **大文字始まりのファイルに同名の型は要りません。** 関数を置く場所として扱います。
- `app/Jobs/` と `app/Console/Commands/` は、名前順の一覧が自動で作られて本体に渡ります。
  ジョブの名前はファイル名そのまま、コマンドの名前はファイル名を `-` 区切りにしたものです。
- **`app/Listeners/` は自動では登録しません。** `bootstrap/app.rs` の `with_events` に書きます
  （[events.md](events.md)）。

### `resources/lang/` の約束

`resources/lang/<言語>.toml` を **ビルド時に読み込んで**バイナリに入れます。
中身を書き換えると `cargo build` が走り直します。詳しくは
[localization.md](localization.md) にあります。

### `storage/` の約束

実行時の書き込み先です。中身はリポジトリに入れません。`storage:init` が 7 つ作ります。

| ディレクトリ                 | 置かれるもの                |
|------------------------------|-----------------------------|
| `storage/app`                | アプリが置くファイル        |
| `storage/app/public`         | `/storage/...` で配信する分 |
| `storage/framework`          | 下の 3 つの親               |
| `storage/framework/cache`    | キャッシュ                  |
| `storage/framework/schedule` | 定期処理の前回時刻          |
| `storage/framework/sessions` | セッション                  |
| `storage/logs`               | ログ                        |

`init` も同じ一覧を使います。本番では `./myapp storage:init` を 1 回流します
（[deployment.md](deployment.md)）。

### `routes/console.rs` の約束

`pub fn schedule(s: &mut Schedule)` を書きます。無ければ定期処理はありません。
詳しくは [scheduling.md](scheduling.md) にあります。

### 取り込まれないもの

- 隠しファイル（`.` で始まるもの）
- `mod.rs` / `main.rs` / `lib.rs`
- `.rs` 以外のファイル（例外は `resources/lang/*.toml`）
- `target/`

## テストの名前

テストファイルの実体は `__bengara_<パスをつないだ名前>` という名前になります。
例: `tests/Feature/HomeTest.rs` → `__bengara_tests_feature_home_test`。
テストの実行結果に出るので、読める形にしてあります。

## うまくいかないとき

- 自動検出は **パニックしません**。モジュール名にできないファイル名や、同じディレクトリでの名前の衝突は、
  コンパイルエラー（`compile_error!`）として出ます。メッセージの通りに直してください。
- ファイルを足したのに見つからないときは、`build.rs` が走り直していない可能性があります。
  `cargo artisan serve` が見張るのは、ビルドに影響するファイルだけです。

| 見張る                                                | 見張らない             |
|-------------------------------------------------------|------------------------|
| `.rs`                                                 | `public/` の中身       |
| `resources/lang/` 直下の `*.toml`                     | 上以外の `.toml`       |
| `.env` `Cargo.toml` `build.rs` `main.rs` `artisan.rs` | データベースのファイル |

`public/` を直してもビルドは走りません。デバッグビルドはディスクを読むので、
そのまま反映されます。

## 関連

- [routing.md](routing.md)
- [configuration.md](configuration.md)
- [testing.md](testing.md)
