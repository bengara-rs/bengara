# マイグレーション

表を作る手順を、日付入りのファイルとして残します。
`cargo artisan migrate` で、まだ流していないものだけが順に実行されます。

## 書く

`database/migrations/` にファイルを置きます。名前は Laravel と同じ
`日付_連番_説明.rs` です。 **名前順に実行されます。**

```rust
// database/migrations/2026_10_05_000000_create_posts_table.rs
use bengara::prelude::*;

pub fn up(schema: &mut Schema) {
    schema.create("posts", |t| {
        t.id();
        t.string("title");
        t.text("body").nullable();
        t.string("status").default("draft");
        t.integer("views").default(0);
        t.timestamps();
        t.index(&["status"]);
    });
}

pub fn down(schema: &mut Schema) {
    schema.drop_if_exists("posts");
}
```

- `pub fn up` と `pub fn down` の **2つが必要**です。無いとコンパイルエラーになります。
- **`up` / `down` は同期の関数です。** ここでやるのは SQL の組み立てだけで、
  実際に投げるのは bengara 側（ランナー）です。
- 登録の作業はありません。ファイルを置けば `build.rs` が拾います。

## 流す

```sh
cargo artisan migrate          # まだ流していないものを順に流す
cargo artisan migrate --seed   # 流してからシーダーも動かす
```

本番は実行ファイル1つで動きます。

```sh
./myapp migrate
```

| コマンド           | すること                                         |
|--------------------|--------------------------------------------------|
| `migrate`          | まだ流していないものを流す                       |
| `migrate:status`   | 流したかどうかを一覧にする                       |
| `migrate:rollback` | 最後のバッチを巻き戻す（`--step=2` で2バッチ分） |
| `migrate:reset`    | 全部巻き戻す                                     |
| `migrate:refresh`  | 全部巻き戻してから流し直す                       |
| `migrate:fresh`    | **表を全部消して**から流し直す                   |
| `db:seed`          | シーダーだけ動かす                               |
| `db:wipe`          | 表を全部消す                                     |

- どれにも `--database=接続の名前` を付けられます。既定は `DB_CONNECTION` です。
- `migrate:fresh` と `db:wipe` は、`APP_ENV=production` のとき `--force` が無いと止まります。
- 流した記録は `migrations` 表に残ります（名前とバッチ番号）。
  同じものが2回流れることはありません。
- **1本ごとにトランザクションで実行します。** 途中で失敗したらその1本を巻き戻して止まります。

```
$ cargo artisan migrate:status
名前                                       状態      バッチ
2026_10_05_000000_create_posts_table     実行済み  1
2026_10_05_000100_create_comments_table  実行済み  1

2 本中 2 本が実行済みです
```

## 列の種類

| 書き方                                                        | SQLite の型                                      |
|---------------------------------------------------------------|--------------------------------------------------|
| `t.id()`                                                      | 自動採番の主キー                                 |
| `t.increments("uid")`                                         | 名前を決めた自動採番の主キー                     |
| `t.integer("views")` / `t.big_integer("n")`                   | `integer`                                        |
| `t.foreign_id("post_id")`                                     | `integer`（ほかの表を指す列）                    |
| `t.string("title")` / `t.string_with("code", 8)`              | `varchar(255)` / `varchar(8)`                    |
| `t.text("body")`                                              | `text`                                           |
| `t.boolean("pinned")`                                         | `boolean`                                        |
| `t.float("x")` / `t.double("y")` / `t.decimal("price", 8, 2)` | `real` / `real` / `numeric(8, 2)`                |
| `t.date("on")` / `t.date_time("at")` / `t.timestamp("at")`    | `date` / `datetime`                              |
| `t.json("meta")`                                              | `text`                                           |
| `t.binary("blob")`                                            | `blob`                                           |
| `t.uuid("key")`                                               | `varchar(36)`                                    |
| `t.raw_column("x", "integer")`                                | 型名をそのまま書く                               |
| `t.timestamps()`                                              | `created_at` と `updated_at`（どちらも null 可） |
| `t.soft_deletes()`                                            | `deleted_at`（null 可）                          |

## 列に付ける指定

続けて書けます。

```rust
t.string("email").unique();
t.text("body").nullable();
t.integer("views").default (0);
t.date_time("at").default_raw("current_timestamp");
t.uuid("key").primary();
```

| 書き方               | 意味                            |
|----------------------|---------------------------------|
| `nullable()`         | null を許す                     |
| `default(値)`        | 既定値                          |
| `default_raw("sql")` | 既定値を SQL で書く             |
| `unique()`           | 重なりを禁じる                  |
| `index()`            | 索引を付ける                    |
| `primary()`          | 主キーにする                    |
| `comment("説明")`    | 説明（SQLite では無視されます） |

**`t.id()` と `t.increments(..)` には、これらの指定が効きません。**
自動採番の主キーは型名ひとつで全部を書くので、置く場所がありません。
`nullable()` / `default()` / `unique()` を付けると、無視したことを警告で知らせます。

## 表に付ける指定

```rust
schema.create("comments", | t| {
t.id();
t.foreign_id("post_id");
t.string("author");
t.timestamps();

t.index(& ["post_id"]);
t.unique(& ["post_id", "author"]);
t.foreign("post_id").on("posts").cascade_on_delete();
});
```

| 書き方                                           | 意味                       |
|--------------------------------------------------|----------------------------|
| `t.index(&["a", "b"])`                           | 索引                       |
| `t.unique(&["email"])`                           | 重なりを禁じる             |
| `t.primary(&["a", "b"])`                         | 複合主キー                 |
| `t.foreign(col).references(col).on(table)`       | 外部キー                   |
| `.on_delete("cascade")` / `.cascade_on_delete()` | 元の行が消えたときの動き   |
| `.on_update("restrict")`                         | 元の行が変わったときの動き |

索引は `create index ...` という別の文になります。名前は自動で付きます
（`posts_status_index` のような形）。

## 表を変える

```rust
pub fn up(schema: &mut Schema) {
    schema.table("posts", |t| {
        t.string("slug").nullable();   // 列を足す
        t.index(&["slug"]);            // 索引を足す
    });
}

pub fn down(schema: &mut Schema) {
    schema.drop_column("posts", "slug");
}
```

**できるのは「列を足す」「索引を足す」までです。**

- 列の型を変える `change()` はありません。SQLite が苦手なためです。
  型を変えたいときは、新しい表を作って移し替えてください。
- **`t.id()` と `t.increments(..)` は使えません。**
  `alter table add column` で自動採番の主キーは足せません。
  書くと警告が出て、その文は流れません。

## そのほか

```rust
schema.create_if_not_exists("posts", | t| { /* ... */ });
schema.drop("posts");
schema.drop_if_exists("posts");
schema.rename("posts", "articles");
schema.drop_column("posts", "slug");
schema.raw("pragma foreign_keys = on");   // SQL をそのまま
```

`create_if_not_exists` は、 **索引の文にも `if not exists` を付けます。**
表だけを飛ばして索引で失敗する、ということはありません。

```
create table if not exists "users" (...)
create index if not exists "users_name_index" on "users" ("name")
```

組み立てた SQL を確かめたいときは、`up` の中で `schema.to_sql()` を見ます。

```rust
pub fn up(schema: &mut Schema) {
    schema.create("posts", |t| {
        t.id();
        t.string("title");
    });
    // その時点までに組み立てた SQL の一覧
    for sql in schema.to_sql() {
        println!("{sql}");
    }
}
```

`schema.driver()` でつながる先の種類も分かります。

## 関連

- [database.md](database.md) — 接続とクエリ
- [models.md](models.md) — 構造体として読み書きする
- [directory-structure.md](directory-structure.md) — ファイルの自動検出
