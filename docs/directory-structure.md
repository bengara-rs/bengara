# ディレクトリ構成と自動検出

どこに何を置くかの説明です。
置いたファイルがどう見つかるか、見つからないときに何が出るかも書きます。

`mod` の書き並べは要りません。ビルドのときにファイルを探して、自動で取り込みます。
そのための入口が次の 2 つで、どちらも `init` が作ります。

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

## 覚えておく 3 つの約束

| 約束                                                            | 守らないとどうなるか         |
|-----------------------------------------------------------------|------------------------------|
| ほかのファイルは `crate::` から参照する                         | `super::` では解決できません |
| 大文字始まりのファイルに、同じ名前の型を 1 つ書く               | **ビルドが通りません**       |
| `config/` 直下の**小文字**のファイルに `pub fn config()` を書く | **ビルドが通りません**       |

```rust
use crate::app::http::controllers::HomeController;
```

### 大文字始まりのファイル

`app/Models/User.rs` には `pub struct User`（`enum` などでも可）を書きます。
その型が `crate::app::models::User` として使えます。

- 型を書き忘れると `unresolved import` のエラーになります。
  エラーが指す行の末尾に、直し方のコメントが出ます。
- 型を置かないファイルは、小文字で始まる名前にしてください。
- 関数を置く場所（下の「置き場所ごとに書くもの」の `seeders` など）では、型は要りません。

### `config/` のファイル

`config/` 直下の小文字のファイルは、設定として自動で登録されます。
大文字で始めると登録されません（[configuration.md](configuration.md)）。

## 名前の付き方

| 置いたもの                                     | Rust での名前                                                       |
|------------------------------------------------|---------------------------------------------------------------------|
| `app` `bootstrap` `config` `database` `routes` | `crate::app` などのモジュール                                       |
| `tests`                                        | テストのときだけ取り込むモジュール                                  |
| ディレクトリ                                   | 小文字と `_` に直す。`app/Http/Controllers/` → `crate::app::http::controllers` |
| 大文字始まりのファイル                         | `app/Models/User.rs` → 型 `crate::app::models::User`                |
| 小文字始まりのファイル                         | `routes/web.rs` → `crate::routes::web`                              |
| 日付で始まるファイル                           | 先頭に `m` が付く（`2026_…` → `m2026_…`）                           |

大文字始まりのファイルは、モジュールとしても見えます
（`User.rs` → `crate::app::models::user`）。

## 置き場所ごとに書くもの

| 場所                    | 書くもの                                                                              | 自動で一覧になるか |
|-------------------------|---------------------------------------------------------------------------------------|--------------------|
| `database/migrations/`  | `pub fn up(schema: &mut Schema)` と `pub fn down(schema: &mut Schema)`                | なる               |
| `database/seeders/`     | `pub async fn run() -> Result<()>`                                                    | なる               |
| `app/Jobs/`             | `pub async fn handle(payload: String) -> Result<()>`                                  | なる               |
| `app/Console/Commands/` | `pub const DESCRIPTION: &str` と `pub async fn handle(args: &[String]) -> Result<()>` | なる               |
| `app/Listeners/`        | `pub async fn handle(payload: String) -> Result<()>`                                  | **ならない**       |
| `database/factories/`   | ただの関数（決まりはありません）                                                      | —                  |
| `routes/console.rs`     | `pub fn schedule(s: &mut Schedule)`                                                   | —                  |

- 一覧になるものに**登録の作業はありません。** 置けば動きます。並びは名前順です。
- **聞く側（`app/Listeners/`）だけは自分で登録します。** `bootstrap/app.rs` の
  `with_events` に書きます（[events.md](events.md)）。
- **マイグレーションのファイル名は日付から始めます**
  （`2026_10_05_000000_create_posts_table.rs`）。日付で始まらないとビルドが止まります。
- シーダー・ジョブ・コマンドは**大文字始まり**のファイルだけが一覧に入ります。
  小文字始まりは警告が出て、一覧に入りません。
- コマンド名はファイル名から作ります（[console-commands.md](console-commands.md)）。

### 下にディレクトリを作って分けられる

マイグレーション・シーダー・ジョブ・自分のコマンドは、下にディレクトリを作って
分けて置けます。集めたものは 1 つの一覧にまとまります。

```
app/Jobs/
├── SendWelcome.rs          ジョブ名 SendWelcome
└── Mail/
    └── SendInvoice.rs      ジョブ名 SendInvoice
```

- **名前は置き場所の全体で重ならないようにしてください。** 別のディレクトリでも
  同じ名前は置けません。ビルドが止まります。
- 置き場所のディレクトリ名は、上の表のとおりに書いてください。
  `app/JOBS/` のように書くと、ジョブの置き場所になりません。

## 取り込まれないもの

| もの                                              | 知らせ方                         |
|---------------------------------------------------|----------------------------------|
| 隠しファイル・隠しディレクトリ（`.` で始まる）    | 何も出ません                     |
| `main.rs` / `lib.rs` という名前のファイル         | 何も出ません                     |
| `mod.rs`                                          | 警告                             |
| `.rs` 以外のファイル                              | 何も出ません                     |
| `target/` という名前のディレクトリ                | 何も出ません                     |
| 名前に `.` や空白が入ったファイルとディレクトリ   | 警告（下の「控え」）             |
| リンク（シンボリックリンク・ジャンクション）      | 警告。たどりません               |
| 32 段より深いディレクトリ                         | 警告                             |

`.rs` のほかにビルドが読むのは、`resources/lang/` の言語ファイルと `public/` だけです。

### 控えのファイルとディレクトリ

名前に `.` や空白が入ったものは**控え**と見て取り込みません。
警告が出るだけで、ビルドは止まりません。

| 置いたもの                     | どうなるか                                    |
|--------------------------------|-----------------------------------------------|
| `app/Models/User.old.rs`       | 取り込まない                                  |
| `app/Http/Controllers copy/`   | 中身ごと取り込まない                          |
| `database/migrations/old.bak/` | 同じ。中の控えは `migrate` に渡りません       |
| `resources/lang/ja.old/`       | 読まない。`ja` の文字には混ざりません         |

コンパイルしたいファイルは、名前を英数字と `_` だけにしてください。

**`database/migrations/` 直下の控えだけはビルドが止まります**（`2026_..._v1.2.rs` など）。
`database/migrations/` の外か、控えのディレクトリの中へ出してください。

## `public/`

静的ファイルを置きます。ビルドの種類で読み方が変わります。

| ビルド     | 読み方                                                           |
|------------|------------------------------------------------------------------|
| デバッグ   | ディスクから読む。直せばすぐ反映されます                         |
| リリース   | **全部を実行ファイルに埋め込む**（隠しファイルを除く）           |

- 1 つが 8 MiB、合計が 32 MiB を超えると、ビルドのときに警告が出ます。止まりません。
- 32 段より深いディレクトリの中は配信されません。
- ディレクトリへのリンク（`public/storage` など）は埋め込みません。警告が 1 行出ます。ビルドは止まりません。

## `resources/lang/`

言語ファイルの置き方は 2 つあります。どちらも**ビルドのときに読んで**実行ファイルに入れます。

| 置き方                            | 鍵の頭                |
|-----------------------------------|-----------------------|
| `resources/lang/<言語>.toml`      | 付かない              |
| `resources/lang/<言語>/<群>.toml` | ファイル名（`<群>.`） |

- 同じ言語で両方を置いてもよく、1 つの表にまとまります。
- **`<言語>/` より下のディレクトリは読みません**（警告が出ます）。
- 書き方は [localization.md](localization.md) にあります。

## `storage/`

実行時の書き込み先です。中身はリポジトリに入れません
（`storage/.gitignore` が全部を無視します）。

| ディレクトリ                 | 置かれるもの                              |
|------------------------------|-------------------------------------------|
| `storage/app`                | アプリが置くファイル                      |
| `storage/app/public`         | `/storage/...` で配信する分               |
| `storage/framework/cache`    | キャッシュ                                |
| `storage/framework/schedule` | 定期処理の前回時刻                        |
| `storage/framework/serve`    | `cargo artisan serve` が使う場所（開発用） |
| `storage/framework/sessions` | セッション                                |
| `storage/logs`               | メールのログ（`mail.log`）                |

- 手元では `cargo run -- init` が作ります。
- 本番では `./myapp storage:init` を 1 回流します（[deployment.md](deployment.md)）。
- どちらも、作ったディレクトリを Unix では所有者だけ（0700）にします。
  すでにあるものには触りません。

## テストの置き場所

テストは `tests/Feature/` か `tests/Unit/` に置きます。
**`tests/` の直下に `.rs` を置かないでください。** 警告が出ます（[testing.md](testing.md)）。

テストの実行結果には、パスをつないだ名前で出ます。

| ファイル                    | 実行結果に出る名前                  |
|-----------------------------|-------------------------------------|
| `tests/Feature/HomeTest.rs` | `__bengara_tests_feature_home_test` |

## 置いたのに動かないとき

自動検出の知らせ方は 2 つです。**まずビルドの出力を見てください。**

### ビルドが止まるもの

| 何が起きたか                                      | 例                                     |
|---------------------------------------------------|----------------------------------------|
| モジュール名にできない名前                        | `app/Models/日本語.rs`、`app/mod/`     |
| 同じディレクトリでの名前の衝突                    | `User.rs` と `user.rs`                 |
| 大文字始まりだが型の名前に使えない                | `app/Models/My-Model.rs`               |
| `database/migrations/` 直下の控え                 | `2026_..._v1.2.rs`                     |
| `database/migrations/` の日付で始まらないファイル | `create_posts_table.rs`                |
| 一覧の名前が置き場所の全体で重なった              | `SendReport.rs` と `Sub/SendReport.rs` |
| 言語ファイルの鍵の重複・書き方の誤り              | 同じ鍵を `ja.toml` と `ja/` の両方に書いた |
| ディレクトリやファイルが読めない                  | 権限が無いとき                         |
| 名前やプロジェクトのパスが UTF-8 で表せない       | —                                      |

名前に使える文字は、英数字と `_` `-` です。Rust の予約語（`mod` `type` など）も使えません。

### 警告だけのもの

| 何が起きたか                                 | 例                                        |
|----------------------------------------------|-------------------------------------------|
| 控えのファイル・ディレクトリ                 | `User.old.rs`、`Controllers copy/`        |
| 一覧に入らない小文字始まりのファイル         | `app/Jobs/sendWelcome.rs`                 |
| `tests/` の直下に置いた `.rs`                | `tests/HomeTest.rs`                       |
| `mod.rs`                                     | 取り込みません                            |
| フレームワークと同じ名前の自分のコマンド     | `app/Console/Commands/List.rs`（呼ばれません） |
| 読まない場所の言語ファイル                   | `resources/lang/ja/admin/x.toml`          |
| リンク・深すぎるディレクトリ                 | —                                         |
| あるはずのディレクトリが無い                 | `config/` が無い                          |

### 後からディレクトリを作ったとき

**無かったディレクトリを後から作ったときは、`build.rs` を保存し直してください**
（`touch build.rs`）。対象は `app` `bootstrap` `config` `database` `routes` `tests`
`public` `resources/lang` です。作っただけでは、ビルドが見つけ直しません。

### テストを足したとき

`cargo artisan serve` は `tests/` を見張りません。テストを足したら `cargo test` を
実行してください。`serve` が見張る範囲は [artisan.md](artisan.md) にあります。

## 関連

- [configuration.md](configuration.md) — 設定ファイル
- [routing.md](routing.md) — ルートの書き方
- [testing.md](testing.md) — テストの書き方
