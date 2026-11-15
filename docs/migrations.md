# マイグレーション

このページで分かること。

- 表を作る・変える手順（マイグレーション）の書き方
- `cargo artisan migrate` などのコマンドと、付けられる指定
- 列の種類と、ドライバごとの型名

## 書く

`database/migrations/` にファイルを置きます。
名前は Laravel と同じ `日付_連番_説明.rs` です。**名前順に実行されます。**

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

- `pub fn up` と `pub fn down` の **2 つが必要**です。無いとコンパイルエラーになります。
- **`up` / `down` は同期の関数です。** ここでは SQL を組み立てるだけです。
  実際に流すのはコマンドの側です。
- 登録の作業はありません。ファイルを置けば `build.rs` が拾います。
- **表名と列名は検査しません。** 外から来た文字列（リクエストや設定の値）を渡さないでください。

## 流す

```sh
cargo artisan migrate          # まだ流していないものを順に流す
cargo artisan migrate --seed   # 流してからシーダーも動かす
```

本番は実行ファイル 1 つで動きます。

```sh
./myapp migrate
```

| コマンド           | すること                                           |
|--------------------|----------------------------------------------------|
| `migrate`          | まだ流していないものを流す                         |
| `migrate:status`   | 流したかどうかを一覧にする                         |
| `migrate:rollback` | 最後のバッチを巻き戻す（`--step=2` で 2 バッチ分） |
| `migrate:reset`    | 全部巻き戻す                                       |
| `migrate:refresh`  | 全部巻き戻してから流し直す                         |
| `migrate:fresh`    | **表を全部消して**から流し直す                     |
| `db:seed`          | シーダーだけ動かす                                 |
| `db:wipe`          | 表を全部消す                                       |
| `migrate:unlock`   | 途中で落ちて残った札（実行中の目印）を外す         |

バッチは、1 回の `migrate` で流したひとまとまりです。

### 使える指定

コマンドごとに使える指定は決まっています。
**使えない指定や知らない指定を付けるとエラーになります**（何も実行しません）。

| コマンド           | `--database=` | `--seed` | `--class=` | `--step=` | `--force` |
|--------------------|---------------|----------|------------|-----------|-----------|
| `migrate`          | ○             | ○        | ○          | ×         | ○         |
| `migrate:status`   | ○             | ×        | ×          | ×         | ×         |
| `migrate:rollback` | ○             | ×        | ×          | ○         | ○         |
| `migrate:reset`    | ○             | ×        | ×          | ×         | ○         |
| `migrate:refresh`  | ○             | ○        | ○          | ×         | ○         |
| `migrate:fresh`    | ○             | ○        | ○          | ×         | ○         |
| `db:seed`          | ○             | ×        | ○          | ×         | ○         |
| `db:wipe`          | ○             | ×        | ×          | ×         | ○         |
| `migrate:unlock`   | ○             | ×        | ×          | ×         | ×         |

| 指定                 | 意味                                                             |
|----------------------|------------------------------------------------------------------|
| `--database=名前`    | 使う接続を選ぶ。既定は `DB_CONNECTION` です                      |
| `--seed`             | 流したあとにシーダーも動かす                                     |
| `--class=名前`       | 動かすシーダーを 1 本に絞る。`db:seed` 以外では `--seed` と一緒に使います |
| `--step=数`          | 巻き戻すバッチの数。既定は 1 です。`--step=0` も 1 バッチ分です  |
| `--force`            | 本番で、中身が消えるコマンドを通す（下の「本番で止まるコマンド」） |

- **シーダーは既定の接続にだけ流せます。** シーダーの中の `DB::table(..)` が既定の接続を
  使うためです。`db:seed` と `--seed` に、既定と違う `--database=` を付けるとエラーになります。
  別の接続に流すときは、`DB_CONNECTION` をその名前にして実行します。
- `migrate` と `db:seed` の `--force` は、付けても付けなくても同じ動きです。
  どちらも本番で止まらないためです（Laravel の手順をそのまま使えるように受けます）。

### 流し方の決まり

- 流した記録は `migrations` 表に残ります（名前とバッチ番号）。
  同じものが 2 回流れることはありません。
- **1 本ごとにトランザクションで実行します。** 途中で失敗したら、その 1 本を巻き戻して止まります。
- **MySQL は例外です。** 表を作る・変える文がトランザクションを勝手に確定するので、
  失敗した文より前の文は残ります。失敗した文を直して流し直してください。
- `migrate:rollback` は、戻すバッチの全部のファイルがいまのバイナリに入っていることを
  先に確かめます。1 本でも無ければ、**何も戻さずに**止まります。
- 組み立てに誤りがあるマイグレーションは、**SQL を 1 本も流さずに**止まります
  （下の「できないこと・警告が出ること」）。
- `migrate:fresh` と `db:wipe` は、外部キーの向きに関係なく全部の表を消します。
  **`migration_locks` 表だけは消しません。**

### 同時に流さない仕組み（札）

表を変えるコマンドは、**実行の前に `migration_locks` 表に「札」を 1 枚取ります。**
同じものが二重に走るのを防ぐためです。

- `migrate:status` と `migrate:unlock` 以外の 7 つが札を取ります。
- すでに誰かが持っていると、**何もせずにエラー**になります。
- 札は終わったら返します。失敗したときも返します。

```
別のプロセスがマイグレーションを実行中です（myhost:1234 が 2026-10-30 10:00:00 に開始）。
終わってからもう一度実行してください。
途中で落ちて札が残っているときは、`migrate:unlock` で消せます。
```

プロセスが強制終了されると札が残ります。`cargo artisan migrate:unlock` で外します。
持ち主（ホスト名とプロセス番号）と時刻を表示するので、本当に動いていないかを確かめられます。

`migrate:unlock` は、`migration_locks` 表が無ければ**作りません**（`migrate:status` と同じです）。
「札は誰も持っていません。」と出して終わります。表を作るのは、札を取るコマンドだけです。

### 本番で止まるコマンド

次の 5 つは、`APP_ENV=production` のとき `--force` が無いと止まります。

`migrate:rollback` / `migrate:reset` / `migrate:refresh` / `migrate:fresh` / `db:wipe`

`rollback` / `reset` / `refresh` は `down` が流れるので、名前に「消す」と無くても中身が消えます。

### 状態を見る

```
$ cargo artisan migrate:status
名前                                       状態      バッチ
2026_10_05_000000_create_posts_table     実行済み  1
2026_10_05_000100_create_comments_table  未実行    -

2 本中 1 本が実行済みです
```

| 状態           | 意味                                                 |
|----------------|------------------------------------------------------|
| `実行済み`     | 流してあります                                       |
| `未実行`       | まだ流していません                                   |
| `ファイルなし` | 記録にはあるが、いまのバイナリにファイルがありません |

`migrate:status` は読むだけです。**`migrations` 表を作りません。**
表が無ければ、全部を「未実行」と出します。

## 列の種類

| 書き方                            | SQLite                 | MySQL                            | PostgreSQL         |
|-----------------------------------|------------------------|----------------------------------|--------------------|
| `t.id()`                          | `integer`（自動採番）  | `bigint unsigned`（自動採番）    | `bigserial`        |
| `t.increments("uid")`             | 同上（名前を決める）   | 同上                             | 同上               |
| `t.integer("views")`              | `integer`              | `integer`                        | `integer`          |
| `t.big_integer("n")`              | `integer`              | `bigint`                         | `bigint`           |
| `t.foreign_id("post_id")`         | `integer`              | `bigint unsigned`                | `bigint`           |
| `t.string("title")`               | `varchar(255)`         | `varchar(255)`                   | `varchar(255)`     |
| `t.string_with("code", 8)`        | `varchar(8)`           | `varchar(8)`                     | `varchar(8)`       |
| `t.text("body")`                  | `text`                 | `text`                           | `text`             |
| `t.boolean("pinned")`             | `boolean`              | `tinyint(1)`                     | `boolean`          |
| `t.float("x")`                    | `real`                 | `float(53)`                      | `double precision` |
| `t.double("y")`                   | `real`                 | `double precision`               | `double precision` |
| `t.decimal("price", 8, 2)`        | `numeric(8, 2)`        | `numeric(8, 2)`                  | `numeric(8, 2)`    |
| `t.date("on")`                    | `date`                 | `date`                           | **`text`**         |
| `t.date_time("at")` / `t.timestamp("at")` | `datetime`     | `datetime`                       | **`text`**         |
| `t.json("meta")`                  | `text`                 | `json`                           | **`text`**         |
| `t.binary("data")`                | `blob`                 | `blob`                           | `bytea`            |
| `t.uuid("key")`                   | `varchar(36)`          | `varchar(36)`                    | `varchar(36)`      |
| `t.raw_column("x", "型名")`       | 型名をそのまま書く     | 同左                             | 同左               |

まとめて足すものが 2 つあります。

| 書き方             | 足す列                                              |
|--------------------|-----------------------------------------------------|
| `t.timestamps()`   | `created_at` と `updated_at`（日時。どちらも null 可） |
| `t.soft_deletes()` | `deleted_at`（日時。null 可）                       |

- `t.id()` と `t.increments(..)` は、自動採番の主キーです。
- `t.foreign_id(列)` は、`t.id()` と符号がそろう型になります。そのまま外部キーを張れます。
- `t.float(列)` と `t.double(列)` は、**どのドライバでも 8 バイト**です（Laravel の既定と違います）。
  4 バイトの列が要るときは `t.raw_column(列, "float")` と書きます。
- **PostgreSQL では、日付・日時・JSON が `text` 列になります。** bengara が日時を文字列で
  扱うためです。PostgreSQL は、日時の列に文字列を渡すと断ります。

## 列に付ける指定

続けて書けます。

```rust
t.string("email").unique();
t.text("body").nullable();
t.integer("views").default(0);
t.date_time("at").default_raw("current_timestamp");
t.uuid("key").primary();
```

| 書き方               | 意味                                                       |
|----------------------|------------------------------------------------------------|
| `nullable()`         | null を許す（付けなければ not null）                       |
| `default(値)`        | 既定値                                                     |
| `default_raw("sql")` | 既定値を SQL で書く                                        |
| `unique()`           | 重なりを禁じる（`{表}_{列}_unique` という名前の一意索引）  |
| `index()`            | 索引を付ける（`{表}_{列}_index`）                          |
| `primary()`          | 主キーにする（表を作るときだけ）                           |
| `comment("説明")`    | **何もしません。** Laravel と同じ名前で書けるだけです      |

- **`t.id()` と `t.increments(..)` には、`nullable` / `default` / `unique` が効きません。**
  警告が出ます。
- 列の `unique()` と、表の `t.unique(&["列"])` は同じものを作ります。両方書いても索引は 1 つです。

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

| 書き方                                           | 意味                                       |
|--------------------------------------------------|--------------------------------------------|
| `t.index(&["a", "b"])`                           | 索引                                       |
| `t.unique(&["email"])`                           | 重なりを禁じる（一意索引）                 |
| `t.primary(&["a", "b"])`                         | 複合主キー                                 |
| `t.foreign(列).on(表)`                           | 外部キー。指す先の列は `id` です           |
| `.references(列)`                                | 指す先の列を `id` 以外にする               |
| `.on_delete("cascade")` / `.cascade_on_delete()` | 元の行が消えたときの動き                   |
| `.on_update("restrict")`                         | 元の行が変わったときの動き                 |

### 外部キーの動き

`on_delete` と `on_update` に渡せるのは、次の 5 つです。

`cascade` / `set null` / `set default` / `restrict` / `no action`

大文字小文字と語の間の空白は、そろえてから見ます。
外れた値もそのまま SQL に出ますが、打ち間違いに気づけるよう**警告が出ます。**

### 索引と外部キーの名前

名前は自動で付きます。形は `{表}_{列を _ でつないだもの}_{index|unique|foreign}` です
（`posts_status_index`・`users_email_unique`・`comments_post_id_foreign`）。

| とき                      | 付く名前                              |
|---------------------------|---------------------------------------|
| ふつう                    | 上の形のまま                          |
| 同じ名前が 2 つできるとき | 末尾に 8 桁の 16 進を足して分けます   |
| 63 バイトを超えるとき     | 縮めて、末尾に 8 桁の 16 進を足します |

- 外部キーに名前が付くのは、**後から足すとき**だけです（下の「表を変える」）。
  表を作るときは、名前をデータベースが決めます。
- `unique()` が作るのは、一意「制約」ではなく一意「索引」です。
  PostgreSQL の `on conflict on constraint 名前` には使えません（`on conflict (列)` は使えます）。
- MySQL で表を作るときだけ、索引は表の定義の中（`key` / `unique key`）に入ります。
  ほかは `create index` の別の文です。

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

**`schema.table(..)` でできるのは「列を足す」「索引を足す」「外部キーを足す」までです。**

| 足すもの   | どうなるか                                                    |
|------------|---------------------------------------------------------------|
| 列         | `alter table add column`                                      |
| 索引       | `create index`                                                |
| 外部キー   | `alter table add constraint {表}_{列}_foreign`（SQLite では足せません） |
| 主キー     | 足せません                                                    |

- **足す列には `.nullable()` か `.default(..)` を付けてください。** 付けないと、
  SQLite では必ず、PostgreSQL では行が 1 件以上あると失敗します。
  MySQL だけは型ごとの既定値で埋めて通るので、MySQL で開発していると気づけません。
- 列の型は変えられません。新しい表を作って移し替えてください。
- SQLite で外部キーを足したいときも、表を作り直してください。

## そのほかの操作

| 書き方                                                 | すること                               |
|--------------------------------------------------------|----------------------------------------|
| `schema.create_if_not_exists("posts", \|t\| { .. })`   | 無ければ表を作る                       |
| `schema.drop("posts")`                                 | 表を消す                               |
| `schema.drop_if_exists("posts")`                       | あれば表を消す                         |
| `schema.rename("posts", "articles")`                   | 表の名前を変える                       |
| `schema.drop_column("posts", "slug")`                  | 列を消す                               |
| `schema.drop_index("posts", &["user_id"])`             | `index()` で作った索引を落とす         |
| `schema.drop_unique("users", &["email"])`              | `unique()` で作った索引を落とす        |
| `schema.drop_index_named("users", "users_email_unique")` | 名前を直に指定して索引を落とす       |
| `schema.raw("SQL")`                                    | SQL をそのまま流す                     |
| `schema.to_sql()`                                      | その時点までに組み立てた SQL の一覧を見る |
| `schema.driver()`                                      | つながる先の種類（`Driver`）を見る     |

`create_if_not_exists` は、**索引の文にも `if not exists` を付けます。**
表だけを飛ばして索引で失敗する、ということはありません。

### 索引を落とす

**`up` で `index()` / `unique()` を付けたら、`down` で落としてください。**
列を落としただけでは索引が残ります。**落とす順は「索引 → 列」です**
（SQLite は索引の付いた列を `drop_column` できません）。

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

名前は、作ったときと同じ規則で組み立てます。

| 作り方                                  | 落とし方                     | 名前               |
|-----------------------------------------|------------------------------|--------------------|
| `t.index(&[列...])` / 列の `.index()`   | `drop_index(表, &[列...])`   | `{表}_{列}_index`  |
| `t.unique(&[列...])` / 列の `.unique()` | `drop_unique(表, &[列...])`  | `{表}_{列}_unique` |
| どちらでも                              | `drop_index_named(表, 名前)` | 名前を直に書く     |

次のときは名前が合いません。`drop_index_named` か `raw` を使ってください。

| とき                                                         | 名前と落とし方                                   |
|--------------------------------------------------------------|--------------------------------------------------|
| 同じ `create` の中に、同じ名前になる別の列の並びがあった     | 末尾に 16 進が付いています。`drop_index_named`   |
| bengara の規則と違う名前の一意制約（列の定義に `unique` と書いて作った表など） | 下の表                         |

| ドライバ   | データベースが付ける名前      | 落とし方                                                           |
|------------|-------------------------------|--------------------------------------------------------------------|
| SQLite     | 自動（`sqlite_autoindex_..`） | 落とせません。表を作り直します                                     |
| MySQL      | 列の名前（`email`）           | `drop_index_named("users", "email")`                               |
| PostgreSQL | `{表}_{列}_key`（一意制約）   | `raw("alter table \"users\" drop constraint \"users_email_key\"")` |

無い索引を落とそうとしたとき、SQLite と PostgreSQL は何もしません。MySQL はエラーになります。

## できないこと・警告が出ること

止め方は 3 つに分かれます。

| 止め方               | いつ                                         |
|----------------------|----------------------------------------------|
| コンパイルエラー     | ビルドのとき                                 |
| **実行の前のエラー** | SQL を 1 本も流しません。表は変わりません    |
| 警告                 | SQL はそのまま流します。失敗するかは中身次第 |

| 書き方                                                  | どうなるか                           | 理由                                               |
|---------------------------------------------------------|--------------------------------------|----------------------------------------------------|
| `pub fn up` / `pub fn down` が無い                      | コンパイルエラー                     | 両方必要です                                       |
| `t.foreign(列)` に `.on(表名)` が無い                   | **実行の前のエラー**                 | 指す先の表が決まりません                           |
| `schema.table(..)` の中の `t.id()` / `t.increments(..)` | **実行の前のエラー**                 | 後から自動採番の主キーは足せません                 |
| `schema.table(..)` の中の列の `.primary()`              | **実行の前のエラー**                 | 後から主キーは足せません                           |
| `schema.table(..)` の中の `t.primary(&[..])`            | 警告が出て、**何も足しません**       | 3 つのドライバでそろう書き方がありません           |
| SQLite で `schema.table(..)` の中の `t.foreign(..)`     | 警告が出て、**何も足しません**       | SQLite には後から外部キーを足す文がありません      |
| `schema.table(..)` で not null かつ既定値なしの列       | 警告が出て、SQLite と PostgreSQL で失敗します | 既にある行に入れる値が決まりません        |
| `t.id()` に `nullable()` / `default()` / `unique()`     | 警告が出て、無視します               | 自動採番の主キーには付けられません                 |
| `t.primary(&[..])` を 2 回呼ぶ                          | 警告。後の指定で置き換わります       | 複合主キーは 1 回で書きます                        |
| `on_delete` / `on_update` に一覧外の値                  | 警告。SQL には出ます                 | 打ち間違いに気づけるようにしてあります             |
| `default(f64::NAN)` などの `NaN` や無限                 | 警告が出て、その文は失敗します       | SQL に書けません                                   |
| `default(Vec<u8>)`（バイト列の既定値）                  | 警告。**値は無視して `null` を書きます** | 書き方がドライバで違います                     |
| 列の型を変える（Laravel の `change()`）                 | **できません**                       | 新しい表を作って移し替えます                       |

まだ無いものは [backlog.md](backlog.md) にまとめてあります。

## 関連

- [database.md](database.md) — 接続とクエリ
- [models.md](models.md) — 構造体として読み書きする（シーダーの書き方も）
- [directory-structure.md](directory-structure.md) — ファイルの自動検出
