# マイグレーション

表を作る手順を、日付入りのファイルとして残します。
`cargo artisan migrate` で、まだ流していないものだけが順に実行されます。

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
- **`up` / `down` は同期の関数です。** ここでやるのは SQL の組み立てだけです。
  実際に投げるのは bengara 側（ランナー）です。
- 登録の作業はありません。ファイルを置けば `build.rs` が拾います。

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

- どれにも `--database=接続の名前` を付けられます。既定は `DB_CONNECTION` です。
- `--step=` は `migrate:rollback` だけで見ます。既定は 1 です。
  **`--step=0` は 1 バッチ分と同じです**（0 バッチを戻す書き方はありません）。
- 流した記録は `migrations` 表に残ります（名前とバッチ番号）。
  同じものが 2 回流れることはありません。
- **1 本ごとにトランザクションで実行します。** 途中で失敗したら、その 1 本を巻き戻して
  止まります。**MySQL は例外です。** 表を作る・変える文がトランザクションを勝手に確定
  するので、そこまでの文は残ります。失敗した文を直して流し直してください。
- `migrate:rollback` は、戻すバッチの全部のファイルがいまのバイナリに入っていることを
  先に確かめます。1 本でも無ければ、**何も戻さずに**止まります。
- 組み立てに誤りがあるマイグレーションは、**SQL を 1 本も流さずに**止まります
  （下の「できないこと・警告が出ること」）。

### 同時に流さない仕組み（札）

表を変えるコマンドは、**実行の前に `migration_locks` 表に「札」を 1 枚取ります。**
同じものが二重に走るのを防ぐためです。

- 札を取るのは `migrate` / `migrate:rollback` / `migrate:reset` / `migrate:refresh` /
  `migrate:fresh` / `db:seed` / `db:wipe` です。`migrate:status` と `migrate:unlock` は
  取りません。
- すでに誰かが持っていると、**何もせずにエラー**になります。
- 札は終わったら返します。失敗したときも返します。

プロセスが強制終了されると札が残ります。`cargo artisan migrate:unlock` で外します。
持ち主（プロセス番号とホスト名）と時刻を表示するので、本当に動いていないかを
先に確かめられます。

```
別のプロセスがマイグレーションを実行中です（myhost:1234 が 2026-10-30 10:00:00 に開始）。
終わってからもう一度実行してください。
途中で落ちて札が残っているときは、`migrate:unlock` で消せます。
```

`migrate:fresh` と `db:wipe` も、**`migration_locks` 表だけは消しません。**

### 本番で止まるコマンド

次の 5 つは、`APP_ENV=production` のとき `--force` が無いと止まります。
`rollback` / `reset` / `refresh` は `down` が流れるので、名前に「消す」と無くても
中身が消えます。

`migrate:rollback` / `migrate:reset` / `migrate:refresh` / `migrate:fresh` / `db:wipe`

### 状態を見る

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

**MySQL と PostgreSQL では型名が変わります。** PostgreSQL では日付と JSON が
`text` 列になります。一覧は [database.md](database.md) の「ドライバの違い」にあります。

気をつけるのは次の 2 つです。

- `t.foreign_id(列)` は、`t.id()` と符号がそろう型になります
  （MySQL は `bigint unsigned`、PostgreSQL は `bigint`）。そのまま外部キーを張れます。
- `t.float(列)` と `t.double(列)` は、**どのドライバでも 8 バイト**です
  （MySQL は `float(53)`、PostgreSQL は `double precision`）。
  4 バイトの列が要るときは `t.raw_column(列, "float")` と書きます。

## 列に付ける指定

続けて書けます。

```rust
t.string("email").unique();
t.text("body").nullable();
t.integer("views").default(0);
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
| `comment("説明")`    | 説明（**どのドライバでも何もしません**） |

**`t.id()` と `t.increments(..)` には、これらの指定が効きません。**
自動採番の主キーは型名ひとつで全部を書くので、置く場所がありません。

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

| 書き方                                           | 意味                       |
|--------------------------------------------------|----------------------------|
| `t.index(&["a", "b"])`                           | 索引                       |
| `t.unique(&["email"])`                           | 重なりを禁じる             |
| `t.primary(&["a", "b"])`                         | 複合主キー                 |
| `t.foreign(col).references(col).on(table)`       | 外部キー                   |
| `.on_delete("cascade")` / `.cascade_on_delete()` | 元の行が消えたときの動き   |
| `.on_update("restrict")`                         | 元の行が変わったときの動き |

`t.primary(&[..])` を 2 回呼ぶと警告が出ます。後の指定で置き換わります。

### 外部キーの動き

`on_delete` と `on_update` に渡せるのは、次の 5 つだけです。

`cascade` / `set null` / `set default` / `restrict` / `no action`

大文字小文字と語の間の空白は、そろえてから見ます。
外れた値は SQL には出ますが、打ち間違いに気づけるよう**警告が出ます。**

### 索引と外部キーの名前

名前は自動で付きます。形は `{表}_{列を _ でつないだもの}_{index|unique|foreign}` です
（`posts_status_index`・`comments_post_id_foreign` のような形）。

| とき                      | 付く名前                              |
|---------------------------|---------------------------------------|
| ふつう                    | 上の形のまま                          |
| 同じ名前が 2 つできるとき | 末尾に 8 桁の 16 進を足して分けます   |
| 63 バイトを超えるとき     | 縮めて、末尾に 8 桁の 16 進を足します |

外部キーに名前が付くのは、**後から足すとき**だけです（下の「表を変える」）。
表を作るときは `create table` の中に書くので、名前はデータベースが決めます。

**MySQL で表を作るときだけ、索引は表の定義の中（`key` / `unique key`）に入ります。**
ほかは `create index ...` の別の文です（`Schema::table` で変えるときは MySQL も別の文）。

## 表を変える

```rust
pub fn up(schema: &mut Schema) {
    schema.table("posts", |t| {
        t.string("slug").nullable();                            // 列を足す
        t.index(&["slug"]);                                     // 索引を足す
        t.foreign("user_id").on("users").cascade_on_delete();   // 外部キーを足す
    });
}

pub fn down(schema: &mut Schema) {
    schema.drop_index("posts", &["slug"]);   // 索引 → 列の順
    schema.drop_column("posts", "slug");
}
```

**できるのは「列を足す」「索引を足す」「外部キーを足す」までです。**

| 足すもの   | どうなるか                                                  |
|------------|-------------------------------------------------------------|
| 列         | `alter table add column`                                    |
| 索引       | `create index`                                              |
| 外部キー   | `alter table add constraint {表}_{列}_foreign`              |
| 複合主キー | **足せません。** 警告だけが出ます                           |

**SQLite では後から外部キーを足せません。** `alter table add constraint` が無いためです。
警告が出て、SQL は出しません。足したいときは表を作り直してください。

列の型を変えたいときも、新しい表を作って移し替えてください。

## そのほかの操作

```rust
schema.create_if_not_exists("posts", |t| { /* ... */ });
schema.drop("posts");
schema.drop_if_exists("posts");
schema.rename("posts", "articles");
schema.drop_column("posts", "slug");
schema.drop_index("posts", &["user_id"]);             // 列から名前を組み立てて落とす
schema.drop_index_named("posts", "posts_email_unique"); // 名前を直に指定して落とす
schema.raw("pragma foreign_keys = on");   // SQL をそのまま
```

### 索引を落とす

**`up` で `index()` / `unique()` を付けたら、`down` で落としてください。**
列を落としただけでは索引が残ります。**落とす順は「索引 → 列」です**
（SQLite は索引の付いた列を `drop_column` できません）。

```rust
pub fn down(schema: &mut Schema) {
    schema.drop_index("posts", &["user_id"]);
    schema.drop_column("posts", "user_id");
}
```

`drop_index(表, &[列...])` は、`index()` と同じ規則で名前を組み立てます。
次の 2 つは名前が合わないので、`drop_index_named(表, 名前)` を使ってください。

| とき                                                         | 名前                               |
|--------------------------------------------------------------|------------------------------------|
| `unique()` で作った索引                                      | 末尾が `_index` ではなく `_unique` |
| 同じ `create` の中に、同じ名前になる別の列の並びがあったとき | 末尾にハッシュが付きます           |

`create_if_not_exists` は、**索引の文にも `if not exists` を付けます。**
表だけを飛ばして索引で失敗する、ということはありません。

組み立てた SQL を確かめたいときは、`up` の中で `schema.to_sql()` を見ます
（その時点までの SQL の一覧が返ります）。`schema.driver()` でつながる先の種類も
分かります。

## できないこと・警告が出ること

止め方は 3 つに分かれます。

| 止め方       | いつ                                       |
|--------------|--------------------------------------------|
| コンパイルエラー | ビルドのとき                            |
| **実行の前のエラー** | SQL を 1 本も流しません。表は変わりません |
| 警告         | SQL はそのまま流します。失敗するかは中身次第 |

| 書き方                                             | どうなるか                           | 理由                                               |
|----------------------------------------------------|--------------------------------------|----------------------------------------------------|
| `pub fn up` / `pub fn down` が無い                 | コンパイルエラー                     | 両方必要です                                       |
| 列の型を変える `change()`                          | **ありません**                       | SQLite が苦手で、3 つのドライバでそろえられないためです |
| `schema.table(..)` の中の `t.id()` / `t.increments(..)` | **実行の前にエラー**            | `alter table add column` では自動採番を足せません |
| `t.foreign(col)` に `.on(表名)` が無い             | **実行の前にエラー**                 | 指す先の表が決まりません                           |
| `t.id()` に `nullable()` / `default()` / `unique()`| 無視して警告                         | 型名ひとつで書くので、置く場所がありません         |
| `schema.table(..)` の中の `t.primary(&[..])`       | 警告が出て、**何も足しません**       | 後から主キーを足す書き方が、3 つのドライバでそろえられません |
| SQLite で `schema.table(..)` の中の `t.foreign(..)`| 警告が出て、**何も足しません**       | `alter table add constraint` がありません          |
| `schema.table(..)` で not null かつ既定値なしの列  | 警告が出て、**SQLite では必ず、PostgreSQL では行が 1 件以上あると実行のときに失敗します** | 既にある行に入れる値が決まりません |
| `t.primary(&[..])` を 2 回呼ぶ                     | 警告。後の指定で置き換わります       | 複合主キーは 1 回で書きます                        |
| `on_delete` / `on_update` に一覧外の値             | 警告。SQL には出ます                 | 打ち間違いに気づけるようにしてあります             |
| `default(f64::NAN)` などの非有限の小数             | 警告が出て、**その文は実行のときに失敗します** | `NaN` / `inf` は SQL に書けません        |
| `default(Vec<u8>)`（バイト列の既定値）             | 警告。**値は無視して `null` を書きます** | 書き方がドライバで違います（`x'..'` / `'\x..'` / `0x..`） |
| `comment("説明")`                                  | **どのドライバでも何もしません**     | Laravel と同じ名前で書けるように置いてあるだけです |

`schema.table(..)` で列を足すときは、`.nullable()` か `.default(..)` を付けてください。
MySQL だけは型ごとの既定値で埋めて通るので、MySQL で開発していると気づけません。

## 関連

まだ無いものは [backlog.md](backlog.md) にまとめてあります。

- [database.md](database.md) — 接続とクエリ
- [models.md](models.md) — 構造体として読み書きする
- [directory-structure.md](directory-structure.md) — ファイルの自動検出
