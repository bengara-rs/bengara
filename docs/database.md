# データベース

`DB::table(...)` で問い合わせを組み立てて、データベースを読み書きします。
SQLite・MySQL（MariaDB）・PostgreSQL で同じコードが動きます。

- 表の作り方は [migrations.md](migrations.md) にあります。
- 構造体として読み書きする方法は [models.md](models.md) にあります。

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
- フラグは 2 つ以上書けます。
- フラグが無くてもコンパイルは通ります。つなごうとしたときに、足すべきフラグを書いたエラーが返ります。
- `mysql` と `postgres` のビルドには C コンパイラが要ります（通信の暗号化のため）。`sqlite` だけなら要りません。
- Rust 1.85 で使うときは、依存を 1 つ固定してください（[getting-started.md](getting-started.md)）。

### `.env` に書くこと

SQLite のとき。

```
DB_CONNECTION=sqlite
DB_DATABASE=database/database.sqlite
DB_TEST_DATABASE=:memory:
```

MySQL / MariaDB / PostgreSQL のとき。

```
DB_CONNECTION=mysql
DB_DATABASE=myapp
DB_HOST=127.0.0.1
DB_PORT=
DB_USERNAME=myapp
DB_PASSWORD=ここにパスワード
DB_TEST_DATABASE=myapp_test
```

| 環境変数             | 既定                                                   |
|----------------------|--------------------------------------------------------|
| `DB_CONNECTION`      | `sqlite`                                               |
| `DB_DATABASE`        | SQLite は `database/database.sqlite`、ほかは `bengara` |
| `DB_HOST`            | `127.0.0.1`                                            |
| `DB_PORT`            | 空なら 3306（MySQL）/ 5432（PostgreSQL）               |
| `DB_USERNAME`        | `root`（MySQL）/ `postgres`（PostgreSQL）              |
| `DB_PASSWORD`        | 空（渡しません）                                       |
| `DB_URL`             | 空。書くと、上の値より優先します                       |
| `DB_MAX_CONNECTIONS` | 5（同時に張る接続の数）                                |
| `DB_TEST_DATABASE`   | SQLite は `:memory:`、ほかは無し（下の「テスト」）     |

- `DB_URL` は `DB_CONNECTION` で選んだ接続にだけ効きます。
- 値に `#` を含むときは引用符で囲みます（`DB_PASSWORD="p#ss"`）。
- MySQL と PostgreSQL は、**データベースを先に作っておいてください。** bengara は作りません。
- SQLite のファイルは無ければ作ります。置くディレクトリは先に用意してください。

## 読む

`DB::table("表の名前")` から始めて、条件を足していきます。
最後に `get()` などを呼ぶと、そこで SQL が流れます。

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

### 結果を取り出す

| 呼ぶもの          | 返るもの                                 |
|-------------------|------------------------------------------|
| `get()`           | `Vec<Row>`                               |
| `first()`         | `Option<Row>`                            |
| `find(id)`        | `Option<Row>`（**`id` という列**で引く） |
| `value::<T>(col)` | `Option<T>`（最初の行の 1 つの値）       |
| `pluck::<T>(col)` | `Vec<T>`（1 列ぶん）                     |

```rust
let titles = DB::table("posts").pluck::<String>("title").await?;

let row = DB::table("posts").find(1).await?.unwrap();
let title: String = row.get("title")?;        // 無い列・型違いはエラー
let body: Option<String> = row.get("body")?;  // null を許すとき
let maybe = row.try_get::<i64>("views");      // 読めなければ None
```

- 読める型は、整数（`i8`〜`i64`・`u8`〜`u64`・`isize`・`usize`）、`f32`・`f64`・`bool`・`String`・`Vec<u8>`・`Value`（生の値）です。
- **NULL がありうる列は `Option<T>` で読みます。** `T` のままだと NULL の行でエラーです。`pluck::<Option<String>>("body")` も同じです。
- 型に収まらない値はエラーです。小数を整数で読むと、`2.0` は `2` になり、`2.5` はエラーです。
- 主キーが `id` でない表は、`find` の代わりに `where_(列, 値).first()` を使います。
- **同じ名前の列が 2 つあると、左の列を採ります。** `join` したら `select` で取る列を決めてください。
- `Row` はそのまま `json(&rows)` で返せます。バイナリの列は数の配列になるので、`select` で外してください。

### 条件を足す

| 書き方                                                 | 意味                           |
|--------------------------------------------------------|--------------------------------|
| `where_("status", "published")`                        | 等しい                         |
| `where_op("views", ">", 100)`                          | 演算子を指定する               |
| `or_where(...)` / `or_where_op(...)`                   | `or` でつなぐ                  |
| `where_in("id", &[1, 2, 3])` / `where_not_in`          | 一覧のどれか                   |
| `or_where_in(...)`                                     | `or` でつないで、一覧のどれか  |
| `where_null("body")` / `where_not_null`                | NULL かどうか                  |
| `where_between("views", 0, 100)` / `where_not_between` | 範囲（両端を含む）             |
| `where_like("title", "%そば%")`                        | 形に当てはまる                 |
| `where_column("updated_at", ">", "created_at")`        | 列と列を比べる                 |
| `where_raw("length(title) > ?", &[3.into()])`          | SQL を直接書く                 |
| `or_where_raw(...)`                                    | `or` でつないで、SQL を直接書く |
| `where_group(\|q\| q.where_("a", 1).or_where("b", 2))` | 括弧でくくる                   |
| `or_where_group(...)`                                  | `or` でつないで、括弧でくくる  |

`where` は Rust の予約語なので `where_` です。
Laravel の `->where('views', '>', 100)` は `where_op("views", ">", 100)` と書きます。

- `None`（NULL）と `=` で比べると `is null` になります。`!=` なら `is not null` です。
- `>` などで NULL と比べると、必ず 0 件です。`where_null` を使ってください。
- `where_in` に空の一覧を渡すと 0 件です。渡せるのは 30,000 件までです。
- `where_group` の中では条件だけが使われます。`limit` などは捨てます。
- `where_raw` の `?` は値の置き場所です。文字列の中の `?` も数えます。

### 並べる・件数を決める

| 書き方                                           | 意味                       |
|--------------------------------------------------|----------------------------|
| `order_by(col)` / `order_by_desc(col)`           | 並べる                     |
| `order_by_raw("sql")`                            | 並べ方を SQL で書く        |
| `latest()` / `oldest()`                          | `created_at` の降順 / 昇順 |
| `latest_by(col)` / `oldest_by(col)`              | 列を指定して並べる         |
| `limit(n)` / `offset(n)` / `take(n)` / `skip(n)` | 件数                       |
| `for_page(page, per_page)`                       | ページから件数を決める     |

**並びが一意になるようにしてください。** 同じ値の行があると、ページの境目で同じ行が 2 回出ます。
`latest()` に `order_by_desc("id")` を足すのが簡単です。

### 表をつなぐ・束ねる

| 書き方                                            | 意味                 |
|---------------------------------------------------|----------------------|
| `join("users", "users.id", "=", "posts.user_id")` | 結合                 |
| `left_join(...)`                                  | 外側結合             |
| `select(&["id", "title"])` / `add_select("body")` | 取る列（式も書ける） |
| `distinct()`                                      | 重なりを捨てる       |
| `group_by(&["status"])`                           | 束ねる               |
| `having_op(col, 演算子, 値)`                      | 束ねた結果で絞る     |
| `having_raw("sql", &[値])`                        | 絞り方を SQL で書く  |

```rust
DB::table("posts")
    .select(&["status", "count(*) as total"])
    .group_by(&["status"])
    .having_raw("count(*) > ?", &[2.into()])
    .order_by_desc("total")
    .get()
    .await?;
```

- `group_by` の無い `having` はエラーです。
- `having_op` に `select` の別名（上の `total`）を渡すと、PostgreSQL で SQL エラーです。`having_raw` に式を書いてください。

### 数える

```rust
let total = DB::table("posts").count().await?;                          // i64
let any = DB::table("posts").where_("status", "draft").exists().await?; // bool
let none = DB::table("posts").doesnt_exist().await?;
let sum = DB::table("posts").sum::<i64>("views").await?;                // Option<i64>
// avg / min / max も同じ形
```

- `group_by` を付けた `count()` は、**グループの数**を返します。`paginate` の件数も同じです。
- `distinct()` を付けて数えるときは、`select` で列を 1 つ指定してください。
- 行が無いとき、`sum` / `avg` / `min` / `max` は `None` です。
- `sum` などは `group_by` と一緒に使えません。`*` も渡せません。

## 書き込む

```rust
// 1 行
DB::table("posts").insert(&[("title", "はじめての記事".into())]).await?;

// 入れて ID を受け取る
let id = DB::table("posts").insert_get_id(&[("title", "次の記事".into())]).await?;

// まとめて
let rows = vec![vec![("title", "A".into())], vec![("title", "B".into())]];
DB::table("posts").insert_many(&rows).await?;

// 更新・削除（返るのは件数）
let changed = DB::table("posts").where_("id", id).update(&[("title", "改題".into())]).await?;
let removed = DB::table("posts").where_("id", id).delete().await?;

// 全部消す（自動採番も 1 に戻る）
DB::table("posts").truncate().await?;
```

- 値は `.into()` で渡します。文字列・整数・小数・真偽・`Vec<u8>`・`Option<T>`（`None` は NULL）が使えます。
- `insert_many` は、どの行も列の名前と並びを最初の行とそろえてください。
- **条件を書かない `update` / `delete` は全行が対象です。** Laravel と同じです。
- `update` は `updated_at` を入れません。要るなら自分で入れます。
- `update` / `delete` に `join` や `limit` は付けられません。先に ID を取り出して `where_in` で絞ります。
- `i64` に収まらない `u64` は、文字列として書き込みます。文字列の列に入れてください。

```rust
let ids: Vec<i64> = DB::table("posts").latest().limit(10).pluck::<i64>("id").await?;
DB::table("posts").where_in("id", &ids).update(&[("status", "archived".into())]).await?;
```

### 数を増やす・減らす

```rust
let changed = DB::table("posts").where_("id", id).increment("views", 1).await?;
let changed = DB::table("posts").where_("id", id).decrement("stock", 1).await?;
```

**数を増やすのに `update` を使わないでください。** 同時に 2 本来ると、片方の分が消えます。
`increment` は 1 文で流すので消えません。`updated_at` は入りません。

### 日時

`created_at` のような列には、`now()` の文字列を入れます。

```rust
DB::table("posts").insert(&[("title", "x".into()), ("created_at", now().into())]).await?;
```

- `now()` は `YYYY-MM-DD HH:MM:SS`（UTC）を返します。
- 日時専用の型はありません。
- モデルの `save()` なら自動で入ります（[models.md](models.md)）。

### 一意制約に当たったか

```rust
use bengara::database::is_unique_violation;

if let Err(error) = DB::table("users").insert(&[("email", email.into())]).await {
    if is_unique_violation(&error) {
        return abort_with(422, "そのメールアドレスは登録済みです");
    }
    return Err(error);
}
```

- エラーの文はドライバごとに違います。文を読まずに、これで見分けてください。
- 見分けられるのは一意制約（`unique` と主キー）だけです。
- トランザクションの中では使えません。1 本失敗すると、そのトランザクションは使えなくなるためです。

## ページ分け

```rust
let page = DB::table("posts").latest().paginate(15, req.page()).await?;

json(&page)   // data / total / per_page / current_page が出ます
```

`req.page()` は `?page=` を読みます（無ければ 1）。

| 名前                                           | 意味                                     |
|------------------------------------------------|------------------------------------------|
| `data` / `total` / `per_page` / `current_page` | 中身と件数                               |
| `last_page()`                                  | 最後のページ番号（0 件でも 1）           |
| `has_more_pages()`                             | 次があるか                               |
| `from()` / `to()`                              | 全体の何件目から何件目か。空なら `None`  |
| `len()` / `is_empty()`                         | このページの件数 / 空か                  |

- 件数と中身で 2 回問い合わせます。0 件なら中身は問い合わせません。
- `per_page` と `page` の 0 は 1 として扱います。

## トランザクション

```rust
let tx = DB::begin().await?;

tx.table("posts").insert(&[("title", "x".into())]).await?;
tx.table("posts").where_("id", 1).update(&[("views", 1.into())]).await?;

tx.commit().await?;
```

- `tx.table(...)` はトランザクションの中、`DB::table(...)` は外です。混ぜないでください。
- `commit()` を呼ばずに `tx` が捨てられたら、巻き戻します。明示するなら `tx.rollback().await?` です。
- **1 本ずつ `await` してください。** 同時に 2 本投げると、2 本目がエラーです。
- モデルを中で使うときは `save_using(&tx)` などを使います（[models.md](models.md)）。

**1 本でも失敗したトランザクションは、もう使えません。** 以後の問い合わせも `commit()` もエラーです。
`timeout` などで途中で止めたときも同じです。`DB::begin()` からやり直してください。

## SQL を直接書く

```rust
let rows = DB::select("select * from posts where views > ?", &[100.into()]).await?;
let affected = DB::statement("update posts set views = views + 1", &[]).await?;
```

| 書き方                      | 返るもの                                                         |
|-----------------------------|------------------------------------------------------------------|
| `DB::select(sql, &[値])`    | `Vec<Row>`                                                       |
| `DB::statement(sql, &[値])` | `Affected`（`rows` は変わった行の数、`last_insert_id` は採番の値） |

- 値の置き場所は、どのドライバでも `?` です。PostgreSQL では bengara が `$1` `$2` に直します。
- `?` の数と値の数が合わないとエラーです。
- トランザクションの中では `tx.select(...)` / `tx.statement(...)` です。
- PostgreSQL で `?` を 1 つも書かないときは、`$1` を直に書けます。`where_raw` などでは書けません。

```rust
DB::statement("update items set price = $1::numeric where id = $2", &["12.5".into(), id.into()]).await?;
```

組み立てた SQL を見るには `to_sql()` を使います。接続はしません。

```rust
let (sql, bindings) = DB::table("posts").where_("id", 1).to_sql();
assert_eq!(sql, "select * from \"posts\" where \"id\" = ?");
```

## 接続を選ぶ

```rust
let rows = DB::connection("logs").table("events").get().await?;
let tx = DB::connection("logs").begin().await?;
```

- `DB::connection("名前")` からは `table` / `select` / `statement` / `begin` が使えます。
- `DB::driver().await?` は既定の接続の種類を返します。
- `DB::table_names().await?` は既定の接続にある表の名前を返します。

### `config/database.rs`

`init` が作る雛形に入っています。置かなくても、`.env` から `sqlite` / `mysql` / `mariadb` / `pgsql` の 4 本が作られます。
接続を足すときは、ここに書きます。

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

| 指定                 | 既定                | 意味                                            |
|----------------------|---------------------|-------------------------------------------------|
| `host(s)`            | `127.0.0.1`         | つなぎ先。空の文字列は無視                      |
| `port(n)`            | 3306 / 5432         | ポート。0 は無視                                |
| `username(s)`        | `root` / `postgres` | 利用者名。空の文字列は無視                      |
| `password(s)`        | 空                  | パスワード。空なら渡さない                      |
| `max_connections(n)` | 5                   | 同時に張る接続の数。メモリ上の SQLite は 1      |
| `foreign_keys(b)`    | 真                  | SQLite で外部キーを効かせるか                   |
| `url(s)`             | 空                  | 接続文字列。書くと、ほかの値より優先            |

接続文字列の形は次のとおりです。
パスワードに `@` `#` `/` `?` があるときは、`%40` のような形（パーセントエンコード）に直します。

```
sqlite://database/database.sqlite
mysql://利用者名:パスワード@127.0.0.1:3306/データベース名
postgres://利用者名:パスワード@127.0.0.1:5432/データベース名
```

### つなぎ方の決まり

- 接続は、最初に使ったときに張ります。以後は使い回します。
- SQLite の相対パスは、プロジェクト直下から見ます。
- ファイルの SQLite は WAL モード（読みと書きを同時にできる形）で開きます。ほかが書いている間は 5 秒まで待ちます。
- 名前か `url` に `:memory:` を含むと、メモリ上のデータベースになります。
- MySQL と PostgreSQL は、サーバが対応していれば暗号化してつなぎます。証明書は OS のものでなく、同梱の一覧で確かめます。
- MySQL と PostgreSQL は、つながらないとき最長 30 秒待ってからエラーになります。

エラー文には、接続の名前とつなぎ先が出ます。接続文字列とパスワードは出しません。

```
MySQL につなげません（接続 `mysql`: 127.0.0.1:3306 / myapp）: 原因
`logs` という接続は設定にありません（ある接続: sqlite, mysql, mariadb, pgsql）。config/database.rs を確かめてください
```

## テスト

- SQLite は、テスト用のデータベースをメモリの上に作れます（`DB_TEST_DATABASE=:memory:`）。
- **MySQL と PostgreSQL では、`DB_TEST_DATABASE` に別のデータベース名を必ず書いてください。** 書かないとテストが止まります。
- テストは表を全部消して作り直します（[testing.md](testing.md)）。開発用のデータベースを守るための決まりです。
- テストの間は、`DB::connection("pgsql")` と書いても、テスト用の 1 本につながります。
- **SQLite だけで開発すると、PostgreSQL だけの決まりに気づけません。** 本番と同じドライバでもテストを流してください。

## ドライバの違い

引用符・値の置き場所・列の型名は bengara が書き分けます。それでも残る違いは次のとおりです。

| こと                         | SQLite                       | MySQL                        | PostgreSQL                         |
|------------------------------|------------------------------|------------------------------|------------------------------------|
| 真偽の列の生の値             | `Int`                        | `Bool`                       | `Bool`                             |
| `decimal` の生の値           | `Float`                      | `Text`                       | `Text`                             |
| `decimal` に文字列を書き戻す | 通る                         | 通る                         | **断られる**。`f64` で読んで書く   |
| 整数の列を文字列と比べる     | 通る                         | 通る                         | **SQL エラー**。整数で渡す         |
| 型の決まらない所の `None`    | 通る                         | 通る                         | エラー。`cast(? as text)` と書く   |
| `truncate()` で流す文        | `delete` と採番の巻き戻し    | `truncate table`             | `truncate table … restart identity` |
| `truncate()` の巻き戻し      | できる                       | **トランザクションを確定する** | できる                           |
| 外部キーで指される表の `truncate()` | 指す側が空ならできる  | エラー（指す側が空でも）     | エラー（指す側が空でも）           |
| 1 つの文に渡せる値の数       | 32,766                       | 65,535                       | 65,535                             |
| `Affected` の `last_insert_id` | `insert` で入る            | `insert` で入る              | 常に `None`                        |
| 外部キー                     | `foreign_keys(false)` で切れる | 常に効く                   | 常に効く                           |
| 使える演算子                 | —                            | `<=>`                        | `ilike` / `not ilike`              |
| 読めない型                   | —                            | —                            | `uuid` / `money` / `inet` / 配列 / 範囲 / `interval` など |

- 値の数の上限を超えると、投げる前にエラーにします。`insert_many` は「行 × 列」で数えます。
- `truncate()` できない表は `delete()` で空にします（採番は戻りません）。
- 読めない型は `select` で文字列にすれば読めます（`cast(列 as text) as 列`）。
- `Schema` で作る日付・日時・JSON の列は、PostgreSQL では `text` 列になります（[migrations.md](migrations.md)）。
- 文字コードと照合順序は指定できません。サーバの既定に従います。

### 読んだ値の形

`row.get::<T>()` は型を合わせるので、どのドライバでも同じ結果です。
生の `Value`（`row.value()`）を見るときだけ、次に気をつけてください。

- 日付は `YYYY-MM-DD`、時刻は `HH:MM:SS`、日時は `YYYY-MM-DD HH:MM:SS` の文字列です。
- PostgreSQL の `timestamptz` は UTC に直します。
- 文字列の `decimal` は末尾の 0 を落とします（`12.50` は `12.5`）。
- MariaDB の `json` 列は `Bytes` です。`row.get::<String>()` なら文字列で読めます。
- MySQL の `unsigned` で `i64` に収まらない値は `Text` です。`year` と `bit` は `Int`、`enum` と `set` は `Text`、空間の型は `Bytes` です。

### 自動採番の番号

`insert_get_id()` はどのドライバでも使えます。
主キーの名前が `id` でないときは、`insert_get_id_as(値, 主キーの名前)` を使います。

- 自動採番の無い表には `insert()` を使ってください。
- 自動採番の無い表に `insert_get_id` を使うと、SQLite は rowid（別の番号）を返します。MySQL はエラーですが、**行は入っています。**
- PostgreSQL で自分で `insert` を書くときは、`returning` を付けて `DB::select` で受け取ります。

## 名前の検査と、式が書ける所

外から来た文字列が SQL に混ざらないように、表名と列名を検査します。

- 使えるのは英数字（日本語も可）と `_` です。`users.id` のように `.` で表名を付けられます。
- 書き込む列の名前には、表名を付けられません（`posts.title` はエラー）。
- 間違いは `get()` や `insert()` が `Err` で返します。組み立ての途中では止まりません。

次の 6 つは、文字列をそのまま SQL に置きます。**外から来た文字列を渡さないでください。**

- `select` / `add_select`
- `where_raw` / `or_where_raw`
- `having_raw`
- `order_by_raw`

### 使える演算子

| 使う所                                   | 演算子                                                                      |
|------------------------------------------|-----------------------------------------------------------------------------|
| `where_op` / `or_where_op` / `having_op` | `=` `!=` `<>` `<` `<=` `>` `>=` `like` `not like` `ilike` `not ilike` `is` `is not` |
| `where_column` / `join` / `left_join`    | `=` `!=` `<>` `<` `<=` `>` `>=` `<=>` `like` `not like` `ilike` `not ilike` |

- 大文字と小文字は区別しません。
- `in` と `between` は渡せません。`where_in` / `where_between` を使います。
- `is` / `is not` に渡せるのは NULL だけです。

## ログ

投げた SQL を見るときは、次のように起動します。

```sh
RUST_LOG=info,sqlx=debug cargo artisan serve
```

- エラー文には SQL の文が出ます。渡した値は bengara からは出しません。
- データベースが返した文はそのまま出ます。**値が入っていることがあります**（MySQL の `Duplicate entry '値'` など）。
- 利用者に見せる文は、`is_unique_violation` で見分けて自分で書いてください。

## できないこと

- 結果を少しずつ読むこと（ストリーム）はできません。全件がメモリに載るので、`limit` / `offset` で分けて読みます。
- `update` / `delete` の `order_by` は黙って無視します。
- `where_` を付けた `truncate()` はエラーです。絞って消すなら `delete()` です。

まだ無い機能は [backlog.md](backlog.md) にまとめています。決めた理由は [decisions.md](decisions.md) にあります。

## 関連

- [migrations.md](migrations.md) — 表を作る
- [models.md](models.md) — 構造体として読み書きする
- [testing.md](testing.md) — DB を使うテスト
- [configuration.md](configuration.md) — `config/*.rs` と `.env`
