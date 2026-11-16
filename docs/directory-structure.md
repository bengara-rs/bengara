# ディレクトリ構成と自動検出

どこに何を置くかの説明です。
置いたファイルが見つからないときに何が出るかも書きます。

`mod` を書き並べる必要はありません。ビルドのときにファイルを探して、自動で取り込みます。
入口は次の 2 つで、どちらも `init` が作ります。

```rust
// build.rs
fn main() {
    bengara_build::discover()
}
```

```rust
// main.rs
bengara::app!();
```

## 構成

```
myapp/
├── main.rs                      本体の入口
├── artisan.rs                   開発用コマンドの入口
├── build.rs                     ファイルの自動検出
├── app/Console/Commands/        自分のコマンド
├── app/Http/Controllers/        コントローラ
├── app/Http/Middleware/         ミドルウェア
├── app/Jobs/                    キューのジョブ
├── app/Listeners/               イベントを聞く側
├── app/Models/                  モデル
├── app/Policies/                誰に何を許すか
├── bootstrap/app.rs             アプリの組み立て
├── config/app.rs                基本設定
├── config/database.rs           データベースの設定
├── database/migrations/         表を作る手順
├── database/seeders/            初期データ
├── database/factories/          テスト用のデータを作る関数
├── public/                      静的ファイル
├── resources/lang/              言語ごとの文字（*.toml）
├── routes/web.rs                ルート定義
├── routes/console.rs            定期処理
├── storage/                     実行時の書き込み先
└── tests/Feature/ tests/Unit/   テスト
```

- `src/` はありません。入口はプロジェクト直下です。
- `init` は `resources/views/` も作りますが、使いません（テンプレートの機能はありません）。

## 3 つの約束

| 約束                                                            | 守らないと                   |
|-----------------------------------------------------------------|------------------------------|
| ほかのファイルは `crate::` から参照する                         | `super::` では解決できない   |
| 大文字始まりのファイルに、同じ名前の型を 1 つ書く               | **ビルドが通らない**         |
| `config/` 直下の**小文字**のファイルに `pub fn config()` を書く | **ビルドが通らない**         |

```rust
use crate::app::http::controllers::HomeController;
```

- `app/Models/User.rs` には `pub struct User`（`enum` なども可）を書きます。
- 型を書き忘れると `unresolved import` のエラーになります。エラーの行の末尾に直し方が出ます。
- 型を置かないファイルは、小文字で始まる名前にします。
- 関数を置く場所（シーダーなど）では、型は要りません。
- `config/` の大文字始まりのファイルは、設定として登録されません（[configuration.md](configuration.md)）。

## 名前の付き方

| 置いたもの                       | Rust での名前                                                     |
|----------------------------------|-------------------------------------------------------------------|
| ディレクトリ                     | 小文字と `_` に直す（`app/Http/Controllers/` → `crate::app::http::controllers`） |
| 大文字始まりのファイル           | 型として使える（`app/Models/User.rs` → `crate::app::models::User`） |
| 小文字始まりのファイル           | モジュール（`routes/web.rs` → `crate::routes::web`）              |
| 日付で始まるファイル             | 先頭に `m` が付く（`2026_…` → `m2026_…`）                         |

- 取り込むのは `app` `bootstrap` `config` `database` `routes` と、テストのときの `tests` です。
- 大文字始まりのファイルは、モジュールとしても見えます（`crate::app::models::user`）。

## 置き場所ごとに書くもの

| 場所                    | 書くもの                                                                              | 自動で一覧になるか |
|-------------------------|---------------------------------------------------------------------------------------|--------------------|
| `database/migrations/`  | `pub fn up(schema: &mut Schema)` と `pub fn down(schema: &mut Schema)`                | なる               |
| `database/seeders/`     | `pub async fn run() -> Result<()>`                                                    | なる               |
| `app/Jobs/`             | `pub async fn handle(payload: String) -> Result<()>`                                  | なる               |
| `app/Console/Commands/` | `pub const DESCRIPTION: &str` と `pub async fn handle(args: &[String]) -> Result<()>` | なる               |
| `app/Listeners/`        | `pub async fn handle(payload: String) -> Result<()>`                                  | **ならない**       |
| `database/factories/`   | ただの関数（決まりはない）                                                            | —                  |
| `routes/console.rs`     | `pub fn schedule(s: &mut Schedule)`                                                   | —                  |

- 一覧になるものは、**置けば動きます。** 登録は要りません。並びは名前順です。
- **聞く側（`app/Listeners/`）だけは自分で登録します。** `bootstrap/app.rs` の `with_events` に書きます（[events.md](events.md)）。
- **マイグレーションのファイル名は日付から始めます**（`2026_10_05_000000_create_posts_table.rs`）。
  日付で始まらないとビルドが止まります。
- シーダー・ジョブ・コマンドは**大文字始まり**のファイルだけが一覧に入ります。小文字始まりは警告が出ます。
- コマンド名はファイル名から作ります（[console-commands.md](console-commands.md)）。

### 下のディレクトリに分けて置く

マイグレーション・シーダー・ジョブ・コマンドは、下にディレクトリを作って分けられます。
集めたものは 1 つの一覧になります。

```
app/Jobs/
├── SendWelcome.rs          ジョブ名 SendWelcome
└── Mail/
    └── SendInvoice.rs      ジョブ名 SendInvoice
```

- **名前は置き場所の全体で重ねないでください。** 別のディレクトリでも、同じ名前だとビルドが止まります。
- 置き場所のディレクトリ名は上の表のとおりに書きます。`app/JOBS/` はジョブの置き場所になりません。

## 取り込まれないもの

- 何も言わずに外すもの: `.` で始まる隠しファイル、`main.rs` / `lib.rs`、`.rs` 以外、`target/`。
- 警告を出して外すもの: `mod.rs`、リンク（たどらない）、32 段より深いディレクトリ、控え（下）。
- `.rs` のほかにビルドが読むのは、`resources/lang/` の言語ファイルと `public/` だけです。

名前に `.` や空白が入ったものは**控え**と見て、取り込みません（`User.old.rs`、`Controllers copy/`、`ja.old/`）。
控えのディレクトリは中身ごと外します。取り込みたいファイルは、名前を英数字と `_` だけにします。

**`database/migrations/` 直下の控えだけは、ビルドが止まります**（`2026_..._v1.2.rs` など）。
外へ出すか、控えのディレクトリ（`old.bak/` など）の中に入れてください。

## `public/`

静的ファイルを置きます。

| ビルド   | 読み方                                               |
|----------|------------------------------------------------------|
| デバッグ | ディスクから読む。直せばすぐ反映される               |
| リリース | **全部を実行ファイルに埋め込む**（隠しファイルを除く） |

- 1 つが 8 MiB、合計が 32 MiB を超えると、ビルドで警告が出ます。止まりはしません。
- 32 段より深いディレクトリの中は配信しません。
- ディレクトリへのリンク（`public/storage` など）は埋め込みません。警告が出ます。

## `resources/lang/`

言語ファイルはビルドのときに読んで、実行ファイルに入れます。置き方は 2 つです。

| 置き方                            | 鍵の頭                |
|-----------------------------------|-----------------------|
| `resources/lang/<言語>.toml`      | 付かない              |
| `resources/lang/<言語>/<群>.toml` | ファイル名（`<群>.`） |

- 同じ言語で両方を置くと、1 つの表にまとまります。
- **`<言語>/` より下のディレクトリは読みません**（警告が出ます）。
- 書き方は [localization.md](localization.md) にあります。

## `storage/`

実行時の書き込み先です。中身はリポジトリに入れません（`storage/.gitignore` が全部を無視します）。

| ディレクトリ                 | 置かれるもの                               |
|------------------------------|--------------------------------------------|
| `storage/app`                | アプリが置くファイル                       |
| `storage/app/public`         | `/storage/...` で配信する分                |
| `storage/framework/cache`    | キャッシュ                                 |
| `storage/framework/schedule` | 定期処理の前回の時刻                       |
| `storage/framework/serve`    | `cargo artisan serve` が使う（開発用）     |
| `storage/framework/sessions` | セッション                                 |
| `storage/logs`               | メールのログ（`mail.log`）                 |

- 手元では `cargo run -- init` が作ります。本番では `./myapp storage:init` を 1 回流します。
- どちらも、作ったディレクトリを Unix では所有者だけ（0700）にします。すでにあるものには触りません。

## テストの置き場所

テストは `tests/Feature/` か `tests/Unit/` に置きます。
**`tests/` の直下に `.rs` を置かないでください。** 警告が出ます（[testing.md](testing.md)）。

## 置いたのに動かないとき

**まずビルドの出力を見てください。** 止まるものと、警告だけのものがあります。

### ビルドが止まるもの

| 起きたこと                                        | 例                                         |
|---------------------------------------------------|--------------------------------------------|
| モジュール名にできない名前                        | `app/Models/日本語.rs`、`app/mod/`         |
| 同じディレクトリで名前がぶつかった                | `User.rs` と `user.rs`                     |
| 大文字始まりだが型の名前に使えない                | `app/Models/My-Model.rs`                   |
| `database/migrations/` 直下の控え                 | `2026_..._v1.2.rs`                         |
| `database/migrations/` の日付で始まらないファイル | `create_posts_table.rs`                    |
| 一覧の名前が置き場所の全体で重なった              | `SendReport.rs` と `Sub/SendReport.rs`     |
| 言語ファイルの鍵の重複・書き方の誤り              | 同じ鍵を `ja.toml` と `ja/` の両方に書いた |
| ディレクトリやファイルが読めない                  | 権限が無い                                 |
| 名前やパスが UTF-8 で表せない                     | —                                          |

名前に使える文字は、英数字と `_` `-` です。Rust の予約語（`mod` `type` など）も使えません。

### 警告だけのもの

| 起きたこと                               | 例                                           |
|------------------------------------------|----------------------------------------------|
| 控え・`mod.rs`・リンク・深すぎる場所     | 上の「取り込まれないもの」                   |
| 一覧に入らない小文字始まりのファイル     | `app/Jobs/sendWelcome.rs`                    |
| `tests/` の直下の `.rs`                  | `tests/HomeTest.rs`                          |
| フレームワークと同じ名前の自分のコマンド | `app/Console/Commands/List.rs`（呼ばれない） |
| 読まない場所の言語ファイル               | `resources/lang/ja/admin/x.toml`             |
| あるはずのディレクトリが無い             | `config/` が無い                             |

### 後からディレクトリを作ったとき

**無かったディレクトリを後から作ったら、`build.rs` を保存し直してください**（`touch build.rs`）。
作っただけでは、ビルドが見つけ直しません。
対象は `app` `bootstrap` `config` `database` `routes` `tests` `public` `resources/lang` です。

### テストを足したとき

`cargo artisan serve` は `tests/` を見張りません。`cargo test` を実行してください。

## 関連

- [configuration.md](configuration.md) — 設定ファイル
- [routing.md](routing.md) — ルートの書き方
- [testing.md](testing.md) — テストの書き方
