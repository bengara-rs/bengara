//! キャッシュ。重い処理の結果を取っておきます。
//!
//! ```ignore
//! let total = Cache::remember("posts.count", 60, || async {
//!     Ok(Post::count().await?.to_string())
//! })
//! .await?;
//! ```
//!
//! 置き場所は**ファイル**（既定）と**メモリ**の2つです。`CACHE_DRIVER` で選びます。
//! 依存クレートは増やしていません。

pub(crate) mod store;

pub use store::{CacheStore, FileCache, MemoryCache};

use std::future::Future;
use std::sync::{Arc, OnceLock};

use crate::error::Result;
use crate::support::crypto;

/// キャッシュの設定。`config/cache.rs` が返します。
///
/// ```ignore
/// pub fn config() -> CacheConfig {
///     CacheConfig { driver: env("CACHE_DRIVER", "file") }
/// }
/// ```
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// 置き場所の名前（`file` / `memory`）。
    pub driver: String,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            driver: crate::env("CACHE_DRIVER", "file"),
        }
    }
}

static STORE: OnceLock<Arc<dyn CacheStore>> = OnceLock::new();

/// 置き場所を差し替える。`bootstrap/app.rs` かテストから呼びます。
///
/// 1回目だけ効きます。2回目以降は何もせず、警告を出します。
pub fn install_store(store: Arc<dyn CacheStore>) {
    if STORE.set(store).is_err() {
        tracing::warn!("キャッシュの置き場所は既に決まっています。この install_store は効きません");
    }
}

/// いま使う置き場所。
fn store() -> &'static Arc<dyn CacheStore> {
    STORE.get_or_init(|| {
        let driver = crate::try_config::<CacheConfig>()
            .map(|config| config.driver.clone())
            .unwrap_or_else(|| CacheConfig::default().driver);
        match driver.as_str() {
            "memory" | "array" => Arc::new(MemoryCache::new()) as Arc<dyn CacheStore>,
            "file" => Arc::new(FileCache::default_path()),
            other => {
                tracing::warn!("CACHE_DRIVER `{other}` は知りません。file を使います");
                Arc::new(FileCache::default_path())
            }
        }
    })
}

/// 置き場所を呼ぶ。ブロックするものだけ裏のスレッドへ回します。
async fn with_store<T, F>(f: F) -> Result<T>
where
    F: FnOnce(&dyn CacheStore) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let store = store().clone();
    if store.blocking() {
        crate::support::blocking(move || f(store.as_ref())).await?
    } else {
        // メモリだけの置き場所は、Mutex 1 回のために往復しない。
        f(store.as_ref())
    }
}

/// 鍵をファイル名に使える形にする。
///
/// **小文字の**英数字と `. _ -` だけはそのまま使います。それ以外が混じっていたら、
/// 鍵全体を SHA-256 にします（日本語の鍵でも動くように）。
///
/// 大文字をそのまま使わないのは、NTFS / APFS が大文字小文字を区別しないためです。
/// `User:1` と `user:1` を素直に写すと、Windows と macOS では同じファイルになり、
/// Linux では別のファイルになります。開発と本番で挙動が変わるのを避けるため、
/// **大文字を含む鍵はハッシュ側に回します**（鍵の見た目は読めなくなりますが、
/// どの OS でも必ず別の置き場所になります）。
///
/// ハッシュ側には `~` を付けます。接頭辞が無いと、64 文字の小文字 16 進は
/// そのまま素通しするので、**別の鍵の SHA-256 を計算してその 16 進を鍵として
/// 渡せば、狙って同じファイルを指せました**（衝突を探す必要もありません）。
///
/// **`:` を含む鍵もハッシュ側に回します。** 以前は `:` をファイル名に使えないので
/// `~` に置き換えていました。ところが `~` は NTFS が長い名前に付ける短い名前
/// （`<先頭6文字>~<数字>`）の目印そのものです。実機（Windows 10 / NTFS）では
/// 鍵 `posts.count.by.author` のファイルに短い名前 `POSTSC~1.AUT` が付き、
/// 鍵 `postsc:1.aut`（置換後 `postsc~1.aut`）から**読み・上書き・削除**ができました。
/// 素通し側に `~` を出さなければ、この道はふさがります
/// （`user:1` のファイル名が読めなくなるのは、大文字と同じ引き換えです）。
///
/// **末尾が `.` の鍵と装置の名前（`nul` など）もハッシュ側に回します。**
/// Windows は末尾の `.` を黙って落とすので、`post.` と `post` が同じファイルに
/// なります（`format!("post.{id}")` の `id` が空なら踏めます）。装置の名前は
/// 書いた中身が捨てられ、`con` は読み戻しで戻ってきません。判定は
/// `Storage` と同じ [`crate::storage::usable_name`] に任せます（決定記録 #074）。
pub(crate) fn normalize_key(key: &str) -> String {
    let safe = !key.is_empty()
        && key.len() <= 150
        // `:` を含む鍵はハッシュ側に回す。置換先の `~` が NTFS の短い名前と
        // 区別できないため（上の説明）。
        && !key.contains(':')
        // 末尾が `.` の名前と装置の名前を断る（`.` と `..` もここで外れる）。
        // `~1` のような短い名前の形もここで外れる。
        && crate::storage::usable_name(key).is_ok()
        && key.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        });
    if safe {
        // 通した文字に `~` は入っていないので、そのまま使える。
        key.to_string()
    } else {
        format!("~{}", crypto::to_hex(&crypto::sha256(key.as_bytes())))
    }
}

/// キャッシュの入口。Laravel の `Cache` に当たります。
///
/// 値は**文字列**です。構造体は `put_json` / `get_json` を使ってください。
pub struct Cache;

impl Cache {
    /// 入れる。`seconds` 秒たつと消えます。
    pub async fn put(key: &str, value: impl Into<String>, seconds: u64) -> Result<()> {
        let key = normalize_key(key);
        let value = value.into();
        with_store(move |store| store.put(&key, &value, Some(seconds))).await
    }

    /// 期限なしで入れる。
    pub async fn forever(key: &str, value: impl Into<String>) -> Result<()> {
        let key = normalize_key(key);
        let value = value.into();
        with_store(move |store| store.put(&key, &value, None)).await
    }

    /// 読む。無い・期限切れなら `None`。
    pub async fn get(key: &str) -> Result<Option<String>> {
        let key = normalize_key(key);
        with_store(move |store| store.get(&key)).await
    }

    /// あるか。
    ///
    /// 値そのものは返しません。`file` は**ファイルの中身を読みません**
    /// （期限が入っている先頭だけを読みます）。`memory` は既定の実装のまま
    /// メモリの値を見ますが、ディスクには触りません。
    pub async fn has(key: &str) -> Result<bool> {
        let key = normalize_key(key);
        with_store(move |store| store.has(&key)).await
    }

    /// 消す。無くてもエラーにはしません。
    pub async fn forget(key: &str) -> Result<()> {
        let key = normalize_key(key);
        with_store(move |store| store.forget(&key)).await
    }

    /// 全部消す。
    pub async fn flush() -> Result<()> {
        with_store(|store| store.flush()).await
    }

    /// 期限切れのものだけ消して、消した件数を返す（`cache:prune`）。
    ///
    /// 掃除の仕組みが無い置き場所（`memory` など）では `None` を返します。
    pub async fn prune() -> Result<Option<usize>> {
        with_store(|store| {
            if store.prunable() {
                store.prune().map(Some)
            } else {
                Ok(None)
            }
        })
        .await
    }

    /// いまの置き場所を人に見せる言い方（コマンドの表示用）。
    pub fn location() -> Option<String> {
        store().location()
    }

    /// 無ければ作って入れる。あればそれを返す。
    ///
    /// ```ignore
    /// let value = Cache::remember("key", 60, || async { Ok("作った値".to_string()) }).await?;
    /// ```
    pub async fn remember<F, Fut>(key: &str, seconds: u64, make: F) -> Result<String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<String>>,
    {
        if let Some(found) = Self::get(key).await? {
            return Ok(found);
        }
        let value = make().await?;
        Self::put(key, value.clone(), seconds).await?;
        Ok(value)
    }

    /// 数を足す。無ければ 0 から始めます。返るのは足した後の値です。
    ///
    /// 置き場所の `increment` に任せるので、`file` と `memory` では
    /// 同時に呼ばれても数を落としません。
    ///
    /// **期限は引き継ぎません。** `Cache::put("n", "1", 60)` の後に `increment` を
    /// 呼ぶと、期限が外れて期限なしになります（回数の記録に使う前提です）。
    /// Laravel の `increment` は元の期限を保つので、そこは挙動が違います。
    pub async fn increment(key: &str, by: i64) -> Result<i64> {
        let key = normalize_key(key);
        with_store(move |store| store.increment(&key, by)).await
    }

    /// 数を引く。
    ///
    /// `by` が `i64::MIN` のときは `-by` が `i64` に収まらないので、`i64::MAX` に
    /// 丸めます（`-by` と書くとここでパニックしていました）。
    /// 足し算も下限で止まるので、引きすぎても `i64::MIN` より下には行きません。
    pub async fn decrement(key: &str, by: i64) -> Result<i64> {
        Self::increment(key, by.saturating_neg()).await
    }

    /// `serde` の型を入れる。
    pub async fn put_json<T: serde::Serialize>(key: &str, value: &T, seconds: u64) -> Result<()> {
        let text = serde_json::to_string(value)?;
        Self::put(key, text, seconds).await
    }

    /// `serde` の型を読む。形が変わっていたら `None` にします。
    pub async fn get_json<T: serde::de::DeserializeOwned>(key: &str) -> Result<Option<T>> {
        let Some(text) = Self::get(key).await? else {
            return Ok(None);
        };
        match serde_json::from_str(&text) {
            Ok(value) => Ok(Some(value)),
            Err(e) => {
                // 型を変えた後に古い値が残っていることがある。捨てて作り直させる。
                tracing::warn!("キャッシュ `{key}` を読めませんでした: {e}");
                Self::forget(key).await?;
                Ok(None)
            }
        }
    }

    /// 無ければ作って入れる（`serde` の型）。
    pub async fn remember_json<T, F, Fut>(key: &str, seconds: u64, make: F) -> Result<T>
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        if let Some(found) = Self::get_json::<T>(key).await? {
            return Ok(found);
        }
        let value = make().await?;
        Self::put_json(key, &value, seconds).await?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 鍵はそのまま使える形に直す() {
        assert_eq!(normalize_key("posts.count"), "posts.count");
        assert_eq!(normalize_key("a_b-c.d"), "a_b-c.d");
        // `:` はファイル名に使えないので、鍵ごとハッシュ側に回す。
        assert_eq!(normalize_key("user:1").len(), 65);
    }

    #[test]
    fn 素通しした鍵に短い名前の印は出ない() {
        // `~` は NTFS が長い名前に付ける短い名前（`<先頭6文字>~<数字>`）の印。
        // 素通し側に出すと、別の鍵が同じファイルを指せる。
        for key in [
            "posts.count.by.author",
            "user:1",
            "a:b:c",
            "post.1",
            "a_b-c.d",
            "日本語の鍵",
            "User:1",
            ":1",
            "~1",
        ] {
            let normalized = normalize_key(key);
            let body = normalized.strip_prefix('~').unwrap_or(&normalized);
            assert!(!body.contains('~'), "{key} -> {normalized}");
        }
    }

    #[test]
    fn 短い名前に化ける鍵は別の置き場所になる() {
        // 実機（Windows 10 / NTFS）で、`posts.count.by.author` のファイルには
        // 短い名前 `POSTSC~1.AUT` が付いた。`:` を `~` に置き換えていたころは、
        // 鍵 `postsc:1.aut` から同じファイルを読み・上書き・削除できた。
        let long = normalize_key("posts.count.by.author");
        let short = normalize_key("postsc:1.aut");
        assert_eq!(long, "posts.count.by.author", "素通しのまま");
        assert_eq!(short.len(), 65, "ハッシュ側に回る");
        assert!(!short.contains("postsc"), "短い名前に化けられない");
        assert_ne!(long, short);
    }

    #[test]
    fn 使えない文字の鍵はハッシュにする() {
        let hashed = normalize_key("日本語の鍵");
        assert_eq!(hashed.len(), 65, "`~` と SHA-256 の16進");
        let hex = hashed.strip_prefix('~').expect("`~` で始まる");
        assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()));
        // 同じ鍵なら同じ結果になる。
        assert_eq!(hashed, normalize_key("日本語の鍵"));
        assert_ne!(hashed, normalize_key("別の鍵"));
    }

    #[test]
    fn ハッシュの16進を鍵にしても同じ置き場所にならない() {
        // 接頭辞が無かったときは、別の鍵の SHA-256 の16進をそのまま鍵として
        // 渡せば、狙って同じファイルを指せた。
        let target = normalize_key("日本語の鍵");
        let hex = target.trim_start_matches('~');
        assert_ne!(normalize_key(hex), target);
        // 16進の鍵は素通しされる（ハッシュ側の名前にはならない）。
        assert_eq!(normalize_key(hex), hex);

        // `:` 始まりの鍵も、`~` 始まりのハッシュとぶつからない。
        let colon = normalize_key(&format!(":{hex}"));
        assert_ne!(colon, target);
        assert_eq!(colon.len(), 65, "ハッシュ側に回る");
        assert_ne!(colon, format!("~{hex}"));
    }

    #[test]
    fn 大文字小文字で別の置き場所になる() {
        // NTFS / APFS は大文字小文字を区別しないので、大文字を含む鍵はハッシュにする。
        let upper = normalize_key("User:1");
        let lower = normalize_key("user:1");
        assert_eq!(upper.len(), 65, "ハッシュにする");
        // `:` が入るので、小文字のほうもハッシュ側に回る。
        assert_eq!(lower.len(), 65);
        assert_ne!(upper, lower);
        // 大文字小文字を無視してもぶつからない。
        assert_ne!(upper.to_ascii_lowercase(), lower.to_ascii_lowercase());

        // `:` が入らない鍵で、大文字だけの違いを押さえる。
        assert_eq!(normalize_key("user.1"), "user.1", "小文字はそのまま");
        assert_eq!(normalize_key("User.1").len(), 65, "ハッシュにする");
    }

    #[test]
    fn 短い名前に化ける鍵でも両方が生き残る() {
        let dir = std::env::temp_dir().join(format!("bengara-shortname-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = FileCache::new(&dir);

        // 実機では `posts.count.by.author` に短い名前 `POSTSC~1.AUT` が付いた。
        let long = normalize_key("posts.count.by.author");
        let short = normalize_key("postsc:1.aut");
        store.put(&long, "長い名前の値", Some(60)).unwrap();
        store.put(&short, "別の鍵の値", Some(60)).unwrap();

        // 前は短い名前ごしに同じファイルを読み・上書きしていた。
        assert_eq!(store.get(&long).unwrap().as_deref(), Some("長い名前の値"));
        assert_eq!(store.get(&short).unwrap().as_deref(), Some("別の鍵の値"));

        // 片方を消しても、もう片方は残る。
        store.forget(&short).unwrap();
        assert_eq!(store.get(&long).unwrap().as_deref(), Some("長い名前の値"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 末尾が点の鍵はハッシュにする() {
        // Windows は末尾の `.` を落とすので、素通しすると `post.` と `post` が
        // 同じファイルになる（実機で片方がもう片方を上書きした）。
        let dotted = normalize_key("post.");
        assert_eq!(dotted.len(), 65, "ハッシュ側に回る");
        assert_ne!(dotted, normalize_key("post"));
        assert_eq!(normalize_key("post"), "post");
        // `.` と `..` も一緒に直る。
        assert_eq!(normalize_key(".").len(), 65);
        assert_eq!(normalize_key("..").len(), 65);
        assert_ne!(normalize_key("."), normalize_key(".."));
        // 途中の `.` はそのまま。
        assert_eq!(normalize_key("post.1"), "post.1");
    }

    #[test]
    fn 装置の名前の鍵はハッシュにする() {
        // `nul` は書いた中身が捨てられ、`con` は読み戻しで戻ってこない。
        for key in ["nul", "con", "COM1", "nul.txt", "aux", "prn", "lpt9"] {
            assert_eq!(normalize_key(key).len(), 65, "{key}");
        }
        // 装置の名前を含むだけの鍵は素通しのまま。
        assert_eq!(normalize_key("console"), "console");
        assert_eq!(normalize_key("nul-1"), "nul-1");
    }

    #[test]
    fn 空と長すぎる鍵もハッシュにする() {
        assert_eq!(normalize_key("").len(), 65);
        assert_eq!(normalize_key(&"a".repeat(200)).len(), 65);
    }

    #[test]
    fn 正規化した鍵は一時ファイルの印を含まない() {
        // `cache/store.rs` の `TEMP_MARK` と `LOCK_DIR` は `+` を使っています。
        // 素通し側にもハッシュ側にも `+` が出ないことを押さえます。
        for key in [
            "a.+tmp.1",
            "+locks",
            "locks",
            "report.2024.12",
            "日本語の鍵",
            "User:1",
        ] {
            assert!(!normalize_key(key).contains('+'), "{key}");
        }
    }

    #[test]
    fn 引く数の符号を反転してもあふれない() {
        // `Cache::decrement` は `-by` を `increment` に渡します。
        // `i64::MIN` のときに `-by` と書くとパニックしていました。
        assert_eq!(i64::MIN.saturating_neg(), i64::MAX);
        assert_eq!(5_i64.saturating_neg(), -5);
        // 足す側も飽和するので、引きすぎても下限で止まります。
        assert_eq!(i64::MIN.saturating_add(i64::MIN), i64::MIN);
    }

    #[test]
    fn 既定の置き場所はファイル() {
        assert_eq!(CacheConfig::default().driver, "file");
    }
}
