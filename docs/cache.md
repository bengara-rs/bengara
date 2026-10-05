# キャッシュ

計算やクエリの結果を取っておいて、次からは取っておいたほうを返す仕組みです。

## 置き場所

```
CACHE_DRIVER=file
```

| `CACHE_DRIVER`  | 置き場所                                   |
|-----------------|--------------------------------------------|
| `file`（既定）  | `storage/framework/cache/`                 |
| `memory`        | プロセスのメモリ。**テストと手元の確認用** |

`config/cache.rs` を置けば `.env` より優先します。無くても動きます（`file` になります）。

```rust
// config/cache.rs
use bengara::prelude::*;

pub fn config() -> CacheConfig {
    CacheConfig {
        driver: env("CACHE_DRIVER", "file"),
    }
}
```

**プロセスを2つ以上動かすときは、全プロセスが同じ `storage/` を見るようにしてください。**
`memory` はプロセスをまたげません。

## 出し入れ

```rust
use bengara::prelude::*;

Cache::put("stats.posts", "42", 600).await?;     // 600 秒だけ
Cache::forever("version", "1.2.3").await?;       // 期限なし

let value: Option<String> = Cache::get("stats.posts").await?;
let there = Cache::has("stats.posts").await?;

Cache::forget("stats.posts").await?;
Cache::flush().await?;                            // 全部捨てる
```

| メソッド                   | 返るもの                  | 覚えること                       |
|----------------------------|---------------------------|----------------------------------|
| `put(鍵, 値, 秒)`          | `Result<()>`              | 秒が 0 なら、その場で期限切れ    |
| `forever(鍵, 値)`          | `Result<()>`              | 期限なし                         |
| `get(鍵)`                  | `Result<Option<String>>`  | 期限切れは `None`。ついでに消す  |
| `has(鍵)`                  | `Result<bool>`            |                                  |
| `forget(鍵)`               | `Result<()>`              | 無くてもエラーにしない           |
| `flush()`                  | `Result<()>`              | 全部捨てる                       |

## 無ければ作る

よく使う形です。あればそれを返し、無ければ作って入れます。

```rust
let posts = Cache::remember("stats.posts", 60, || async {
    Ok(Post::count().await?.to_string())
})
.await?;
```

## 数を足し引きする

```rust
let count = Cache::increment("visits", 1).await?;   // 足した後の値が返る
Cache::decrement("visits", 1).await?;
```

鍵が無ければ 0 から始めます。

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

// 無ければ作る版もあります。
let stats: Stats = Cache::remember_json("stats", 600, || async { Ok(count().await?) }).await?;
```

形が変わって読めなくなったときは `None` が返ります（落ちません）。

## 鍵の付け方

鍵はそのままファイル名になります。ただし、安全な文字だけのときです。

| 鍵                     | ファイル名                   |
|------------------------|------------------------------|
| `stats.posts`          | `stats.posts`                |
| `user:1`               | `user~1`（`:` を `~` に）    |
| 日本語や記号が入るもの | SHA-256 の 16 進（64 文字）  |

`storage/framework/cache/` を見れば何が入っているか分かるようにするための決まりです。
**鍵には何を入れても構いません。** 壊れるファイル名にはなりません。

## 掃除

期限切れは読んだときに消えますが、誰も読まない鍵は残ります。まとめて消せます。

```sh
cargo artisan cache:prune     # 期限切れだけ
cargo artisan cache:clear     # 全部
```

`cache:prune` は cron やタスクスケジューラから1日1回ほど呼ぶとよいです。
放っておいても壊れませんが、ファイルが増えていきます。

## 置き場所を自分で作る

`CacheStore` を実装して、`cache::install_store` で入れ替えます。 **起動時に 1 回だけ呼びます。**

```rust
// main.rs の入口や bootstrap/app.rs の中で
bengara::cache::install_store(std::sync::Arc::new(MyStore::new()));
```

実装するのは次の 4 つです。 **`async` ではありません**（`spawn_blocking` の中から呼ばれるので、
待つ処理をそのまま書いて構いません）。

```rust
impl CacheStore for MyStore {
    fn put(&self, key: &str, value: &str, seconds: Option<u64>) -> Result<()> { /* ... */ }
    fn get(&self, key: &str) -> Result<Option<String>> { /* ... */ }
    fn forget(&self, key: &str) -> Result<()> { /* ... */ }
    fn flush(&self) -> Result<()> { /* ... */ }
}
```

- **複数のプロセスから同時に使われます。** プロセスのメモリだけに置かないでください。
- 期限切れは読むときに捨ててください。

## テスト

`#[bengara::test]` はキャッシュを自動では消しません。 **鍵が重ならないようにしてください。**
テストは並んで走るので、同じ鍵を使うと互いに踏みます。

```rust
#[bengara::test]
async fn 数えた結果を取っておく() {
    Cache::forget("test.count").await.unwrap();
    assert_eq!(Cache::increment("test.count", 1).await.unwrap(), 1);
    Cache::forget("test.count").await.unwrap();
}
```

## 無いもの

| 項目                      | 代わりにすること                       |
|---------------------------|----------------------------------------|
| Redis / Memcached         | `file` を使う。全プロセスで同じ場所を見る |
| タグ（`Cache::tags()`）   | 鍵に頭を付ける（`user:1:posts`）。まとめて消すなら自分で覚えておく |
| アトミックなロック        | ありません                             |

## 関連

- [scheduling.md](scheduling.md) — 定期処理。最後に動かした時刻はキャッシュに置きます
- [session.md](session.md) — セッション。置き場所の考え方は同じです
- [configuration.md](configuration.md) — `config/*.rs` と `.env`
