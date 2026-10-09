# データベース

接続の設定と、問い合わせの組み立て（`DB::table(...)`）、トランザクションの説明です。
表の作り方は [migrations.md](migrations.md)、構造体として読み書きする方法は
[models.md](models.md) にあります。

## 使えるようにする

データベースは **機能フラグ**です。`Cargo.toml` に、使うものだけを書きます。

```toml
[dependencies]
bengara = { version = "0.1", features = ["sqlite"] }
```

| データベース    | フラグ                  | `DB_CONNECTION` に書く名前 |
|-----------------|-------------------------|----------------------------|
| SQLite          | `sqlite`                | `sqlite`                   |
| MySQL           | `mysql`                 | `mysql`                    |
| MariaDB         | `mariadb`（`mysql` と同じもの） | `mariadb`          |
| PostgreSQL      | `postgres`              | `pgsql`                    |

`DB_CONNECTION` の名前は `config/database.rs` が返す接続の名前です。
`cargo run -- init` が作る `config/database.rs` には、上の 4 つが入っています。

下回りは sqlx 0.8 ですが、その型は外に出ません。

**既定ではどのドライバも入りません。** `cargo run -- init` が作る `Cargo.toml` には
`features = ["sqlite"]` が書かれます。 **データベースを使わないプロジェクトでは、
その 1 語（または `features` の行ごと）を消してください。** 消しても他の機能は動きます。

フラグが無いままでもコンパイルは通ります。
`DB::...` を呼んだときに、直し方を書いたエラーが返ります。

```
mysql につなぐには Cargo.toml を `bengara = { version = "0.1", features = ["mysql"] }` にしてください
```

2 つ以上を同時に入れてもかまいません。`DB::connection("名前")` で使い分けられます。

```toml
bengara = { version = "0.1", features = ["sqlite", "postgres"] }
```

### ビルドに要るもの

`mysql` と `postgres` を入れると、通信を暗号化するために C コンパイラが要ります
（rustls が使う `ring`）。`sqlite` だけなら要りません。

| OS      | 用意するもの                      |
|---------|-----------------------------------|
| Linux   | `cc`（`build-essential` など）    |
| macOS   | Xcode Command Line Tools          |
| Windows | Visual Studio の C++ ビルドツール |

> Rust 1.85（最低対応版）で使うときは、依存を 1 つ固定してください。
> `cargo update idna_adapter --precise 1.2.0`（[getting-started.md](getting-started.md)）

## 設定

`.env` に書きます。

SQLite のとき。

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

# **テスト用に別のデータベースを用意してください**（下の「テスト」）
DB_TEST_DATABASE=myapp_test
```

見る環境変数は次のとおりです。

| 環境変数             | 既定                          | 使うドライバ   |
|----------------------|-------------------------------|----------------|
| `DB_CONNECTION`      | `sqlite`                      | すべて         |
| `DB_DATABASE`        | SQLite は `database/database.sqlite`、ほかは `bengara` | すべて |
| `DB_URL`             | 空                            | すべて         |
| `DB_HOST`            | `127.0.0.1`                   | MySQL / pgsql  |
| `DB_PORT`            | 3306 / 5432                   | MySQL / pgsql  |
| `DB_USERNAME`        | `root` / `postgres`           | MySQL / pgsql  |
| `DB_PASSWORD`        | 空（渡しません）              | MySQL / pgsql  |
| `DB_MAX_CONNECTIONS` | 5                             | すべて         |
| `DB_TEST_DATABASE`   | SQLite は `:memory:`、ほかは無し | すべて      |

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
            // MySQL / MariaDB。ホストと利用者名は書かなければ 127.0.0.1 / root です。
            ConnectionConfig::mysql("mysql", env::<String>("DB_DATABASE", "myapp"))
                .host(env::<String>("DB_HOST", "127.0.0.1"))
                .username(env::<String>("DB_USERNAME", "root"))
                .password(env::<String>("DB_PASSWORD", "")),
            // PostgreSQL。
            ConnectionConfig::postgres("pgsql", env::<String>("DB_DATABASE", "myapp"))
                .host(env::<String>("DB_HOST", "127.0.0.1"))
                .username(env::<String>("DB_USERNAME", "postgres"))
                .password(env::<String>("DB_PASSWORD", "")),
        ],
    }
}
```

| 作り方                        | ポートの既定 | 利用者名の既定 |
|-------------------------------|--------------|----------------|
| `ConnectionConfig::sqlite`    | –            | –              |
| `ConnectionConfig::mysql`     | 3306         | `root`         |
| `ConnectionConfig::postgres`  | 5432         | `postgres`     |

| 指定                 | 既定        | 意味                                                     |
|----------------------|-------------|----------------------------------------------------------|
| `host(s)`            | `127.0.0.1` | つなぎ先。**空の文字列は無視します**                     |
| `port(n)`            | 3306 / 5432 | ポート。**0 は無視します**（既定のまま）                 |
| `username(s)`        | 上の表      | 利用者名。**空の文字列は無視します**                     |
| `password(s)`        | 空          | パスワード。空のままなら渡しません                       |
| `max_connections(n)` | 5           | 同時に張る接続の数。`:memory:` のときは 1 に固定されます |
| `foreign_keys(b)`    | 真          | SQLite で外部キーの制約を効かせるか（ほかでは常に効きます） |
| `url(s)`             | 空          | 接続文字列を直接書く。書くと他の値より優先します         |

`url(s)` の形は sqlx と同じです。

```
sqlite://database/database.sqlite
mysql://利用者名:パスワード@127.0.0.1:3306/データベース名
postgres://利用者名:パスワード@127.0.0.1:5432/データベース名
```

気をつけること。

- SQLite のファイルは **無ければ作ります。** 置くディレクトリは先に用意してください。
  MySQL と PostgreSQL では、**データベースは先に作っておいてください。** 作りません。
- 相対パスはプロジェクト直下（`base_path()`）から見ます。
- 接続は **最初に使ったときに**張ります。起動は速いままです。
- メモリ上のデータベースかどうかは、**`:memory:` を含むかどうか**だけで決めます。
  `data/memory_2026.db` のように `memory` という語を含むだけのファイル名は、
  メモリ扱いになりません（接続 1 本に固定されず、ファイル向けの設定が使われます）。
- MySQL と PostgreSQL は、サーバが暗号化に対応していれば暗号化してつなぎます
  （sqlx の既定。MySQL は `PREFERRED`、PostgreSQL は `prefer`）。
  **証明書は OS の置き場所を見ません**（rustls + webpki-roots）。

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

`order_by_raw` は引用も検査もしません（下の「式が書けるのは 6 つだけ」）。

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
    .join("users", "users.id", "=", "posts.user_id")
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

**`group_by` の無い `having` はエラーです。** 束ねていないと、絞る相手が決まりません。
束ねずに絞りたいときは `where` を使ってください。

**`having_op` に渡せるのは実在の列だけです。**
`select` で付けた別名（`count(*) as total` の `total`）は渡せません。
PostgreSQL は `having` で別名を解決しないので、`column "total" does not exist` になります。
SQLite と MySQL では通るので、気づかないまま移すと落ちます。

集計の結果で絞るときは、別名ではなく式を `having_raw` に書いてください
（上の例の `count(*) > ?`）。`order_by_desc` は別名で並べられます。

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

小数の列を整数として読むときは、次のようになります。

| 値                        | どうなるか                      |
|---------------------------|---------------------------------|
| `2.9` / `-2.9`            | `2` / `-2`（0 のほうへ落とす）  |
| `NaN` / 無限              | **エラー**                      |
| `i64` に収まらない大きさ  | **エラー**                      |

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

`paginate` の `total` も同じ数え方です。どちらも 3 つのドライバで同じ数が返ります。

**`distinct()` を付けて数えるときは、`select` で列を 1 つ指定してください。**
数える列が決まらないとエラーになります（指定が無い・2 つ以上ある・式を書いた、のどれか）。

```rust
let authors = DB::table("posts")
    .distinct()
    .select(&["author_id"])
    .count()
    .await?;
```

`exists()` と `doesnt_exist()` は、**並び順と件数の指定を外して**から数えます。
有無を見るだけなので、`latest()` が付いていても並べ替えは走りません。

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

// 全部消す（自動採番も 1 に戻る）
DB::table("posts").truncate().await?;
```

`truncate()` は **採番も戻します。** 消したあとの `insert` は `id = 1` から始まります。
SQLite には `truncate` 文が無いので、条件なしの `delete` と採番の記録の削除を
続けて流しています。`truncate table` を持つ方言と同じ結果にそろえるためです。

**条件を書かないと全行が対象です。** Laravel と同じです。

`update` / `delete` / `truncate` には、付けられない指定があります。
下の「できないこと・エラーになること」を見てください。
件数を絞って書き換えたいときは、先に主キーを取り出して `where_in` で絞ります。

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

**`update` で数を増やしてはいけません。**

```rust
// 駄目な書き方
DB::table("posts").where_("id", id).update(&[("views", (post.views + 1).into())]).await?;
```

読んだ値に 1 を足して書き戻す形なので、**同時に 2 本来ると片方の分が消えます。**
`increment` は `set "views" = "views" + ?` を 1 文で流すので消えません。

- `updated_at` は入れません（`update` と同じです）。
- `join` や `limit` を付けるとエラーです（`update` と同じ理由）。
- Laravel の「ほかの列も一緒に更新する」引数はありません。

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

**エラーの文を自分で読まないでください。** 文は方言ごとに違います。
いまはこの 1 つだけです。ほかの種類（外部キー違反など）はまだ見分けられません。

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

### 使えなくなるとき

落ちたトランザクションは、**二度と使えません。**
`tx.table(...)` で作ったクエリビルダが生き残っていても、エラーになります。

| とき                                             | 以後の問い合わせ |
|--------------------------------------------------|------------------|
| `commit()` / `rollback()` が済んだ               | エラー           |
| `tx` が落ちた（`commit()` を呼ばなかった）        | エラー           |
| 問い合わせの途中で中断された（`timeout` など）   | エラー           |

```
問い合わせが中断されたので、このトランザクションは使えません。
中身は巻き戻されます。DB::begin() からやり直してください
```

書いたつもりの内容が、黙って捨てられないようにしてあります。

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
| 集計・取り出し | `sum` / `avg` / `min` / `max` / `pluck` / `value` の列名                                                                                                                          |
| 書き込み      | `insert` / `insert_many` / `insert_get_id_as` / `update` / `increment` / `decrement` の列名と、`insert_get_id_as` の主キー名                                                       |

エラーが出るタイミングは 2 つに分かれます。

| どこ                                 | いつエラーになるか                 |
|--------------------------------------|------------------------------------|
| 入口・条件・並び・束ね・結合         | **終端のメソッド**（`get()` など） |
| 書き込み（`insert` / `update` など） | 呼んだ **その場で `Err`**          |

細かい決まりは次の 2 つです。

- **英数字は Unicode で見ます。** 日本語の列名（`"題名"`）もそのまま通ります。
  `"` `;` `(` `-` と空白は Unicode でも英数字ではないので、検査の強さは変わりません。
- **引用符（`"` と `` ` ``）を含む名前は、式として素通ししません。**
  `""` と ``` `` ``` に逃がしてくくります。引用符を 1 つ混ぜて検査を抜ける道をふさぐためです。

### 式が書けるのは 6 つだけ

次の 6 つは、渡した文字列を **そのまま SQL に置きます。検査しません。**
外から来た文字列を渡さないでください。

| 式が書けるもの               | どこに入るか   |
|------------------------------|----------------|
| `select` / `add_select`      | 取る列         |
| `where_raw` / `or_where_raw` | 条件           |
| `having_raw`                 | 束ねた後の条件 |
| `order_by_raw`               | 並び順         |

`*_raw` の `?` の決まりは、上の「条件を足す」にある `where_raw` と同じです。

### 演算子の一覧は 2 つに分かれています

**値と比べる側は狭いです。** `where_op` / `or_where_op` / `having_op` に渡せるのは
`=` `!=` `<>` `<` `<=` `>` `>=` `like` `not like` `ilike` `not ilike` `is` `is not` だけです。

`in` や `between` を渡すとエラーになります。値 1 つでは SQL が組み立たないからです。
`where_in` / `where_between` を使ってください。

列と列を比べる側（`where_column` / `join`）は、これに `in` / `between` / `<=>` を加えた
広い一覧を使います。

## ドライバの違い

**同じコードが 3 つのドライバで動きます。** SQL の引用符・プレースホルダ・列の型名は
bengara が書き分けます。それでも残る違いをここにまとめます。

### 列の型

`Schema` が書く SQL の型名です（[migrations.md](migrations.md)）。

| 書き方                 | SQLite                      | MySQL                       | PostgreSQL         |
|------------------------|-----------------------------|-----------------------------|--------------------|
| `id()`                 | `integer primary key autoincrement` | `bigint unsigned auto_increment` | `bigserial` |
| `integer(..)`          | `integer`                   | `integer`                   | `integer`          |
| `big_integer(..)`      | `integer`                   | `bigint`                    | `bigint`           |
| `foreign_id(..)`       | `integer`                   | `bigint unsigned`           | `bigint`           |
| `string(..)`           | `varchar(255)`              | `varchar(255)`              | `varchar(255)`     |
| `text(..)`             | `text`                      | `text`                      | `text`             |
| `boolean(..)`          | `boolean`                   | `tinyint(1)`                | `boolean`          |
| `float(..)`            | `real`                      | `float(53)`                 | `double precision` |
| `double(..)`           | `real`                      | `double precision`          | `double precision` |
| `decimal(.., 8, 2)`    | `numeric(8, 2)`             | `numeric(8, 2)`             | `numeric(8, 2)`    |
| `date(..)`             | `date`                      | `date`                      | **`text`**         |
| `date_time(..)` / `timestamp(..)` | `datetime`       | `datetime`                  | **`text`**         |
| `json(..)`             | `text`                      | `json`                      | **`text`**         |
| `binary(..)`           | `blob`                      | `blob`                      | `bytea`            |
| `uuid(..)`             | `varchar(36)`               | `varchar(36)`               | `varchar(36)`      |

`float(..)` と `double(..)` は **どのドライバでも 8 バイト**です。
MySQL の `float` は 4 バイトなので、精度 53 を付けて `double` と同じ幅にそろえています
（ここは Laravel の既定と違います）。4 バイトの列が要るときは
`raw_column(列, "float")` と書きます。

`foreign_id(..)` は `id()` と符号がそろう型にします。
MySQL は符号の違う列への外部キーを断るためです。

**PostgreSQL では日付と JSON が `text` 列になります。** bengara は日時を文字列で
扱うので（上の「日時」）、`insert` に渡すのも文字列です。PostgreSQL は値を渡す場所の
型に厳しく、`$1` が文字列のままでは `timestamp` 列に入れさせてくれません。

```
column "created_at" is of type timestamp without time zone but expression is of type text
```

SQLite と MySQL は文字列を日付として受け取るので、本来の型のままにしています。

本物の `timestamp` 列や `json` 列が**すでにある**データベースにつなぐときは、
**読む**ぶんには困りません（下の表）。書くときは `cast` を自分で書いてください。

### 読み出した値

`row.get::<T>()` は型を合わせて読むので、**どのドライバでも同じ結果になります。**
下の表は `row.value()` で生の `Value` を見たときの腕です。

| 列                    | SQLite        | MySQL         | PostgreSQL    |
|-----------------------|---------------|---------------|---------------|
| 整数                  | `Int`         | `Int`         | `Int`         |
| 小数（float/double）  | `Float`       | `Float`       | `Float`       |
| 真偽（boolean）       | **`Int`**     | `Bool`        | `Bool`        |
| `decimal` / `numeric` | **`Float`**   | `Text`        | `Text`        |
| 文字列                | `Text`        | `Text`        | `Text`        |
| 日付・日時            | `Text`        | `Text`        | `Text`        |
| JSON                  | `Text`        | `Text`        | `Text`        |
| バイト列              | `Bytes`       | `Bytes`       | `Bytes`       |
| `null`                | `Null`        | `Null`        | `Null`        |

- **`decimal` は MySQL と PostgreSQL では文字列で返します。** 小数に直すと桁が
  落ちるためです。SQLite には小数の型が無いので `Float` になります。
- 文字列の形は 2 つのドライバでそろえます。末尾の 0 は落とします
  （`12.50` も `12.5000` も `12.5`）。
- 日付と日時は `YYYY-MM-DD HH:MM:SS` に直して返します。`now()` と同じ形です。
  PostgreSQL の `timestamptz` は **UTC に直して**返します。
- MySQL の `bigint unsigned` が `i64` に収まらないときは、丸めずに `Text` で返します。
- **照合順序が `_bin` の文字列の列は `Bytes` になります。** MySQL の通信の決まりでは
  「バイナリ」と伝わるためです。MariaDB の `json` 列がこれに当たります
  （MariaDB の `json` は `utf8mb4_bin` の `longtext` です）。
  `row.get::<String>()` なら、どちらでも文字列で読めます。

### 読めない列

`Value` に直せない型は、**黙って別の値にせず**エラーにします。

```
列 `id` の型 `UUID` は読めません。select で文字列に変換して取り出してください（例: `cast(列 as text) as 列`）
```

| ドライバ   | 読めない型の例                                          |
|------------|---------------------------------------------------------|
| PostgreSQL | `uuid` / `money` / `inet` / 配列 / 範囲（range） / `interval` |
| MySQL      | 上の表に無い型                                          |
| SQLite     | ほぼ起きません（値ごとに型が決まるため）                |

`select` で `cast(列 as text) as 列` と書けば読めます。

### 自動採番の番号

| 書き方                      | SQLite / MySQL        | PostgreSQL                       |
|-----------------------------|-----------------------|----------------------------------|
| `insert_get_id()`           | **使えます**          | **使えます**（`returning` を付けます） |
| `Affected::last_insert_id`  | 入ります              | **いつも `None`**                |

PostgreSQL には「最後に入れた行の番号」を返す仕組みがありません。
`insert_get_id()` は内側で `returning "id"` を付けるので、どのドライバでも同じように
使えます。 **`statement()` で自分で `insert` を書いたときだけ**、PostgreSQL では
番号が返りません。`returning` を自分で書いて `select()` で受け取ってください。

`insert_get_id_as(値, 主キー)` で列の名前を渡せます。
**その名前を見るのは PostgreSQL だけです。**

| ドライバ   | 返るもの                    | 渡した名前の使い道 |
|------------|-----------------------------|--------------------|
| PostgreSQL | `returning 渡した列` の値   | 列名として SQL に入る |
| SQLite     | 直前に入れた行の rowid      | 形を確かめるだけ   |
| MySQL      | `auto_increment` の列の値   | 形を確かめるだけ   |

SQLite の `integer primary key` は rowid の別名、MySQL の `auto_increment` は表に 1 つなので、
**自動採番の列を渡しているなら、どのドライバでも正しい値が返ります。**
自動採番でない列を渡すと、SQLite と MySQL では別の値（rowid や 0）が返ります。

### そのほか

| こと                   | 内容                                                                 |
|------------------------|----------------------------------------------------------------------|
| `truncate()`           | SQLite には `truncate` が無いので、条件なしの `delete` と採番の戻しの 2 文になります |
| `offset` だけ指定      | 補う `limit` が方言で違います（`limit all` / `limit -1` など）        |
| `ilike`                | PostgreSQL だけの演算子です。ほかのドライバでは SQL エラーになります |
| `<=>`                  | MySQL だけの演算子です。`where_op`（値 1 つ）では使えません          |
| 索引                   | MySQL には `create index if not exists` が無いので、`create_if_not_exists` のときは表の定義の中に書きます |
| 外部キー               | MySQL と PostgreSQL では常に効きます。`foreign_keys(false)` は SQLite だけに効きます |

### テスト

SQLite はテスト用のデータベースをメモリの上に逃がせます（`DB_TEST_DATABASE=:memory:`）。
MySQL と PostgreSQL は**すでにあるデータベース**を指すので、逃がせません。

**`DB_TEST_DATABASE` に別のデータベース名を必ず書いてください。**
書かないと、テストの最初につなぐところで止まります。

```
mysql のテストには DB_TEST_DATABASE が必要です。テスト用のデータベースを別に作って、
その名前を .env の DB_TEST_DATABASE に書いてください（テストは表を全部消すので、開発用のデータベースには触りません）
```

テストは表を全部消して作り直します（[testing.md](testing.md)）。
開発用のデータベースを指すと中身が消えるので、この確かめを入れています。

## できないこと・エラーになること

| 書き方                                                               | どうなるか                   | 理由                                             |
|----------------------------------------------------------------------|------------------------------|--------------------------------------------------|
| `group_by` の無い `having`                                           | エラー                       | 絞る相手が決まりません                           |
| `sum` / `avg` / `min` / `max` に `group_by`                          | エラー                       | `select` に式を書いて `get()` してください       |
| `distinct()` を付けた `sum` などに、式や `*` の列                    | エラー                       | 重なりを除けるのはただの列名のときだけです       |
| `distinct()` を付けた `count()` / `paginate()` で、数える列が決まらない | エラー                    | `select` で列を 1 つ指定してください             |
| `having_op` に `select` で付けた別名                                 | PostgreSQL で SQL エラー     | `having_raw` に式を書いてください                |
| `update` / `delete` に `join`                                        | エラー                       | SQL に入らず、黙って全件に当たります             |
| `update` / `delete` に `limit` / `offset`                            | エラー                       | 同じ理由です                                     |
| `update` / `delete` に `group_by` / `having` / `distinct`            | エラー                       | 同じ理由です                                     |
| `update` / `delete` に `order_by`                                    | **黙って無視**               | SQL に入らなくても、当たる行が変わりません       |
| `where_` を付けた `truncate()`                                       | エラー                       | 絞って消すなら `delete()` を使います             |
| `truncate()` に `join` / `limit` / `offset` / `group_by` / `having` / `distinct` | エラー            | `update` / `delete` と同じです                   |
| `insert` 系に `where_` / `join` / `limit` など                       | エラー                       | SQL に入らず、書いたつもりの条件が消えます       |
| `increment` / `decrement` に `join` / `limit` など                   | エラー                       | `update` と同じです                              |
| `where_op` / `having_op` に `in` / `between` / `<=>`                 | エラー                       | 値 1 つでは組み立ちません。`where_in` などを使います |
| `where_raw` の `?` の数と値の数が合わない                            | 終端のメソッドでエラー       |                                                  |
| 使えない文字を含む列名・表名                                         | 終端、または呼んだその場     | 上の「列名と表名は検査します」を参照             |
| 幅の狭い型に収まらない値を読む                                       | 読み出しでエラー             | `f32` に収まらない小数も同じです                 |
| トランザクションで同時に 2 本投げる                                  | エラー                       | 1 本ずつ `await` してください                    |
| 終わった・落ちたトランザクションで問い合わせる                       | エラー                       | `DB::begin()` からやり直してください             |
| `NaN` や範囲外の小数を整数として読む                                 | 読み出しでエラー             | 小数は 0 のほうへ落として整数にします            |
| 機能フラグ無しで `DB::...` を呼ぶ                                    | 直し方を書いたエラー         | `features = ["sqlite"]` を足してください         |

## 気をつけること

| こと                         | 内容                                                                              |
|------------------------------|-----------------------------------------------------------------------------------|
| 列名に外から来た値を入れない | 値（`where_` の第 2 引数）は必ずプレースホルダです。式が書ける 6 つは検査しません |
| `:memory:` は接続 1 本       | メモリ上のデータベースは接続ごとに別物になるため、1 本に固定します               |
| 1 プロセスで 1 つの接続プール | 既定の接続は最初の 1 回だけ作られ、以後は使い回します                            |

## 関連

- [migrations.md](migrations.md) — 表を作る
- [models.md](models.md) — 構造体として読み書きする
- [testing.md](testing.md) — DB を使うテスト
- [configuration.md](configuration.md) — `config/*.rs` と `.env`
