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
/// **小文字の**英数字と `. _ - :` だけはそのまま使います。それ以外が混じっていたら、
/// 鍵全体を SHA-256 にします（日本語の鍵でも動くように）。
///
/// 大文字をそのまま使わないのは、NTFS / APFS が大文字小文字を区別しないためです。
/// `User:1` と `user:1` を素直に写すと、Windows と macOS では同じファイルになり、
/// Linux では別のファイルになります。開発と本番で挙動が変わるのを避けるため、
/// **大文字を含む鍵はハッシュ側に回します**（鍵の見た目は読めなくなりますが、
/// どの OS でも必ず別の置き場所になります）。
pub(crate) fn normalize_key(key: &str) -> String {
    let safe = !key.is_empty()
        && key.len() <= 150
        && key.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-' | b':')
        });
    if safe {
        // `:` は Windows のファイル名に使えないので、置き場所側で避ける。
        key.replace(':', "~")
    } else {
        crypto::to_hex(&crypto::sha256(key.as_bytes()))
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
    /// 値そのものは返しません（置き場所が対応していれば、中身を読まずに答えます）。
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
    pub async fn decrement(key: &str, by: i64) -> Result<i64> {
        Self::increment(key, -by).await
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
        // `:` はファイル名に使えないので置き換える。
        assert_eq!(normalize_key("user:1"), "user~1");
    }

    #[test]
    fn 使えない文字の鍵はハッシュにする() {
        let hashed = normalize_key("日本語の鍵");
        assert_eq!(hashed.len(), 64, "SHA-256 の16進");
        assert!(hashed.bytes().all(|b| b.is_ascii_hexdigit()));
        // 同じ鍵なら同じ結果になる。
        assert_eq!(hashed, normalize_key("日本語の鍵"));
        assert_ne!(hashed, normalize_key("別の鍵"));
    }

    #[test]
    fn 大文字小文字で別の置き場所になる() {
        // NTFS / APFS は大文字小文字を区別しないので、大文字を含む鍵はハッシュにする。
        let upper = normalize_key("User:1");
        let lower = normalize_key("user:1");
        assert_eq!(upper.len(), 64, "ハッシュにする");
        assert_eq!(lower, "user~1", "小文字はそのまま");
        assert_ne!(upper, lower);
        // 大文字小文字を無視してもぶつからない。
        assert_ne!(upper.to_ascii_lowercase(), lower.to_ascii_lowercase());
    }

    #[test]
    fn 空と長すぎる鍵もハッシュにする() {
        assert_eq!(normalize_key("").len(), 64);
        assert_eq!(normalize_key(&"a".repeat(200)).len(), 64);
    }

    #[test]
    fn 既定の置き場所はファイル() {
        assert_eq!(CacheConfig::default().driver, "file");
    }
}
