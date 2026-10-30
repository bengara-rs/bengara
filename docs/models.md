# モデル

表の行を Rust の構造体として読み書きします。Laravel の Eloquent に当たるものです。

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

| 指定                        | 置き場所   | 既定                             |
|-----------------------------|------------|----------------------------------|
| `#[model(table = "posts")]` | 構造体     | **必須**。表の名前は推測しません |
| `#[model(primary)]`         | フィールド | **列名**が `id` の項目           |
| `#[model(column = "名前")]` | フィールド | フィールド名と同じ               |
| `#[model(skip)]`            | フィールド | 表に無い項目として扱う           |

主キーは `#[model(primary)]` で指します。指さないときは、**列名**が `id` の項目です
（フィールド名ではなく列名で見ます）。

フィールドに使える型は次です。どれも `Option` にできます。

| 分類   | 型                                                                    |
|--------|-----------------------------------------------------------------------|
| 整数   | `i8` `i16` `i32` `i64` `isize` `u8` `u16` `u32` `u64` `usize`（10 個） |
| 小数   | `f32` `f64`                                                           |
| その他 | `bool` `String` `Vec<u8>`                                             |

- **null を許す列は `Option<...>` にしてください。** そうしないと読み出しでエラーです。
  ふだん使うのは `i64` と `String` です。
- 幅の狭い型に収まらない値は、読み出しでエラーになります。
- **`i64` を超える整数は `t.string(..)` の列に入れてください。** `u64` と `usize` で
  `i64` に収まらない値は文字列として書き込むので、整数の列では桁が落ちます。

列名の一覧は `Model::COLUMNS` に入ります。枠組みは読まないので、
自分のコードで列名が欲しいときに使ってください。

### 主キーは自動採番の整数だけ

**`#[derive(Model)]` は、自動採番の整数の主キー専用です。** 主キーに書けるのは
整数の型（上の表の 10 個）だけで、実際に使うのは `i64` です。
それ以外の型はコンパイルエラーになります。

文字列の主キーを使いたいときは、下の「文字列の主キー」を見てください。

### コンパイルエラーになる書き方

間に合わせの動きをさせず、ビルドのときに止めます。

| 書き方                                                           | 理由                                 |
|------------------------------------------------------------------|--------------------------------------|
| `#[model(table = "...")]` が無い                                 | 表の名前は推測しない                 |
| 主キーが決まらない（列名 `id` も `#[model(primary)]` も無い）    | どの列で `update` するか決まらない   |
| `#[model(primary)]` を 2 つ以上のフィールドに付ける              | 主キーが決まらない                   |
| 主キーの型が整数でない                                           | 自動採番の整数の主キーだけに対応する |
| `#[model(skip)]` と `#[model(primary)]` を同じフィールドに付ける | 表に無い項目が主キーにはなれない     |
| 同じ列名が 2 回出てくる                                          | `insert` が落ちる                    |
| 列になるフィールドが 1 つも無い                                  | 読み書きするものが無い               |
| 主キー以外の列が 1 つも無い                                      | `save()` で入れるものが無い          |
| 名前の無いフィールドの構造体（タプル・ユニット）                 | 列名が決まらない                     |
| 型引数・ライフタイムが付いている                                 | 対応していない                       |

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

`find_or_fail` は見つからないとき 404 のエラーを返します。
ハンドラから `?` で返すと、そのまま 404 の応答になります。

```rust
pub async fn show(req: Request) -> Result<Response> {
    let id: i64 = req.param_as("post")?;
    let post = Post::find_or_fail(id).await?;   // 無ければ 404
    json(&bengara::serde_json::json!({ "title": post.title }))
}
```

### 条件の書き方はクエリビルダと同じ

`Post::query()` が返す `ModelQuery<Post>` は、条件の足し方が
[クエリビルダ](database.md)と同じです。終端だけが型付きです。

| 終端                                  | 返るもの               |
|---------------------------------------|------------------------|
| `get()`                               | `Vec<Post>`            |
| `first()`                             | `Option<Post>`         |
| `first_or_fail()`                     | `Post`（無ければ 404） |
| `find(id)`                            | `Option<Post>`         |
| `count()`                             | `i64`                  |
| `exists()` / `doesnt_exist()`         | `bool`                 |
| `sum::<T>(col)` / `avg::<T>(col)`     | `Option<T>`            |
| `min::<T>(col)` / `max::<T>(col)`     | `Option<T>`            |
| `pluck::<T>(col)` / `value::<T>(col)` | `Vec<T>` / `Option<T>` |
| `update(&[...])` / `delete()`         | `u64`（件数）          |
| `paginate(per_page, page)`            | `Paginator<Post>`      |

`select` / `add_select` / `distinct` / `group_by` / `having_op` / `having_raw` /
`order_by_raw` なども、モデルの型を保ったまま使えます。
できないことの決まりも[クエリビルダ](database.md)と同じです
（`update` に `limit` を付けられない、列名に式を書けない、など）。

### クエリビルダに降りる

生の `Row` が欲しいときと、`join` / `left_join` を使いたいときは `query_builder()` を
呼びます。降りるとモデルの型は外れます。

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

`save()` は主キーを見て `insert` と `update` を選びます。主キーが `0` なら新しい行です。

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

### 主キーに文字列を渡したとき

パス引数やセッションの ID は文字列で届きます。`Post::find("12")`・
`req.model::<Post>("post")`・`req.auth().user::<User>()` は、**整数に読める文字列を
整数に直してから**主キーと比べます（PostgreSQL は `bigint = text` を断るためです）。

`Model` を自分で実装して文字列の主キーを使うときは、`const INTEGER_KEY: bool = false;` を
書いてください。`"007"` のような値が文字列のまま比べられます。

### `save()` がエラーになるとき

書き込みが静かに失われないように、次の 3 つはエラーにします。

| とき                                   | 直し方                                   |
|----------------------------------------|------------------------------------------|
| 更新した行が **0 件**だった             | 行が消えていないか、主キーの値を確かめる |
| 採番された主キーを構造体に入れられない | 主キーの型を `i64` などにする            |
| 採番された値が**返らなかった**          | 主キーが自動採番の列か確かめる           |

後ろの 2 つは「行は入ったのに主キーが空のまま」です。そのまま `save()` を呼び直すと
2 行目が入るので、そこで気づけるようにしてあります。
自分で主キーを決める表では、値を入れてから `save()` を呼んでください。

採番された番号の取り方はドライバで違いますが、**自動採番の列なら、どのドライバでも
正しい値が返ります**（[database.md](database.md) の「自動採番の番号」）。

### `created_at` と `updated_at`

`created_at` / `updated_at` という**列名**のフィールドがあると、`save()` が入れます。
入れる値は `now()` が返す文字列です。`String` から `Into` で変えられる型なら動きます。

| とき     | `created_at`     | `updated_at` |
|----------|------------------|--------------|
| 新しい行 | 入る             | 入る         |
| 更新     | **書き戻さない** | 入る         |

更新で `created_at` を外すのは、読み直さずに作った構造体（`created_at` が空）を
保存しても表の値を壊さないためです。変えたいときは `DB::table(..).update(..)` を使います。

> **クエリビルダ（`DB::table(...)`）で直接入れたときは、時刻は入りません。**
> `timestamps()` の列は null を許すので、そのままモデルで読むと
> 「NULL の値を 文字列 として読めません」になります。
> `now()` を自分で入れるか、モデルの `save()` を通してください。Laravel も同じです。

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

## トランザクションの中で使う

**トランザクションの中では `_using` の付いたメソッドを使います。**

| 既定の接続を使う | トランザクションの中で流す |
|------------------|----------------------------|
| `post.save()`    | `post.save_using(&tx)`     |
| `post.delete()`  | `post.delete_using(&tx)`   |
| `post.fresh()`   | `post.fresh_using(&tx)`    |
| `Post::query()`  | `Post::on(&tx)`            |

```rust
let tx = DB::begin().await?;

let posts = Post::on(&tx).where_("status", "draft").get().await?;
let mut post = Post::draft("やきそば");
post.save_using(&tx).await?;

tx.commit().await?;
```

`save()` / `delete()` / `fresh()` は**既定の接続**を使うので、トランザクションの中で
呼ぶと書き込みが外に出ます（`rollback()` しても残ります）。`:memory:` では接続が 1 本
なので、空くのを待って固まります。

`fresh_using(&tx)` は同じトランザクションの中から読み直します。終わった（または落ちた）
トランザクションに `_using` で渡すとエラーです
（[database.md](database.md) の「使えなくなるとき」）。

## 文字列の主キー

`#[derive(Model)]` は**自動採番の整数**の主キー専用です。UUID のように自分で決める
主キーのときは、クエリビルダで入れるか、`bengara::database::Model` を自分で実装します。

```rust
DB::table("sessions")
    .insert(&[("id", key.into()), ("payload", body.into())])
    .await?;
```

## ソフトデリート

`t.soft_deletes()` で `deleted_at` 列は作れます（[migrations.md](migrations.md)）。
**ただしモデル側の仕組みはありません。** `post.delete()` は `delete from` を流し、
`Post::all()` は消した印の付いた行も返します。絞り込みは自分で書いてください。

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

初期データは `database/seeders/` に置きます。

```rust
// database/seeders/DatabaseSeeder.rs
use bengara::prelude::*;

pub async fn run() -> Result<()> {
    let mut post = Post::draft("最初の記事");
    post.status = "published".to_string();
    post.save().await?;
    Ok(())
}
```

- ファイル名は大文字始まりです。`pub async fn run() -> Result<()>` を定義します。
- `cargo artisan db:seed` で `DatabaseSeeder` が動きます。
  `--class=PostSeeder` で 1 本だけ指定できます。
- ほかのシーダーを呼ぶときは、ただの関数呼び出しです
  （`crate::database::seeders::post_seeder::run().await?`）。

テスト用のデータを作る関数（Laravel のファクトリ）は `database/factories/` に置きます。
**専用の仕組みはありません。** ただの関数です。

```rust
// database/factories/PostFactory.rs
pub fn make(i: i64) -> Post {
    let mut post = Post::draft(format!("記事 {i}"));
    post.views = i * 10;
    post
}
```

## テスト

DB を使うテストの書き方は [testing.md](testing.md) にあります。

## 関連

まだ無いものは [backlog.md](backlog.md) にまとめてあります。

- [database.md](database.md) — 接続とクエリビルダ
- [migrations.md](migrations.md) — 表を作る
- [testing.md](testing.md) — DB を使うテスト
- [laravel-differences.md](laravel-differences.md) — Laravel と違う点
