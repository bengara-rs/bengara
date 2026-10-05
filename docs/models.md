# モデル

表の行を Rust の構造体として読み書きします。Laravel の Eloquent に当たります。

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
| `#[model(primary)]`         | フィールド | `id` という名前のフィールド      |
| `#[model(column = "名前")]` | フィールド | フィールド名と同じ               |
| `#[model(skip)]`            | フィールド | 表に無い項目として扱う           |

フィールドに使える型は `i64` / `i32` / `u32` / `u64` / `f64` / `bool` / `String` /
`Vec<u8>` と、その `Option` です。 **null を許す列は `Option<...>` にしてください。** そうしないと読み出しでエラーになります。

表の名前を推測しないのは、英語の複数形が一定でないためです
（`person` → `people`、`category` → `categories`）。1行書くほうが確実です。

## 読む

```rust
let post = Post::find(1).await?;              // Option<Post>
let post = Post::find_or_fail(1).await?;      // 無ければ 404 のエラー
let posts = Post::all().await?;               // Vec<Post>
let total = Post::count().await?;             // i64

let posts = Post::query()
    .where_("status", "published")
    .where_op("views", ">", 100)
    .latest()
    .limit(10)
    .get()
    .await?;
```

`find_or_fail` は見つからないとき `404` のエラーを返します。
ハンドラから `?` で返すと、そのまま 404 の応答になります。

```rust
pub async fn show(req: Request) -> Result<Response> {
    let id: i64 = req.param_as("post")?;
    let post = Post::find_or_fail(id).await?;   // 無ければ 404
    json(&bengara::serde_json::json!({ "title": post.title }))
}
```

`Post::query()` が返すのは `ModelQuery<Post>` です。
条件の書き方は[クエリビルダ](database.md)と同じで、終端だけが型付きです。

| 終端                                  | 返るもの               |
|---------------------------------------|------------------------|
| `get()`                               | `Vec<Post>`            |
| `first()`                             | `Option<Post>`         |
| `first_or_fail()`                     | `Post`（無ければ 404） |
| `count()` / `exists()`                | `i64` / `bool`         |
| `pluck::<T>(col)` / `value::<T>(col)` | `Vec<T>` / `Option<T>` |
| `update(&[...])` / `delete()`         | `u64`（件数）          |
| `paginate(per_page, page)`            | `Paginator<Post>`      |

生の行が欲しいときは `query_builder()` でクエリビルダを取り出せます。

## 書く

```rust
// 新しい行
let mut post = Post {
    id: 0,                       // 0 なら「まだ保存していない」
    title: "やきそば".to_string(),
    body: None,
    status: "draft".to_string(),
    views: 0,
    created_at: String::new(),
    updated_at: String::new(),
};
post.save().await?;              // insert され、post.id に ID が入る

// 更新
post.title = "焼きそば".to_string();
post.save().await?;              // update になる

// 読み直す
let fresh = post.fresh().await?; // Option<Post>

// 消す
post.delete().await?;            // 返るのは件数
```

`save()` は主キーを見て `insert` と `update` を選びます。
主キーが空（`0` / `null` / `""`）なら新しい行です。

### `created_at` と `updated_at`

`created_at` / `updated_at` という `String` のフィールドがあると、`save()` が入れます。

| とき     | `created_at` | `updated_at` |
|----------|--------------|--------------|
| 新しい行 | 入る         | 入る         |
| 更新     | そのまま     | 入る         |

> **クエリビルダ（`DB::table(...)`）で直接入れたときは、時刻は入りません。**
> `timestamps()` の列は null を許すので、そのままモデルで読むと
> 「NULL の値を 文字列 として読めません」になります。
> `now()` を自分で入れるか、モデルの `save()` を通してください。Laravel も同じです。

### 書きやすくする

毎回フィールドを全部書くのは大変なので、`impl` に用意しておくと楽です。

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

読むときは `await` が付きます。 **問い合わせが走る場所が目に見えます。**

```rust
let comments = post.comments().get().await?;
let owner = comment.post().first().await?;
```

絞り込みも足せます。

```rust
let recent = post.comments().where_op("created_at", ">", "2026-01-01").get().await?;
```

### 触っただけでは読みません

Laravel の `$post->comments` は、触った瞬間に裏で問い合わせが走ります。
bengara ではこれをやりません。 **N+1**（一覧を1回引いたあと、行ごとに追加の問い合わせが走る状態）を
起こしにくくするためです。

### まとめて読む

Laravel の `with('comments')` に当たるものは、`where_in` で引いて束ねます。

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

問い合わせは **2回**です。記事の数に関係なく2回のままです。

## ページ分け

```rust
pub async fn index(req: Request) -> Result<Response> {
    let page = Post::published().paginate(15, req.page()).await?;

    json(&bengara::serde_json::json!({
        "total": page.total,
        "last_page": page.last_page(),
        "has_more": page.has_more_pages(),
    }))
}
```

## トランザクションの中で使う

```rust
let tx = DB::begin().await?;

let posts = Post::on(&tx).where_("status", "draft").get().await?;
Post::query().using(&tx).where_("id", 1).delete().await?;

tx.commit().await?;
```

`save()` と `delete()`（モデルのメソッド）は、いまは **既定の接続**を使います。
トランザクションの中で入れたいときは、`tx.table(...)` か `Post::on(&tx)` を使ってください。

## 文字列の主キー

`save()` は **自動採番の整数**の主キーを前提にしています。
UUID のように自分で決める主キーのときは、クエリビルダで入れてください。

```rust
DB::table("sessions")
    .insert(&[("id", key.into()), ("payload", body.into())])
    .await?;
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

- ファイル名は大文字始まり。`pub async fn run() -> Result<()>` を定義します。
- `cargo artisan db:seed` で `DatabaseSeeder` が動きます。
  `--class=PostSeeder` で1本だけ指定できます。
- ほかのシーダーを呼ぶときは、 **ただの関数呼び出し**です。

```rust
crate::database::seeders::post_seeder::run().await?;
```

テスト用のデータを作る関数（Laravel のファクトリ）は `database/factories/` に置きます。 **専用の仕組みはありません。**
ただの関数です。

```rust
// database/factories/PostFactory.rs
pub fn make(i: i64) -> Post {
    let mut post = Post::draft(format!("記事 {i}"));
    post.views = i * 10;
    post
}
```

```rust
let post = crate::database::factories::post_factory::make(1);
```

## テスト

DB を使うテストの書き方は [testing.md](testing.md) にあります。

## 関連

- [database.md](database.md) — 接続とクエリビルダ
- [migrations.md](migrations.md) — 表を作る
- [testing.md](testing.md) — DB を使うテスト
- [laravel-differences.md](laravel-differences.md) — Laravel と違う点
