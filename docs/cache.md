# キャッシュ

計算やクエリの結果を取っておき、次からは取っておいたほうを返す仕組みです。

## 置き場所

置き場所は `.env` の `CACHE_DRIVER` で選びます。

```
CACHE_DRIVER=file
```

| `CACHE_DRIVER` | 置き場所                                   |
|----------------|--------------------------------------------|
| `file`（既定） | `storage/framework/cache/`                 |
| `memory`       | プロセスのメモリ。**テストと手元の確認用** |

**プロセスを 2 つ以上動かすときは、全プロセスが同じ `storage/` を見るようにしてください。**
`memory` はプロセスをまたげません。いまの置き場所は `Cache::location()` で分かります。

`config/cache.rs` を置くと `.env` より優先します。無くても動きます。

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

| メソッド          | 返るもの                 | 覚えること                           |
|-------------------|--------------------------|--------------------------------------|
| `put(鍵, 値, 秒)` | `Result<()>`             | 秒が 0 なら、その場で期限切れ        |
| `forever(鍵, 値)` | `Result<()>`             | 期限なし                             |
| `get(鍵)`         | `Result<Option<String>>` | 期限切れは `None`。ついでに消す      |
| `has(鍵)`         | `Result<bool>`           | 期限切れは `false`。**消さない**     |
| `forget(鍵)`      | `Result<()>`             | 無くてもエラーにしない               |
| `flush()`         | `Result<()>`             | 全部捨てる                           |
| `prune()`         | `Result<Option<usize>>`  | 期限切れだけ消す。下の「掃除」を見る |
| `location()`      | `Option<String>`         | 置き場所の表示。`await` は要らない    |

`has` は**期限だけ**を見ます。`put_json` で入れた大きな中身は読みません。
期限切れのファイルも消しません。片づけは `get` と `cache:prune` が引き受けます。

### 鍵の付け方

**鍵には何を入れても壊れません。** ただし、次のものを含む鍵はファイル名がハッシュに
なるので、`storage/framework/cache/` を見ても何の鍵か分かりません。**避けてください。**

- 大文字（`User.1`）
- `:` や `~`（`user:1`）
- 日本語や記号
- 末尾の `.`、空の鍵、150 バイトより長い鍵
- Windows の装置の名前（`nul` / `con` / `com1` など）

そのままファイル名になるのは、**小文字の英数字と `. _ -` だけ**の鍵です
（`stats.posts`・`post.1` など）。くわしくは下の「ファイル名の決め方」にあります。

## 無ければ作る

よく使う形です。あればそれを返し、無ければ作って入れます。

```rust
let posts = Cache::remember("stats.posts", 60, || async {
    Ok(Post::count().await?.to_string())
})
.await?;
```

**同時に何本も来ると、作る処理が何回か走ることがあります。** 錠は取りません。
作る処理は、何度走っても同じ結果になる形にしてください。

## 数を足し引きする

```rust
let count = Cache::increment("visits", 1).await?;   // 足した後の値が返る
Cache::decrement("visits", 1).await?;
```

| 決まり                       | 内容                                   |
|------------------------------|----------------------------------------|
| 鍵が無いとき                 | 0 から始める                           |
| 同時に呼ばれたとき           | 置き場所側で排他するので、数は落ちない |
| 期限                         | **引き継がない。** 期限なしで書き直す  |
| 数として読めない値だったとき | エラーを返す                           |
| `i64` の上限・下限           | **そこで止まる。** あふれない          |

**期限を保ちたいなら `increment` は使えません。** `Cache::put("n", "1", 60)` のあとに
`increment("n", 1)` を呼ぶと、期限が外れます（Laravel は保つので、ここは違います）。

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

## 掃除

期限切れは読んだときに消えますが、誰も読まない鍵は残ります。まとめて消せます。

```sh
cargo artisan cache:prune     # 期限切れだけ
cargo artisan cache:clear     # 全部
```

コードからも呼べます。`file` では消した件数が、`memory` では `None` が返ります。

```rust
match Cache::prune().await? {
    Some(n) => println!("{n} 件消しました"),
    None => println!("この置き場所は掃除できません"),
}
```

- **`cache:prune` は cron やタスクスケジューラから 1 日 1 回ほど呼ぶとよいです。**
  放っておいても壊れませんが、ファイルが増えていきます。
- `memory` は件数を数えられないので `None` ですが、**掃除そのものはしています**
  （書き込み 100 回ごとに期限切れを落とします）。
- **`cache:clear`（`Cache::flush()`）は `+locks/` を消しません。** 中の錠ファイルも
  残します。残るのは空のディレクトリ 1 つだけです。

## テスト

`#[bengara::test]` はキャッシュを自動では消しません。**鍵が重ならないようにしてください。**
テストは並んで走るので、同じ鍵を使うと互いに踏みます。

```rust
#[bengara::test]
async fn 数えた結果を取っておく() {
    Cache::forget("test.count").await.unwrap();
    assert_eq!(Cache::increment("test.count", 1).await.unwrap(), 1);
    Cache::forget("test.count").await.unwrap();
}
```

## 置き場所を自分で作る

`CacheStore` を実装して、`cache::install_store` で入れ替えます。**起動時に 1 回だけ
呼びます。** 2 回目以降は効きません（警告がログに出ます）。

```rust
// main.rs の入口や bootstrap/app.rs の中で
bengara::cache::install_store(std::sync::Arc::new(MyStore::new()));
```

必ず書くのは次の 4 つです。**`async` ではありません**（裏のスレッドから呼ばれるので、
待つ処理をそのまま書いて構いません）。

```rust
impl CacheStore for MyStore {
    fn put(&self, key: &str, value: &str, seconds: Option<u64>) -> Result<()> { /* ... */ }
    fn get(&self, key: &str) -> Result<Option<String>> { /* ... */ }
    fn forget(&self, key: &str) -> Result<()> { /* ... */ }
    fn flush(&self) -> Result<()> { /* ... */ }
}
```

残りは**既定の実装つき**です。書かなくてもコンパイルは通ります。

| メソッド    | 既定の動き                       | 自分で書くとよいとき                   |
|-------------|----------------------------------|----------------------------------------|
| `has`       | `get` して捨てる                 | 値が大きいとき                         |
| `increment` | 読む → 足す → 書く（排他しない） | 同時に呼ばれても数を落としたくないとき |
| `prune`     | 何もしない                       | 期限切れをまとめて消せるとき           |
| `prunable`  | `false`                          | `prune` を書いたとき                   |
| `location`  | `None`                           | 置き場所を人に見せたいとき             |
| `blocking`  | `true`（裏のスレッドを経由する） | メモリだけで済む置き場所のとき         |

- **複数のプロセスから同時に使われます。** プロセスのメモリだけに置かないでください。
- 期限切れは読むときに捨ててください。

## ファイル名の決め方

`file` の置き場所が中でどう置いているかの話です。**ふだんは気にしなくて構いません。**
`storage/framework/cache/` を直接のぞくときに読んでください。

| 置くもの       | 名前                                          |
|----------------|-----------------------------------------------|
| ふつうの鍵     | 鍵そのまま（小文字の英数字と `. _ -` のとき） |
| そのほかの鍵   | `~` ＋ SHA-256 の 16 進（65 文字）            |
| 書き込みの途中 | `<鍵>.+tmp.<プロセス番号>.<連番>`             |
| `increment` の錠 | `+locks/` の中                              |

- **`~` と `+` は印です。** 素通しする鍵のファイル名には出てこない文字なので、
  本体のファイルとぶつかりません（鍵 `locks` もそのまま使えます）。
- 大文字・`:`・`~`・末尾の `.`・Windows の装置の名前をハッシュに回すのは、
  OS によってファイルが取り違えられるためです。`Storage` が同じ名前を全 OS で
  断っているのと同じ考え方です（[storage.md](storage.md)）。
- 一時ファイルは鍵ごとに分かれているので、似た鍵を同時に書いても値が混ざりません。
- 錠には**持ち主の印**を書きます。取れないときは合計 12 秒まで試し直し、10 秒より
  古い錠は外します。手放すときは自分の印のときだけ消すので、2 つが同時に錠を
  持つことはありません。

## 無いもの

Redis や Memcached はありません。`file` を使い、全プロセスで同じ場所を見てください。
タグ（`Cache::tags()`）の代わりに、鍵に頭を付けます（`user:1:posts`）。
ほかにまだ無いものは [backlog.md](backlog.md) にまとめてあります。

## 関連

- [scheduling.md](scheduling.md) — 定期処理。前回の時刻はキャッシュではなくファイルに置きます
- [session.md](session.md) — セッション。置き場所の考え方は同じです
- [configuration.md](configuration.md) — `config/*.rs` と `.env`
