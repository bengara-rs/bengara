# ディレクトリ構成と自動検出

どこに何を置くか、置いたファイルがどう見つかるかの説明です。

`mod` の書き並べはありません。`build.rs` がファイルを見つけて、
`bengara::app!()` がそれを取り込みます。

```rust
// build.rs
fn main() { bengara_build::discover() }
```

```rust
// main.rs
bengara::app!();
```

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

| 約束                                                            | 守らないとどうなるか         |
|-----------------------------------------------------------------|------------------------------|
| ほかのファイルは `crate::` から参照する                         | `super::` では解決できません |
| 大文字始まりのファイルに、同名の型を 1 つ定義する               | 型として使えません           |
| `config/` 直下の**小文字**のファイルに `pub fn config()` を書く | **ビルドが通りません**       |

```rust
use crate::app::http::controllers::HomeController;
```

`app/Models/User.rs` には `pub struct User`（または `enum` など）を 1 つ書きます。
その型が `crate::app::models::User` として使えます。

`config/` 直下の小文字のファイルは、設定として自動登録されます。
`pub fn config() -> 何らかの型` が必ず必要です。
**大文字で始めると設定になりません**（[configuration.md](configuration.md)）。

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

- 隠しファイルと隠しディレクトリ（`.` で始まるもの）
- `mod.rs` / `main.rs` / `lib.rs`
- `.rs` 以外のファイル。ビルド時に読むのは次の 2 つだけです
    - `resources/lang/<言語>.toml` と `resources/lang/<言語>/<群>.toml`
    - `public/` の中のファイル。**リリースビルドでは拡張子に関係なく全部**
      実行ファイルに埋め込みます（隠しファイルは除く）。デバッグビルドでは
      ディスクから読みます
- `target/`
- **語幹に `.` や空白が入った `.rs`** と、**名前に `.` や空白が入ったディレクトリ**
- リンク（シンボリックリンク・ジャンクション）。たどりません。警告が出ます

### 控えのファイルとディレクトリ

`.` や空白が入った名前は **控え**と見て取り込みません。警告が出るだけで、
ビルドは止まりません。`cp` で作った控えと `cp -r` で作った控えで、扱いがそろっています。

| 置いたもの                     | どうなるか                                     |
|--------------------------------|------------------------------------------------|
| `app/Models/User.old.rs`       | 取り込まない（警告）                           |
| `app/Http/Controllers copy/`   | 中身ごと取り込まない（警告）                   |
| `database/migrations/old.bak/` | 同じ。中の控えは `migrate` に渡りません        |
| `resources/lang/ja.old/`       | 読まない（警告）。`ja` の文字には混ざりません  |

コンパイルしたいファイルなら、名前を英数字と `_` だけにしてください。

**`database/migrations/` の中の `.rs` だけはビルドが止まります。**
`2026_..._v1.2.rs` のような控えは、`database/migrations/` の外か、
控えのディレクトリの中へ出してください。

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

| 置き場所                | 一覧になるか   | 名前の付き方                      |
|-------------------------|----------------|-----------------------------------|
| `database/migrations/`  | なる（名前順） | ファイル名。**日付から始めること** |
| `database/seeders/`     | なる（名前順） | ファイル名                        |
| `app/Jobs/`             | なる（名前順） | ファイル名そのまま                |
| `app/Console/Commands/` | なる（名前順） | ファイル名を `-` 区切りにしたもの |
| `app/Listeners/`        | **ならない**   | `with_events` に手で書く          |

- 一覧になるものに **登録の作業はありません。** 置けば本体に渡ります。
- **マイグレーションのファイル名は日付から始めてください**
  （`2026_10_05_000000_create_posts_table.rs`）。日付で始まらない `.rs` を
  `database/migrations/` に置くと、**ビルドが止まります。**
- **`app/Listeners/` だけは自動では登録しません。** `bootstrap/app.rs` の
  `with_events` に書きます（[events.md](events.md)）。
- `seeders/` `factories/` `app/Jobs/` `app/Listeners/` `app/Console/Commands/` では、
  **大文字始まりのファイルに同名の型は要りません。** 関数を置く場所として扱います。

### 入れ子のディレクトリに置ける

マイグレーション・シーダー・ジョブ・自分のコマンドは、下にディレクトリを作って
分けて置けます。集めたものは 1 つの一覧にまとまります。

```
app/Jobs/
├── SendWelcome.rs          ジョブ名 SendWelcome
└── Mail/
    └── SendInvoice.rs      ジョブ名 SendInvoice
```

- **名前は置き場所の全体で重ならないようにしてください。** 別のディレクトリでも
  同じ名前は置けません。ぶつかるとコンパイルエラーになります。
- 置き場所の判定は**モジュール名**で見ます。`app/JOBS/` はモジュール名が `j_o_b_s` に
  なるので、ジョブの置き場所になりません。`app/Jobs/` と書いてください。

## `resources/lang/` の約束

置き方は 2 つあり、どちらも**ビルド時に読み込んで**バイナリに入れます。

| 置き方                            | 鍵の頭                |
|-----------------------------------|-----------------------|
| `resources/lang/<言語>.toml`      | 付かない              |
| `resources/lang/<言語>/<群>.toml` | ファイル名（`<群>.`） |

- 同じ言語で両方を置いてもよく、1 つの表にまとまります。
- **`<言語>/` より下のディレクトリは読みません**（警告が出ます）。
- 中身を書き換えると `cargo build` が走り直します（[localization.md](localization.md)）。
- 先頭に UTF-8 BOM が付いていても読めます。

## `storage/` の約束

実行時の書き込み先です。中身はリポジトリに入れません。
`storage:init` が下の 8 つを作ります（`storage/` 自身も作るので、画面には 9 行出ます）。

| ディレクトリ                 | 置かれるもの                 |
|------------------------------|------------------------------|
| `storage/app`                | アプリが置くファイル         |
| `storage/app/public`         | `/storage/...` で配信する分  |
| `storage/framework`          | 下の 4 つの親                |
| `storage/framework/cache`    | キャッシュ                   |
| `storage/framework/schedule` | 定期処理の前回時刻           |
| `storage/framework/serve`    | `serve` が作る控え（開発用） |
| `storage/framework/sessions` | セッション                   |
| `storage/logs`               | ログ                         |

本番では `./myapp storage:init` を 1 回流します（[deployment.md](deployment.md)）。

**`storage/` の下に `.gitkeep` は置きません。** `storage/.gitignore` が `*` で
全部を無視します。ディレクトリは `storage:init` が作ります。

## テストの置き場所と名前

テストは `tests/Feature/` か `tests/Unit/` に置きます。
**`tests/` の直下に `.rs` を置かないでください。** 置くと警告が出ます
（[testing.md](testing.md)）。

テストファイルの実体は `__bengara_<パスをつないだ名前>` という名前になります。
例: `tests/Feature/HomeTest.rs` → `__bengara_tests_feature_home_test`。
テストの実行結果にこの名前で出ます。

## 見つからないとき

自動検出は**パニックしません**。知らせ方は 2 つです。

**コンパイルエラー（`compile_error!`）になるもの**

| 何が起きたか                                             | 例                                       |
|----------------------------------------------------------|------------------------------------------|
| モジュール名にできない名前                               | `app/Models/日本語.rs`、`app/mod/`       |
| 同じディレクトリでの名前の衝突                           | `User.rs` と `user.rs`                   |
| 大文字始まりだが**型名に使えない**名前                   | `app/Models/My-Model.rs`                 |
| `database/migrations/` の控えの名前                      | `2026_..._v1.2.rs`                       |
| `database/migrations/` の日付で始まらない名前            | `create_posts_table.rs`                  |
| 一覧の名前が全体で重なった                               | 入れ子も合わせて見ます                   |
| コマンド名に使えない文字                                 | `app/Console/Commands/Send@Report.rs`    |
| 言語ファイルの鍵の重複・TOML の構文エラー                | `resources/lang/ja.toml` の `"あ\qい"`   |
| ディレクトリが読めない                                   | 権限が無いときなど                       |
| ファイル名・ディレクトリ名が UTF-8 で表せない            | —                                        |
| **プロジェクトのパスが UTF-8 で表せない**                | —                                        |

**警告（`cargo:warning`）になるもの**

| 何が起きたか                                       | 例                                        |
|----------------------------------------------------|-------------------------------------------|
| 控えのファイル・控えのディレクトリ                 | `User.old.rs`、`Controllers copy/`        |
| 深すぎるディレクトリ（**32 段**より下は見ません）  | —                                         |
| 置き場所の決まりに外れた名前                       | `app/Jobs/sendWelcome.rs`（小文字始まり） |
| `tests/` の直下に置いた `.rs`                      | `tests/HomeTest.rs`                       |
| `mod.rs`                                           | 取り込みません                            |
| 無いディレクトリ・リンク・読み飛ばした言語ファイル | `resources/lang/ja/admin/x.toml`          |

警告のほうはビルドが通ります。置いたのに動かないときは、ビルドの出力を見てください。
`public/` の集め方も 32 段までです。超えた分は配信されません。

**無かったディレクトリを後から作ったときは、`touch build.rs` をしてください。**
`build.rs` は無いディレクトリを見張らないので、作っただけでは走り直しません。

### `serve` が見張る範囲

`cargo artisan serve` が見張るのは、ビルドに影響するファイルだけです。

| 見張る                                                             | 見張らない                   |
|--------------------------------------------------------------------|------------------------------|
| `app/` `bootstrap/` `config/` `database/` `routes/` の `.rs`       | **`tests/` の中身**          |
| `resources/lang/<言語>.toml`                                       | `public/` の中身             |
| `resources/lang/<言語>/*.toml`                                     | `<言語>/` より下の `*.toml`  |
| `.env` `Cargo.toml` `Cargo.lock` `build.rs` `main.rs` `artisan.rs` | 上以外の `.toml`             |
| `.cargo/config.toml` `rust-toolchain.toml`                         | データベースのファイル       |
|                                                                    | `target/` `storage/` `.git/` |

- **`tests/` は見張りません。** テストを足したときは `cargo test` を実行してください。
  `build.rs` は `tests/` も見張るので、そちらでは自動で見つかります。
- `public/` を直しても `serve` は再ビルドしません。デバッグビルドはディスクを読むので、
  そのまま反映されます。

## 関連

- [routing.md](routing.md)
- [configuration.md](configuration.md)
- [testing.md](testing.md)
