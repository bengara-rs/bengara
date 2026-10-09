# 設定

設定は Rust のコードです。`config/` 直下にファイルを置くと、起動時に自動で登録されます。
設定がバイナリに入るので、本番に設定ファイルを置く必要はありません。

## 設定ファイルを書く

```rust
// config/app.rs
use bengara::prelude::*;

pub fn config() -> AppConfig {
    AppConfig {
        name: env("APP_NAME", "myapp"),
        env: env("APP_ENV", "production"),
        debug: env("APP_DEBUG", false),
        url: env("APP_URL", "http://localhost:8000"),
        // セッションと署名付き URL の署名に使います。`cargo artisan key:generate` で作ります。
        key: env("APP_KEY", ""),
    }
}
```

**`config/` 直下のファイルには `pub fn config() -> 何らかの型` を必ず定義します。**
無いとビルドが通りません。

### 読み出す

型をキーにして取り出します。`config("app.name")` のような文字列キーは用意していません
（[backlog.md](backlog.md)）。

```rust
let app = config::<AppConfig>();          // 無ければパニック
let app = try_config::<AppConfig>();      // Option 相当。無ければ取れないだけ
```

### 自分の設定型を増やす

1. `config/<名前>.rs` を作る
2. 好きな型を定義する（`Send + Sync + 'static` であること）
3. `pub fn config() -> その型` を書く

```rust
// config/mail.rs
use bengara::prelude::*;

pub struct MailConfig {
    pub from: String,
    pub port: u16,
}

pub fn config() -> MailConfig {
    MailConfig {
        from: env("MAIL_FROM", "noreply@example.com"),
        port: env("MAIL_PORT", 587),
    }
}
```

```rust
let mail = config::<crate::config::mail::MailConfig>();
```

型ごとに 1 つだけ保管されます。同じ型を 2 つのファイルから返さないでください。

### 今の環境を調べる

`AppConfig` には、`env` を見るだけの短い関数が 2 つあります。

```rust
if config::<AppConfig>().is_local() {
    // 手元の開発環境のときだけ
}
```

| 関数              | true になる条件                  |
|-------------------|----------------------------------|
| `is_production()` | `APP_ENV` が `production` のとき |
| `is_local()`      | `APP_ENV` が `local` のとき      |

比較するのは **この 2 つの文字列だけ**です。`staging` のような他の値を使うときは、
`config::<AppConfig>().env` を自分で比べてください。

## env() の型

`env(key, default)` が返す型は、 **既定値の型**で決まります。

```rust
let port: u16 = env("APP_PORT", 8000);
let debug: bool = env("APP_DEBUG", false);
let name: String = env("APP_NAME", "myapp");
let dir: std::path::PathBuf = env("CACHE_DIR", "storage/cache");
```

| 対応する型                                          |
|-----------------------------------------------------|
| `String`                                            |
| `bool`                                              |
| `std::path::PathBuf`                                |
| 整数（`i8`〜`i128` `isize` / `u8`〜`u128` `usize`） |
| 浮動小数（`f32` `f64`）                             |

変換できない値が入っていた場合は、警告を出して既定値を使います。止まりません。

### 既定値に戻るとき

| 値の書き方                    | `env()` が返すもの  |
|-------------------------------|---------------------|
| 設定されていない              | 既定値              |
| 空（`MAIL_FROM_NAME=`）       | 既定値              |
| 空白だけ（`A="   "`）         | 既定値              |
| 前後に空白がある（`A=" x "`） | `x`（空白は落ちる） |

**値の前後の空白は残りません。** 引用符で囲んでも落とします。`A="  padded  "` と
書いても、受け取るのは `padded` です。空白そのものを値にしたいときは、設定を読む側で
既定値を空白にしてください（`env("A", " ")`）。

同じ理由で、 **`.env` の書き方では値を空にできません。** 空にしたいときは
`env("MAIL_FROM_NAME", "")` のように、既定値そのものを空にしてください。

## .env の書き方

```ini
APP_NAME=myapp
APP_ENV=local
APP_DEBUG=true
APP_URL=http://localhost:8000

DB_CONNECTION=sqlite
DB_DATABASE=database/database.sqlite
DB_TEST_DATABASE=:memory:

APP_HOST=127.0.0.1
APP_PORT=8000

# 信頼する前段のプロキシ（カンマ区切り。`*` で全部）
# TRUSTED_PROXIES=10.0.0.1

# ログの細かさ（trace / debug / info / warn / error）
# RUST_LOG=info
```

- 実際の環境変数が優先されます。`.env` は「まだ設定されていないものだけ」を埋めます。
- 同じ名前が 2 回書かれていたら、**先に出てくるほう**が勝ちます。
- `#` から行末まではコメントです。 **前に空白は要りません。**
  `APP_PORT=8000#dev` の値は `8000` です。
- **引用符で囲まない値は、最初の `#` で切れます。**
  `NAME=Bob's App # コメント` の値は `Bob's App` です。
- 引用符で**囲んだ**値の中の `#` は値の一部です（`A="a#b"` の値は `a#b`）。
- 行頭の `export` は無視します。区切りは空白でもタブでもかまいません。
- `"..."` の中で解釈するのは `\n` `\r` `\t` `\\` `\"` `\'` の 6 つだけです。
  **それ以外は `\` ごとそのまま残ります。** Windows のパスをそのまま書けます
  （`DB_DATABASE="C:\data\app.sqlite"`）。`'...'` はすべてそのままの文字です。
- 読めない行は飛ばしますが、**黙って捨てずに警告を出します。**
  `=` 忘れ・使えない文字のキー・閉じていない引用符は、起動時のログに出ます。
- ファイル先頭の UTF-8 BOM は外します（メモ帳で保存しても 1 行目が読めます）。
- **`init` が作る `.env` は、Unix では 0600（所有者だけ）にします。**
  `APP_KEY` が入るファイルなので、同じマシンの別の利用者から読めないようにします。
- **読めなかった行は飛ばしますが、黙って捨てません。** 行番号と理由を警告に出します
  （`=` が無い、`=` の前にキーが無い、キーに英数字と `_` 以外が入っている）。
- **閉じていない引用符も、行番号つきの警告が出ます。**

## 環境変数の一覧

用途ごとに分けて並べます。「読む側」は、その値を誰が読むかです。

| 読む側             | 意味                                                                 |
|--------------------|----------------------------------------------------------------------|
| **本体**           | フレームワークが直接読みます。読み方は変えられません                 |
| **`config/*.rs`**  | 雛形の `config/*.rs` が読んでいるだけです。読み方を変えるのは自由です |
| **本体か config/** | 対応する `config/*.rs` が無ければ本体が読みます。置けばそちらが優先  |

### アプリ

| 名前        | 既定                    | 何に効くか                                 | 読む側        |
|-------------|-------------------------|--------------------------------------------|---------------|
| `APP_NAME`  | `init` 時のパッケージ名 | アプリ名。`AppConfig.name`                 | `config/*.rs` |
| `APP_ENV`   | `production`            | 環境名。`AppConfig.env`                    | `config/*.rs` |
| `APP_DEBUG` | `false`                 | エラーページの詳細表示、ログの既定の細かさ | `config/*.rs` |
| `APP_URL`   | `http://localhost:8000` | `AppConfig.url` と `url()` の土台          | `config/*.rs` |
| `RUST_LOG`  | （なし）                | ログの細かさ。無ければ `APP_DEBUG` で決まる | 本体          |

### HTTP とプロセス

| 名前                   | 既定        | 何に効くか                                 | 読む側 |
|------------------------|-------------|--------------------------------------------|--------|
| `APP_HOST`             | `127.0.0.1` | 待ち受けるホスト                           | 本体   |
| `APP_PORT`             | `8000`      | 待ち受けるポート                           | 本体   |
| `APP_SHUTDOWN_TIMEOUT` | `30`        | 停止の上限（秒）。**`0` は無制限**         | 本体   |
| `TRUSTED_PROXIES`      | （空）      | 信頼する前段のアドレス。下の節を参照       | 本体   |

### 置き場所

| 名前               | 既定     | 何に効くか                                     | 読む側 |
|--------------------|----------|------------------------------------------------|--------|
| `APP_BASE_PATH`    | （なし） | 基準ディレクトリ。**絶対パスのみ**             | 本体   |
| `APP_STORAGE_PATH` | （なし） | `storage/` の場所。**絶対パスのみ**            | 本体   |
| `STORAGE_DISK`     | `local`  | 既定のファイル置き場（[storage.md](storage.md)） | 本体か config/ |

**`APP_BASE_PATH` は `.env` に書いても効きません。** `.env` の場所そのものを決める値なので、
`.env` を読む前に必要になります。本物の環境変数で渡してください。
**指した場所が無いときは起動しません。** 直し方を添えたエラーが出ます
（[deployment.md](deployment.md)）。
`APP_STORAGE_PATH` は `.env` でも効きます。

### セッションと認証

| 名前               | 既定     | 何に効くか                                    | 読む側         |
|--------------------|----------|-----------------------------------------------|----------------|
| `APP_KEY`          | （なし） | 署名と暗号化に使う鍵。**64 文字以上**。下の節 | 本体か config/ |
| `SESSION_DRIVER`   | `file`   | セッションの置き場所（`file` / `memory`）     | 本体           |
| `SESSION_LIFETIME` | `120`    | セッションが消えるまでの分                    | 本体           |
| `HASH_ITERATIONS`  | `120000` | パスワードの変換の繰り返し回数                | 本体か config/ |

`APP_KEY` は `config/app.rs` が読み、`HASH_ITERATIONS` は `config/hashing.rs` が
無いときに本体が読みます。パスワードの変換は [authentication.md](authentication.md) で説明しています。

### データベース

| 名前               | 既定                       | 何に効くか                                        | 読む側        |
|--------------------|----------------------------|---------------------------------------------------|---------------|
| `DB_CONNECTION`    | `sqlite`                   | 既定で使う接続の名前                              | `config/*.rs` |
| `DB_DATABASE`      | `database/database.sqlite` | SQLite のファイル。相対パスはプロジェクト直下から | `config/*.rs` |
| `DB_TEST_DATABASE` | `:memory:`                 | テストで使うデータベース                          | 本体          |

`DB_CONNECTION` と `DB_DATABASE` は `config/database.rs` が読みます（[database.md](database.md)）。

### キャッシュとメールと言語

| 名前                  | 既定                  | 何に効くか                                | 読む側         |
|-----------------------|-----------------------|-------------------------------------------|----------------|
| `CACHE_DRIVER`        | `file`                | キャッシュの置き場所（`file` / `memory`） | 本体か config/ |
| `MAIL_DRIVER`         | `log`                 | メールの送り先（`log` / `array`）         | 本体か config/ |
| `MAIL_FROM`           | `noreply@example.com` | 差出人                                    | 本体か config/ |
| `MAIL_FROM_NAME`      | `bengara`             | 差出人の名前。普通はアプリ名を書く        | 本体か config/ |
| `APP_LOCALE`          | `ja`                  | 画面の言語                                | 本体か config/ |
| `APP_FALLBACK_LOCALE` | `ja`                  | 鍵が無いときに見る言語                    | 本体か config/ |

くわしくは [cache.md](cache.md) / [mail.md](mail.md) / [localization.md](localization.md) にあります。
`config/cache.rs`・`config/filesystems.rs`・`config/mail.rs` を置くと、そちらが優先します。

## APP_KEY

署名と暗号化に使う鍵です。`cargo artisan key:generate` が 64 文字を作って `.env` に書きます。

```sh
cargo artisan key:generate
```

- **64 文字以上が必須です。** 短いと、セッション・署名付き URL・暗号化を使う
  **最初のリクエストでエラーになります**。
- 空のままでも同じです。これらを使うリクエストが失敗します。
- 鍵は用途ごとに作り分けます。`APP_KEY` をそのまま使いません。

| 用途         | 作り分けの名前       |
|--------------|----------------------|
| セッション   | `bengara:session`    |
| 署名付き URL | `bengara:signed-url` |
| 暗号化       | `bengara:encryption` |

**鍵の作り方を変えたので、前の版で作ったセッション・暗号文・配布済みの署名付き URL は
無効になります。** `APP_KEY` を作り直したときと同じです。

くわしくは [session.md](session.md) と [authentication.md](authentication.md) にあります。

## TRUSTED_PROXIES

前段にプロキシを置くときに、そのアドレスを書きます。
`X-Forwarded-For` と `X-Real-IP` を見るかどうかがこれで決まります。

```ini
TRUSTED_PROXIES=10.0.0.1,10.0.0.2
```

| 書き方       | 動き                                                 |
|--------------|------------------------------------------------------|
| 空（既定）   | どちらのヘッダーも見ない。つないできた相手だけを見る |
| アドレスの列 | カンマ区切り。載っている相手からのヘッダーだけ見る   |
| `*`          | すべて信頼する                                       |

- **範囲の書き方（`10.0.0.0/8` のような CIDR）は読めません。** アドレスを 1 つずつ並べます。
- 読めない値は警告を出して無視します。止まりません。
- 効くのは回数の制限（`Throttle`）です（[middleware.md](middleware.md)）。
  `Request::ip()` は、この値に関係なく **つないできた相手**を返します。

### どのアドレスを採るか

`X-Forwarded-For` は `送ってきた人, プロキシ1, プロキシ2` のように**左から右へ**積まれます。
左端はクライアントが好きに書ける値です（nginx の `$proxy_add_x_forwarded_for` は、
受け取った値に**追記**します）。そのため左端を信じてはいけません。

| `TRUSTED_PROXIES` | 採るアドレス                                           |
|-------------------|--------------------------------------------------------|
| アドレスの列      | **右端から見て、一覧に無い最初のアドレス**             |
| `*`               | 左端（ほかに手がかりがないため）                       |

**アドレスとして読めない値は捨てます。** 1 つも読めなければ、つないできた相手に落とします。

`*` は「前段がヘッダーを必ず上書きする」構成でだけ使ってください。
追記する構成で `*` にすると、左端を書き換えるだけで回数の制限を回せます。

## 機能フラグ

bengara は使わない機能をバイナリに入れないようにしています。今あるフラグは 3 つです。

| フラグ       | 既定             | 中身                                                              |
|--------------|------------------|-------------------------------------------------------------------|
| `log-filter` | 入っている       | `RUST_LOG=myapp=debug` のような細かい絞り込みを使えるようにします |
| `sqlite`     | **入っていない** | SQLite につながるようにします（[database.md](database.md)）       |
| `encryption` | **入っていない** | `encrypt` / `decrypt` が使えます（+14 クレート）                  |

認証（`Hash` / `Auth` / `authorize` / `PasswordReset`）は **フラグが要りません。**
依存クレートを増やさずに作っているためです（[authentication.md](authentication.md)）。
キャッシュ・ファイル・メール・多言語・イベント・定期処理も、フラグなしで使えます。

### sqlite

**依存クレートが大きく増えます**（依存の木が 59 → 133）。
データベースを使わないアプリには入れないでください。
**キューも `sqlite` が要ります**（[queue.md](queue.md)）。

```toml
[dependencies]
bengara = { version = "0.1", features = ["sqlite"] }
```

`cargo run -- init` で作ったプロジェクトには最初から入っています。

### log-filter

依存クレートを減らしたいときに外せます。外すと 3 つ減り
（`matchers` / `regex-automata` / `regex-syntax`）、`RUST_LOG` は
`trace` / `debug` / `info` / `warn` / `error` の 1 語だけになります。

```toml
[dependencies]
bengara = { version = "0.1", default-features = false }
```

## パス

| 関数                | 返すもの                                                      |
|---------------------|---------------------------------------------------------------|
| `base_path()`       | 基準ディレクトリ。**決まらなければ起動しません**              |
| `app_path(rel)`     | ルートからの相対パスを絶対パスにする                          |
| `public_path(rel)`  | `public/` 以下のパス                                          |
| `storage_path(rel)` | `storage/` 以下のパス                                         |

基準ディレクトリの決め方は [deployment.md](deployment.md) にあります。

`app_path()` は **Laravel と意味が違います**。Laravel は `app/` を指しますが、bengara は
基準ディレクトリからの相対パスです（[laravel-differences.md](laravel-differences.md)）。

## 関連

- [getting-started.md](getting-started.md)
- [directory-structure.md](directory-structure.md)
- [deployment.md](deployment.md) — 基準ディレクトリと本番の `.env`
- [cache.md](cache.md) / [storage.md](storage.md) / [mail.md](mail.md) / [localization.md](localization.md) — 周辺機能の設定
