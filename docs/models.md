# モデル

このページで分かること。

- 表の行を Rust の構造体として読み書きする方法（Laravel の Eloquent に当たります）
- リレーション・トランザクション・シーダーの書き方
- `save()` の動きと、エラーになる場面

条件の書き方は [database.md](database.md) と同じです。表の作り方は
[migrations.md](migrations.md) にあります。

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

| 指定                        | 置き場所   | 意味                                               |
|-----------------------------|------------|----------------------------------------------------|
| `#[model(table = "posts")]` | 構造体     | 表の名前。**必須**です。推測しません               |
| `#[model(primary)]`         | フィールド | 主キーにする。無ければ、**列名**が `id` の項目です |
| `#[model(column = "名前")]` | フィールド | 列名を変える。無ければ、フィールド名と同じです     |
| `#[model(skip)]`            | フィールド | 表に無い項目として扱う                             |

- `r#type` のようなフィールドは、`r#` を外した `type` が列名になります。
- `#[model(skip)]` の項目は、読み出したとき既定値（`Default`）になります。
- 列名の一覧は `Post::COLUMNS` で取れます。

### フィールドに使える型

どれも `Option` にできます。

| 分類   | 型                                                            |
|--------|---------------------------------------------------------------|
| 整数   | `i8` `i16` `i32` `i64` `isize` `u8` `u16` `u32` `u64` `usize` |
| 小数   | `f32` `f64`                                                   |
| その他 | `bool` `String` `Vec<u8>`                                     |

- **null を許す列は `Option<...>` にしてください。** そうしないと読み出しでエラーです。
- 幅の狭い型に収まらない値は、読み出しでエラーになります。ふだん使うのは `i64` と `String` です。
- **`i64` を超える整数は `t.string(..)` の列に入れてください。** `u64` と `usize` で
  `i64` に収まらない値は、文字列として書き込みます。

### 主キーは自動採番の整数だけ

**`#[derive(Model)]` は、自動採番の整数の主キー専用です。**
主キーに書けるのは上の表の整数の型だけです（`Option` も書けません）。ふだんは `i64` を使います。

文字列の主キーを使いたいときは、下の「文字列の主キー」を見てください。

### コンパイルエラーになる書き方

書き間違いは、ビルドのときに止めます。

| 書き方                                                           | 理由                                 |
|------------------------------------------------------------------|--------------------------------------|
| `#[model(table = "...")]` が無い                                 | 表の名前は推測しません               |
| 表名・列名に使えない文字がある（空白や記号、前後の空白）         | 実行すると必ず失敗します             |
| 主キーが決まらない（列名 `id` も `#[model(primary)]` も無い）    | どの列で更新するか決まりません       |
| `#[model(primary)]` を 2 つ以上のフィールドに付ける              | 主キーが決まりません                 |
| 主キーの型が整数でない                                           | 自動採番の整数の主キーだけに対応します |
| `#[model(skip)]` と `#[model(primary)]` を同じフィールドに付ける | 表に無い項目は主キーになれません     |
| 同じ列名が 2 回出てくる                                          | `insert` が失敗します                |
| 主キー以外の列が 1 つも無い                                      | `save()` で入れるものがありません    |
| 名前の無いフィールドの構造体（タプル・ユニット）                 | 列名が決まりません                   |
| 型引数・ライフタイムが付いている                                 | 対応していません                     |

名前に使える文字は、表名が英数字と `_` と `.`（`スキーマ.表` の区切り）、
列名が英数字と `_` です。日本語も使えます。

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

`find_or_fail` は、見つからないとき 404 のエラー（「posts が見つかりません」）を返します。
ハンドラから `?` で返すと、そのまま 404 の応答になります。

```rust
pub async fn show(req: Request) -> Result<Response> {
    let id: i64 = req.param_as("post")?;
    let post = Post::find_or_fail(id).await?;   // 無ければ 404
    json(&bengara::serde_json::json!({ "title": post.title }))
}
```

### 条件の書き方はクエリビルダと同じ

`Post::query()` が返すもの（`ModelQuery<Post>`）は、条件の足し方が
[クエリビルダ](database.md)と同じです。終端だけがモデルの型で返ります。

| 終端                                  | 返るもの               |
|---------------------------------------|------------------------|
| `get()`                               | `Vec<Post>`            |
| `first()`                             | `Option<Post>`         |
| `first_or_fail()`                     | `Post`（無ければ 404） |
| `find(id)`                            | `Option<Post>`（条件に加えて主キーで絞る） |
| `count()`                             | `i64`                  |
| `exists()` / `doesnt_exist()`         | `bool`                 |
| `sum::<T>(col)` / `avg::<T>(col)`     | `Option<T>`            |
| `min::<T>(col)` / `max::<T>(col)`     | `Option<T>`            |
| `pluck::<T>(col)` / `value::<T>(col)` | `Vec<T>` / `Option<T>` |
| `update(&[...])` / `delete()`         | `u64`（件数）          |
| `paginate(per_page, page)`            | `Paginator<Post>`      |

- 条件・並び・件数・`group_by`・`having_op` などは、モデルの型を保ったまま使えます。
  できないことの決まりも[クエリビルダ](database.md)と同じです。
- **`get()` や `first()` は、構造体の全部の列を読みます。** `select` で列を絞ると、
  足りない列で読み出しのエラーになります。一部の列だけ欲しいときは `pluck` を使うか、
  下の `query_builder()` で降ります。
- `to_sql()` で、組み立てた SQL を見られます。`{:?}` で表示すると、組み立てのときに
  見つけた間違い（使えない列名など）も出ます。

### クエリビルダに降りる

次のときは `query_builder()` を呼びます。降りるとモデルの型は外れ、`Row` が返ります。

- `join` / `left_join` を使いたい
- `increment` / `decrement` を使いたい
- 一部の列だけを `Row` で受け取りたい

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

`save()` は主キーを見て `insert` と `update` を選びます。

| 主キーの値       | `save()` がすること                                    |
|------------------|--------------------------------------------------------|
| `0`（空）        | 新しい行を入れる。採番された値を主キーに入れます       |
| それ以外         | その主キーの行を更新する                               |

`Post::draft` は、自分で用意する関数です（下の「書きやすくする」）。

### 更新のときは全列を書き戻します

**`save()` は、構造体が持っている値を全部書きます**（`created_at` だけは外します）。
変えたのが 1 つの項目だけでも同じです。
**読んでから保存するまでの間に他の人が動かした列は、巻き戻ります。**

```rust
// 駄目な書き方。題名だけ変えたいのに、読んだときの views も一緒に書き戻る。
let mut post = Post::find_or_fail(id).await?;
post.title = "新しい題名".to_string();
post.save().await?;
```

変えたい列が決まっているなら、**クエリビルダの `update` を使ってください。**
`updated_at` は入らないので、自分で入れます。

```rust
// 良い書き方。触る列は title と updated_at だけ。
DB::table("posts")
    .where_("id", id)
    .update(&[("title", "新しい題名".into()), ("updated_at", now().into())])
    .await?;
```

`increment` で増える列（閲覧数など）がある表では、必ずこの形にします。

### `save()` がエラーになるとき

書き込みが静かに失われないように、次の 3 つはエラーにします。

| とき                                   | 直し方                                   |
|----------------------------------------|------------------------------------------|
| 更新した行が **0 件**だった            | 行が消えていないか、主キーの値を確かめる |
| 採番された主キーを構造体に入れられない | 主キーの型を `i64` などにする            |
| 採番された値が**返らなかった**         | 主キーが自動採番の列か確かめる           |

- 後ろの 2 つは「行は入ったのに主キーが空のまま」です。
  そのまま `save()` を呼び直すと 2 行目が入るので、エラーにしています。
- MySQL で自動採番の無い表に `save()` すると、エラーの文は
  `insert が ID を返しませんでした` になります。**このときも行は入っています。**
- **主キーに値を入れてから `save()` を呼ぶと、更新になります。** 行がまだ無いと、
  1 つ目のエラーになります。主キーを自分で決める行は `DB::table(..).insert(..)` で入れます。
- まだ保存していないモデルの `delete()` は、何もせずに 0 を返します。

### `created_at` と `updated_at`

`created_at` / `updated_at` という**列名**のフィールドがあると、`save()` が時刻を入れます。
入れる値は `now()` の文字列（UTC の `YYYY-MM-DD HH:MM:SS`）です。

| とき     | `created_at`     | `updated_at` |
|----------|------------------|--------------|
| 新しい行 | 入る             | 入る         |
| 更新     | **書き戻さない** | 入る         |

- 更新で `created_at` を外すので、空のままの構造体を保存しても、表の値は壊れません。
- **クエリビルダ（`DB::table(...)`）で直接入れたときは、時刻は入りません。**
  `timestamps()` の列は null を許すので、`String` のフィールドで読むとエラーになります。
  `now()` を自分で入れるか、モデルの `save()` を通してください。

### 書きやすくする

毎回フィールドを全部書くのは大変です。`impl` に用意しておくと楽になります。

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

パス引数やセッションの ID は文字列で届きます。`Post::find("12")` のように
**整数に読める文字列は、整数に直してから**主キーと比べます。

**整数に読めない値では探しません。** `/posts/abc` のような値は、SQL エラーにせず
「無い」として扱います。どのドライバでも 404 にそろえるためです。

| 書き方                                            | 整数に読めない値のとき                |
|---------------------------------------------------|---------------------------------------|
| `Post::find("abc")` / `Post::query().find("abc")` | 問い合わせずに `None`                 |
| `Post::find_or_fail("abc")`                       | 問い合わせずに 404                    |
| `req.model::<Post>("post")`                       | 問い合わせずに 404                    |
| `req.auth().user::<User>()`                       | 問い合わせずに `None`                 |
| `Post::query().where_("id", "abc")`               | 必ず 0 件（`where 1 = 0` になります） |

`find(None::<i64>)` のように `NULL` を渡したときも、問い合わせずに `None` です。

## よく使う条件をまとめる

Laravel のスコープに当たるものは、ただのメソッドです。

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

**メソッドで書きます。** 宣言（`hasMany` など）はありません。

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
```

読むときは `await` が付きます。絞り込みも足せます。

```rust
let comments = post.comments().get().await?;
let owner = comment.post().first().await?;
let recent = post.comments().where_op("created_at", ">", "2026-01-01").get().await?;
```

**触っただけでは読みません。** Laravel の `$post->comments` のような遅延読み込みは
ありません。問い合わせが走るのは `await` を書いた所だけです。

### まとめて読む

Laravel の `with('comments')` に当たるものは、`where_in` で引いて束ねます。
問い合わせは記事の数に関係なく **2 回**です。

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

## ページ分け

```rust
let page = Post::published().paginate(15, req.page()).await?;
json(&bengara::serde_json::json!({ "total": page.total, "last_page": page.last_page() }))
```

返るものの中身は [database.md](database.md) の「ページ分け」にあります。

## トランザクションの中で使う

**トランザクションの中では、`tx` を渡す書き方を使います。**

| 既定の接続を使う        | トランザクションの中で流す       |
|-------------------------|----------------------------------|
| `post.save()`           | `post.save_using(&tx)`           |
| `post.delete()`         | `post.delete_using(&tx)`         |
| `post.fresh()`          | `post.fresh_using(&tx)`          |
| `Post::query()`         | `Post::on(&tx)`                  |
| 組み立て済みのクエリ    | `query.using(&tx)`               |

```rust
let tx = DB::begin().await?;

let posts = Post::on(&tx).where_("status", "draft").get().await?;
let mut post = Post::draft("やきそば");
post.save_using(&tx).await?;

tx.commit().await?;
```

- **`save()` / `delete()` / `fresh()` / `Post::find()` は既定の接続を使います。**
  トランザクションの中で呼ぶと、書き込みが外に出ます（`rollback()` しても残ります）。
- メモリ上の SQLite（`:memory:`）は接続が 1 本です。中から `save()` を呼ぶと、
  接続が空くのを待ち続けて、時間切れのエラーになります。
- 終わった（または失敗した）トランザクションを渡すとエラーです
  （[database.md](database.md) の「使えなくなるとき」）。

## 文字列の主キー

UUID のように自分で決める主キーの行は、**クエリビルダで入れます。**

```rust
DB::table("sessions")
    .insert(&[("id", key.into()), ("payload", body.into())])
    .await?;
```

`bengara::database::Model` を自分で実装すると、読み出しと更新はモデルで書けます。

| すること                        | 文字列の主キーのモデルで                               |
|---------------------------------|--------------------------------------------------------|
| 読む（`find` / `query()` など） | 使えます                                               |
| 更新する（`save()`）            | 使えます（主キーに値が入っている行）                   |
| 消す（`delete()`）              | 使えます                                               |
| **新しく入れる**                | **`save()` では入りません。** 上の `insert` で入れます |

**必ず `const INTEGER_KEY: bool = false;` を書いてください。**
書かないと整数の主キーとして扱うので、`"a1"` のような主キーは 1 件も見つかりません。

## ソフトデリート

`t.soft_deletes()` で `deleted_at` 列は作れます（[migrations.md](migrations.md)）。
**モデル側で自動に除く仕組みはありません。** `post.delete()` は行を消し、
`Post::all()` は印の付いた行も返します。絞り込みは自分で書いてください。

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

初期データを入れる関数（シーダー）は `database/seeders/` に置きます。

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

- ファイル名は大文字始まりです。`pub async fn run() -> Result<()>` を定義します。
- `cargo artisan db:seed` で `DatabaseSeeder` が動きます。
  `DatabaseSeeder` が無いときは、全部のシーダーを名前順に動かします。
- `--class=PostSeeder` で 1 本だけ指定できます。
- ほかのシーダーを呼ぶときは、ただの関数呼び出しです
  （`crate::database::seeders::post_seeder::run().await?`）。
- シーダーは既定の接続に書きます（[migrations.md](migrations.md) の「使える指定」）。

テスト用のデータを作る関数（Laravel のファクトリ）は `database/factories/` に置きます。
**専用の仕組みはありません。** ただの関数です。

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

まだ無いものは [backlog.md](backlog.md) にまとめてあります。

- [database.md](database.md) — 接続とクエリビルダ
- [migrations.md](migrations.md) — 表を作る
- [testing.md](testing.md) — DB を使うテスト
- [laravel-differences.md](laravel-differences.md) — Laravel と違う点
