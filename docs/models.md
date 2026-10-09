# モデル

表の行を Rust の構造体として読み書きします。
Laravel の Eloquent に当たるものです。

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

主キーを `#[model(primary)]` で指していないときは、**列名**が `id` の項目を主キーにします。
フィールド名ではなく列名で見ます。
そのため `#[model(column = "id")]` を付けたフィールドも主キーになります。

フィールドに使える型は次です。どれも `Option` にできます。

| 分類   | 型                                                                    |
|--------|-----------------------------------------------------------------------|
| 整数   | `i8` `i16` `i32` `i64` `isize` `u8` `u16` `u32` `u64` `usize`（10 個） |
| 小数   | `f32` `f64`                                                           |
| その他 | `bool` `String` `Vec<u8>`                                             |

**null を許す列は `Option<...>` にしてください。** そうしないと読み出しでエラーになります。
ふだん使うのは `i64` と `String` です。

表の名前を推測しないのは、英語の複数形が一定でないためです
（`person` → `people`、`category` → `categories`）。1 行書くほうが確実です。

### 主キーは自動採番の整数だけ

**`#[derive(Model)]` は、自動採番の整数の主キー専用です。**
`insert` のあとにデータベースが決めた番号を書き戻す作りなので、
自分で決める主キー（文字列・UUID）は送れません。

主キーに書けるのは **整数の型**だけです。
`i8` / `i16` / `i32` / `i64` / `isize` / `u8` / `u16` / `u32` / `u64` / `usize` の 10 個です。
それ以外の型は、分かりやすい **コンパイルエラー**になります。

実際に使うのは **`i64`** です。

- 行から読める整数の型も、上の 10 個と同じです。
- 幅の狭い型に **収まらない値は読み出しでエラー**になります。

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

### 条件の書き方はクエリビルダと同じ

`Post::query()` が返すのは `ModelQuery<Post>` です。
条件の足し方は[クエリビルダ](database.md)と同じで、終端だけが型付きです。

| 終端                                  | 返るもの               |
|---------------------------------------|------------------------|
| `get()`                               | `Vec<Post>`            |
| `first()`                             | `Option<Post>`         |
| `first_or_fail()`                     | `Post`（無ければ 404） |
| `count()`                             | `i64`                  |
| `exists()` / `doesnt_exist()`         | `bool`                 |
| `sum::<T>(col)` / `avg::<T>(col)`     | `Option<T>`            |
| `min::<T>(col)` / `max::<T>(col)`     | `Option<T>`            |
| `pluck::<T>(col)` / `value::<T>(col)` | `Vec<T>` / `Option<T>` |
| `update(&[...])` / `delete()`         | `u64`（件数）          |
| `paginate(per_page, page)`            | `Paginator<Post>`      |

モデルの型を保ったまま、次のものも使えます。

`select` / `add_select` / `distinct` / `or_where_in` / `where_not_between` /
`or_where_group` / `group_by` / `having_op` / `having_raw` / `order_by_raw` /
`doesnt_exist` / `sum` / `avg` / `min` / `max`

できないことの決まりは[クエリビルダ](database.md)と同じです。

- `update` と `delete` に `join` / `limit` / `offset` / `group_by` / `having` / `distinct` を
  付けるとエラーになります。`order_by` は黙って無視されます。
- 列名と表名に渡せるのは英数字と `_` `.` だけです。式は書けません。
- `sum` / `avg` / `min` / `max` は `group_by` と組み合わせられません。
- `distinct()` を付けて `count()` / `paginate()` するときは、`select` で列を 1 つ指定します。
- `having_op` に渡せるのは実在の列だけです。集計の結果で絞るときは `having_raw` です。

### クエリビルダに降りる

モデルの形に収まらないものが欲しいときは `query_builder()` を呼びます。
降りるとモデルの型は外れます。

| したいこと                  | 理由                                              |
|-----------------------------|---------------------------------------------------|
| 生の `Row` が欲しい         | `get()` が `Vec<Post>` ではなく `Vec<Row>` を返す |
| `join` / `left_join` を使う | モデル側には置いていない                          |

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
主キーが `0` なら新しい行です。

### `save()` がエラーになるとき

書き込みが静かに失われないように、次の 2 つはエラーにします。

| とき                                   | 返るもの | 直し方                                   |
|----------------------------------------|----------|------------------------------------------|
| 更新した行が **0 件**だった             | エラー   | 行が消えていないか、主キーの値を確かめる |
| 採番された主キーを構造体に入れられない | エラー   | 主キーの型を `i64` などにする            |

2 つ目は、主キーの型が狭くて採番された番号が入らないときに起きます
（`u8` の主キーに `256` が返ったときなど）。
`insert` は済んでいるので、行は 1 つ入っています。
主キーだけが空のままなので、**そのまま `save()` を呼び直すと 2 行目が入ります。**
エラーにして、そこで気づけるようにしてあります。

### 採番された番号はドライバで取り方が違います

主キーの**列の名前**を見るのは PostgreSQL だけです。

| ドライバ   | 返るもの                    |
|------------|-----------------------------|
| PostgreSQL | `returning 主キーの列` の値 |
| SQLite     | 直前に入れた行の rowid      |
| MySQL      | `auto_increment` の列の値   |

`#[derive(Model)]` の主キーは自動採番の列なので、**どのドライバでも正しい値が返ります。**
自動採番でない列を主キーにしたときだけ、SQLite と MySQL で別の値が返ります
（[database.md](database.md) の「自動採番の番号」）。

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

読むときは `await` が付きます。**問い合わせが走る場所が目に見えます。**

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
bengara ではこれをやりません。
**N+1**（一覧を 1 回引いたあと、行ごとに追加の問い合わせが走る状態）を
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

問い合わせは **2 回**です。記事の数に関係なく 2 回のままです。

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

**トランザクションの中では `_using` の付いたメソッドを使います。**

```rust
let tx = DB::begin().await?;

let posts = Post::on(&tx).where_("status", "draft").get().await?;

let mut post = Post::draft("やきそば");
post.save_using(&tx).await?;

tx.commit().await?;
```

| 既定の接続を使う | トランザクションの中で流す |
|------------------|----------------------------|
| `post.save()`    | `post.save_using(&tx)`     |
| `post.delete()`  | `post.delete_using(&tx)`   |
| `post.fresh()`   | `post.fresh_using(&tx)`    |
| `Post::query()`  | `Post::on(&tx)`            |

`save()` / `delete()` / `fresh()` は **既定の接続**を使います。
トランザクションの中で呼ぶと、次の 2 つが起きます。

- 書き込みがトランザクションの外に出ます。`rollback()` しても残ります。
- `:memory:` のデータベースは接続が 1 本なので、空くのを待って固まります。

`fresh_using(&tx)` は、同じトランザクションの中から読み直します。
`save_using(&tx)` で保存した内容は、`commit()` するまで外からは見えません。

終わった（または落ちた）トランザクションに `_using` で渡すと、エラーになります。
詳しくは [database.md](database.md) の「使えなくなるとき」にあります。

## 文字列の主キー

`#[derive(Model)]` は **自動採番の整数**の主キー専用です。
UUID のように自分で決める主キーのときは、次のどちらかにします。

- クエリビルダ（`DB::table(...)`）で入れる。
- `bengara::database::Model` を自分で実装する。

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

- ファイル名は大文字始まりです。`pub async fn run() -> Result<()>` を定義します。
- `cargo artisan db:seed` で `DatabaseSeeder` が動きます。
  `--class=PostSeeder` で 1 本だけ指定できます。
- ほかのシーダーを呼ぶときは、**ただの関数呼び出し**です。

```rust
crate::database::seeders::post_seeder::run().await?;
```

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
