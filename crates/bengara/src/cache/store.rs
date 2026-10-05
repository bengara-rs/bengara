//! キャッシュの置き場所。
//!
//! 既定は `FileCache`（`storage/framework/cache/`）です。
//! テストやディスクを使いたくないときは `MemoryCache` を使えます。
//!
//! 形は `SessionStore`（Phase 2）にそろえてあります。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::{Error, Result};
use crate::support::time;

/// キャッシュの置き場所。
///
/// 自分で作るときは、次の3つを満たしてください。
///
/// - **複数のプロセスから同時に使われます。** プロセスのメモリだけに置かないでください。
/// - 期限切れは読み込みのときに捨ててください。
/// - 読み書きはブロックしてかまいません（`spawn_blocking` の中で呼びます）。
pub trait CacheStore: Send + Sync + 'static {
    /// 入れる。`seconds` が `None` なら期限なし。
    fn put(&self, key: &str, value: &str, seconds: Option<u64>) -> Result<()>;

    /// 読む。無い・期限切れは `None`。
    fn get(&self, key: &str) -> Result<Option<String>>;

    /// 消す。
    fn forget(&self, key: &str) -> Result<()>;

    /// 全部消す。
    fn flush(&self) -> Result<()>;
}

/// 1件ぶんの中身。ファイルにはこの形で入ります。
///
/// `期限|値` の 1 行です。期限が `0` なら期限なしです。
/// JSON にしないのは、値の中に改行があっても壊れないようにするためです。
struct Entry {
    expires_at: i64,
    value: String,
}

impl Entry {
    fn encode(value: &str, seconds: Option<u64>) -> String {
        let expires_at = match seconds {
            Some(seconds) => time::now_seconds() + seconds as i64,
            None => 0,
        };
        format!("{expires_at}|{value}")
    }

    fn decode(raw: &str) -> Option<Self> {
        let (expires, value) = raw.split_once('|')?;
        Some(Self {
            expires_at: expires.trim().parse().ok()?,
            value: value.to_string(),
        })
    }

    fn expired(&self) -> bool {
        self.expires_at != 0 && self.expires_at <= time::now_seconds()
    }
}

/// `storage/framework/cache/` にファイルとして置く。既定の置き場所です。
///
/// 1つの鍵が1つのファイルになります。
/// プロセスを2つ以上動かすときは、全プロセスで同じ場所を指してください。
pub struct FileCache {
    dir: PathBuf,
}

impl FileCache {
    /// 置き場所を決めて作る。
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// `storage/framework/cache/` に置く（既定）。
    pub fn default_path() -> Self {
        // join を 2 回に分けて、Windows でも区切りが混ざらないようにする。
        Self::new(crate::paths::storage_path("framework").join("cache"))
    }

    fn path_of(&self, key: &str) -> PathBuf {
        self.dir.join(key)
    }

    fn ensure_dir(&self) -> Result<()> {
        if self.dir.is_dir() {
            return Ok(());
        }
        // storage/ そのものが無いなら、作らずに知らせる（決定記録 #023）。
        let storage = crate::paths::storage_path("");
        if !storage.is_dir() {
            return Err(Error::msg(format!(
                "{} がありません。デプロイのときに storage/ を作ってください",
                storage.display()
            )));
        }
        std::fs::create_dir_all(&self.dir)?;
        Ok(())
    }
}

impl CacheStore for FileCache {
    fn put(&self, key: &str, value: &str, seconds: Option<u64>) -> Result<()> {
        self.ensure_dir()?;
        let path = self.path_of(key);
        // 書いている途中の中身を読まれないよう、別の名前で書いてから置き換える。
        let temp = path.with_extension("tmp");
        std::fs::write(&temp, Entry::encode(value, seconds))?;
        std::fs::rename(&temp, &path)?;
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<String>> {
        let path = self.path_of(key);
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::Io(e)),
        };
        let Some(entry) = Entry::decode(&raw) else {
            // 壊れていたら無かったことにする。落とすほどのことではない。
            tracing::warn!("キャッシュのファイルを読めませんでした: {}", path.display());
            let _ = std::fs::remove_file(&path);
            return Ok(None);
        };
        if entry.expired() {
            let _ = std::fs::remove_file(&path);
            return Ok(None);
        }
        Ok(Some(entry.value))
    }

    fn forget(&self, key: &str) -> Result<()> {
        match std::fs::remove_file(self.path_of(key)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Io(e)),
        }
    }

    fn flush(&self) -> Result<()> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(Error::Io(e)),
        };
        for entry in entries.flatten() {
            if entry.path().is_file() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        Ok(())
    }
}

/// プロセスのメモリに置く。**プロセスをまたいで共有されません。**
///
/// テストと、ディスクを使いたくないときに使います。
#[derive(Default)]
pub struct MemoryCache {
    entries: Mutex<HashMap<String, String>>,
}

impl MemoryCache {
    /// 空の置き場所を作る。
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl CacheStore for MemoryCache {
    fn put(&self, key: &str, value: &str, seconds: Option<u64>) -> Result<()> {
        self.lock()
            .insert(key.to_string(), Entry::encode(value, seconds));
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<String>> {
        let mut entries = self.lock();
        let Some(raw) = entries.get(key) else {
            return Ok(None);
        };
        let Some(entry) = Entry::decode(raw) else {
            entries.remove(key);
            return Ok(None);
        };
        if entry.expired() {
            entries.remove(key);
            return Ok(None);
        }
        Ok(Some(entry.value))
    }

    fn forget(&self, key: &str) -> Result<()> {
        self.lock().remove(key);
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        self.lock().clear();
        Ok(())
    }
}

/// 期限切れのファイルを消す。`cache:clear` と `cache:prune` が使います。
pub(crate) fn sweep_expired(dir: &Path) -> Result<usize> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(Error::Io(e)),
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let expired = match std::fs::read_to_string(&path) {
            Ok(raw) => Entry::decode(&raw).map(|e| e.expired()).unwrap_or(true),
            // 読めないものは消す対象にする。
            Err(_) => true,
        };
        if expired && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 中身の形は期限と値に分かれる() {
        let raw = Entry::encode("値", Some(60));
        let entry = Entry::decode(&raw).expect("読める");
        assert_eq!(entry.value, "値");
        assert!(!entry.expired());
        assert!(entry.expires_at > time::now_seconds());

        // 期限なしは 0。
        let raw = Entry::encode("値", None);
        assert!(raw.starts_with("0|"));
        assert!(!Entry::decode(&raw).unwrap().expired());
    }

    #[test]
    fn 改行を含む値も壊れない() {
        let raw = Entry::encode("1行目\n2行目|区切りも入る", Some(10));
        let entry = Entry::decode(&raw).unwrap();
        assert_eq!(entry.value, "1行目\n2行目|区切りも入る");
    }

    #[test]
    fn 壊れた中身はnoneになる() {
        assert!(Entry::decode("区切りなし").is_none());
        assert!(Entry::decode("abc|値").is_none(), "期限が数でない");
    }

    #[test]
    fn 期限切れの判定() {
        let entry = Entry {
            expires_at: time::now_seconds() - 1,
            value: "古い".into(),
        };
        assert!(entry.expired());
    }

    #[test]
    fn メモリの置き場所は出し入れできる() {
        let store = MemoryCache::new();
        store.put("a", "1", Some(60)).unwrap();
        assert_eq!(store.get("a").unwrap().as_deref(), Some("1"));
        assert_eq!(store.get("none").unwrap(), None);

        store.forget("a").unwrap();
        assert_eq!(store.get("a").unwrap(), None);

        store.put("b", "2", None).unwrap();
        store.flush().unwrap();
        assert_eq!(store.get("b").unwrap(), None);
    }

    #[test]
    fn メモリの置き場所は期限切れを捨てる() {
        let store = MemoryCache::new();
        // 期限を過去にして入れる。
        store
            .entries
            .lock()
            .unwrap()
            .insert("old".into(), format!("{}|値", time::now_seconds() - 10));
        assert_eq!(store.get("old").unwrap(), None, "期限切れは読めない");
        assert!(
            !store.entries.lock().unwrap().contains_key("old"),
            "読んだときに捨てる"
        );
    }

    #[test]
    fn ファイルの置き場所は出し入れできる() {
        let dir = std::env::temp_dir().join(format!("bengara-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = FileCache::new(&dir);

        store.put("a", "1", Some(60)).unwrap();
        assert_eq!(store.get("a").unwrap().as_deref(), Some("1"));
        assert_eq!(store.get("none").unwrap(), None);

        store.forget("a").unwrap();
        assert_eq!(store.get("a").unwrap(), None);
        store.forget("a").unwrap();

        // 期限切れは読めず、ファイルも消える。
        store.put("old", "古い", Some(0)).unwrap();
        assert_eq!(store.get("old").unwrap(), None);
        assert!(!dir.join("old").exists());

        store.put("b", "2", None).unwrap();
        assert_eq!(sweep_expired(&dir).unwrap(), 0, "期限なしは消さない");
        store.flush().unwrap();
        assert_eq!(store.get("b").unwrap(), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 期限切れの掃除() {
        let dir = std::env::temp_dir().join(format!("bengara-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = FileCache::new(&dir);

        store.put("keep", "1", Some(600)).unwrap();
        store.put("gone", "2", Some(0)).unwrap();
        std::fs::write(dir.join("broken"), "こわれた中身").unwrap();

        assert_eq!(sweep_expired(&dir).unwrap(), 2, "期限切れと壊れたもの");
        assert!(dir.join("keep").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 無いディレクトリの掃除は0件() {
        assert_eq!(
            sweep_expired(Path::new("存在しないディレクトリ")).unwrap(),
            0
        );
    }
}
