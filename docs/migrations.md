# マイグレーション

マイグレーションは、表を作る・変える手順をファイルに書いたものです。
`cargo artisan migrate` で順に流します。

## 書く

`database/migrations/` にファイルを置きます。
名前は Laravel と同じ `日付_連番_説明.rs` です。**名前順に流れます。**

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

- `pub fn up` と `pub fn down` の **2 つが必要**です。無いとコンパイルエラーです。
- `up` / `down` は同期の関数です。ここでは SQL を組み立てるだけで、流すのはコマンドです。
- 登録は要りません。ファイルを置けば `build.rs` が拾います。
- **表名と列名は検査しません。** 外から来た文字列を渡さないでください。

## 流す

```sh
cargo artisan migrate          # まだ流していないものを流す
cargo artisan migrate --seed   # 流してからシーダーも動かす
./myapp migrate                # 本番は実行ファイル 1 つで
```

| コマンド           | すること                               |
|--------------------|----------------------------------------|
| `migrate`          | まだ流していないものを流す             |
| `migrate:status`   | 流したかどうかを一覧にする             |
| `migrate:rollback` | 最後のバッチを巻き戻す                 |
| `migrate:reset`    | 全部巻き戻す                           |
| `migrate:refresh`  | 全部巻き戻してから流し直す             |
| `migrate:fresh`    | **表を全部消して**から流し直す         |
| `db:seed`          | シーダーだけ動かす                     |
| `db:wipe`          | 表を全部消す                           |
| `migrate:unlock`   | 途中で落ちて残った札（下で説明）を外す |

バッチは、1 回の `migrate` で流したひとまとまりです。

### 付けられる指定

| 指定              | 意味                                     | 使えるコマンド                                  |
|-------------------|------------------------------------------|-------------------------------------------------|
| `--database=名前` | 使う接続を選ぶ（既定は `DB_CONNECTION`） | すべて                                          |
| `--seed`          | 流したあとにシーダーも動かす             | `migrate` / `migrate:refresh` / `migrate:fresh` |
| `--class=名前`    | 動かすシーダーを 1 本に絞る              | `--seed` が使えるものと `db:seed`               |
| `--step=数`       | 巻き戻すバッチの数（既定 1。0 も 1）     | `migrate:rollback`                              |
| `--force`         | 本番で、中身が消えるコマンドを通す       | `migrate:status` と `migrate:unlock` 以外       |

- 使えない指定や知らない指定を付けると、何もせずにエラーになります。
- `--class` は、`db:seed` 以外では `--seed` と一緒に使います。
- **シーダーは既定の接続にだけ流せます。** 別の接続に流すときは、`DB_CONNECTION` をその名前にして実行します。
- `migrate` と `db:seed` の `--force` は、付けても動きは同じです（本番でも止まらないため）。

### 本番で止まるコマンド

`APP_ENV=production` のとき、次の 5 つは `--force` が無いと止まります。

- `migrate:rollback`
- `migrate:reset`
- `migrate:refresh`
- `migrate:fresh`
- `db:wipe`

`rollback` / `reset` / `refresh` も `down` が流れるので、中身が消えます。

### 流し方の決まり

- 流した記録は `migrations` 表に残ります。同じものは 2 回流れません。
- **1 本ごとにトランザクションで流します。** 失敗したら、その 1 本を巻き戻して止まります。
- **MySQL は例外です。** 表を変える文が勝手に確定するので、失敗した文より前の文は残ります。
- `migrate:rollback` は、戻すファイルが全部いまのバイナリにあるかを先に確かめます。無ければ何も戻しません。
- 組み立てに誤りがあると、SQL を 1 本も流さずに止まります（下の「エラーと警告」）。
- `migrate:fresh` と `db:wipe` は全部の表を消します。`migration_locks` 表だけは残します。

### 二重に流さない仕組み（札）

表を変えるコマンドは、流す前に `migration_locks` 表に「札」を 1 枚取ります。
同じコマンドが 2 つ同時に走るのを防ぐためです。

- `migrate:status` と `migrate:unlock` 以外の 7 つが札を取ります。
- 誰かが札を持っていると、何もせずにエラーになります。
- 札は、終わったら返します。失敗したときも返します。

```
別のプロセスがマイグレーションを実行中です（myhost:1234 が 2026-10-30 10:00:00 に開始）。
終わってからもう一度実行してください。
途中で落ちて札が残っているときは、`migrate:unlock` で消せます。
```

プロセスが強制終了されると、札が残ります。`cargo artisan migrate:unlock` で外してください。
持ち主（ホスト名とプロセス番号）と時刻が出るので、本当に止まっているかを確かめられます。

### 状態を見る

```
$ cargo artisan migrate:status
名前                                     状態      バッチ
2026_10_05_000000_create_posts_table     実行済み  1
2026_10_05_000100_create_comments_table  未実行    -

2 本中 1 本が実行済みです
```

- `ファイルなし` は、記録にはあるのに、いまのバイナリにファイルが無いものです。
- `migrate:status` と `migrate:unlock` は読むだけです。表を作りません。

## 列の種類

| 書き方                                    | SQLite                                        | MySQL                         | PostgreSQL         |
|-------------------------------------------|-----------------------------------------------|-------------------------------|--------------------|
| `t.id()`                                  | `integer`（自動採番）                         | `bigint unsigned`（自動採番） | `bigserial`        |
| `t.increments("uid")`                     | 同上（名前を決める）                          | 同上                          | 同上               |
| `t.integer("views")`                      | `integer`                                     | `integer`                     | `integer`          |
| `t.big_integer("n")`                      | `integer`                                     | `bigint`                      | `bigint`           |
| `t.foreign_id("post_id")`                 | `integer`                                     | `bigint unsigned`             | `bigint`           |
| `t.string("title")`                       | `varchar(255)`                                | `varchar(255)`                | `varchar(255)`     |
| `t.string_with("code", 8)`                | `varchar(8)`                                  | `varchar(8)`                  | `varchar(8)`       |
| `t.text("body")`                          | `text`                                        | `text`                        | `text`             |
| `t.boolean("pinned")`                     | `boolean`                                     | `tinyint(1)`                  | `boolean`          |
| `t.float("x")`                            | `real`                                        | `float(53)`                   | `double precision` |
| `t.double("y")`                           | `real`                                        | `double precision`            | `double precision` |
| `t.decimal("price", 8, 2)`                | `numeric(8, 2)`                               | `numeric(8, 2)`               | `numeric(8, 2)`    |
| `t.date("on")`                            | `date`                                        | `date`                        | **`text`**         |
| `t.date_time("at")` / `t.timestamp("at")` | `datetime`                                    | `datetime`                    | **`text`**         |
| `t.json("meta")`                          | `text`                                        | `json`                        | **`text`**         |
| `t.binary("data")`                        | `blob`                                        | `blob`                        | `bytea`            |
| `t.uuid("key")`                           | `varchar(36)`                                 | `varchar(36)`                 | `varchar(36)`      |
| `t.raw_column("x", "型名")`               | 型名のまま                                    | 型名のまま                    | 型名のまま         |
| `t.timestamps()`                          | `created_at` と `updated_at`（日時、NULL 可） | 同左                          | 同左               |
| `t.soft_deletes()`                        | `deleted_at`（日時、NULL 可）                 | 同左                          | 同左               |

- `t.foreign_id(列)` は `t.id()` と型がそろうので、そのまま外部キーを張れます。
- `t.float` と `t.double` は、どのドライバでも 8 バイトです（Laravel と違います）。4 バイトなら `t.raw_column(列, "float")`
  です。
- **PostgreSQL では、日付・日時・JSON が `text` 列になります。** bengara は日時を文字列で扱い、PostgreSQL
  は日時の列に文字列を渡すと断るためです。

## 列に付ける指定

```rust
t.string("email").unique();
t.text("body").nullable();
t.integer("views").default(0);
t.date_time("at").default_raw("current_timestamp");
t.uuid("key").primary();
```

| 書き方               | 意味                                                |
|----------------------|-----------------------------------------------------|
| `nullable()`         | NULL を許す（付けなければ not null）                |
| `default(値)`        | 既定値                                              |
| `default_raw("sql")` | 既定値を SQL で書く                                 |
| `unique()`           | 重なりを禁じる（一意索引を作る）                    |
| `index()`            | 索引を付ける                                        |
| `primary()`          | 主キーにする（表を作るときだけ）                    |
| `comment("説明")`    | **何もしません。** Laravel と同じ形で書けるだけです |

- `t.id()` と `t.increments(..)` には `nullable` / `default` / `unique` が効きません。警告が出ます。
- 列の `unique()` と表の `t.unique(&["列"])` は同じものです。両方書いても索引は 1 つです。

## 表に付ける指定

```rust
schema.create("comments", |t| {
    t.id();
    t.foreign_id("post_id");
    t.string("author");
    t.timestamps();

    t.index(&["post_id"]);
    t.unique(&["post_id", "author"]);
    t.foreign("post_id").on("posts").cascade_on_delete();
});
```

| 書き方                                           | 意味                         |
|--------------------------------------------------|------------------------------|
| `t.index(&["a", "b"])`                           | 索引                         |
| `t.unique(&["email"])`                           | 重なりを禁じる（一意索引）   |
| `t.primary(&["a", "b"])`                         | 複合主キー                   |
| `t.foreign(列).on(表)`                           | 外部キー。指す先の列は `id`  |
| `.references(列)`                                | 指す先の列を `id` 以外にする |
| `.on_delete("cascade")` / `.cascade_on_delete()` | 元の行が消えたときの動き     |
| `.on_update("restrict")`                         | 元の行が変わったときの動き   |

- `on_delete` / `on_update` に渡せるのは `cascade` / `set null` / `set default` / `restrict` / `no action` です。
- ほかの値もそのまま SQL に出ますが、打ち間違いに気づけるよう警告が出ます。

### 索引の名前

名前は自動で付きます。形は `{表}_{列を _ でつないだもの}_{index|unique|foreign}` です。
例: `posts_status_index`・`users_email_unique`・`comments_post_id_foreign`。

- 同じ名前が 2 つできるときや、63 バイトを超えるときは、末尾に 8 桁の 16 進を足します。
- 表の名前の `.` と `-` は `_` にします（`main.posts` → `main_posts_status_index`）。
- 外部キーに名前が付くのは、後から足すときだけです。表を作るときはデータベースが決めます。
- `unique()` が作るのは一意「索引」です。PostgreSQL の `on conflict on constraint 名前` には使えません（`on conflict (列)`
  は使えます）。

## 表を変える

```rust
pub fn up(schema: &mut Schema) {
    schema.table("posts", |t| {
        t.foreign_id("user_id").default(0);   // 列を足す
        t.index(&["user_id"]);                // 索引を足す
    });
}

pub fn down(schema: &mut Schema) {
    schema.drop_index("posts", &["user_id"]);   // 索引 → 列の順
    schema.drop_column("posts", "user_id");
}
```

`schema.table(..)` でできるのは、列・索引・外部キーを**足す**ことだけです。

- **足す列には `.nullable()` か `.default(..)` を付けてください。** 付けないと SQLite では必ず、PostgreSQL
  では行があると失敗します。MySQL だけは通るので、気づきにくいです。
- 主キーは後から足せません。
- SQLite では、外部キーを後から足せません。表を作り直してください。
- 列の型は変えられません（Laravel の `change()` はありません）。新しい表を作って移し替えます。

## そのほかの操作

| 書き方                                                   | すること                        |
|----------------------------------------------------------|---------------------------------|
| `schema.create_if_not_exists("posts", \|t\| { .. })`     | 無ければ表を作る（索引も同じ）  |
| `schema.drop("posts")`                                   | 表を消す                        |
| `schema.drop_if_exists("posts")`                         | あれば表を消す                  |
| `schema.rename("posts", "articles")`                     | 表の名前を変える                |
| `schema.drop_column("posts", "slug")`                    | 列を消す                        |
| `schema.drop_index("posts", &["user_id"])`               | `index()` で作った索引を落とす  |
| `schema.drop_unique("users", &["email"])`                | `unique()` で作った索引を落とす |
| `schema.drop_index_named("users", "users_email_unique")` | 名前を指定して索引を落とす      |
| `schema.raw("SQL")`                                      | SQL をそのまま流す              |
| `schema.to_sql()`                                        | 組み立てた SQL の一覧を見る     |
| `schema.driver()`                                        | つなぐ先の種類を見る            |

### 索引を落とす

**`up` で `index()` / `unique()` を付けたら、`down` で落としてください。**
列を落としても索引は残ります。落とす順は「索引 → 列」です（SQLite は索引の付いた列を消せません）。

```rust
pub fn up(schema: &mut Schema) {
    schema.table("users", |t| {
        t.string("handle").nullable().unique();
    });
}

pub fn down(schema: &mut Schema) {
    schema.drop_unique("users", &["handle"]);
    schema.drop_column("users", "handle");
}
```

`drop_index` / `drop_unique` は、作ったときと同じ規則で名前を組み立てます。
名前が合わないときは、`drop_index_named` か `raw` を使います。

- 名前の末尾に 16 進が付いたもの: `drop_index_named` で名前を直に書きます。
- 列の定義に SQL で `unique` と書いて作った制約: MySQL は列の名前（`drop_index_named("users", "email")`）、PostgreSQL は
  `{表}_{列}_key` を `raw` で `drop constraint` します。SQLite は落とせないので、表を作り直します。
- 無い索引を落とすと、SQLite と PostgreSQL は何もしません。MySQL はエラーです。

## エラーと警告

| 書き方                                                                      | どうなるか                                                     |
|-----------------------------------------------------------------------------|----------------------------------------------------------------|
| `pub fn up` / `pub fn down` が無い                                          | コンパイルエラー                                               |
| `t.foreign(列)` に `.on(表名)` が無い                                       | **流す前にエラー**（SQL を 1 本も流さない）                    |
| `schema.table(..)` の中で `t.id()` / `t.increments(..)` / 列の `.primary()` | **流す前にエラー**                                             |
| `schema.create(..)` で主キーを 2 つ指定する                                 | **流す前にエラー**。複合主キーは `t.primary(&[..])` だけで書く |
| `schema.table(..)` の中の `t.primary(&[..])`                                | 警告。何も足さない                                             |
| SQLite で `schema.table(..)` の中の `t.foreign(..)`                         | 警告。何も足さない                                             |
| `schema.table(..)` で NULL 不可かつ既定値なしの列                           | 警告。SQLite と PostgreSQL で失敗する                          |
| `t.id()` に `nullable()` / `default()` / `unique()`                         | 警告。無視する                                                 |
| `t.primary(&[..])` を 2 回                                                  | 警告。後の指定で置き換わる                                     |
| `on_delete` / `on_update` に一覧外の値                                      | 警告。SQL には出る                                             |
| `default(f64::NAN)` など `NaN` や無限                                       | 警告。その文は失敗する                                         |
| `default(Vec<u8>)`（バイト列の既定値）                                      | 警告。値は無視して `null` を書く                               |

まだ無い機能は [backlog.md](backlog.md) にまとめています。

## 関連

- [database.md](database.md) — 接続とクエリ
- [models.md](models.md) — 構造体として読み書きする（シーダーの書き方も）
- [directory-structure.md](directory-structure.md) — ファイルの自動検出
