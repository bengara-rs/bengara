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

use crate::error::{Error, Result};
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
/// 1回目だけ効きます。2回目以降は何もしません。
pub fn install_store(store: Arc<dyn CacheStore>) {
    let _ = STORE.set(store);
}

/// いま使う置き場所。
fn store() -> &'static Arc<dyn CacheStore> {
    STORE.get_or_init(|| {
        let config = crate::try_config::<CacheConfig>()
            .cloned()
            .unwrap_or_default();
        match config.driver.as_str() {
            "memory" | "array" => Arc::new(MemoryCache::new()) as Arc<dyn CacheStore>,
            "file" => Arc::new(FileCache::default_path()),
            other => {
                tracing::warn!("CACHE_DRIVER `{other}` は知りません。file を使います");
                Arc::new(FileCache::default_path())
            }
        }
    })
}

/// 鍵をファイル名に使える形にする。
///
/// 英数字と `. _ - :` だけはそのまま使います。それ以外が混じっていたら、
/// 鍵全体を SHA-256 にします（日本語の鍵でも動くように）。
pub(crate) fn normalize_key(key: &str) -> String {
    let safe = !key.is_empty()
        && key.len() <= 150
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':'));
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
        let store = store().clone();
        crate::support::blocking(move || store.put(&key, &value, Some(seconds))).await?
    }

    /// 期限なしで入れる。
    pub async fn forever(key: &str, value: impl Into<String>) -> Result<()> {
        let key = normalize_key(key);
        let value = value.into();
        let store = store().clone();
        crate::support::blocking(move || store.put(&key, &value, None)).await?
    }

    /// 読む。無い・期限切れなら `None`。
    pub async fn get(key: &str) -> Result<Option<String>> {
        let key = normalize_key(key);
        let store = store().clone();
        crate::support::blocking(move || store.get(&key)).await?
    }

    /// あるか。
    pub async fn has(key: &str) -> Result<bool> {
        Ok(Self::get(key).await?.is_some())
    }

    /// 消す。無くてもエラーにはしません。
    pub async fn forget(key: &str) -> Result<()> {
        let key = normalize_key(key);
        let store = store().clone();
        crate::support::blocking(move || store.forget(&key)).await?
    }

    /// 全部消す。
    pub async fn flush() -> Result<()> {
        let store = store().clone();
        crate::support::blocking(move || store.flush()).await?
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
    pub async fn increment(key: &str, by: i64) -> Result<i64> {
        let current = match Self::get(key).await? {
            Some(found) => found
                .trim()
                .parse::<i64>()
                .map_err(|_| Error::msg(format!("`{key}` の値は数ではありません")))?,
            None => 0,
        };
        let next = current + by;
        // 数は期限なしで持つ（回数の記録に使うため）。
        Self::forever(key, next.to_string()).await?;
        Ok(next)
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
    fn 空と長すぎる鍵もハッシュにする() {
        assert_eq!(normalize_key("").len(), 64);
        assert_eq!(normalize_key(&"a".repeat(200)).len(), 64);
    }

    #[test]
    fn 既定の置き場所はファイル() {
        assert_eq!(CacheConfig::default().driver, "file");
    }
}
