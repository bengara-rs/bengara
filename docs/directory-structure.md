# ディレクトリ構成と自動検出

どこに何を置くか、置いたファイルがどう見つかるかの話です。
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
├── app/Http/Middleware/         ミドルウェア
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

| 約束                                            | 守らないとどうなるか          |
|-------------------------------------------------|-------------------------------|
| ほかのファイルは `crate::` から参照する         | `super::` では解決できません  |
| 大文字始まりのファイルに、同名の型を 1 つ定義する | 型として使えません            |
| `config/` 直下のファイルに `pub fn config()` を書く | **ビルドが通りません**        |

### `crate::` から参照する

```rust
use crate::app::http::controllers::HomeController;
```

ファイルの実体はクレート直下に平らに置かれます。
名前空間は再エクスポートで組み立てているので、`super::` は使いません。

### 大文字始まりのファイルと型

`app/Models/User.rs` には `pub struct User`（または `enum` など）を 1 つ書きます。
その型が `crate::app::models::User` として使えるようになります。

### `config/` の約束

`config/` 直下の小文字のファイルは、設定として自動登録されます。
`pub fn config() -> 何らかの型` が必ず必要です。

## 名前の付き方

| 対象                                           | どうなるか                                                                                    |
|------------------------------------------------|-----------------------------------------------------------------------------------------------|
| `app` `bootstrap` `config` `database` `routes` | モジュールとして公開される                                                                    |
| `tests`                                        | `#[cfg(test)]` 付きで取り込まれる（型の再エクスポートはしない）                               |
| ディレクトリ                                   | snake_case のモジュールへ。`app/Http/Controllers/` → `crate::app::http::controllers`          |
| 大文字始まりのファイル                         | `app/Models/User.rs` → 型 `crate::app::models::User` と モジュール `crate::app::models::user` |
| 小文字始まりのファイル                         | `routes/web.rs` → `crate::routes::web`                                                        |
| 日付で始まるファイル                           | 先頭に `m` を付けたモジュール名になる（`2026_…` → `m2026_…`）                                 |

### 取り込まれないもの

- 隠しファイル（`.` で始まるもの）
- `mod.rs` / `main.rs` / `lib.rs`
- `.rs` 以外のファイル（例外は `resources/lang/<言語>.toml` と
  `resources/lang/<言語>/<群>.toml`）
- `target/`

## 置き場所ごとに書くもの

| 場所                                            | 書くもの                                                                              |
|-------------------------------------------------|---------------------------------------------------------------------------------------|
| `database/migrations/` の、日付で始まるファイル | `pub fn up(schema: &mut Schema)` と `pub fn down(schema: &mut Schema)`                |
| `database/seeders/` の、大文字始まりのファイル  | `pub async fn run() -> Result<()>`                                                    |
| `database/factories/`                           | ただの関数（決まりはありません）                                                      |
| `app/Jobs/`                                     | `pub async fn handle(payload: String) -> Result<()>`                                  |
| `app/Listeners/`                                | 同じ形                                                                                |
| `app/Console/Commands/`                         | `pub const DESCRIPTION: &str` と `pub async fn handle(args: &[String]) -> Result<()>` |
| `routes/console.rs`                             | `pub fn schedule(s: &mut Schedule)`。無ければ定期処理はありません                     |

### 自動で一覧になるもの

| 置き場所                | 一覧になるか  | 名前の付き方                     |
|-------------------------|---------------|----------------------------------|
| `database/migrations/`  | なる（名前順） | ファイル名                       |
| `database/seeders/`     | なる（名前順） | ファイル名                       |
| `app/Jobs/`             | なる（名前順） | ファイル名そのまま               |
| `app/Console/Commands/` | なる（名前順） | ファイル名を `-` 区切りにしたもの |
| `app/Listeners/`        | **ならない**  | `with_events` に手で書く         |

- 一覧になるものに **登録の作業はありません。** 置けば本体に渡ります。
- **`app/Listeners/` だけは自動では登録しません。** `bootstrap/app.rs` の `with_events` に
  書きます（[events.md](events.md)）。
- `seeders/` `factories/` `app/Jobs/` `app/Listeners/` `app/Console/Commands/` では、
  **大文字始まりのファイルに同名の型は要りません。** 関数を置く場所として扱います。
- 詳しくは [migrations.md](migrations.md) と [models.md](models.md) にあります。

### 入れ子のディレクトリに置ける

マイグレーション・シーダー・ジョブ・自分のコマンドは、**下にディレクトリを作って
分けて置けます。** 集めたものは 1 つの一覧にまとまります。

```
app/Jobs/
├── SendWelcome.rs          ジョブ名 SendWelcome
└── Mail/
    └── SendInvoice.rs      ジョブ名 SendInvoice
```

- **名前は置き場所の全体で重ならないようにしてください。** 別のディレクトリでも同じ名前は
  置けません。ぶつかるとコンパイルエラーになり、2 つのファイルのパスが出ます。
- 置き場所の判定は**モジュール名**で見ます。`app/JOBS/` はモジュール名が `j_o_b_s` に
  なるので、ジョブの置き場所になりません。`app/Jobs/` と書いてください。

## `resources/lang/` の約束

置き方は 2 つあり、どちらも**ビルド時に読み込んで**バイナリに入れます。

| 置き方                            | 鍵の頭                |
|-----------------------------------|-----------------------|
| `resources/lang/<言語>.toml`      | 付かない              |
| `resources/lang/<言語>/<群>.toml` | ファイル名（`<群>.`） |

同じ言語で両方を置いてもよく、1 つの表にまとまります。
**`<言語>/` より下のディレクトリは読みません**（警告が出ます）。
中身を書き換えると `cargo build` が走り直します（[localization.md](localization.md)）。

## `storage/` の約束

実行時の書き込み先です。中身はリポジトリに入れません。`storage:init` が下の 8 つを作ります
（`storage/` 自身も作るので、画面には 9 行出ます）。

| ディレクトリ                 | 置かれるもの                |
|------------------------------|-----------------------------|
| `storage/app`                | アプリが置くファイル        |
| `storage/app/public`         | `/storage/...` で配信する分 |
| `storage/framework`          | 下の 4 つの親               |
| `storage/framework/cache`    | キャッシュ                  |
| `storage/framework/schedule` | 定期処理の前回時刻          |
| `storage/framework/serve`    | `serve` が作る控え（開発用）|
| `storage/framework/sessions` | セッション                  |
| `storage/logs`               | ログ                        |

`init` も同じ一覧を使います。
本番では `./myapp storage:init` を 1 回流します（[deployment.md](deployment.md)）。

## テストの名前

テストファイルの実体は `__bengara_<パスをつないだ名前>` という名前になります。
例: `tests/Feature/HomeTest.rs` → `__bengara_tests_feature_home_test`。
テストの実行結果に出るので、読める形にしてあります。

## 見つからないとき

自動検出は**パニックしません**。次のものはコンパイルエラー（`compile_error!`）で出ます。

- モジュール名にできないファイル名
- 同じディレクトリでの名前の衝突

メッセージの通りに直してください。

ファイルを足したのに見つからないときは、`build.rs` が走り直していない可能性があります。
`cargo artisan serve` が見張るのは、ビルドに影響するファイルだけです。

| 見張る                                                | 見張らない                   |
|-------------------------------------------------------|------------------------------|
| `app/` `bootstrap/` `config/` `database/` `routes/` の `.rs` | **`tests/` の中身**   |
| `resources/lang/<言語>.toml`                          | `public/` の中身             |
| `resources/lang/<言語>/*.toml`                        | `<言語>/` より下の `*.toml`  |
| `.env` `Cargo.toml` `build.rs` `main.rs` `artisan.rs` | 上以外の `.toml`             |
|                                                       | データベースのファイル       |
|                                                       | `target/` `storage/` `.git/` |

**`tests/` は見張りません。** `serve` はアプリを動かすだけで、テストは走らせないためです。
テストを足したときは `cargo test` を実行してください。`build.rs` は `tests/` も
見張っているので、そちらでは自動で見つかります。

`public/` を直しても `serve` は再ビルドしません。
デバッグビルドはディスクを読むので、そのまま反映されます。

### 走り直す条件（`build.rs` 側）

`serve` とは別に、`build.rs` 自身も cargo に「ここを見張れ」と伝えています。

| 決まり               | 内容                                   | なぜ                                                                   |
|----------------------|----------------------------------------|------------------------------------------------------------------------|
| 生成コードの書き出し | **中身が変わったときだけ書く**         | 無条件に書くと、更新時刻だけが進んでアプリが丸ごと再コンパイルされます |
| あるディレクトリ     | そのディレクトリ自身と、入れ子も見張る | 下にファイルを足しても、親の更新時刻は動きません                       |
| 無いディレクトリ     | 代わりにルートを 1 回だけ見張る        | 無いパスを出すと、cargo が毎回ビルドスクリプトを走らせます             |
| `public/`            | **デバッグビルドでも見張る**           | `release` に切り替えたときに集め直せるようにするためです               |

## 関連

- [routing.md](routing.md)
- [configuration.md](configuration.md)
- [testing.md](testing.md)
