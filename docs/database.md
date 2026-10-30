# データベース

接続の設定と、問い合わせの組み立て（`DB::table(...)`）、トランザクションの書き方です。
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
- 2 つ以上を同時に書けます。`DB::connection("名前")` で使い分けます。
- フラグが無いままでもコンパイルは通ります。`DB::...` を呼んだときに、
  直し方を書いたエラーが返ります。

```
mysql につなぐには Cargo.toml を `bengara = { version = "0.1", features = ["mysql"] }` にしてください
```

### ビルドに要るもの

`mysql` と `postgres` を入れると、通信の暗号化のために C コンパイラが要ります。
`sqlite` だけなら要りません。

| OS      | 用意するもの                      |
|---------|-----------------------------------|
| Linux   | `cc`（`build-essential` など）    |
| macOS   | Xcode Command Line Tools          |
| Windows | Visual Studio の C++ ビルドツール |

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

見る環境変数は次のとおりです。

| 環境変数             | 既定                                                  | 使うドライバ  |
|----------------------|-------------------------------------------------------|---------------|
| `DB_CONNECTION`      | `sqlite`                                              | すべて        |
| `DB_DATABASE`        | SQLite は `database/database.sqlite`、ほかは `bengara` | すべて        |
| `DB_URL`             | 空                                                    | すべて        |
| `DB_HOST`            | `127.0.0.1`                                           | MySQL / pgsql |
| `DB_PORT`            | 3306 / 5432                                           | MySQL / pgsql |
| `DB_USERNAME`        | `root` / `postgres`                                   | MySQL / pgsql |
| `DB_PASSWORD`        | 空（渡しません）                                      | MySQL / pgsql |
| `DB_MAX_CONNECTIONS` | 5                                                     | すべて        |
| `DB_TEST_DATABASE`   | SQLite は `:memory:`、ほかは無し                       | すべて        |

`DB_URL` は `DB_CONNECTION` で選んだ接続にだけ効きます。

細かく決めたいときは `config/database.rs` を書きます。置かなくても、上の環境変数から
`sqlite` / `mysql` / `mariadb` / `pgsql` の 4 本が組み立てられるので、
`DB::connection("mysql")` のように名前で選べます。

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
            // mysql / postgres も同じ形です。
            ConnectionConfig::mysql("mysql", env::<String>("DB_DATABASE", "myapp"))
                .host(env::<String>("DB_HOST", "127.0.0.1"))
                .username(env::<String>("DB_USERNAME", "root"))
                .password(env::<String>("DB_PASSWORD", "")),
        ],
    }
}
```

続けて書ける指定は次のとおりです。

| 指定                 | 既定               | 意味                                                     |
|----------------------|--------------------|----------------------------------------------------------|
| `host(s)`            | `127.0.0.1`        | つなぎ先。空の文字列は無視します                         |
| `port(n)`            | 3306 / 5432        | ポート。0 は無視します                                   |
| `username(s)`        | `root` / `postgres` | 利用者名。空の文字列は無視します                        |
| `password(s)`        | 空                 | パスワード。空のままなら渡しません                       |
| `max_connections(n)` | 5                  | 同時に張る接続の数。`:memory:` のときは 1 に固定されます |
| `foreign_keys(b)`    | 真                 | SQLite で外部キーの制約を効かせるか（ほかでは常に効きます） |
| `url(s)`             | 空                 | 接続文字列を直接書く。書くと他の値より優先します         |

`url(s)` の形は次のとおりです。

```
sqlite://database/database.sqlite
mysql://利用者名:パスワード@127.0.0.1:3306/データベース名
postgres://利用者名:パスワード@127.0.0.1:5432/データベース名
```

気をつけることは 4 つです。

- SQLite のファイルは無ければ作ります。置くディレクトリは先に用意してください。
  MySQL と PostgreSQL では、**データベースを先に作っておいてください。** 作りません。
- 相対パスはプロジェクト直下（`base_path()`）から見ます。
- 接続は最初に使ったときに張ります。接続プールは 1 プロセスに 1 つで、以後は使い回します。
- 名前か `url` に `:memory:` を含むとき、または `url` のクエリに `mode=memory` が
  あるときは、メモリ上のデータベースとして扱います。
  `data/memory_2026.db` のようなファイル名は該当しません。

MySQL と PostgreSQL は、サーバが対応していれば暗号化してつなぎます。
**証明書は OS の置き場所を見ません。**

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
等しいかどうかは `where_`、それ以外は `where_op` に分かれています。

```rust
// Laravel: ->where('views', '>', 100)
DB::table("posts").where_op("views", ">", 100)
```

- `where_in` に空の一覧を渡すと、必ず 0 件になります。
- `where_in` に渡せるのは **30,000 件まで**です。超えると終端のメソッドでエラーです。
  多いときは一覧を分けて何回か問い合わせてください。
- `where_raw` の `?` は**値の置き場所としてだけ**使えます。数が合わないとき、
  文字列の中に書いたとき、PostgreSQL で `$1` を書いたときは、終端のメソッドでエラーです。
  `$1` を直に書けるのは `DB::select` / `DB::statement` だけです（下の「SQL を直接書く」）。

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

式で並べたいときは `order_by_raw` です。検査はしません
（下の「式が書けるのは 6 つだけ」）。

```rust
DB::table("posts")
    .order_by_raw("case when pinned then 0 else 1 end asc")
    .order_by_desc("id")
```

### 表をつなぐ

| 書き方                                            | 意味           |
|---------------------------------------------------|----------------|
| `join("users", "users.id", "=", "posts.user_id")` | 結合           |
| `left_join(...)`                                  | 外側結合       |
| `select(&["id", "title"])` / `add_select("body")` | 取る列         |
| `distinct()`                                      | 重なりを捨てる |

`select` は式も書けます（`count(*) as total` など）。検査はしません。

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
  `order_by_desc` は別名で並べられます。

### 結果を取り出す

| 終端              | 返るもの                |
|-------------------|-------------------------|
| `get()`           | `Vec<Row>`              |
| `first()`         | `Option<Row>`           |
| `find(id)`        | `Option<Row>`（id で引く） |
| `value::<T>(col)` | `Option<T>`（1 つの値） |
| `pluck::<T>(col)` | `Vec<T>`（1 列ぶん）    |

```rust
let titles = DB::table("posts").pluck::<String>("title").await?;

let row = DB::table("posts").find(1).await?.unwrap();
let title: String = row.get("title")?;        // 無い列・型違いはエラー
let body: Option<String> = row.get("body")?;  // null を許すとき
let maybe = row.try_get::<i64>("views");      // 読めなければ None
```

読める型は `i64` `i32` `u32` `u64` `f64` `f32` `bool` `String` `Vec<u8>` `Value` と、
その `Option` です（`i8` `i16` `isize` `u8` `u16` `usize` も読めます）。
**幅の狭い型に収まらない値はエラー**です。小数を整数として読むときは、`2.0` は `2` に
なりますが、小数部のある値・`NaN`・無限・`i64` に収まらない値はエラーになります
（小数部が要るときは `f64` で読んでください）。

`Row` はそのまま `json(&rows)` で返せます。`Value` の腕ごとに次の形で出ます。

| `Value` | JSON                                 |
|---------|--------------------------------------|
| `Null`  | `null`                               |
| `Bool`  | `true` / `false`                     |
| `Int`   | 数                                   |
| `Float` | 数（`NaN` と無限は `null`）          |
| `Text`  | 文字列                               |
| `Bytes` | **数の配列**（`[137, 80, 78, ...]`） |

**バイナリの列を含む行は、そのまま `json()` しないでください。**
1 バイトが数 1 つになるので、応答が何倍にもふくらみます。`select` でその列を外すか、
`row.get::<Vec<u8>>(..)` で取り出して自分で組み立ててください。

`value` と `pluck` は **1 列だけ取って、その位置から値を読みます。** 名前では引かないので、
表名を付けた `users.name` や別名付きの式も渡せます。

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
- `exists()` と `doesnt_exist()` は、並び順と件数の指定を外してから数えます。
- `sum` / `avg` / `min` / `max` は `group_by` と併用できません。
  グループごとの値は `select` に式を書いて `get()` します。
- `distinct()` を付けた `sum` は重なりを除いた値を返します。
  渡せるのはただの列名だけです。

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

**`i64` を超える整数は、文字列の列（`string` / `text`）に入れてください。**
`u64` と `usize` で `i64` に収まらない値は文字列として書き込むので、整数の列に入れると
桁が落ちて読み戻しがエラーになります。

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
- `truncate()` は採番も戻します。消したあとの `insert` は `id = 1` から始まります。
- **MySQL の `truncate` はトランザクションを勝手に確定します。**
  巻き戻せる形で空にしたいときは `delete()` を使ってください。
- `update` / `delete` / `truncate` には付けられない指定があります
  （下の「できないこと・エラーになること」）。件数を絞って書き換えたいときは、
  先に主キーを取り出して `where_in` で絞ります。

```rust
let ids: Vec<i64> = DB::table("posts").latest().limit(10).pluck::<i64>("id").await?;
DB::table("posts").where_in("id", &ids).update(&[("status", "archived".into())]).await?;
```

### 数を増やす・減らす

```rust
// views = views + 1 を 1 文で流す（返るのは件数）
let changed = DB::table("posts").where_("id", id).increment("views", 1).await?;
let changed = DB::table("posts").where_("id", id).decrement("stock", 1).await?;
```

**`update` で数を増やしてはいけません。** 読んだ値に 1 を足して書き戻す形
（`update(&[("views", (post.views + 1).into())])`）は、同時に 2 本来ると片方の分が
消えます。`increment` は 1 文で流すので消えません。

`updated_at` は入りません。`join` や `limit` を付けるとエラーです。

### 入らなかった理由を見分ける

一意制約（`unique`）に当たったかどうかを聞けます。

```rust
use bengara::database::is_unique_violation;

if let Err(error) = DB::table("users").insert(&[("email", email.into())]).await {
    if is_unique_violation(&error) {
        return abort_with(422, "そのメールアドレスは登録済みです");
    }
    return Err(error);
}
```

- **エラーの文を自分で読まないでください。** 文は方言ごとに違います。
- 見分けられるのは一意制約だけです。
- **使えるのはトランザクションの外（`DB::table`）だけです。** `tx` の中で 1 本失敗すると、
  以後の問い合わせも `commit()` もエラーになります（下の「使えなくなるとき」）。

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

json(&bengara::serde_json::json!({
    "data": page.data,
    "total": page.total,
    "last_page": page.last_page(),
}))
```

ページ番号は**引数で渡します。** `req.page()` が `?page=` を読みます（無ければ 1）。

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
- `DB::table(...)` はトランザクションの外です。混ぜないでください。
- **1 本ずつ `await` してください。** 同時に 2 本投げるとエラーになります。

### 使えなくなるとき

落ちたトランザクションは**二度と使えません。** `tx.table(...)` で作ったクエリビルダが
生き残っていても、エラーになります。

| とき                                           | 以後の問い合わせ            | 回復                     |
|------------------------------------------------|-----------------------------|--------------------------|
| `commit()` / `rollback()` が済んだ             | エラー                      | しません                 |
| `tx` が落ちた（`commit()` を呼ばなかった）     | エラー                      | しません                 |
| 問い合わせの途中で中断された（`timeout` など） | エラー                      | しません                 |
| 問い合わせがエラーを返した                     | エラー。`commit()` もエラー | しません                 |
| 同じトランザクションに 2 本を同時に投げた      | 2 本目がエラー              | **します**（1 本目が終われば続けられます） |

```
直前の問い合わせが失敗したか中断されたので、このトランザクションは使えません。
中身は巻き戻されます。DB::begin() からやり直してください
```

**1 本でも失敗したら、`DB::begin()` からやり直してください。**
書いたつもりの内容が黙って捨てられないようにしてあります。

トランザクションの中で重なりを避けたいときは、先に `exists()` で確かめるか、
エラーになったら `DB::begin()` からやり直してください。
`is_unique_violation` で書き分ける形は、トランザクションの外だけで使えます。

### モデルを保存するとき

トランザクションの中でモデルを保存するときは、**`save_using(&tx)`** を使います。

```rust
let tx = DB::begin().await?;
let mut post = Post::draft("やきそば");
post.save_using(&tx).await?;
tx.commit().await?;
```

`save()` / `delete()` / `fresh()` は**既定の接続**を使うので、トランザクションの中で
呼ぶと書き込みが外に出ます（`rollback()` しても残ります）。`:memory:` では接続が 1 本
なので、空くのを待って固まります。詳しくは [models.md](models.md) にあります。

## SQL を直接書く

```rust
let rows = DB::select("select * from posts where views > ?", &[100.into()]).await?;
let affected = DB::statement("update posts set views = views + 1", &[]).await?;
```

値の置き場所は**どのドライバでも `?`** で書きます。PostgreSQL では投げる前に
`$1` `$2` … へ振り直します。`?` の数と値の数が合わないときはエラーです。
`Transaction::select` / `statement` も同じです。

**ここだけは `$1` を直に書けます。** PostgreSQL で、`?` を 1 つも書いていないときの
例外です。

```rust
// 通る。? が無く $ があるので素通しする。
DB::statement("update items set price = $1::numeric where id = $2", &["12.5".into(), id.into()]).await?;
```

| 書く所                              | `?`      | `$1` の直書き |
|-------------------------------------|----------|---------------|
| `DB::select` / `DB::statement`      | 使えます | **使えます**  |
| `Transaction::select` / `statement` | 使えます | **使えます**  |
| ビルダーの `*_raw`                  | 使えます | エラー        |

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

**渡した値（`?` の中身）はエラーメッセージに出しません。** SQL が失敗したときは、
文だけを添えて返します。

## 列名と表名は検査します

外から来た文字列が SQL に混ざらないようにするためです。
渡せる列名と表名は**英数字と `_` `.` だけ**で、演算子は許可一覧で照合します。
英数字は Unicode で見るので、日本語の列名（`"題名"`）も通ります。
引用符（`"` と `` ` ``）を含む名前は、式として素通しせず、逃がしてくくります。

エラーが出るタイミングは 2 つに分かれます。

| どこ                                 | いつエラーになるか                 |
|--------------------------------------|------------------------------------|
| 入口・条件・並び・束ね・結合         | **終端のメソッド**（`get()` など） |
| 書き込み（`insert` / `update` など） | 呼んだ**その場で `Err`**           |

### 式が書けるのは 6 つだけ

次の 6 つは、渡した文字列をそのまま SQL に置きます。**検査しません。**
外から来た文字列を渡さないでください。

| 式が書けるもの               | どこに入るか   |
|------------------------------|----------------|
| `select` / `add_select`      | 取る列         |
| `where_raw` / `or_where_raw` | 条件           |
| `having_raw`                 | 束ねた後の条件 |
| `order_by_raw`               | 並び順         |

`where_raw` / `or_where_raw` / `having_raw` / `order_by_raw` の 4 つは、PostgreSQL で
`$` を書くと終端のメソッドでエラーです。値を渡す 3 つは、`?` の数が値の数と合わないときも
エラーです。`select` / `add_select` は値を取らないので、この検査はありません。

### 演算子の一覧は 2 つに分かれています

**値と比べる側は狭いです。** `where_op` / `or_where_op` / `having_op` に渡せるのは
`=` `!=` `<>` `<` `<=` `>` `>=` `like` `not like` `ilike` `not ilike` `is` `is not` だけです。
`in` や `between` はエラーです。`where_in` / `where_between` を使ってください。

列と列を比べる側（`where_column` / `join`）は、これに `in` / `between` / `<=>` を
加えた広い一覧を使います。

## ドライバの違い

**同じコードが 3 つのドライバで動きます。** SQL の引用符・プレースホルダ・列の型名は
bengara が書き分けます。それでも残る違いは次のとおりです。

| こと                     | 残る違い                                                                                   |
|--------------------------|--------------------------------------------------------------------------------------------|
| 日付・日時・JSON の列    | PostgreSQL では **`text` 列**になります（下の「列の型」）                                   |
| `decimal` の読み書き     | 読むと文字列（SQLite だけ小数）、書くときは数。**読んだ値をそのまま書き戻せるのは SQLite と MySQL だけ**です |
| 主キーに文字列を渡す     | PostgreSQL だけエラーです（`bigint = text`）。整数で渡してください                         |
| 真偽の列を `row.value()` で見る | SQLite では `Int`、ほかは `Bool`（下の「読み出した値」）                            |
| 自動採番の番号           | `insert_get_id()` はどれでも使えます。`Affected::last_insert_id` は PostgreSQL では常に `None` |
| 読めない型               | PostgreSQL の `uuid` / `money` / `inet` / 配列 / 範囲 / `interval` と、MySQL で下の「読み出した値」に無い型はエラーです（SQLite ではほぼ起きません） |
| `ilike`                  | PostgreSQL だけの演算子です。ほかでは SQL エラーです                                       |
| `<=>`                    | MySQL だけの演算子です。`where_op` では使えません                                          |
| `truncate()`             | SQLite は `delete` と採番の戻しの 2 文になります。MySQL はトランザクションを確定します     |
| `offset` だけ指定        | 補う `limit` が方言で違います（`limit all` / `limit -1` など）                             |
| 索引を作る・落とす       | MySQL に `create index if not exists` が無いので、文の形が変わります（[migrations.md](migrations.md)） |
| 外部キー                 | MySQL と PostgreSQL では常に効きます。`foreign_keys(false)` は SQLite だけに効きます       |
| `charset` / `collation`  | 指定できません。サーバの既定に従います                                                     |

**SQLite で開発していると、PostgreSQL だけの決まりに気づけません。** 本番と同じドライバ
でもテストを流してください（下の「テスト」）。

### 列の型

`Schema` が書く SQL の型名です（[migrations.md](migrations.md)）。

| 書き方                            | SQLite                              | MySQL                            | PostgreSQL         |
|-----------------------------------|-------------------------------------|----------------------------------|--------------------|
| `id()`                            | `integer primary key autoincrement` | `bigint unsigned auto_increment` | `bigserial`        |
| `integer(..)`                     | `integer`                           | `integer`                        | `integer`          |
| `big_integer(..)`                 | `integer`                           | `bigint`                         | `bigint`           |
| `foreign_id(..)`                  | `integer`                           | `bigint unsigned`                | `bigint`           |
| `string(..)`                      | `varchar(255)`                      | `varchar(255)`                   | `varchar(255)`     |
| `text(..)`                        | `text`                              | `text`                           | `text`             |
| `boolean(..)`                     | `boolean`                           | `tinyint(1)`                     | `boolean`          |
| `float(..)`                       | `real`                              | `float(53)`                      | `double precision` |
| `double(..)`                      | `real`                              | `double precision`               | `double precision` |
| `decimal(.., 8, 2)`               | `numeric(8, 2)`                     | `numeric(8, 2)`                  | `numeric(8, 2)`    |
| `date(..)`                        | `date`                              | `date`                           | **`text`**         |
| `date_time(..)` / `timestamp(..)` | `datetime`                          | `datetime`                       | **`text`**         |
| `json(..)`                        | `text`                              | `json`                           | **`text`**         |
| `binary(..)`                      | `blob`                              | `blob`                           | `bytea`            |
| `uuid(..)`                        | `varchar(36)`                       | `varchar(36)`                    | `varchar(36)`      |

- `float(..)` と `double(..)` は**どのドライバでも 8 バイト**です（Laravel の既定と違います）。
  4 バイトの列が要るときは `raw_column(列, "float")` と書きます。
- `foreign_id(..)` は `id()` と符号がそろう型になります。
- **PostgreSQL で日付と JSON が `text` 列になるのは、bengara が日時を文字列で扱うため**です。
  本物の `timestamp` 列や `json` 列がすでにあるデータベースは、読むぶんには困りません。
  書くときは `cast` を自分で書いてください。

`decimal` の列は、読みと書きで形が違います。

```rust
let price: f64 = row.get("price")?;    // 数として読む
DB::table("items").update(&[("price", 12.5.into())]).await?;   // 数で書く
```

**読んだ値をそのまま書き戻すと、PostgreSQL だけが断ります**
（`column "price" is of type numeric but expression is of type text`）。
`row.get::<f64>(..)` で数として読んでから書いてください。
桁を落とさずに書きたいときは `DB::statement` で `::numeric` を自分で書きます。

### 読み出した値

`row.get::<T>()` は型を合わせて読むので、**どのドライバでも同じ結果になります。**
下の表は `row.value()` で生の `Value` を見たときの腕です。

| 列                    | SQLite      | MySQL   | PostgreSQL |
|-----------------------|-------------|---------|------------|
| 整数                  | `Int`       | `Int`   | `Int`      |
| 小数（float/double）  | `Float`     | `Float` | `Float`    |
| 真偽（boolean）       | **`Int`**   | `Bool`  | `Bool`     |
| `decimal` / `numeric` | **`Float`** | `Text`  | `Text`     |
| 文字列                | `Text`      | `Text`  | `Text`     |
| 日付・日時            | `Text`      | `Text`  | `Text`     |
| JSON                  | `Text`      | `Text`  | `Text`     |
| バイト列              | `Bytes`     | `Bytes` | `Bytes`    |
| `null`                | `Null`      | `Null`  | `Null`     |

- 文字列で返る `decimal` は、末尾の 0 を落とします（`12.50` も `12.5000` も `12.5`）。
- 日付と日時は `YYYY-MM-DD HH:MM:SS` に直して返します。PostgreSQL の `timestamptz` は
  **UTC に直して**返します。
- MySQL の `bigint unsigned` が `i64` に収まらないときは、丸めずに `Text` で返します。
- **照合順序が `_bin` の文字列の列は `Bytes` になります**（MariaDB の `json` 列など）。
  `row.get::<String>()` なら、どちらでも文字列で読めます。
- `Value` に直せない型は、黙って別の値にせずエラーにします。`select` で
  `cast(列 as text) as 列` と書けば読めます。

```
列 `id` の型 `UUID` は読めません。select で文字列に変換して取り出してください（例: `cast(列 as text) as 列`）
```

### 自動採番の番号

`insert_get_id()` は内側で `returning "id"` を付けるので、**どのドライバでも同じように
使えます。** `statement()` で自分で `insert` を書いたときだけ、PostgreSQL では番号が
返りません（`returning` を自分で書いて `select()` で受け取ってください）。

`Affected::last_insert_id` が入るのは、流した文が `insert` か `replace` のときだけです。

| 流した文                  | SQLite   | MySQL    | PostgreSQL |
|---------------------------|----------|----------|------------|
| `insert` / `replace`      | 入ります | 入ります | `None`     |
| `update` / `delete`       | `None`   | `None`   | `None`     |
| 表を作る・変える文（DDL） | `None`   | `None`   | `None`     |

`insert_get_id_as(値, 主キー)` で列の名前を渡せます。**その名前を見るのは
PostgreSQL だけです。**

| ドライバ   | 返るもの                  | 渡した名前の使い道    |
|------------|---------------------------|-----------------------|
| PostgreSQL | `returning 渡した列` の値 | 列名として SQL に入る |
| SQLite     | 直前に入れた行の rowid    | 形を確かめるだけ      |
| MySQL      | `auto_increment` の列の値 | 形を確かめるだけ      |

**自動採番の列を渡しているなら、どのドライバでも正しい値が返ります。**
自動採番でない列を渡すと、SQLite と MySQL では別の値（rowid や 0）が返ります。

### テスト

SQLite はテスト用のデータベースをメモリの上に逃がせます（`DB_TEST_DATABASE=:memory:`）。
MySQL と PostgreSQL は**すでにあるデータベース**を指すので、逃がせません。

**`DB_TEST_DATABASE` に別のデータベース名を必ず書いてください。** 書かないと、
テストの最初につなぐところで止まります。テストは表を全部消して作り直すので
（[testing.md](testing.md)）、開発用のデータベースを守るための確かめです。

**テストの間は、接続名より `refresh_database()` のつなぎ先が優先されます。**
`DB::connection("pgsql")` と書いても、問い合わせと SQL の組み立てはテスト用の 1 本に
そろいます。`to_sql()` も同じです。別のドライバの SQL を確かめたいときは、
そのドライバのテストとして走らせてください。

## できないこと・エラーになること

| 書き方                                                                          | どうなるか                             | 直し方                                   |
|---------------------------------------------------------------------------------|----------------------------------------|------------------------------------------|
| `group_by` の無い `having`                                                      | エラー                                 | 束ねずに絞るなら `where`                 |
| `sum` / `avg` / `min` / `max` に `group_by`                                     | エラー                                 | `select` に式を書いて `get()`            |
| `distinct()` を付けた `sum` などに、式や `*` の列                               | エラー                                 | ただの列名を渡す                         |
| `distinct()` を付けた `count()` / `paginate()` で、数える列が決まらない         | エラー                                 | `select` で列を 1 つ指定する             |
| `having_op` に `select` で付けた別名                                            | PostgreSQL で SQL エラー               | `having_raw` に式を書く                  |
| `update` / `delete` / `truncate` に `join` / `limit` / `offset` / `group_by` / `having` / `distinct` | エラー             | 主キーを取り出して `where_in` で絞る     |
| `update` / `delete` に `order_by`                                               | **黙って無視**                         | 当たる行は変わりません                   |
| `where_` を付けた `truncate()`                                                  | エラー                                 | 絞って消すなら `delete()`                |
| `insert` 系に `where_` / `join` / `limit` など                                  | エラー                                 | 条件は付けられません                     |
| `increment` / `decrement` に `join` / `limit` など                              | エラー                                 | `update` と同じです                      |
| `where_op` / `having_op` に `in` / `between` / `<=>`                            | エラー                                 | `where_in` / `where_between` を使う      |
| `where_op(列, "is", Value::Null)`                                               | `is null` になります                   | そのまま使えます                         |
| `where_raw` の `?` の数と値の数が合わない                                       | 終端のメソッドでエラー                 | 数をそろえる                             |
| ビルダーの `*_raw` に PostgreSQL で `$` を書く                                  | 終端のメソッドでエラー                 | `?` を使う                               |
| 使えない文字を含む列名・表名                                                    | 終端、または呼んだその場               | 上の「列名と表名は検査します」           |
| 幅の狭い型に収まらない値を読む                                                  | 読み出しでエラー                       | 広い型で読む                             |
| `NaN` や範囲外・小数部のある小数を整数として読む                                | 読み出しでエラー                       | `f64` で読む                             |
| `where_in` に 30,000 件を超える一覧                                             | 終端のメソッドでエラー                 | 一覧を分けて問い合わせる                 |
| トランザクションで同時に 2 本投げる                                             | エラー                                 | 1 本ずつ `await` する                    |
| 終わった・落ちたトランザクションで問い合わせる                                  | エラー                                 | `DB::begin()` からやり直す               |
| トランザクションの中で 1 本失敗した後に続ける                                   | 以後の問い合わせと `commit()` がエラー | `DB::begin()` からやり直す               |
| `DB::select` / `DB::statement` の `?` の数と値の数が合わない                    | エラー                                 | PostgreSQL でも `?` で書く               |
| `DB::select` / `DB::statement` に PostgreSQL で `$1` を直書き                   | **そのまま通します**                   | `?` が 1 つも無いときだけの例外です      |
| 機能フラグ無しで `DB::...` を呼ぶ                                               | 直し方を書いたエラー                   | `features = ["sqlite"]` を足す           |

まだ無いものは [backlog.md](backlog.md) にまとめてあります。
こう決めた理由は [decisions.md](decisions.md) にあります。

## 関連

- [migrations.md](migrations.md) — 表を作る
- [models.md](models.md) — 構造体として読み書きする
- [testing.md](testing.md) — DB を使うテスト
- [configuration.md](configuration.md) — `config/*.rs` と `.env`
