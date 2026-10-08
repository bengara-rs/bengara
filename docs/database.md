# データベース

接続の設定と、問い合わせの組み立て（`DB::table(...)`）、トランザクションの説明です。
表の作り方は [migrations.md](migrations.md)、構造体として読み書きする方法は
[models.md](models.md) にあります。

## 使えるようにする

データベースは **機能フラグ**です。`Cargo.toml` に書きます。

```toml
[dependencies]
bengara = { version = "0.1", features = ["sqlite"] }
```

`cargo run -- init` で作ったプロジェクトには最初から入っています。
下回りは sqlx 0.8 ですが、その型は外に出ません。

| 使えるもの         | 状況                                                    |
|--------------------|---------------------------------------------------------|
| SQLite             | **使えます**（`features = ["sqlite"]`）                 |
| MySQL / PostgreSQL | **まだ使えません。** SQL の組み立てだけ用意してあります |

フラグが無いままでもコンパイルは通ります。
`DB::...` を呼んだときに、直し方を書いたエラーが返ります。

> Rust 1.85（最低対応版）で使うときは、依存を 1 つ固定してください。
> `cargo update idna_adapter --precise 1.2.0`（[getting-started.md](getting-started.md)）

## 設定

`.env` に書きます。

```
DB_CONNECTION=sqlite
DB_DATABASE=database/database.sqlite

# テストで使うデータベース（:memory: ならディスクに残りません）
DB_TEST_DATABASE=:memory:
```

細かく決めたいときは `config/database.rs` を書きます。
置かなければ、上の環境変数から組み立てた既定の設定が使われます。

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
        ],
    }
}
```

| 指定                 | 既定 | 意味                                                     |
|----------------------|------|----------------------------------------------------------|
| `max_connections(n)` | 5    | 同時に張る接続の数。`:memory:` のときは 1 に固定されます |
| `foreign_keys(b)`    | 真   | SQLite で外部キーの制約を効かせるか                      |
| `url(s)`             | 空   | 接続文字列を直接書く。書くと `database` より優先します   |

気をつけること。

- SQLite のファイルは **無ければ作ります。** 置くディレクトリは先に用意してください。
- 相対パスはプロジェクト直下（`base_path()`）から見ます。
- 接続は **最初に使ったときに**張ります。起動は速いままです。
- メモリ上のデータベースかどうかは、**`:memory:` を含むかどうか**だけで決めます。
  `data/memory_2026.db` のように `memory` という語を含むだけのファイル名は、
  メモリ扱いになりません（接続 1 本に固定されず、ファイル向けの設定が使われます）。

## 問い合わせを組み立てる

`DB::table("表の名前")` から始めて、やりたいことを足していきます。
最後に **終端のメソッド**（`get()` など）を呼ぶと、そこで SQL が投げられます。

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

この節は、やりたいこと別に分かれています。

| やりたいこと         | 何をするか                           |
|----------------------|--------------------------------------|
| 条件を足す           | `where` のたぐい                     |
| 並べる・件数を決める | 並び順、`limit`、ページ             |
| 表をつなぐ           | `join` と、取る列を選ぶ `select`     |
| 束ねて絞る           | `group_by` と `having`               |
| 結果を取り出す       | `get` / `first` など、終端のメソッド |
| 数える               | 件数と合計                           |

### 条件を足す

| 書き方                                                 | 意味             |
|--------------------------------------------------------|------------------|
| `where_("status", "published")`                        | 等しい           |
| `where_op("views", ">", 100)`                          | 演算子を指定する |
| `or_where(...)` / `or_where_op(...)`                   | `or` でつなぐ    |
| `where_in("id", &[1, 2, 3])` / `where_not_in`          | 一覧のどれか     |
| `where_null("body")` / `where_not_null`                | `null` かどうか  |
| `where_between("views", 0, 100)` / `where_not_between` | 範囲             |
| `where_like("title", "%そば%")`                        | 形に当てはまる   |
| `where_column("updated_at", ">", "created_at")`        | 列と列を比べる   |
| `where_raw("length(title) > ?", &[3.into()])`          | SQL を直接書く   |
| `where_group(\|q\| q.where_("a", 1).or_where("b", 2))` | 括弧でくくる     |

`where` は Rust の予約語なので `where_` です。
**等しいかどうかは `where_`、それ以外は `where_op`** に分かれています。
Rust には引数の数による使い分けが無いためです。

```rust
// Laravel: ->where('views', '>', 100)
DB::table("posts").where_op("views", ">", 100)
```

`where_in` に空の一覧を渡すと、**必ず 0 件**になります（`1 = 0` を置きます）。

`where_raw` の `?` は **値の置き場所としてだけ**使えます。

- `?` の数と渡した値の数が合わないと、終端のメソッドでエラーになります。
- 文字列の中の `?` も置き場所として数えます。`'a?b'` のような書き方はできません。

### 並べる・件数を決める

| 書き方                                           | 意味                       |
|--------------------------------------------------|----------------------------|
| `order_by(col)` / `order_by_desc(col)`           | 並べる                     |
| `order_by_raw("sql")`                            | 並べ方を SQL で書く        |
| `latest()` / `oldest()`                          | `created_at` の降順 / 昇順 |
| `latest_by(col)` / `oldest_by(col)`              | 列を指定して並べる         |
| `limit(n)` / `offset(n)` / `take(n)` / `skip(n)` | 件数                       |
| `for_page(page, per_page)`                       | ページから件数を決める     |

**並びが一意になるようにしてください。**
同じ値の行が複数あると、ページの境目で同じ行が 2 回出ることがあります。
`latest()` に `order_by_desc("id")` を足すなどしてください。

式で並べたいときは `order_by_raw` です。

```rust
DB::table("posts")
    .order_by_raw("case when pinned then 0 else 1 end asc")
    .order_by_desc("id")
```

`order_by_raw` は **引用も検査もしません。** 外から来た文字列を渡さないでください。

### 表をつなぐ

| 書き方                                            | 意味           |
|---------------------------------------------------|----------------|
| `join("users", "users.id", "=", "posts.user_id")` | 結合           |
| `left_join(...)`                                  | 外側結合       |
| `select(&["id", "title"])` / `add_select("body")` | 取る列         |
| `distinct()`                                      | 重なりを捨てる |

`select` は式も書けます。
`order_by_raw` と同じく検査しないので、外から来た文字列を渡さないでください。

### 束ねて絞る

```rust
DB::table("posts")
    .select(&["status", "count(*) as total"])
    .join("users", "users.id", "=", "posts.user_id")
    .group_by(&["status"])
    .having_op("total", ">", 2)
    .order_by_desc("total")
    .get()
    .await?;
```

| 書き方                  | 意味               |
|-------------------------|--------------------|
| `group_by(&["status"])` | 束ねる             |
| `having_op(col, 演算子, 値)` | 束ねた結果で絞る   |
| `having_raw("sql")`     | 絞り方を SQL で書く |

**`group_by` の無い `having` はエラーです。** 束ねていないと、絞る相手が決まりません。
束ねずに絞りたいときは `where` を使ってください。

### 結果を取り出す

| 終端                           | 返るもの            |
|--------------------------------|---------------------|
| `get()`                        | `Vec<Row>`          |
| `first()`                      | `Option<Row>`       |
| `find(id)`                     | `Option<Row>`       |
| `value::<T>(col)`              | `Option<T>`（1 つの値）  |
| `pluck::<T>(col)`              | `Vec<T>`（1 列ぶん） |

```rust
let row = DB::table("posts").where_("id", 1).first().await?;
let row = DB::table("posts").find(1).await?;                   // id で引く
let title = DB::table("posts").where_("id", 1)
    .value::<String>("title").await?;
let titles = DB::table("posts").pluck::<String>("title").await?;
```

`Row` から値を取り出します。

```rust
let row = DB::table("posts").find(1).await?.unwrap();

let title: String = row.get("title")?;        // 無い列・型違いはエラー
let views: i64 = row.get("views")?;
let body: Option<String> = row.get("body")?;  // null を許すとき
let maybe = row.try_get::<i64>("views");      // 読めなければ None
```

読める型は `i64` / `i32` / `u32` / `u64` / `f64` / `f32` / `bool` / `String` / `Vec<u8>` /
`Value` と、その `Option` です（`i8` / `i16` / `isize` / `u8` / `u16` / `usize` も読めます）。
幅の狭い型に **収まらない値はエラー**です。`f32` に収まらない小数も同じです。

`Row` は `serde::Serialize` を実装しているので、`json(&rows)` でそのまま返せます。

`value` と `pluck` は **1 列だけ取って、その位置から値を読みます。**
名前では引かないので、表名を付けた `users.name` や別名付きの式も渡せます。

```rust
let names = DB::table("posts")
    .join("users", "users.id", "=", "posts.user_id")
    .pluck::<String>("users.name")
    .await?;
```

### 数える

```rust
let total = DB::table("posts").count().await?;                          // i64
let any = DB::table("posts").where_("status", "draft").exists().await?; // bool
let none = DB::table("posts").doesnt_exist().await?;
let sum = DB::table("posts").sum::<i64>("views").await?;                // Option<i64>
// avg / min / max も同じ形
```

`count()` の数え方は、付いている指定で変わります。

| 付いている指定 | `count()` が返すもの |
|----------------|----------------------|
| 何も無い       | 行の数               |
| `group_by`     | **グループの数**     |
| `distinct`     | 重なりを除いた行の数 |

`paginate` の `total` も同じ数え方です。

`sum` / `avg` / `min` / `max` は `group_by` と併用できません。
グループごとの値が欲しいときは、`select` に式を書いて `get()` します。

```rust
DB::table("posts")
    .select(&["status", "sum(views) as views"])
    .group_by(&["status"])
    .get()
    .await?;
```

`distinct()` を付けると `sum(distinct "views")` の形になり、重なりを除いた値が返ります。
重なりを除いて集計できるのは、ただの列名を渡したときだけです。

## 書き込む

### 入れる

```rust
// 1 行
DB::table("posts")
    .insert(&[("title", "はじめての記事".into()), ("views", 0.into())])
    .await?;

// 入れて ID を受け取る
let id = DB::table("posts")
    .insert_get_id(&[("title", "はじめての記事".into())])
    .await?;

// まとめて
let rows = vec![
    vec![("title", "A".into())],
    vec![("title", "B".into())],
];
DB::table("posts").insert_many(&rows).await?;
```

値は `.into()` で `Value` にします。
`&str` / `String` / 整数 / 小数 / 真偽 / `Vec<u8>` / `Option<T>`（`None` は `null`）が渡せます。

`u64` は `i64` に収まらない値も扱えます。
収まらないときは文字列として入れ、読み出しでも `u64` に戻します。

### 更新する・消す

```rust
// 更新（返るのは件数）
let changed = DB::table("posts")
    .where_("id", id)
    .update(&[("title", "改題".into())])
    .await?;

// 削除（返るのは件数）
let removed = DB::table("posts").where_("id", id).delete().await?;

// 全部消す
DB::table("posts").truncate().await?;
```

**条件を書かないと全行が対象です。** Laravel と同じです。

`update` / `delete` / `truncate` には、付けられない指定があります。
下の「できないこと・エラーになること」を見てください。
件数を絞って書き換えたいときは、先に主キーを取り出して `where_in` で絞ります。

```rust
let ids: Vec<i64> = DB::table("posts").latest().limit(10).pluck::<i64>("id").await?;
DB::table("posts").where_in("id", &ids).update(&[("status", "archived".into())]).await?;
```

### 日時

`created_at` のような列には、`now()` で作った文字列を入れます。

```rust
DB::table("posts")
    .insert(&[("title", "x".into()), ("created_at", now().into())])
    .await?;
```

`now()` は `YYYY-MM-DD HH:MM:SS`（UTC）を返します。日時専用の型は持っていません。
モデル経由（`save()`）なら自動で入ります（[models.md](models.md)）。

## ページ分け

```rust
pub async fn index(req: Request) -> Result<Response> {
    let page = DB::table("posts").latest().paginate(15, req.page()).await?;

    json(&bengara::serde_json::json!({
        "data": page.data,
        "total": page.total,
        "current_page": page.current_page,
        "last_page": page.last_page(),
        "has_more": page.has_more_pages(),
    }))
}
```

ページ番号は **引数で渡します。** `req.page()` が `?page=` を読みます（無ければ 1）。

| 名前                                           | 意味                     |
|------------------------------------------------|--------------------------|
| `data` / `total` / `per_page` / `current_page` | 中身と件数               |
| `last_page()`                                  | 最後のページ番号         |
| `has_more_pages()`                             | 次があるか               |
| `from()` / `to()`                              | 全体で何件目から何件目か |

## トランザクション

```rust
let tx = DB::begin().await?;

tx.table("posts").insert(&[("title", "x".into())]).await?;
tx.table("posts").where_("id", 1).update(&[("views", 1.into())]).await?;

tx.commit().await?;
```

- `tx.table(...)` で作った問い合わせは、そのトランザクションの中で動きます。
- **`commit()` を呼ばずに `tx` が落ちたら巻き戻します。** 明示するなら `tx.rollback().await?`。
- `DB::table(...)` は **トランザクションの外**です。混ぜないでください。
- **1 本ずつ `await` してください。** 同時に 2 本投げるとエラーになります。

### モデルを保存するとき

トランザクションの中でモデルを保存するときは、**`save_using(&tx)`** を使います。

```rust
let tx = DB::begin().await?;
let mut post = Post::draft("やきそば");
post.save_using(&tx).await?;
tx.commit().await?;
```

`save()` / `delete()` / `fresh()` は **既定の接続**を使います。
トランザクションの中で呼ぶと、次の 2 つが起きます。

- 書き込みがトランザクションの外に出ます。`rollback()` しても残ります。
- `:memory:` のデータベースは接続が 1 本なので、空くのを待って固まります。

詳しくは [models.md](models.md) にあります。

## SQL を直接書く

```rust
let rows = DB::select("select * from posts where views > ?", &[100.into()]).await?;
let affected = DB::statement("update posts set views = views + 1", &[]).await?;
```

組み立てた SQL を見たいときは `to_sql()` が使えます。接続しません。

```rust
let (sql, bindings) = DB::table("posts").where_("id", 1).to_sql();
assert_eq!(sql, "select * from \"posts\" where \"id\" = ?");
```

## 接続を選ぶ

```rust
let rows = DB::connection("logs").table("events").get().await?;
let tx = DB::connection("logs").begin().await?;
```

## ログ

投げた SQL は既定では出ません。見たいときは次のようにします。

```sh
RUST_LOG=info,sqlx=debug cargo artisan serve
```

**渡した値（`?` の中身）はエラーメッセージに出しません。** 個人情報が混じるためです。
SQL が失敗したときは、文だけを添えて返します。

## 列名と表名は検査します

外から来た文字列が SQL に混ざらないようにするためです。
渡せる列名と表名は **英数字と `_` `.` だけ**で、演算子は許可一覧で照合します。

検査する場所の一覧です（読み飛ばしてかまいません）。

| 場所          | 検査するもの                                                                                                                                                                      |
|---------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| 入口          | `DB::table(...)` の表名                                                                                                                                                           |
| 条件          | `where_` / `where_op` / `or_where` / `or_where_op` / `where_in` / `where_not_in` / `or_where_in` / `where_null` / `where_not_null` / `where_between` / `where_not_between` / `where_like` / `where_column` |
| 並び・束ね    | `order_by` / `order_by_desc` / `group_by` / `having_op`                                                                                                                           |
| 結合          | `join` / `left_join`（表名・両側の列名・演算子）                                                                                                                                  |
| 書き込み      | `insert` / `insert_many` / `insert_get_id_as` / `update` の列名と、`insert_get_id_as` の主キー名                                                                                   |

エラーが出るタイミングは 2 つに分かれます。

| どこ                                 | いつエラーになるか                 |
|--------------------------------------|------------------------------------|
| 入口・条件・並び・束ね・結合         | **終端のメソッド**（`get()` など） |
| 書き込み（`insert` / `update` など） | 呼んだ **その場で `Err`**          |

細かい決まりは次の 3 つです。

- **英数字は Unicode で見ます。** 日本語の列名（`"題名"`）もそのまま通ります。
  `"` `;` `(` `-` と空白は Unicode でも英数字ではないので、検査の強さは変わりません。
- **引用符（`"` と `` ` ``）を含む名前は、式として素通ししません。**
  `""` と ``` `` ``` に逃がしてくくります。引用符を 1 つ混ぜて検査を抜ける道をふさぐためです。
- 式を書きたいときは `select` / `order_by_raw` / `having_raw` / `where_raw` を使います。
  **この 4 つは検査しません。** 外から来た文字列を渡さないでください。

## できないこと・エラーになること

| 書き方                                                               | どうなるか                   | 理由                                             |
|----------------------------------------------------------------------|------------------------------|--------------------------------------------------|
| `group_by` の無い `having`                                           | エラー                       | 絞る相手が決まりません                           |
| `sum` / `avg` / `min` / `max` に `group_by`                          | エラー                       | `select` に式を書いて `get()` してください       |
| `distinct()` を付けた `sum` などに、式や `*` の列                    | エラー                       | 重なりを除けるのはただの列名のときだけです       |
| `update` / `delete` に `join`                                        | エラー                       | SQL に入らず、黙って全件に当たります             |
| `update` / `delete` に `limit` / `offset`                            | エラー                       | 同じ理由です                                     |
| `update` / `delete` に `group_by` / `having` / `distinct`            | エラー                       | 同じ理由です                                     |
| `update` / `delete` に `order_by`                                    | **黙って無視**               | SQL に入らなくても、当たる行が変わりません       |
| `where_` を付けた `truncate()`                                       | エラー                       | 絞って消すなら `delete()` を使います             |
| `truncate()` に `join` / `limit` / `offset` / `group_by` / `having` / `distinct` | エラー            | `update` / `delete` と同じです                   |
| `where_raw` の `?` の数と値の数が合わない                            | 終端のメソッドでエラー       |                                                  |
| 使えない文字を含む列名・表名                                         | 終端、または呼んだその場     | 上の「列名と表名は検査します」を参照             |
| 幅の狭い型に収まらない値を読む                                       | 読み出しでエラー             | `f32` に収まらない小数も同じです                 |
| トランザクションで同時に 2 本投げる                                  | エラー                       | 1 本ずつ `await` してください                    |
| 機能フラグ無しで `DB::...` を呼ぶ                                    | 直し方を書いたエラー         | `features = ["sqlite"]` を足してください         |

## 気をつけること

| こと                         | 内容                                                                              |
|------------------------------|-----------------------------------------------------------------------------------|
| 列名に外から来た値を入れない | 値（`where_` の第 2 引数）は必ずプレースホルダです。式が書ける 4 つは検査しません |
| `:memory:` は接続 1 本       | メモリ上のデータベースは接続ごとに別物になるため、1 本に固定します               |
| 1 プロセスで 1 つの接続プール | 既定の接続は最初の 1 回だけ作られ、以後は使い回します                            |

## 関連

- [migrations.md](migrations.md) — 表を作る
- [models.md](models.md) — 構造体として読み書きする
- [testing.md](testing.md) — DB を使うテスト
- [configuration.md](configuration.md) — `config/*.rs` と `.env`
