# モデル

モデルは、表の 1 行を Rust の構造体として読み書きする仕組みです（Laravel の Eloquent に当たります）。

- 条件の書き方は [database.md](database.md) と同じです。
- 表の作り方は [migrations.md](migrations.md) にあります。

## 書く

`app/Models/` に置きます。

```rust
// app/Models/Post.rs
use bengara::prelude::*;

#[derive(Model, Debug, Clone)]
#[model(table = "posts")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub body: Option<String>,
    pub status: String,
    pub views: i64,
    pub created_at: String,
    pub updated_at: String,
}
```

| 指定                        | 付ける所   | 意味                                                |
|-----------------------------|------------|-----------------------------------------------------|
| `#[model(table = "posts")]` | 構造体     | 表の名前。**必須**です                              |
| `#[model(primary)]`         | フィールド | 主キーにする。無ければ `id` の項目が主キー          |
| `#[model(column = "名前")]` | フィールド | 列名を変える。無ければフィールド名と同じ            |
| `#[model(skip)]`            | フィールド | 表に無い項目にする。読むときは `Default` の値が入る |

- 使える型は、整数・`f32`・`f64`・`bool`・`String`・`Vec<u8>` です。ふだんは `i64` と `String` を使います。
- **NULL を許す列は `Option<...>` にします。** そうしないと、読むときにエラーです。
- **主キーは自動採番の整数です。** `Option` にはできません。
- `r#type` のようなフィールドは、`type` が列名になります。
- 列名の一覧は `Post::COLUMNS` で取れます。

### コンパイルエラーになる書き方

| 書き方                                                   | 理由                                 |
|----------------------------------------------------------|--------------------------------------|
| `#[model(table = "...")]` が無い                         | 表の名前は推測しない                 |
| 表名・列名に使えない文字がある                           | 流すと必ず失敗する                   |
| 主キーが決まらない（`id` も `#[model(primary)]` も無い） | どの行を更新するか決まらない         |
| `#[model(primary)]` が 2 つ以上                          | 主キーが決まらない                   |
| 主キーの型が整数でない                                   | 自動採番の整数だけに対応する         |
| `skip` と `primary` を同じフィールドに付ける             | 表に無い項目は主キーになれない       |
| 同じ列名が 2 回出る                                      | `insert` が失敗する                  |
| 主キー以外の列が無い                                     | `save()` で入れるものが無い          |
| タプル構造体・型引数・ライフタイムがある                 | 名前付きフィールドの構造体だけ使える |

名前に使えるのは英数字（日本語も可）と `_` です。表名には `.`（`スキーマ.表`）も使えます。

## 読む

```rust
let post = Post::find(1).await?;              // Option<Post>
let post = Post::find_or_fail(1).await?;      // 無ければ 404 のエラー
let posts = Post::all().await?;               // Vec<Post>
let total = Post::count().await?;             // i64

let posts = Post::query()
    .where_("status", "published")
    .latest()
    .limit(10)
    .get()
    .await?;
```

`find_or_fail` のエラーは、ハンドラから `?` で返すだけで 404 の応答になります。

### 条件と結果

`Post::query()` の条件の足し方は、[database.md](database.md) と同じです。
結果がモデルの型で返ります。

| 呼ぶもの                                | 返るもの                                                         |
|-----------------------------------------|------------------------------------------------------------------|
| `get()`                                 | `Vec<Post>`                                                      |
| `first()`                               | `Option<Post>`                                                   |
| `first_or_fail()`                       | `Post`（無ければ 404）                                           |
| `find(id)`                              | `Option<Post>`                                                   |
| `count()`                               | `i64`                                                            |
| `exists()` / `doesnt_exist()`           | `bool`                                                           |
| `sum` / `avg` / `min` / `max::<T>(col)` | `Option<T>`                                                      |
| `pluck::<T>(col)` / `value::<T>(col)`   | `Vec<T>` / `Option<T>`                                           |
| `update(&[...])` / `delete()`           | `u64`（件数）                                                    |
| `paginate(per_page, page)`              | `Paginator<Post>`（[database.md](database.md) の「ページ分け」） |

- **`get()` や `first()` は全部の列を読みます。** `select` で列を絞ると、足りない列でエラーです。
- 一部の列だけ欲しいときは、`pluck` か下の `query_builder()` を使います。

### クエリビルダに降りる

`join` や `increment` を使うときは、`query_builder()` を呼びます。
降りると、結果はモデルでなく `Row` で返ります。

```rust
let rows = Post::query()
    .where_("status", "published")
    .query_builder()
    .join("users", "users.id", "=", "posts.user_id")
    .select(&["posts.title", "users.name"])
    .get()
    .await?;        // Vec<Row>
```

## 保存する

```rust
let mut post = Post::draft("やきそば");   // 主キーが 0 なら「まだ保存していない」
post.save().await?;                       // insert され、post.id に ID が入る

post.title = "焼きそば".to_string();
post.save().await?;                       // update になる

let fresh = post.fresh().await?;          // Option<Post>（読み直す）
post.delete().await?;                     // 返るのは件数
```

- 主キーが `0` なら新しい行を入れて、採番された値を主キーに入れます。
- 主キーが `0` 以外なら、その行を更新します。
- `Post::draft` は自分で用意する関数です（下の「書きやすくする」）。

### 変える列だけ書く（`save_only`）

**`save()` は、構造体の値を全部書き戻します**（`created_at` を除く）。
読んでから保存するまでに他の人が変えた列は、元に戻ってしまいます。
変える列が決まっているなら、`save_only(&[列, ..])` を使います。

```rust
let mut post = Post::find_or_fail(id).await?;
post.title = "新しい題名".to_string();
if post.save_only(&["title"]).await? == 0 {
    // 読んだ後で行が消えた。
    return Err(Error::http(404, "記事が見つかりません"));
}
```

閲覧数のように `increment` で増やす列がある表では、必ずこの形にします。

| 項目               | `save()`                         | `save_only(&[..])`                  |
|--------------------|----------------------------------|-------------------------------------|
| 書く列             | 主キーと `created_at` 以外の全部 | 指定した列と `updated_at`（あれば） |
| 新しい行           | 入れる                           | **エラー**（入れない）              |
| 当てはまる行が無い | **エラー**                       | **`Ok(0)`**                         |
| 返るもの           | `()`                             | 当てはまった行の数（`u64`）         |

- 次のときはエラーです。SQL は流しません。
    - 列を 1 つも指定していない
    - モデルに無い列か、主キーの列を指定した
    - まだ保存していない行（主キーが `0`）
- 持ち主や状態でも絞りたいときは、クエリビルダの `update` を使います。`updated_at` は自分で入れます。

### `save()` がエラーになるとき

| とき                               | 直し方                               |
|------------------------------------|--------------------------------------|
| 更新した行が 0 件だった            | 行が消えていないか、主キーを確かめる |
| 採番された値を主キーに入れられない | 主キーの型を `i64` などにする        |
| 採番された値が返らなかった         | 主キーが自動採番の列か確かめる       |

- 下の 2 つは「行は入ったのに主キーが空」です。呼び直すと 2 行目が入るので、エラーにしています。
- 主キーに値を入れて `save()` すると、更新になります。主キーを自分で決める行は `DB::table(..).insert(..)` で入れます。
- まだ保存していないモデルの `delete()` は、何もせずに 0 を返します。

### `created_at` と `updated_at`

その列名のフィールドがあると、`save()` が `now()` の時刻を入れます。

| とき     | `created_at` | `updated_at` |
|----------|--------------|--------------|
| 新しい行 | 入る         | 入る         |
| 更新     | 書き戻さない | 入る         |

**`DB::table(...)` で直接入れた行には、時刻が入りません。**
`timestamps()` の列は NULL を許すので、`String` のフィールドで読むとエラーです。
`now()` を自分で入れるか、フィールドを `Option<String>` にします。

### 書きやすくする

毎回フィールドを全部書かずに済むよう、`impl` に関数を用意します。

```rust
impl Post {
    pub fn draft(title: impl Into<String>) -> Self {
        Self {
            id: 0,
            title: title.into(),
            body: None,
            status: "draft".to_string(),
            views: 0,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }
}
```

## 主キーに文字列を渡したとき

パス引数は文字列で届きます。
`Post::find("12")` のように、**整数に読める文字列は整数に直して**探します。
整数に読めない値（`"abc"`）は、SQL エラーにせず「無い」とします。

- `Post::find("abc")` と `req.auth().user::<User>()` は `None` です。
- `Post::find_or_fail("abc")` と `req.model::<Post>("post")` は 404 です。
- `where_("id", "abc")` は 0 件、`where_op("id", "!=", "abc")` は全部当たります。
- `where_in` に混ざった `"abc"` は、一覧から外します。
- 直すのは主キーの列だけです。

## よく使う条件をまとめる

Laravel のスコープに当たるものは、ただのメソッドで書きます。

```rust
impl Post {
    /// 公開済みを新しい順に。
    pub fn published() -> ModelQuery<Post> {
        Post::query()
            .where_("status", "published")
            .latest()
            .order_by_desc("id")   // 並びが一意になるように
    }
}

let posts = Post::published().limit(10).get().await?;
```

## リレーション

リレーション（表と表のつながり）も、メソッドで書きます。

```rust
impl Post {
    /// この記事のコメント（Laravel の hasMany）
    pub fn comments(&self) -> ModelQuery<Comment> {
        Comment::query().where_("post_id", self.id).oldest()
    }
}

impl Comment {
    /// このコメントが付いた記事（Laravel の belongsTo）
    pub fn post(&self) -> ModelQuery<Post> {
        Post::query().where_("id", self.post_id)
    }
}

let comments = post.comments().get().await?;
let owner = comment.post().first().await?;
```

問い合わせが流れるのは、`await` を書いた所だけです。

### まとめて読む

Laravel の `with('comments')` に当たるものは、`where_in` で引いて `group_by` で束ねます。
記事が何件あっても、問い合わせは 2 回です。

```rust
let posts = Post::published().limit(10).get().await?;
let ids: Vec<i64> = posts.iter().map(|p| p.id).collect();

let comments = Comment::query().where_in("post_id", &ids).get().await?;
let by_post = bengara::database::group_by(comments, |c| c.post_id);

for post in &posts {
    let mine = by_post.get(&post.id).map(Vec::as_slice).unwrap_or(&[]);
    // ...
}
```

## トランザクションの中で使う

トランザクションの中では、`tx` を渡す形を使います。

| ふだん                  | トランザクションの中               |
|-------------------------|------------------------------------|
| `post.save()`           | `post.save_using(&tx)`             |
| `post.save_only(&[..])` | `post.save_only_using(&tx, &[..])` |
| `post.delete()`         | `post.delete_using(&tx)`           |
| `post.fresh()`          | `post.fresh_using(&tx)`            |
| `Post::query()`         | `Post::on(&tx)`                    |
| 組み立て済みのクエリ    | `query.using(&tx)`                 |

```rust
let tx = DB::begin().await?;
let posts = Post::on(&tx).where_("status", "draft").get().await?;
let mut post = Post::draft("やきそば");
post.save_using(&tx).await?;
tx.commit().await?;
```

- **`save()` や `Post::find()` は、トランザクションの外で流れます。** 取り消しても残ります。
- メモリ上の SQLite は接続が 1 本です。中から `save()` を呼ぶと、待ち続けて時間切れになります。

## 文字列の主キー

UUID のように自分で決める主キーの行は、クエリビルダで入れます。

```rust
DB::table("sessions")
    .insert(&[("id", key.into()), ("payload", body.into())])
    .await?;
```

- `bengara::database::Model` を自分で実装すると、読む・更新・削除はモデルで書けます。
- **そのときは `const INTEGER_KEY: bool = false;` を必ず書きます。** 書かないと `"a1"` のような主キーが見つかりません。

## ソフトデリート

ソフトデリートは、行を消す代わりに `deleted_at` に印を付ける方法です。
列は `t.soft_deletes()` で作れます。
`post.delete()` は行を消し、`Post::all()` は印の付いた行も返すので、絞り込みは自分で書きます。

```rust
impl Post {
    /// 消していないものだけ。
    pub fn alive() -> ModelQuery<Post> {
        Post::query().where_null("deleted_at")
    }
}

// 消す代わりに印を付ける。
DB::table("posts").where_("id", id).update(&[("deleted_at", now().into())]).await?;
```

## シーダーとファクトリ

シーダーは、最初のデータを入れる関数です。`database/seeders/` に置きます。

```rust
// database/seeders/DatabaseSeeder.rs
use bengara::prelude::*;

use crate::app::models::Post;

pub async fn run() -> Result<()> {
    let mut post = Post::draft("最初の記事");
    post.status = "published".to_string();
    post.save().await?;
    Ok(())
}
```

- ファイル名は大文字で始めます。中に `pub async fn run() -> Result<()>` を書きます。
- `cargo artisan db:seed` で `DatabaseSeeder` が動きます。無ければ、全部のシーダーを名前順に動かします。
- `--class=PostSeeder` で 1 本だけ動かせます。
- ほかのシーダーは、関数として呼びます（`crate::database::seeders::post_seeder::run().await?`）。

ファクトリ（テスト用のデータを作る関数）は `database/factories/` に、ただの関数として置きます。

```rust
// database/factories/PostFactory.rs
use crate::app::models::Post;

pub fn make(i: i64) -> Post {
    let mut post = Post::draft(format!("記事 {i}"));
    post.views = i * 10;
    post
}
```

## 関連

まだ無い機能は [backlog.md](backlog.md) にあります。

- [database.md](database.md) — 接続とクエリビルダ
- [migrations.md](migrations.md) — 表を作る
- [testing.md](testing.md) — DB を使うテスト
- [laravel-differences.md](laravel-differences.md) — Laravel と違う点
