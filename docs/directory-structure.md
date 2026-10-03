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
├── app/Http/Controllers/        コントローラ
├── bootstrap/app.rs             アプリの組み立て
├── config/app.rs                設定
├── database/                    （まだ使いません）
├── public/                      静的ファイル
├── resources/views/             （まだ使いません）
├── routes/web.rs                ルート定義
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

| 対象 | どうなるか |
|---|---|
| `app` `bootstrap` `config` `database` `routes` | モジュールとして公開される |
| `tests` | `#[cfg(test)]` 付きで取り込まれる（型の再エクスポートはしない） |
| ディレクトリ | snake_case のモジュールへ。`app/Http/Controllers/` → `crate::app::http::controllers` |
| 大文字始まりのファイル | `app/Models/User.rs` → 型 `crate::app::models::User` と モジュール `crate::app::models::user` |
| 小文字始まりのファイル | `routes/web.rs` → `crate::routes::web` |
| 日付で始まるファイル | 先頭に `m` を付けたモジュール名になり、`MIGRATIONS` 定数に名前の一覧ができる |

### 取り込まれないもの

- 隠しファイル（`.` で始まるもの）
- `mod.rs` / `main.rs` / `lib.rs`
- `.rs` 以外のファイル
- `target/`

## テストの名前

テストファイルの実体は `__bengara_<パスをつないだ名前>` という名前になります。
例: `tests/Feature/HomeTest.rs` → `__bengara_tests_feature_home_test`。
テストの実行結果に出るので、読める形にしてあります。

## うまくいかないとき

- 自動検出は**パニックしません**。モジュール名にできないファイル名や、同じディレクトリでの名前の衝突は、
  コンパイルエラー（`compile_error!`）として出ます。メッセージの通りに直してください。
- ファイルを足したのに見つからないときは、`build.rs` が走り直していない可能性があります。
  `cargo artisan serve` は `app bootstrap config database public resources routes` の中と、
  `.env Cargo.toml build.rs main.rs artisan.rs` を見張っているので、通常は自動で拾います。

## 関連

- [routing.md](routing.md)
- [configuration.md](configuration.md)
- [testing.md](testing.md)
