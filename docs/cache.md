# キャッシュ

キャッシュは、計算やクエリの結果を取っておく仕組みです。
次からは取っておいたほうを返すので、速くなります。

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

値は文字列です。構造体は下の「構造体を入れる」を使います。

| メソッド          | 返るもの                 | 覚えること                         |
|-------------------|--------------------------|------------------------------------|
| `put(鍵, 値, 秒)` | `Result<()>`             | 秒が 0 なら、すぐ期限切れ          |
| `forever(鍵, 値)` | `Result<()>`             | 期限なし                           |
| `get(鍵)`         | `Result<Option<String>>` | 期限切れは `None`                  |
| `has(鍵)`         | `Result<bool>`           | 期限切れは `false`                 |
| `forget(鍵)`      | `Result<()>`             | 無くてもエラーにしない             |
| `flush()`         | `Result<()>`             | 全部捨てる                         |
| `prune()`         | `Result<Option<usize>>`  | 期限切れだけ消す（下の「掃除」）   |
| `location()`      | `Option<String>`         | 置き場所の表示。`await` は要らない |

- 壊れたファイルは「無い」として扱います。エラーにはなりません。
- `file` の `get` と `has` は、期限切れのファイルを消しません。消すのは `cache:prune` です。

### 鍵の付け方

**小文字の英数字と `. _ -` だけで付けてください**（`stats.posts`、`post.1`）。

ほかの文字（大文字・`:`・日本語など）でも動きます。
ただし、ファイル名がハッシュ（中身から作る英数字の列）になり、どの鍵のファイルか分からなくなります。
150 バイトより長い鍵も同じです。

## 無ければ作る

あればそれを返し、無ければ作って入れます。

```rust
let posts = Cache::remember("stats.posts", 60, || async {
    Ok(Post::count().await?.to_string())
})
.await?;
```

同時に何本も来ると、作る処理が何回か走ることがあります。
何度走っても同じ結果になる処理にしてください。

## 数を足し引きする

```rust
let count = Cache::increment("visits", 1).await?;   // 足した後の値が返る
Cache::decrement("visits", 1).await?;
```

- 鍵が無ければ 0 から始めます。
- 同時に呼ばれても、数は落ちません。
- 値が数として読めないときはエラーです。
- `i64` の上限・下限で止まります。あふれません。
- **期限付きの鍵に使うと、期限が外れます。** Laravel は期限を保つので、ここは違います。

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

構造体の形を変えて読めなくなった値は、`None` を返して消します。

## 置き場所

`.env` の `CACHE_DRIVER` で選びます。

| `CACHE_DRIVER`         | 置き場所                                   |
|------------------------|--------------------------------------------|
| `file`（既定）         | `storage/framework/cache/`                 |
| `memory`（`array` も） | プロセスのメモリ。テストと手元の確認用     |

- 知らない名前を書くと、警告を出して `file` にします。
- **プロセスを 2 つ以上動かすときは `file` にして、全部が同じ `storage/` を見るようにします。** `memory` はプロセスをまたげません。
- `file` は `storage/` を自分では作りません。無いと書き込みでエラーになるので、`storage:init` で作ってください（[deployment.md](deployment.md)）。

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

## 掃除

`file` の期限切れは、読んでも消えません。`cache:prune` でまとめて消します。

```sh
cargo artisan cache:prune     # 期限切れだけ
cargo artisan cache:clear     # 全部
```

**`cache:prune` は、cron などから 1 日 1 回ほど呼んでください。** 呼ばないとファイルが増えていきます。

コードからは `Cache::prune()` を呼びます。
`file` は消した件数を返します。`memory` は自分で掃除するので `None` を返します。

```rust
match Cache::prune().await? {
    Some(n) => println!("{n} 件消しました"),
    None => println!("この置き場所は件数を返しません"),
}
```

## テスト

`#[bengara::test]` はキャッシュを自動では消しません。
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

`CacheStore` を実装して、`bootstrap/app.rs` で入れ替えます。

```rust
// bootstrap/app.rs の中で
bengara::cache::install_store(std::sync::Arc::new(MyStore::new()));
```

**効くのは 1 回目だけです。** キャッシュを最初に使う前に呼んでください。

必ず書くのは次の 4 つです。`async` ではないので、待つ処理をそのまま書けます。

```rust
impl CacheStore for MyStore {
    fn put(&self, key: &str, value: &str, seconds: Option<u64>) -> Result<()> { /* ... */ }
    fn get(&self, key: &str) -> Result<Option<String>> { /* ... */ }
    fn forget(&self, key: &str) -> Result<()> { /* ... */ }
    fn flush(&self) -> Result<()> { /* ... */ }
}
```

- `seconds` が `None` なら期限なしです。期限切れは、読むときに捨ててください。
- 複数のプロセスから使われます。プロセスのメモリだけに置かないでください。

残りのメソッドには既定の動きがあります。必要なら書き足します。

| メソッド    | 既定の動き                         | 書くとよいとき                         |
|-------------|------------------------------------|----------------------------------------|
| `has`       | `get` して捨てる                   | 値が大きいとき                         |
| `increment` | 読む → 足す → 書く（数が落ちうる） | 同時に呼ばれても数を落としたくないとき |
| `prune`     | 何もしない                         | 期限切れをまとめて消せるとき           |
| `prunable`  | `false`                            | `prune` を書いたとき（`true` にする）  |
| `location`  | `None`                             | 置き場所を見せたいとき                 |
| `blocking`  | `true`（裏のスレッドで呼ぶ）       | メモリだけで済むとき（`false` にする） |

置き場所は `file` と `memory` だけです。まだ無い機能は [backlog.md](backlog.md) にまとめています。

## 関連

- [scheduling.md](scheduling.md) — 定期処理
- [session.md](session.md) — セッション。置き場所の考え方は同じです
- [configuration.md](configuration.md) — `config/*.rs` と `.env`
