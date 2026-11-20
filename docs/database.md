# データベース

`DB::table(...)` で問い合わせを組み立てて、データベースを読み書きします。
SQLite・MySQL（MariaDB）・PostgreSQL で同じコードが動きます。

- 表の作り方は [migrations.md](migrations.md) にあります。
- 構造体として読み書きするなら [models.md](models.md) を見てください。

## 使えるようにする

使うデータベースを `Cargo.toml` の機能フラグ（`features`）に書きます。

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

- `cargo run -- init` が作る `Cargo.toml` には `sqlite` が入っています。フラグは 2 つ以上書けます。
- フラグが無いと、つなぐときに「足すべきフラグ」を書いたエラーが返ります。
- `mysql` と `postgres` のビルドには C コンパイラが要ります。

### `.env` に書くこと

| 環境変数             | 既定                                                   |
|----------------------|--------------------------------------------------------|
| `DB_CONNECTION`      | `sqlite`                                               |
| `DB_DATABASE`        | SQLite は `database/database.sqlite`、ほかは `bengara` |
| `DB_HOST`            | `127.0.0.1`                                            |
| `DB_PORT`            | 空なら 3306（MySQL）/ 5432（PostgreSQL）               |
| `DB_USERNAME`        | `root`（MySQL）/ `postgres`（PostgreSQL）              |
| `DB_PASSWORD`        | 空（渡しません）                                       |
| `DB_URL`             | 空。書くと上の値より優先します                         |
| `DB_MAX_CONNECTIONS` | 5（同時に張る接続の数）                                |
| `DB_TEST_DATABASE`   | SQLite は `:memory:`、ほかは無し（下の「テスト」）     |

- `DB_URL` は `DB_CONNECTION` で選んだ接続にだけ効きます。
- 値に `#` を含むときは引用符で囲みます（`DB_PASSWORD="p#ss"`）。
- **MySQL と PostgreSQL は、データベースを先に作っておきます。** bengara は作りません。
- SQLite のファイルは無ければ作ります。置くディレクトリは先に用意します。

## 読む

`DB::table("表の名前")` に条件を足していきます。
最後に `get()` などを呼んだときに、SQL が流れます。

```rust
let posts = DB::table("posts")
    .where_("status", "published")
    .where_op("views", ">", 100)
    .latest()
    .limit(10)
    .get()
    .await?;
```

### 結果を取り出す

| 呼ぶもの          | 返るもの                             |
|-------------------|--------------------------------------|
| `get()`           | `Vec<Row>`                           |
| `first()`         | `Option<Row>`                        |
| `find(id)`        | `Option<Row>`（`id` という列で引く） |
| `value::<T>(col)` | `Option<T>`（最初の行の 1 つの値）   |
| `pluck::<T>(col)` | `Vec<T>`（1 列ぶん）                 |

```rust
let titles = DB::table("posts").pluck::<String>("title").await?;

let row = DB::table("posts").find(1).await?.unwrap();
let title: String = row.get("title")?;        // 無い列・型違いはエラー
let body: Option<String> = row.get("body")?;  // NULL を許すとき
let maybe = row.try_get::<i64>("views");      // 読めなければ None
```

- 読める型は、整数・`f32`・`f64`・`bool`・`String`・`Vec<u8>`・`Carbon`（日時）・`Value`（生の値）です。
- **NULL がありうる列は `Option<T>` で読みます。** `T` のままだと、NULL の行でエラーです。
- 小数を整数で読むと、`2.0` は `2` になり、`2.5` はエラーです。
- 主キーが `id` でない表は、`where_(列, 値).first()` で引きます。
- 同じ名前の列が 2 つあると、左の列を採ります。`join` したら `select` で列を決めます。
- `Row` はそのまま `json(&rows)` で返せます。
- 結果は全件がメモリに載ります。多いときは `limit` / `offset` で分けて読みます。

### 条件を足す

`where` は Rust の予約語なので `where_` と書きます。
Laravel の `->where('views', '>', 100)` は `where_op("views", ">", 100)` です。

| 書き方                                                 | 意味                          |
|--------------------------------------------------------|-------------------------------|
| `where_("status", "published")`                        | 等しい                        |
| `where_op("views", ">", 100)`                          | 演算子を指定する              |
| `or_where(...)` / `or_where_op(...)`                   | `or` でつなぐ                 |
| `where_in("id", &[1, 2, 3])` / `where_not_in`          | 一覧のどれか                  |
| `or_where_in(...)`                                     | `or` でつないで、一覧のどれか |
| `where_null("body")` / `where_not_null`                | NULL かどうか                 |
| `where_between("views", 0, 100)` / `where_not_between` | 範囲（両端を含む）            |
| `where_like("title", "%そば%")`                        | 形に当てはまる                |
| `where_column("updated_at", ">", "created_at")`        | 列と列を比べる                |
| `where_raw("length(title) > ?", &[3.into()])`          | SQL を直接書く                |
| `or_where_raw(...)`                                    | `or` でつないで SQL を書く    |
| `where_group(\|q\| q.where_("a", 1).or_where("b", 2))` | 括弧でくくる                  |
| `or_where_group(...)`                                  | `or` でつないで括弧でくくる   |

- `None` と `=` で比べると `is null` になります。`!=` なら `is not null` です。
- `>` などで NULL と比べると必ず 0 件です。`where_null` を使います。
- `where_in` に空の一覧を渡すと 0 件です。渡せるのは 30,000 件までです。
- `where_group` の中では、条件だけが使われます。
- `where_raw` の `?` は値の置き場所です。文字列の中の `?` も数えます。
- 列には、モデルの型付きの列（`Post::TITLE`）も渡せます。値の型をコンパイルのときに確かめます（[models.md](models.md)
  の「型付きの列」）。

### 並べる・件数を決める

| 書き方                                           | 意味                       |
|--------------------------------------------------|----------------------------|
| `order_by(col)` / `order_by_desc(col)`           | 並べる                     |
| `order_by_raw("sql")`                            | 並べ方を SQL で書く        |
| `latest()` / `oldest()`                          | `created_at` の降順 / 昇順 |
| `latest_by(col)` / `oldest_by(col)`              | 列を指定して並べる         |
| `limit(n)` / `offset(n)` / `take(n)` / `skip(n)` | 件数                       |
| `for_page(page, per_page)`                       | ページから件数を決める     |

**並びは一意にします。** 同じ値の行があると、ページの境目で同じ行が 2 回出ます。
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
    .get()
    .await?;
```

`group_by` の無い `having` はエラーです。

### 数える

```rust
let total = DB::table("posts").count().await?;                          // i64
let any = DB::table("posts").where_("status", "draft").exists().await?; // bool
let none = DB::table("posts").doesnt_exist().await?;
let sum = DB::table("posts").sum::<i64>("views").await?;                // Option<i64>
// avg / min / max も同じ形
```

- `group_by` を付けた `count()` は、グループの数を返します。
- 行が無いとき、`sum` / `avg` / `min` / `max` は `None` です。
- `sum` などは `group_by` と一緒に使えません。

## 書き込む

```rust
DB::table("posts").insert(&[("title", "はじめての記事".into())]).await?;
let id = DB::table("posts").insert_get_id(&[("title", "次の記事".into())]).await?;

let rows = vec![vec![("title", "A".into())], vec![("title", "B".into())]];
DB::table("posts").insert_many(&rows).await?;

// 返るのは件数
let changed = DB::table("posts").where_("id", id).update(&[("title", "改題".into())]).await?;
let removed = DB::table("posts").where_("id", id).delete().await?;

DB::table("posts").truncate().await?;   // 全部消す。採番も 1 に戻る
```

- 値は `.into()` で渡します。`Option<T>` の `None` は NULL です。
- `insert_many` の行は、列の名前と並びを最初の行とそろえます。
- 主キーが `id` でない表の採番は、`insert_get_id_as(値, 主キーの名前)` で受け取ります。
- 自動採番の無い表には `insert()` を使います。`insert_get_id` は MySQL でエラーになりますが、**行は入っています。**
- **条件の無い `update` / `delete` は全行が対象です。**
- `update` は `updated_at` を入れません（モデルの `update` は入れます）。
- `i64` に収まらない `u64` は、文字列として書き込みます。
- `update` / `delete` に `join` や `limit` は付けられません。`order_by` は無視します。
- 絞って消すなら `delete()` です。`where_` を付けた `truncate()` はエラーです。

`join` や `limit` で絞りたいときは、先に ID を取り出します。

```rust
let ids: Vec<i64> = DB::table("posts").latest().limit(10).pluck::<i64>("id").await?;
DB::table("posts").where_in("id", &ids).update(&[("status", "archived".into())]).await?;
```

### 数を増やす・減らす

```rust
DB::table("posts").where_("id", id).increment("views", 1).await?;
DB::table("posts").where_("id", id).decrement("stock", 1).await?;
```

**数を増やすのに `update` を使わないでください。** 同時に 2 本来ると、片方の分が消えます。
`increment` は 1 つの文で流すので消えません。

### 日時

日時は `YYYY-MM-DD HH:MM:SS`（UTC）の文字列で入れます。
`now()` がこの形の文字列を返します。`Carbon`（日時の型。[models.md](models.md)）もそのまま渡せます。

```rust
DB::table("posts").insert(&[("title", "x".into()), ("created_at", now().into())]).await?;
```

モデルの `save()` なら自動で入ります。

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

- エラーの文はドライバごとに違います。文を読まずに、これで見分けます。
- 見分けられるのは、一意制約と主キーの重なりだけです。
- トランザクションの中では使えません。1 本失敗すると、そのトランザクションは使えなくなるためです。

## ページ分け

```rust
let page = DB::table("posts").latest().paginate(15, req.page()).await?;
json(&page)   // data / total / per_page / current_page が出る
```

`req.page()` は `?page=` を読みます（無ければ 1）。

| 名前                                           | 意味                              |
|------------------------------------------------|-----------------------------------|
| `data` / `total` / `per_page` / `current_page` | 中身と件数                        |
| `last_page()`                                  | 最後のページ番号（0 件でも 1）    |
| `has_more_pages()`                             | 次があるか                        |
| `from()` / `to()`                              | 何件目から何件目か。空なら `None` |
| `len()` / `is_empty()`                         | このページの件数 / 空か           |

- 件数と中身で 2 回問い合わせます。
- `per_page` と `page` の 0 は 1 として扱います。

### `distinct()` を付けて数えるとき

`paginate` と `count()` は、`select` で選んだ列で数え方が決まります。

| 選んだ列                                               | 数えるもの                                     |
|--------------------------------------------------------|------------------------------------------------|
| 列が 1 つ（`select(&["author_id"])`）                  | 重なりを除いた数。`count()` は NULL を数えない |
| `posts.*` が 1 つだけ                                  | 重なりを除いた行の数                           |
| 出力の名前が重ならない列（`posts.id` と `users.name`） | 同上                                           |
| 指定なし・`*`（`join` が無いとき）                     | 同上                                           |
| それ以外                                               | エラー                                         |

「それ以外」は、`join` した `*`、名前が重なる列（`posts.id` と `comments.id`）、式（`id as key`）、`posts.*` とほかの列の組み合わせです。
名前の重なりは大文字小文字を区別せずに見ます。

## トランザクション

トランザクションは、いくつかの書き込みを「全部通す」か「全部取り消す」かにまとめる仕組みです。

```rust
let tx = DB::begin().await?;
tx.table("posts").insert(&[("title", "x".into())]).await?;
tx.table("posts").where_("id", 1).update(&[("views", 1.into())]).await?;
tx.commit().await?;
```

- 中は `tx.table(...)`、外は `DB::table(...)` です。混ぜないでください。
- `commit()` を呼ばずに `tx` を捨てると、取り消します。明示するなら `tx.rollback().await?` です。
- **1 本ずつ `await` します。** 同時に 2 本投げると、2 本目がエラーです。
- **1 本でも失敗したら、そのトランザクションはもう使えません。** `DB::begin()` からやり直します。
- SQLite では、ほかのトランザクションの途中なら、始めるところで 5 秒まで待ちます。
- モデルを中で使うときは `save_using(&tx)` などを使います（[models.md](models.md)）。

## SQL を直接書く

```rust
let rows = DB::select("select * from posts where views > ?", &[100.into()]).await?;
let affected = DB::statement("update posts set views = views + 1", &[]).await?;
```

| 書き方                      | 返るもの                                                           |
|-----------------------------|--------------------------------------------------------------------|
| `DB::select(sql, &[値])`    | `Vec<Row>`                                                         |
| `DB::statement(sql, &[値])` | `Affected`（`rows` は変わった行の数、`last_insert_id` は採番の値） |

- 値の置き場所は、どのドライバでも `?` です。数が合わないとエラーです。
- トランザクションの中では `tx.select(...)` / `tx.statement(...)` です。
- PostgreSQL で `?` を 1 つも書かないときは、`$1` を直に書けます。
- 組み立てた SQL は `.to_sql()` で見られます（`DB::table("posts").where_("id", 1).to_sql()`）。接続はしません。

## 接続を選ぶ

```rust
let rows = DB::connection("logs").table("events").get().await?;
let tx = DB::connection("logs").begin().await?;
```

- `DB::connection("名前")` からは `table` / `select` / `statement` / `begin` が使えます。
- 既定の接続の種類は `DB::driver().await?`、表の名前の一覧は `DB::table_names().await?` で取れます。

### `config/database.rs`

接続を足すときは、ここに書きます。
無くても `.env` から `sqlite` / `mysql` / `mariadb` / `pgsql` の 4 本が作られます。

```rust
pub fn config() -> DatabaseConfig {
    let database: String = env("DB_DATABASE", "database/database.sqlite");

    DatabaseConfig {
        default: env("DB_CONNECTION", "sqlite"),
        connections: vec![
            ConnectionConfig::sqlite("sqlite", database).max_connections(5),
            // 2 本目。DB::connection("logs") で指す
            ConnectionConfig::sqlite("logs", "database/logs.sqlite"),
            // postgres も同じ形（ConnectionConfig::postgres）
            ConnectionConfig::mysql("mysql", env::<String>("DB_DATABASE", "myapp"))
                .host(env::<String>("DB_HOST", "127.0.0.1"))
                .username(env::<String>("DB_USERNAME", "root"))
                .password(env::<String>("DB_PASSWORD", "")),
        ],
    }
}
```

| 指定                 | 既定                | 意味                                       |
|----------------------|---------------------|--------------------------------------------|
| `host(s)`            | `127.0.0.1`         | つなぎ先。空の文字列は無視                 |
| `port(n)`            | 3306 / 5432         | ポート。0 は無視                           |
| `username(s)`        | `root` / `postgres` | 利用者名。空の文字列は無視                 |
| `password(s)`        | 空                  | パスワード。空なら渡さない                 |
| `max_connections(n)` | 5                   | 同時に張る接続の数。メモリ上の SQLite は 1 |
| `foreign_keys(b)`    | 真                  | SQLite で外部キーを効かせるか              |
| `url(s)`             | 空                  | 接続文字列。書くと、ほかの値より優先       |

- 接続文字列は `mysql://利用者名:パスワード@127.0.0.1:3306/データベース名` の形です（`sqlite://` / `postgres://` も同じ）。
- パスワードの `@` `#` `/` `?` は、`%40` のような形に直します。

### つなぎ方の決まり

- 接続は、最初に使ったときに張ります。
- SQLite の相対パスは、プロジェクト直下から見ます。
- 名前か `url` に `:memory:` を含むと、メモリ上のデータベースになります。
- **メモリ上の SQLite は接続が 1 本です。** トランザクションの途中で `DB::table(...)` や `save()` を呼ぶと、5
  秒待ってエラーになります。中では `tx` を使います。
- MySQL と PostgreSQL は、つながらないと 30 秒待ってからエラーになります。
- エラー文には接続の名前とつなぎ先が出ます。パスワードは出しません。

## テスト

テストは `DB_TEST_DATABASE` のデータベースを、表を全部消して作り直します（[testing.md](testing.md)）。

- **MySQL と PostgreSQL では、`DB_TEST_DATABASE` に別のデータベース名を必ず書きます。** 書かないとテストが止まります。
- テストの間は、どの接続名を書いてもテスト用の 1 本につながります。
- SQLite だけで開発すると、PostgreSQL だけの決まりに気づけません。本番と同じドライバでもテストを流します。

## ドライバの違い

引用符・値の置き場所・列の型名は bengara が書き分けます。
それでも残る違いは次のとおりです。

| こと                                | SQLite               | MySQL                       | PostgreSQL                                     |
|-------------------------------------|----------------------|-----------------------------|------------------------------------------------|
| `decimal` の列に文字列を書く        | 通る                 | 通る                        | **断られる**。`f64` で読んで書く               |
| 整数の列を文字列と比べる            | 通る                 | 通る                        | **SQL エラー**。整数で渡す                     |
| 型の決まらない所の `None`           | 通る                 | 通る                        | エラー。`cast(? as text)` と書く               |
| `having_op` に別名（`total`）を渡す | 通る                 | 通る                        | エラー。`having_raw` に式を書く                |
| トランザクションの中の `truncate()` | できる               | **エラー**。`delete()` で   | できる                                         |
| 外部キーで指される表の `truncate()` | 指す側が空ならできる | エラー                      | エラー                                         |
| `ilike` / `not ilike`               | エラー               | エラー                      | 使える                                         |
| `<=>`                               | エラー               | 使える                      | エラー                                         |
| 1 つの文に渡せる値の数              | 32,766               | 65,535                      | 65,535                                         |
| `Affected` の `last_insert_id`      | `insert` で入る      | `insert` で入る             | 常に `None`。`returning` を付けて `DB::select` |
| 読めない型                          | —                    | —                           | `uuid` / `money` / `inet` / 配列 / `interval`  |
| 生の `Value` の真偽                 | `Int`                | `Bool`                      | `Bool`                                         |
| 生の `Value` の `decimal`           | `Float`              | `Text`（末尾の 0 を落とす） | `Text`（末尾の 0 を落とす）                    |

- 使えないドライバの演算子は、流す前にエラーにします。`ilike` の代わりは `like` です（SQLite は英字の大文字小文字を区別しない。MySQL
  は照合順序による）。
- 値の数の上限を超えると、投げる前にエラーにします。`insert_many` は「行 × 列」で数えます。
- `truncate()` できない表は、`delete()` で空にします（採番は戻りません）。
- 読めない型は、`cast(列 as text) as 列` で文字列にすれば読めます。
- 生の値の違いは、`row.value()` で `Value` を直接見るときだけ気にします。`row.get::<T>()` ならどのドライバでも同じです。
- 日付・時刻・日時は、どのドライバでも `YYYY-MM-DD` / `HH:MM:SS` / `YYYY-MM-DD HH:MM:SS` の文字列で返ります。PostgreSQL の
  `timestamptz` は UTC に直します。

## 安全のための決まり

表名と列名は検査します。外から来た文字列が SQL に混ざらないようにするためです。

- 使えるのは英数字（日本語も可）と `_` です。`users.id` のように `.` で表名を付けられます。
- 間違いは、`get()` や `insert()` が `Err` で返します。
- 演算子は決まったものだけ渡せます（下の表）。大文字と小文字は区別しません。

| 使う所                                   | 演算子                                                                              |
|------------------------------------------|-------------------------------------------------------------------------------------|
| `where_op` / `or_where_op` / `having_op` | `=` `!=` `<>` `<` `<=` `>` `>=` `like` `not like` `ilike` `not ilike` `is` `is not` |
| `where_column` / `join` / `left_join`    | `=` `!=` `<>` `<` `<=` `>` `>=` `<=>` `like` `not like` `ilike` `not ilike`         |

**次のものは、文字列をそのまま SQL に置きます。外から来た文字列を渡さないでください。**
`select` / `add_select` / `where_raw` / `or_where_raw` / `having_raw` / `order_by_raw` です。

## ログ

投げた SQL を見るときは、`RUST_LOG=info,sqlx=debug cargo artisan serve` で起動します。

- 渡した値は、bengara からはログに出しません。
- データベースが返したエラー文には、値が入っていることがあります（MySQL の `Duplicate entry '値'` など）。
- 利用者に見せる文は、`is_unique_violation` で見分けて自分で書きます。

## 関連

- [migrations.md](migrations.md) — 表を作る
- [models.md](models.md) — 構造体として読み書きする
- [testing.md](testing.md) — DB を使うテスト
- [configuration.md](configuration.md) — `config/*.rs` と `.env`
