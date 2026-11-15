# キャッシュ

計算やクエリの結果を取っておき、次からは取っておいたほうを返す仕組みです。
このページで、置き場所の選び方・出し入れ・掃除の仕方が分かります。

## 置き場所

`.env` の `CACHE_DRIVER` で選びます。

```
CACHE_DRIVER=file
```

| `CACHE_DRIVER`         | 置き場所                                   |
|------------------------|--------------------------------------------|
| `file`（既定）         | `storage/framework/cache/`                 |
| `memory`（`array` も） | プロセスのメモリ。**テストと手元の確認用** |

- 知らない名前を書いたときは、警告をログに出して `file` にします。
- **プロセスを 2 つ以上動かすときは `file` を使い、全プロセスが同じ `storage/` を見るようにします。**
  `memory` はプロセスをまたげません。
- いまの置き場所は `Cache::location()` で分かります。

**`file` は `storage/` を自分では作りません。** 無いと、書き込みが次のエラーになります。
`storage:init` で作ってください（[deployment.md](deployment.md)）。

```
<場所>/storage がありません。storage:init（開発では `cargo artisan storage:init`、本番では `./<実行ファイル> storage:init`） を実行してください
```

`config/cache.rs` を置くと、そちらを使います。無くても動きます。

```rust
// config/cache.rs
use bengara::prelude::*;

pub fn config() -> CacheConfig {
    CacheConfig {
        driver: env("CACHE_DRIVER", "file"),
    }
}
```

## 出し入れ

```rust
use bengara::prelude::*;

Cache::put("stats.posts", "42", 600).await?;     // 600 秒だけ
Cache::forever("version", "1.2.3").await?;       // 期限なし

let value: Option<String> = Cache::get("stats.posts").await?;
let there = Cache::has("stats.posts").await?;

Cache::forget("stats.posts").await?;
Cache::flush().await?;                           // 全部捨てる
```

値は**文字列**です。構造体は下の「構造体を入れる」を使います。

| メソッド          | 返るもの                 | 覚えること                         |
|-------------------|--------------------------|------------------------------------|
| `put(鍵, 値, 秒)` | `Result<()>`             | 秒が 0 なら、その場で期限切れ      |
| `forever(鍵, 値)` | `Result<()>`             | 期限なし                           |
| `get(鍵)`         | `Result<Option<String>>` | 期限切れは `None`。ついでに消す    |
| `has(鍵)`         | `Result<bool>`           | 期限切れは `false`                 |
| `forget(鍵)`      | `Result<()>`             | 無くてもエラーにしない             |
| `flush()`         | `Result<()>`             | 全部捨てる                         |
| `prune()`         | `Result<Option<usize>>`  | 期限切れだけ消す（下の「掃除」）   |
| `location()`      | `Option<String>`         | 置き場所の表示。`await` は要らない |

- `file` の `has` は期限だけを見ます。大きな値でも中身は読まず、期限切れのファイルも消しません。
- **壊れたファイルは「無い」として扱います。** `has` は `false`、`get` は `None` です。
  `get` はそのファイルを消します。エラーにはなりません。

### 鍵の付け方

**小文字の英数字と `. _ -` だけで付けてください**（`stats.posts`、`post.1`）。
区切りには `.` を使います。

ほかの文字が入っても動きます。ただしファイル名がハッシュ（中身から作った英数字の列）になり、
`storage/framework/cache/` を見ても、どの鍵か分からなくなります。

| ハッシュになる鍵                 | 例                   |
|----------------------------------|----------------------|
| 大文字を含む                     | `User.1`             |
| `:` や `~` などの記号を含む      | `user:1`             |
| 日本語を含む                     | `記事.1`             |
| 末尾が `.`、空、150 バイトより長い | `post.`            |
| Windows の装置の名前             | `nul`、`con`、`com1` |

## 無ければ作る

よく使う形です。あればそれを返し、無ければ作って入れます。

```rust
let posts = Cache::remember("stats.posts", 60, || async {
    Ok(Post::count().await?.to_string())
})
.await?;
```

**同時に何本も来ると、作る処理が何回か走ることがあります。**
何度走っても同じ結果になる処理にしてください。

## 数を足し引きする

```rust
let count = Cache::increment("visits", 1).await?;   // 足した後の値が返る
Cache::decrement("visits", 1).await?;
```

| とき                   | 動き                                  |
|------------------------|---------------------------------------|
| 鍵が無い               | 0 から始める                          |
| 同時に呼ばれた         | 数は落ちない（順番に処理する）        |
| 値が数として読めない   | エラーを返す                          |
| `i64` の上限・下限     | そこで止まる。あふれない              |
| 期限付きの鍵に使った   | **期限が外れる。** 期限なしで書き直す |

**期限を保ちたい鍵には使えません。** `Cache::put("n", "1", 60)` のあとに `increment("n", 1)` を
呼ぶと、期限なしになります（Laravel は期限を保つので、ここは違います）。

## 構造体を入れる

`serde::Serialize` が付いていれば、そのまま入れられます。

```rust
#[derive(bengara::serde::Serialize, bengara::serde::Deserialize)]
#[serde(crate = "bengara::serde")]
struct Stats {
    posts: i64,
    users: i64,
}

Cache::put_json("stats", &stats, 600).await?;
let stats: Option<Stats> = Cache::get_json("stats").await?;

// 無ければ作る版
let stats: Stats = Cache::remember_json("stats", 600, || async { Ok(count().await?) }).await?;
```

構造体の形を変えて読めなくなった値は、`None` が返ります。その値は、そのとき消します。

## 掃除

期限切れは読んだときに消えます。誰も読まない鍵は残るので、まとめて消します。

```sh
cargo artisan cache:prune     # 期限切れだけ
cargo artisan cache:clear     # 全部
```

**`cache:prune` は、cron やタスクスケジューラから 1 日 1 回ほど呼んでください。**
呼ばなくても壊れませんが、ファイルが増えていきます。

コードからは `Cache::prune()` と `Cache::flush()` です。

```rust
match Cache::prune().await? {
    Some(n) => println!("{n} 件消しました"),
    None => println!("この置き場所は件数を返しません"),
}
```

| 置き場所 | `prune()` が返すもの | 補足                                               |
|----------|----------------------|----------------------------------------------------|
| `file`   | 消した件数           | 数えるのは、期限切れと壊れたファイル               |
| `memory` | `None`               | 掃除は自分でしている（書き込み 100 回ごと）        |

## テスト

`#[bengara::test]` は、キャッシュを自動では消しません。
テストは並んで走るので、**鍵をテストごとに変えてください。**

```rust
#[bengara::test]
async fn 数を足し引きできる() {
    Cache::forget("test.count").await.unwrap();

    assert_eq!(Cache::increment("test.count", 1).await.unwrap(), 1);
    assert_eq!(Cache::increment("test.count", 2).await.unwrap(), 3);
    assert_eq!(Cache::decrement("test.count", 1).await.unwrap(), 2);

    Cache::forget("test.count").await.unwrap();
}
```

## 置き場所を自分で作る

`CacheStore` を実装して、`cache::install_store` で入れ替えます。

```rust
// bootstrap/app.rs の中で
bengara::cache::install_store(std::sync::Arc::new(MyStore::new()));
```

**効くのは 1 回目だけです。** 2 回目以降は何もせず、警告をログに出します。
キャッシュを最初に使う前に呼んでください。

必ず書くのは次の 4 つです。`async` ではありません。待つ処理をそのまま書けます。

```rust
impl CacheStore for MyStore {
    fn put(&self, key: &str, value: &str, seconds: Option<u64>) -> Result<()> { /* ... */ }
    fn get(&self, key: &str) -> Result<Option<String>> { /* ... */ }
    fn forget(&self, key: &str) -> Result<()> { /* ... */ }
    fn flush(&self) -> Result<()> { /* ... */ }
}
```

残りは既定の実装があります。書かなくても動きます。

| メソッド    | 既定の動き                       | 自分で書くとよいとき                   |
|-------------|----------------------------------|----------------------------------------|
| `has`       | `get` して捨てる                 | 値が大きいとき                         |
| `increment` | 読む → 足す → 書く（数が落ちうる） | 同時に呼ばれても数を落としたくないとき |
| `prune`     | 何もしない                       | 期限切れをまとめて消せるとき           |
| `prunable`  | `false`                          | `prune` を書いたとき（`true` にする）  |
| `location`  | `None`                           | 置き場所を人に見せたいとき             |
| `blocking`  | `true`（裏のスレッドで呼ぶ）     | メモリだけで済むとき（`false` にする） |

- `put` の `seconds` が `None` なら、期限なしです。
- 期限切れは、読むときに捨ててください。
- 複数のプロセスから同時に使われます。プロセスのメモリだけに置かないでください。
- 渡ってくる鍵は、上の「鍵の付け方」の規則でファイル名の形に直したものです。

## ファイルの並び

`storage/framework/cache/` を直接のぞくときの手引きです。**ふだんは気にしなくて構いません。**

| 置くもの         | 名前                                          |
|------------------|-----------------------------------------------|
| ふつうの鍵       | 鍵そのまま                                    |
| ハッシュになる鍵 | `~` ＋ SHA-256 の 16 進（65 文字）            |
| 書き込みの途中   | `<鍵>.+tmp.<プロセス番号>.<連番>`             |
| `increment` の錠 | `+locks/` の中                                |

- `cache:clear` は `+locks/` を消しません。空のディレクトリが 1 つ残ります。
- 書き込みに失敗して残った古い一時ファイルは、`cache:prune` と `cache:clear` が消します。

## できないこと

| できないこと                        | 代わりにすること                                   |
|-------------------------------------|----------------------------------------------------|
| Redis や Memcached に置く           | `file` を使い、全プロセスで同じ `storage/` を見る  |
| タグでまとめて消す（`Cache::tags()`） | 鍵に頭を付ける（`user.1.posts`）                 |
| 排他の錠を取る（`Cache::lock()`）   | ありません。`increment` は中で排他します           |

まだ無い機能の一覧は [backlog.md](backlog.md) にあります。

## 関連

- [scheduling.md](scheduling.md) — 定期処理。前回の時刻はキャッシュではなくファイルに置きます
- [session.md](session.md) — セッション。置き場所の考え方は同じです
- [configuration.md](configuration.md) — `config/*.rs` と `.env`
