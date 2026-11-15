# データベース

このページで分かること。

- データベースへのつなぎ方（機能フラグと `.env`）
- 問い合わせの書き方（`DB::table(...)`）と、トランザクションの書き方
- SQLite / MySQL / PostgreSQL で動きが違う所

表の作り方は [migrations.md](migrations.md)、構造体として読み書きする方法は
[models.md](models.md) にあります。

## 使えるようにする

使うデータベースを `Cargo.toml` の機能フラグに書きます。

```toml
[dependencies]
bengara = { version = "0.1", features = ["sqlite"] }
```

| データベース | フラグ                          | `DB_CONNECTION` に書く名前 |
|--------------|---------------------------------|----------------------------|
| SQLite       | `sqlite`                        | `sqlite`                   |
| MySQL        | `mysql`                         | `mysql`                    |
| MariaDB      | `mariadb`（`mysql` と同じもの） | `mariadb`                  |
| PostgreSQL   | `postgres`                      | `pgsql`                    |

- `cargo run -- init` が作る `Cargo.toml` には `features = ["sqlite"]` が入ります。
  データベースを使わないなら、その 1 語を消してください。
- フラグは 2 つ以上書けます。`DB::connection("名前")` で使い分けます。
- フラグが無くてもコンパイルは通ります。つなごうとしたときに、次のエラーが返ります。

```
mysql につなぐには Cargo.toml の bengara の features に "mysql" を足してください（MariaDB なら "mariadb" でも同じものが入ります）
```

### ビルドに要るもの

`mysql` と `postgres` は、通信の暗号化のために C コンパイラが要ります。
`sqlite` だけなら要りません。
Linux は `build-essential` など、macOS は Xcode Command Line Tools、
Windows は Visual Studio の C++ ビルドツールを入れてください。

> Rust 1.85（最低対応版）で使うときは、依存を 1 つ固定してください。
> `cargo update idna_adapter --precise 1.2.0`（[getting-started.md](getting-started.md)）

## 設定

`.env` に書きます。SQLite のとき。

```
DB_CONNECTION=sqlite
DB_DATABASE=database/database.sqlite

# テストで使うデータベース（:memory: ならディスクに残りません）
DB_TEST_DATABASE=:memory:
```

MySQL / MariaDB / PostgreSQL のとき。

```
DB_CONNECTION=mysql
DB_DATABASE=myapp
DB_HOST=127.0.0.1
# 空にすると 3306（mysql）/ 5432（pgsql）を使います
DB_PORT=
DB_USERNAME=myapp
DB_PASSWORD=ここにパスワード

# テスト用に別のデータベースを用意してください（下の「テスト」）
DB_TEST_DATABASE=myapp_test
```

| 環境変数             | 既定                                                   | 使うドライバ  |
|----------------------|--------------------------------------------------------|---------------|
| `DB_CONNECTION`      | `sqlite`                                               | すべて        |
| `DB_DATABASE`        | SQLite は `database/database.sqlite`、ほかは `bengara` | すべて        |
| `DB_URL`             | 空                                                     | すべて        |
| `DB_HOST`            | `127.0.0.1`                                            | MySQL / pgsql |
| `DB_PORT`            | 3306 / 5432                                            | MySQL / pgsql |
| `DB_USERNAME`        | `root` / `postgres`                                    | MySQL / pgsql |
| `DB_PASSWORD`        | 空（渡しません）                                       | MySQL / pgsql |
| `DB_MAX_CONNECTIONS` | 5                                                      | すべて        |
| `DB_TEST_DATABASE`   | SQLite は `:memory:`、ほかは無し                       | すべて        |

- `DB_URL` は接続文字列です。`DB_CONNECTION` で選んだ接続にだけ効きます。
- `.env` の値に `#` を含むときは、引用符で囲んでください（`DB_PASSWORD="p#ss"`）。

### `config/database.rs`

細かく決めたいときに書きます。`init` が作る雛形に入っています。
置かなくても、上の環境変数から `sqlite` / `mysql` / `mariadb` / `pgsql` の
4 本の接続が組み立てられます。

```rust
use bengara::prelude::*;

pub fn config() -> DatabaseConfig {
    let database: String = env("DB_DATABASE", "database/database.sqlite");

    DatabaseConfig {
        default: env("DB_CONNECTION", "sqlite"),
        connections: vec![
            ConnectionConfig::sqlite("sqlite", database).max_connections(5),
            // 2 本目。DB::connection("logs") で指します。
            ConnectionConfig::sqlite("logs", "database/logs.sqlite"),
            // postgres も同じ形です（ConnectionConfig::postgres）。
            ConnectionConfig::mysql("mysql", env::<String>("DB_DATABASE", "myapp"))
                .host(env::<String>("DB_HOST", "127.0.0.1"))
                .username(env::<String>("DB_USERNAME", "root"))
                .password(env::<String>("DB_PASSWORD", "")),
        ],
    }
}
```

接続は `ConnectionConfig::sqlite(名前, 場所)`・`mysql(名前, データベース名)`・
`postgres(名前, データベース名)` で作ります。続けて書ける指定は次のとおりです。

| 指定                 | 既定                | 意味                                                        |
|----------------------|---------------------|-------------------------------------------------------------|
| `host(s)`            | `127.0.0.1`         | つなぎ先。空の文字列は無視します                            |
| `port(n)`            | 3306 / 5432         | ポート。0 は無視します                                      |
| `username(s)`        | `root` / `postgres` | 利用者名。空の文字列は無視します                            |
| `password(s)`        | 空                  | パスワード。空のままなら渡しません                          |
| `max_connections(n)` | 5                   | 同時に張る接続の数。メモリ上の SQLite は 1 に固定されます   |
| `foreign_keys(b)`    | 真                  | SQLite で外部キーの制約を効かせるか（ほかでは常に効きます） |
| `url(s)`             | 空                  | 接続文字列。書くと、ほかの値より優先します                  |

`url(s)` の形は次のとおりです。

```
sqlite://database/database.sqlite
mysql://利用者名:パスワード@127.0.0.1:3306/データベース名
postgres://利用者名:パスワード@127.0.0.1:5432/データベース名
```

### つなぎ方の決まり

- 接続は、最初に使ったときに張ります。以後は接続ごとに 1 つのプールを使い回します。
- SQLite のファイルは無ければ作ります。置くディレクトリは先に用意してください。
- MySQL と PostgreSQL は、**データベースを先に作っておいてください。** 作りません。
- SQLite の相対パスは、プロジェクト直下（`base_path()`）から見ます。
- ファイルの SQLite は WAL モード（読みと書きを同時に進められる形）で開きます。
  ほかのプロセスが書いている間は、5 秒まで待ちます。
- 名前か `url` に `:memory:` を含むとき、または `url` のクエリに `mode=memory` が
  あるときは、メモリ上のデータベースとして扱います。
- MySQL と PostgreSQL は、サーバが対応していれば暗号化してつなぎます。
  証明書を確かめる設定にしたときは、OS に入れた証明書を見ません
  （同梱の認証局の一覧を使います）。

### つながらないとき

エラー文には、接続の名前とつなぎ先が出ます。**接続文字列とパスワードは出しません。**

```
# つなげない
MySQL につなげません（接続 `mysql`: 127.0.0.1:3306 / myapp）: 原因

# url でつないでいて、つなげない
MySQL につなげません（接続 `mysql`: url（DB_URL）の指定）: 原因

# url の書式が読めない
接続 `mysql` の接続文字列（url / DB_URL）が読めません: 原因

# 設定に無い接続名を指した
`logs` という接続は設定にありません（ある接続: sqlite, mysql, mariadb, pgsql）。config/database.rs を確かめてください
```

- パスワードに `@` `#` `/` `?` などの記号があるときは、`url` の中でパーセントエンコード
  （`%40` のような書き方）にしてください。
- SQLite は、ファイルを置くディレクトリが無いとエラーになります。
- MySQL と PostgreSQL は、つながらないとき最長 30 秒待ってからエラーになります。

## 問い合わせを組み立てる

`DB::table("表の名前")` から始めて、やりたいことを足していきます。
最後に**終端のメソッド**（`get()` など）を呼ぶと、そこで SQL が投げられます。

```rust
use bengara::prelude::*;

pub async fn index() -> Result<Response> {
    let posts = DB::table("posts")
        .where_("status", "published")
        .where_op("views", ">", 100)
        .latest()
        .limit(10)
        .get()
        .await?;

    json(&posts)
}
```

### 条件を足す

| 書き方                                                 | 意味                     |
|--------------------------------------------------------|--------------------------|
| `where_("status", "published")`                        | 等しい                   |
| `where_op("views", ">", 100)`                          | 演算子を指定する         |
| `or_where(...)` / `or_where_op(...)`                   | `or` でつなぐ            |
| `where_in("id", &[1, 2, 3])` / `where_not_in`          | 一覧のどれか             |
| `or_where_in("id", &[1, 2, 3])`                        | `or` でつないで、一覧のどれか |
| `where_null("body")` / `where_not_null`                | `null` かどうか          |
| `where_between("views", 0, 100)` / `where_not_between` | 範囲（両端を含みます）   |
| `where_like("title", "%そば%")`                        | 形に当てはまる           |
| `where_column("updated_at", ">", "created_at")`        | 列と列を比べる           |
| `where_raw("length(title) > ?", &[3.into()])`          | SQL を直接書く           |
| `or_where_raw(...)`                                    | `or` でつないで、SQL を直接書く |
| `where_group(\|q\| q.where_("a", 1).or_where("b", 2))` | 括弧でくくる             |
| `or_where_group(...)`                                  | `or` でつないで、括弧でくくる |

`where` は Rust の予約語なので `where_` です。
等しいかどうかは `where_`、それ以外は `where_op` に分かれています。

```rust
// Laravel: ->where('views', '>', 100)
DB::table("posts").where_op("views", ">", 100)
```

気をつけること。

- **`None`（`NULL`）と比べると `is null` になります。** `where_("body", None::<String>)` は
  `"body" is null`、`where_op("body", "!=", None::<String>)` は `"body" is not null` です。
  読み替えるのは `=` `!=` `<>` `is` `is not` だけです。
- `where_in` に空の一覧を渡すと、必ず 0 件になります。
- `where_in` に渡せるのは **30,000 件まで**です。多いときは一覧を分けてください。
- `where_group` の中で使われるのは条件だけです。中で `select` や `limit` を書いても捨てます。
- `where_raw` の `?` は**値の置き場所としてだけ**使えます。文字列の中の `?` も数えます。
  `?` の数と値の数が合わないときはエラーです。

### 並べる・件数を決める

| 書き方                                           | 意味                       |
|--------------------------------------------------|----------------------------|
| `order_by(col)` / `order_by_desc(col)`           | 並べる                     |
| `order_by_raw("sql")`                            | 並べ方を SQL で書く        |
| `latest()` / `oldest()`                          | `created_at` の降順 / 昇順 |
| `latest_by(col)` / `oldest_by(col)`              | 列を指定して並べる         |
| `limit(n)` / `offset(n)` / `take(n)` / `skip(n)` | 件数                       |
| `for_page(page, per_page)`                       | ページから件数を決める     |

**並びが一意になるようにしてください。** 同じ値の行が複数あると、ページの境目で
同じ行が 2 回出ます。`latest()` に `order_by_desc("id")` を足すなどしてください。

式で並べたいときは `order_by_raw` です。

```rust
DB::table("posts")
    .order_by_raw("case when pinned then 0 else 1 end asc")
    .order_by_desc("id")
```

### 表をつなぐ・取る列を決める

| 書き方                                            | 意味           |
|---------------------------------------------------|----------------|
| `join("users", "users.id", "=", "posts.user_id")` | 結合           |
| `left_join(...)`                                  | 外側結合       |
| `select(&["id", "title"])` / `add_select("body")` | 取る列         |
| `distinct()`                                      | 重なりを捨てる |

`select` には式も書けます（`count(*) as total` など）。

### 束ねて絞る

```rust
DB::table("posts")
    .select(&["status", "count(*) as total"])
    .group_by(&["status"])
    .having_raw("count(*) > ?", &[2.into()])
    .order_by_desc("total")
    .get()
    .await?;
```

| 書き方                       | 意味                |
|------------------------------|---------------------|
| `group_by(&["status"])`      | 束ねる              |
| `having_op(col, 演算子, 値)` | 束ねた結果で絞る    |
| `having_raw("sql", &[値])`   | 絞り方を SQL で書く |

- `group_by` の無い `having` はエラーです。束ねずに絞るなら `where` を使います。
- **`having_op` に渡せるのは実在の列だけです。** `select` で付けた別名
  （`count(*) as total` の `total`）を渡すと、PostgreSQL で SQL エラーになります。
  集計の結果で絞るときは `having_raw` に式を書いてください。
- `order_by_desc` は別名で並べられます。

### 結果を取り出す

| 終端              | 返るもの                              |
|-------------------|---------------------------------------|
| `get()`           | `Vec<Row>`                            |
| `first()`         | `Option<Row>`                         |
| `find(id)`        | `Option<Row>`（**`id` という列**で引く） |
| `value::<T>(col)` | `Option<T>`（最初の行の、1 つの値）   |
| `pluck::<T>(col)` | `Vec<T>`（1 列ぶん）                  |

```rust
let titles = DB::table("posts").pluck::<String>("title").await?;

let row = DB::table("posts").find(1).await?.unwrap();
let title: String = row.get("title")?;        // 無い列・型違いはエラー
let body: Option<String> = row.get("body")?;  // null を許すとき
let maybe = row.try_get::<i64>("views");      // 読めなければ None
```

`row.get::<T>()` で読める型は次のとおりです。どれも `Option` にできます。

| 分類   | 型                                                             |
|--------|----------------------------------------------------------------|
| 整数   | `i8` `i16` `i32` `i64` `isize` `u8` `u16` `u32` `u64` `usize`  |
| 小数   | `f32` `f64`                                                    |
| その他 | `bool` `String` `Vec<u8>` `Value`（生の値）                    |

- **NULL がありうる列は `Option<T>` で読んでください。** `T` のままだと NULL の行でエラーです。
  `value` と `pluck` も同じです（`pluck::<Option<String>>("body")`）。
- **幅の狭い型に収まらない値はエラーです。**
- 小数を整数として読むとき、`2.0` は `2` になります。
  小数部のある値・`NaN`・無限はエラーです（`f64` で読んでください）。
- `value` と `pluck` に渡せるのは、ただの列名だけです（`users.name` は渡せます）。
  式が欲しいときは `select(...)` に書いて `get()` します。
- 主キーの名前が `id` でない表には、`find` の代わりに `where_(列, 値).first()` を使います。

**同じ名前の列が 2 つある行は、左の列を採ります。** `join` して `select` を書かないと
`posts.id` と `users.id` がどちらも `id` で返ります。取る列は `select` で決めてください。

### JSON で返す

`Row` はそのまま `json(&rows)` で返せます。値の種類（`Value`）ごとに次の形で出ます。

| `Value` | JSON                                 |
|---------|--------------------------------------|
| `Null`  | `null`                               |
| `Bool`  | `true` / `false`                     |
| `Int`   | 数                                   |
| `Float` | 数（`NaN` と無限は `null`）          |
| `Text`  | 文字列                               |
| `Bytes` | **数の配列**（`[137, 80, 78, ...]`） |

**バイナリの列を含む行は、そのまま `json()` しないでください。**
1 バイトが数 1 つになるので、応答が何倍にもふくらみます。`select` でその列を外してください。

### 数える

```rust
let total = DB::table("posts").count().await?;                          // i64
let any = DB::table("posts").where_("status", "draft").exists().await?; // bool
let none = DB::table("posts").doesnt_exist().await?;
let sum = DB::table("posts").sum::<i64>("views").await?;                // Option<i64>
// avg / min / max も同じ形
```

`count()` の数え方は、付いている指定で変わります。`paginate` の `total` も同じです。

| 付いている指定 | `count()` が返すもの |
|----------------|----------------------|
| 何も無い       | 行の数               |
| `group_by`     | **グループの数**     |
| `distinct`     | 重なりを除いた行の数 |

- `distinct()` を付けて数えるときは、`select` で列を 1 つ指定してください。
  決まらないとエラーです。
- `exists()` と `doesnt_exist()` は、並び順と件数の指定を外してから調べます。
- `sum` / `avg` / `min` / `max` は `group_by` と併用できません。
  グループごとの値は `select` に式を書いて `get()` します。
- 行が 1 つも無いとき、`sum` / `avg` / `min` / `max` は `None` を返します。

```rust
let authors = DB::table("posts").distinct().select(&["author_id"]).count().await?;
```

## 書き込む

### 入れる

```rust
// 1 行
DB::table("posts").insert(&[("title", "はじめての記事".into())]).await?;

// 入れて ID を受け取る
let id = DB::table("posts").insert_get_id(&[("title", "次の記事".into())]).await?;

// まとめて
let rows = vec![vec![("title", "A".into())], vec![("title", "B".into())]];
DB::table("posts").insert_many(&rows).await?;
```

値は `.into()` で `Value` にします。`&str` / `String` / 整数 / 小数 / 真偽 / `Vec<u8>` /
`Option<T>`（`None` は `null`）が渡せます。

- `insert_many` は、どの行も**列の名前と並びを最初の行とそろえて**ください。ずれるとエラーです。
- `insert` 系に `where_` や `limit` は付けられません。エラーです。
- **`i64` を超える整数は、文字列の列（`string` / `text`）に入れてください。**
  `u64` と `usize` で `i64` に収まらない値は、文字列として書き込みます。

### 更新する・消す

```rust
// 更新（返るのは件数）
let changed = DB::table("posts").where_("id", id).update(&[("title", "改題".into())]).await?;

// 削除（返るのは件数）
let removed = DB::table("posts").where_("id", id).delete().await?;

// 全部消す（自動採番も 1 に戻る）
DB::table("posts").truncate().await?;
```

- **条件を書かないと全行が対象です。** Laravel と同じです。
- `update` は `updated_at` を入れません。要るときは自分で入れます。
- `update` / `delete` / `truncate` には `join` や `limit` を付けられません。エラーです。
  件数を絞って書き換えたいときは、先に主キーを取り出して `where_in` で絞ります。

```rust
let ids: Vec<i64> = DB::table("posts").latest().limit(10).pluck::<i64>("id").await?;
DB::table("posts").where_in("id", &ids).update(&[("status", "archived".into())]).await?;
```

`truncate()` は、ドライバで流す文が違います。

| ドライバ   | 流す文                               | 気をつけること                             |
|------------|--------------------------------------|--------------------------------------------|
| SQLite     | `delete from` と、採番を戻す文       | 巻き戻せます                               |
| MySQL      | `truncate table`                     | **トランザクションを勝手に確定します**     |
| PostgreSQL | `truncate table … restart identity`  | 巻き戻せます                               |

**ほかの表から外部キーで指されている表は、MySQL と PostgreSQL では `truncate()` できません。**
指している側の表が空でも断られます。その表を空にするときは `delete()` を使ってください
（採番は戻りません）。

### 数を増やす・減らす

```rust
// views = views + 1 を 1 文で流す（返るのは件数）
let changed = DB::table("posts").where_("id", id).increment("views", 1).await?;
let changed = DB::table("posts").where_("id", id).decrement("stock", 1).await?;
```

**`update` で数を増やしてはいけません。** 読んだ値に 1 を足して書き戻す形は、
同時に 2 本来ると片方の分が消えます。`increment` は 1 文で流すので消えません。

`updated_at` は入りません。`join` や `limit` を付けるとエラーです。

### 渡せる値の数

**1 つの文に渡せる値（`?`）の総数には、ドライバごとの上限があります。**
超えるときは、投げる前にエラーにします。

| ドライバ   | 上限   |
|------------|--------|
| SQLite     | 32,766 |
| MySQL      | 65,535 |
| PostgreSQL | 65,535 |

- `insert_many` は「行 × 列」が値の数です。10 列の表なら、SQLite では 3,276 行までです。
  多いときは行を分けて、何回かに分けて呼んでください。
- `where_in` を重ねたときは、その合計で数えます。

### 入らなかった理由を見分ける

一意制約（`unique` と主キー）に当たったかどうかを聞けます。

```rust
use bengara::database::is_unique_violation;

if let Err(error) = DB::table("users").insert(&[("email", email.into())]).await {
    if is_unique_violation(&error) {
        return abort_with(422, "そのメールアドレスは登録済みです");
    }
    return Err(error);
}
```

- **エラーの文を自分で読まないでください。** 文はドライバごとに違います。
- 見分けられるのは一意制約だけです。
- **使えるのはトランザクションの外（`DB::table`）だけです。** `tx` の中で 1 本失敗すると、
  そのトランザクションは使えなくなります（下の「使えなくなるとき」）。

### 日時

`created_at` のような列には、`now()` で作った文字列を入れます。

```rust
DB::table("posts").insert(&[("title", "x".into()), ("created_at", now().into())]).await?;
```

`now()` は `YYYY-MM-DD HH:MM:SS`（UTC）を返します。日時専用の型はありません。
モデル経由（`save()`）なら自動で入ります（[models.md](models.md)）。

## ページ分け

```rust
let page = DB::table("posts").latest().paginate(15, req.page()).await?;

json(&page)   // data / total / per_page / current_page が出ます
```

ページ番号は**引数で渡します。** `req.page()` が `?page=` を読みます（無ければ 1）。
返るのは `Paginator` です。

| 名前                                           | 意味                                   |
|------------------------------------------------|----------------------------------------|
| `data` / `total` / `per_page` / `current_page` | 中身と件数（そのまま読める値）         |
| `last_page()`                                  | 最後のページ番号（0 件でも 1）         |
| `has_more_pages()`                             | 次があるか                             |
| `from()` / `to()`                              | 全体で何件目から何件目か。空なら `None` |
| `len()` / `is_empty()`                         | このページの件数 / 空か                |

- 問い合わせは 2 回です（件数と中身）。件数が 0 のときは、中身を問い合わせません。
- `per_page` と `page` に 0 を渡すと、1 として扱います。

## トランザクション

```rust
let tx = DB::begin().await?;

tx.table("posts").insert(&[("title", "x".into())]).await?;
tx.table("posts").where_("id", 1).update(&[("views", 1.into())]).await?;

tx.commit().await?;
```

- `tx.table(...)` で作った問い合わせは、そのトランザクションの中で動きます。
- **`commit()` を呼ばずに `tx` が落ちたら巻き戻します。** 明示するなら `tx.rollback().await?`。
- `DB::table(...)` はトランザクションの外です。混ぜないでください。
- **1 本ずつ `await` してください。** 同時に 2 本投げるとエラーになります。
- モデルを中で読み書きするときは、`save_using(&tx)` などを使います
  （[models.md](models.md) の「トランザクションの中で使う」）。

### 使えなくなるとき

**1 本でも失敗したトランザクションは、二度と使えません。** `DB::begin()` からやり直してください。
書いたつもりの内容が黙って捨てられないようにするためです。

| とき                                           | 以後の問い合わせ            | 続けられるか             |
|------------------------------------------------|-----------------------------|--------------------------|
| `commit()` / `rollback()` が済んだ             | エラー                      | 続けられません           |
| `tx` が落ちた（`commit()` を呼ばなかった）     | エラー                      | 続けられません           |
| 問い合わせの途中で中断された（`timeout` など） | エラー                      | 続けられません           |
| 問い合わせがエラーを返した                     | エラー。`commit()` もエラー | 続けられません           |
| 同じトランザクションに 2 本を同時に投げた      | 2 本目がエラー              | 1 本目が終われば続けられます |

```
直前の問い合わせが失敗したか中断されたので、このトランザクションは使えません。
中身は巻き戻されます。DB::begin() からやり直してください
```

トランザクションの中で重なりを避けたいときは、先に `exists()` で確かめてください。

## SQL を直接書く

```rust
let rows = DB::select("select * from posts where views > ?", &[100.into()]).await?;
let affected = DB::statement("update posts set views = views + 1", &[]).await?;
```

| 書き方                      | 返るもの                                                    |
|-----------------------------|-------------------------------------------------------------|
| `DB::select(sql, &[値])`    | `Vec<Row>`                                                  |
| `DB::statement(sql, &[値])` | `Affected`（`rows` が変わった行の数、`last_insert_id` が採番） |

- 値の置き場所は**どのドライバでも `?`** で書きます。PostgreSQL では投げる前に
  `$1` `$2` … へ振り直します。
- `?` の数と値の数が合わないときはエラーです。文字列の中の `?` も数えます。
- トランザクションの中では `tx.select(...)` / `tx.statement(...)` を使います。決まりは同じです。

**ここだけは `$1` を直に書けます。** PostgreSQL で、`?` を 1 つも書いていないときの例外です。

```rust
DB::statement("update items set price = $1::numeric where id = $2", &["12.5".into(), id.into()]).await?;
```

| 書く所                                  | `?`      | `$1` の直書き |
|-----------------------------------------|----------|---------------|
| `DB::select` / `DB::statement`          | 使えます | **使えます**  |
| `tx.select` / `tx.statement`            | 使えます | **使えます**  |
| ビルダーの `where_raw` / `having_raw` / `order_by_raw` | 使えます | エラー |

### 組み立てた SQL を見る

`to_sql()` は、組み立てた `select` の SQL と値の一覧を返します。接続しません。

```rust
let (sql, bindings) = DB::table("posts").where_("id", 1).to_sql();
assert_eq!(sql, "select * from \"posts\" where \"id\" = ?");
```

`{:?}` で表示すると、SQL に加えて、組み立てのときに見つけた間違いも出ます。

## 接続を選ぶ

```rust
let rows = DB::connection("logs").table("events").get().await?;
let tx = DB::connection("logs").begin().await?;
```

`DB::connection("名前")` からは `table` / `select` / `statement` / `begin` が使えます。

| 書き方                       | 返るもの                                   |
|------------------------------|--------------------------------------------|
| `DB::driver().await?`        | 既定の接続のデータベースの種類（`Driver`） |
| `DB::table_names().await?`   | 既定の接続にある表の名前の一覧             |

## ログ

投げた SQL は既定では出ません。見たいときは次のようにします。

```sh
RUST_LOG=info,sqlx=debug cargo artisan serve
```

SQL が失敗したときのエラー文は、次のようになっています。

| 中身                           | エラー文に出るか                                   |
|--------------------------------|----------------------------------------------------|
| SQL の文                       | 出ます                                             |
| 渡した値（`?` の中身）         | bengara からは出しません                           |
| データベースが返した文         | そのまま出ます。**値が入っていることがあります**   |

MySQL の一意制約違反は `Duplicate entry '値' for key ...` を返します。
この文はログとデバッグ画面にも出ます。利用者に見せる文は、`is_unique_violation` で
見分けて自分で書いてください。

## 列名と表名は検査します

外から来た文字列が SQL に混ざらないようにするためです。

| 渡すもの               | 使える文字                                     |
|------------------------|------------------------------------------------|
| 表名、条件や並びの列名 | 英数字と `_`。`.` で表名と列名を区切れます（`users.id`） |
| 書き込む列の名前       | 英数字と `_` だけ。**表名は付けられません**    |
| 演算子                 | 下の一覧にあるものだけ                         |

- 英数字は Unicode で見るので、日本語の列名（`"題名"`）も通ります。
- `.` の前後が空の名前（`.`・`a..b`・`users.`）はエラーです。
- **間違いは、終端のメソッド（`get()` や `insert()`）が `Err` で返します。**
  組み立ての途中ではパニックしません。

### 式が書けるのは 6 つだけ

次の 6 つは、渡した文字列をそのまま SQL に置きます。**検査しません。**
外から来た文字列を渡さないでください。

| 式が書けるもの               | どこに入るか   |
|------------------------------|----------------|
| `select` / `add_select`      | 取る列         |
| `where_raw` / `or_where_raw` | 条件           |
| `having_raw`                 | 束ねた後の条件 |
| `order_by_raw`               | 並び順         |

`select` / `add_select` を除く 4 つは、PostgreSQL で `$` を書くとエラーです。

### 演算子の一覧

| 使う所                                  | 渡せる演算子                                                              |
|-----------------------------------------|---------------------------------------------------------------------------|
| `where_op` / `or_where_op` / `having_op` | `=` `!=` `<>` `<` `<=` `>` `>=` `like` `not like` `ilike` `not ilike` `is` `is not` |
| `where_column` / `join` / `left_join`   | 上に加えて `<=>`（MySQL だけの演算子）                                    |

- 大文字と小文字は区別しません。
- `in` と `between` は `where_op` に渡せません。`where_in` / `where_between` を使ってください。
- **`is` / `is not` に渡せるのは `NULL` だけです。** `where_op("flag", "is", true)` はエラーです。
  値と比べるときは `=` / `!=` を使ってください。
- 列と列を比べる側は `in` / `between` も検査を通りますが、SQL として成り立ちません。
  使わないでください。

## ドライバの違い

**同じコードが 3 つのドライバで動きます。** SQL の引用符・値の置き場所・列の型名は
bengara が書き分けます。それでも残る違いは次のとおりです。

| こと                     | 残る違い                                                                                   |
|--------------------------|--------------------------------------------------------------------------------------------|
| 日付・日時・JSON の列    | `Schema` で作ると、PostgreSQL では **`text` 列**になります（[migrations.md](migrations.md) の「列の種類」） |
| `decimal` の読み書き     | 読むと文字列（SQLite だけ小数）、書くときは数です（下の「`decimal` の列」）                |
| 整数の列を文字列と比べる | `DB::table(..).where_("id", "abc")` は PostgreSQL だけ SQL エラーです。整数で渡してください。モデルは自分で直します（[models.md](models.md)） |
| `NULL` を渡す場所        | PostgreSQL では、型が文から決まらない所（`select ?`、`? is null`）に `None` を渡せません。`cast(? as text)` と書きます |
| 真偽の列の生の値         | SQLite では `Int`、ほかは `Bool`（下の「読み出した値」）                                   |
| 自動採番の番号           | `insert_get_id()` はどれでも使えます。`Affected` の `last_insert_id` は PostgreSQL では常に `None` です |
| 読めない型               | PostgreSQL の `uuid` / `money` / `inet` / 配列 / 範囲 / `interval` などはエラーです（下の「読み出した値」） |
| `ilike`                  | PostgreSQL だけの演算子です。ほかでは SQL エラーです                                       |
| `<=>`                    | MySQL だけの演算子です                                                                     |
| `truncate()`             | 流す文が違います（上の「更新する・消す」）                                                 |
| 1 つの文に渡せる値の数   | 上限が違います（上の「渡せる値の数」）                                                     |
| 用意した文の使い回し     | PostgreSQL では、問い合わせのたびに文を用意し直します。ほかは接続が覚えます                |
| 外部キー                 | MySQL と PostgreSQL では常に効きます。`foreign_keys(false)` は SQLite だけに効きます       |
| 文字コードと照合順序     | 指定できません。サーバの既定に従います                                                     |

**SQLite で開発していると、PostgreSQL だけの決まりに気づけません。** 本番と同じドライバ
でもテストを流してください（下の「テスト」）。

### `decimal` の列

読みと書きで形が違います。

```rust
let price: f64 = row.get("price")?;                            // 数として読む
DB::table("items").update(&[("price", 12.5.into())]).await?;   // 数で書く
```

- **読んだ生の値（文字列）をそのまま書き戻すと、PostgreSQL だけが断ります。**
  `row.get::<f64>(..)` で数として読んでから書いてください。
- 桁を落とさずに書きたいときは、`DB::statement` で `$1::numeric` を自分で書きます。

### 読み出した値

`row.get::<T>()` は型を合わせて読むので、**どのドライバでも同じ結果になります。**
下の表は `row.value()` で生の `Value` を見たときの種類です。

| 列                    | SQLite      | MySQL   | PostgreSQL |
|-----------------------|-------------|---------|------------|
| 整数                  | `Int`       | `Int`   | `Int`      |
| 小数（float/double）  | `Float`     | `Float` | `Float`    |
| 真偽（boolean）       | **`Int`**   | `Bool`  | `Bool`     |
| `decimal` / `numeric` | **`Float`** | `Text`  | `Text`     |
| 文字列                | `Text`      | `Text`  | `Text`     |
| 日付・時刻・日時      | `Text`      | `Text`  | `Text`     |
| JSON                  | `Text`      | `Text`  | `Text`     |
| バイト列              | `Bytes`     | `Bytes` | `Bytes`    |
| `null`                | `Null`      | `Null`  | `Null`     |

- 文字列で返る `decimal` は、末尾の 0 を落とします（`12.50` も `12.5000` も `12.5`）。
- MySQL と PostgreSQL の日付は `YYYY-MM-DD`、時刻は `HH:MM:SS`、日時は
  `YYYY-MM-DD HH:MM:SS` に直して返します。PostgreSQL の `timestamptz` は **UTC に直します。**
- MariaDB の `json` 列の生の値は `Bytes` です。`row.get::<String>()` なら文字列で読めます。

MySQL だけにある型は、次のように読みます。

| MySQL の型                   | 生の値                                                 |
|------------------------------|--------------------------------------------------------|
| 符号なしの整数（`unsigned`） | `Int`。`i64` に収まらない値は、丸めずに `Text`         |
| `year` / `bit`               | `Int`                                                  |
| `enum` / `set`               | `Text`                                                 |
| 空間の型（`geometry` など）  | `Bytes`（中身は解釈しません。`st_astext(列)` で文字列にできます） |

**読めない型は、黙って別の値にせずエラーにします。** `select` で文字列に変えれば読めます。

```
列 `id` の型 `UUID` は読めません。select で文字列に変換して取り出してください（例: `cast(列 as text) as 列`）
```

### 自動採番の番号

`insert_get_id()` は、**どのドライバでも同じように使えます。**
PostgreSQL では内側で `returning "id"` を付けます。SQLite と MySQL は、接続が覚えている番号を使います。

主キーの名前が `id` でないときは、`insert_get_id_as(値, 主キーの名前)` を使います。
**その名前を見るのは PostgreSQL だけです。**

| ドライバ   | 返るもの                  | 渡した名前の使い道    |
|------------|---------------------------|-----------------------|
| PostgreSQL | `returning 渡した列` の値 | 列名として SQL に入る |
| SQLite     | 直前に入れた行の rowid    | 形を確かめるだけ      |
| MySQL      | `auto_increment` の列の値 | 形を確かめるだけ      |

自動採番の列を渡しているなら、どのドライバでも正しい値が返ります。
**自動採番でない列を渡したときは、ドライバで動きが違います。**

| ドライバ | 自動採番の無い表に使ったとき                                           |
|----------|------------------------------------------------------------------------|
| SQLite   | 主キーとは別の値（rowid）が返ります                                    |
| MySQL    | エラーになります（`insert が ID を返しませんでした`）。**行は入っています** |

自動採番の無い表には `insert()` を使ってください。

`DB::statement` で自分で `insert` を書いたときは、`Affected` の `last_insert_id` を見ます。

| 流した文                  | SQLite   | MySQL    | PostgreSQL |
|---------------------------|----------|----------|------------|
| `insert` / `replace`      | 入ります | 入ります | `None`     |
| それ以外                  | `None`   | `None`   | `None`     |

PostgreSQL では、`returning` を自分で書いて `DB::select` で受け取ってください。

### テスト

SQLite はテスト用のデータベースをメモリの上に逃がせます（`DB_TEST_DATABASE=:memory:`）。
MySQL と PostgreSQL は**すでにあるデータベース**を指すので、逃がせません。

**MySQL と PostgreSQL では、`DB_TEST_DATABASE` に別のデータベース名を必ず書いてください。**
書かないと、テストの最初につなぐところで止まります。テストは表を全部消して作り直すので
（[testing.md](testing.md)）、開発用のデータベースを守るための確かめです。

**テストの間は、接続名よりテスト用のつなぎ先が優先されます。**
`DB::connection("pgsql")` と書いても、問い合わせと SQL の組み立てはテスト用の 1 本に
そろいます。別のドライバの SQL を確かめたいときは、そのドライバでテストを走らせてください。

## できないこと・エラーになること

| 書き方                                                                          | どうなるか                             | 直し方                                   |
|---------------------------------------------------------------------------------|----------------------------------------|------------------------------------------|
| 機能フラグ無しで `DB::...` を呼ぶ                                               | 直し方を書いたエラー                   | `features` に `"sqlite"` などを足す      |
| 使えない文字を含む列名・表名                                                    | エラー                                 | 上の「列名と表名は検査します」           |
| `insert` / `update` / `increment` の列名に表名を付ける（`posts.title`）         | エラー                                 | 列の名前だけを渡す                       |
| `where_op` / `having_op` に `in` / `between` / `<=>`                            | エラー                                 | `where_in` / `where_between` を使う      |
| `where_op(列, "is", NULL 以外の値)`                                             | エラー                                 | `=` / `!=` か `where_null` を使う        |
| `where_op(列, ">", None)` のように、`=` `!=` 以外で `NULL` と比べる             | **必ず 0 件**                          | `where_null` / `where_not_null` を使う   |
| `where_in` に 30,000 件を超える一覧                                             | エラー                                 | 一覧を分けて問い合わせる                 |
| 1 つの文に渡す値が上限を超える                                                  | 投げる前にエラー                       | 分けて何回かに分けて流す                 |
| `where_raw` などの `?` の数と値の数が合わない                                   | エラー                                 | 数をそろえる                             |
| ビルダーの `*_raw` に PostgreSQL で `$` を書く                                  | エラー                                 | `?` を使う                               |
| `group_by` の無い `having`                                                      | エラー                                 | 束ねずに絞るなら `where`                 |
| `having_op` に `select` で付けた別名                                            | PostgreSQL で SQL エラー               | `having_raw` に式を書く                  |
| `sum` / `avg` / `min` / `max` に `group_by`                                     | エラー                                 | `select` に式を書いて `get()`            |
| `distinct()` を付けた `sum` などに、式や `*`                                    | エラー                                 | ただの列名を渡す                         |
| `distinct()` を付けた `count()` / `paginate()` で、数える列が決まらない         | エラー                                 | `select` で列を 1 つ指定する             |
| `value` / `pluck` / `sum` などに式を渡す                                        | エラー                                 | `select` に式を書いて `get()`            |
| `update` / `delete` / `truncate` に `join` / `limit` / `offset` / `group_by` / `having` / `distinct` | エラー            | 主キーを取り出して `where_in` で絞る     |
| `update` / `delete` に `order_by`                                               | **黙って無視**                         | 当たる行は変わりません                   |
| `where_` を付けた `truncate()`                                                  | エラー                                 | 絞って消すなら `delete()`                |
| 外部キーで指されている表に `truncate()`                                         | MySQL と PostgreSQL で SQL エラー      | `delete()` を使う                        |
| `insert` 系に `where_` / `join` / `limit` など                                  | エラー                                 | 足す値だけを渡す                         |
| 幅の狭い型に収まらない値、小数部のある値を整数として読む                        | 読み出しでエラー                       | 広い型か `f64` で読む                    |
| NULL の値を `Option` でない型で読む                                             | 読み出しでエラー                       | `Option<T>` で読む                       |
| トランザクションで同時に 2 本投げる                                             | エラー                                 | 1 本ずつ `await` する                    |
| 終わった・失敗したトランザクションで問い合わせる                                | エラー                                 | `DB::begin()` からやり直す               |
| 結果を少しずつ読む（ストリーム）                                                | **できません。** 結果は全件がメモリに載ります | `limit` / `offset` で分けて読む   |

まだ無いものは [backlog.md](backlog.md) にまとめてあります。
こう決めた理由は [decisions.md](decisions.md) にあります。

## 関連

- [migrations.md](migrations.md) — 表を作る
- [models.md](models.md) — 構造体として読み書きする
- [testing.md](testing.md) — DB を使うテスト
- [configuration.md](configuration.md) — `config/*.rs` と `.env`
