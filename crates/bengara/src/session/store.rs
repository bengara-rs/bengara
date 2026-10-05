//! セッションの置き場所。
//!
//! 既定は `FileStore`（`storage/framework/sessions/`）です。
//! テストや、ディスクを使いたくないときは `MemoryStore` を使えます。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};

type Data = BTreeMap<String, String>;

/// セッションの中身を置いておく場所。
///
/// 自分で作るときは、次の3つを満たしてください。
///
/// - **複数のプロセスから同時に使われます。** プロセスのメモリだけに置かないでください。
/// - 期限切れは読み込みのときに捨ててください。
/// - 読み書きはブロックしてかまいません（`spawn_blocking` の中で呼びます）。
pub trait SessionStore: Send + Sync + 'static {
    /// ID から中身を読む。無ければ `None`。
    fn read(&self, id: &str) -> Result<Option<Data>>;

    /// 中身を書く。
    fn write(&self, id: &str, data: &Data) -> Result<()>;

    /// 中身を消す。
    fn destroy(&self, id: &str) -> Result<()>;
}

/// `storage/framework/sessions/` にファイルとして置く。既定の置き場所です。
///
/// 1つのセッションが1つのファイルになります。
/// プロセスを2つ以上動かすときは、全プロセスで同じ場所を指してください。
pub struct FileStore {
    dir: PathBuf,
    lifetime_secs: u64,
}

impl FileStore {
    /// 置き場所と、放っておいて消えるまでの秒数を決めて作る。
    pub fn new(dir: impl Into<PathBuf>, lifetime_secs: u64) -> Self {
        Self {
            dir: dir.into(),
            lifetime_secs,
        }
    }

    /// `storage/framework/sessions/` に置く（既定）。
    pub fn default_path(lifetime_secs: u64) -> Self {
        Self::new(
            crate::paths::storage_path("framework/sessions"),
            lifetime_secs,
        )
    }

    /// ID からファイルの場所を決める。
    ///
    /// ID は自分で作った16進の文字列ですが、外から来た値でもあるので念のため確かめます。
    /// `..` や `/` が混じったファイル名を作らせないためです。
    fn path_of(&self, id: &str) -> Result<PathBuf> {
        if id.is_empty()
            || id.len() > 128
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(Error::msg("セッション ID の形が正しくありません"));
        }
        Ok(self.dir.join(id))
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

impl SessionStore for FileStore {
    fn read(&self, id: &str) -> Result<Option<Data>> {
        let path = self.path_of(id)?;
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::Io(e)),
        };
        let stored: Stored = match serde_json::from_str(&raw) {
            Ok(stored) => stored,
            // 壊れていたら無かったことにする。落とすほどのことではない。
            Err(_) => {
                tracing::warn!("セッションのファイルを読めませんでした: {}", path.display());
                let _ = std::fs::remove_file(&path);
                return Ok(None);
            }
        };
        if stored.expires_at <= now() {
            let _ = std::fs::remove_file(&path);
            return Ok(None);
        }
        Ok(Some(stored.data))
    }

    fn write(&self, id: &str, data: &Data) -> Result<()> {
        let path = self.path_of(id)?;
        self.ensure_dir()?;
        let stored = Stored {
            expires_at: now() + self.lifetime_secs,
            data: data.clone(),
        };
        let body = serde_json::to_string(&stored)?;

        // 書いている途中のファイルを読まれないように、別名で書いてから置き換える。
        let temp = path.with_extension("tmp");
        std::fs::write(&temp, body)?;
        std::fs::rename(&temp, &path)?;
        Ok(())
    }

    fn destroy(&self, id: &str) -> Result<()> {
        let path = self.path_of(id)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Io(e)),
        }
    }
}

/// プロセスのメモリに置く。**テスト専用です。**
///
/// プロセスをまたげないので、本番では使えません
/// （[340_方針_本番運用] のとおり、本番は複数のプロセスで動かします）。
pub struct MemoryStore {
    entries: Mutex<BTreeMap<String, (u64, Data)>>,
    lifetime_secs: u64,
}

impl MemoryStore {
    /// 放っておいて消えるまでの秒数を決めて作る。
    pub fn new(lifetime_secs: u64) -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
            lifetime_secs,
        }
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new(7200)
    }
}

impl SessionStore for MemoryStore {
    fn read(&self, id: &str) -> Result<Option<Data>> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        match entries.get(id) {
            Some((expires_at, _)) if *expires_at <= now() => {
                entries.remove(id);
                Ok(None)
            }
            Some((_, data)) => Ok(Some(data.clone())),
            None => Ok(None),
        }
    }

    fn write(&self, id: &str, data: &Data) -> Result<()> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.insert(id.to_string(), (now() + self.lifetime_secs, data.clone()));
        Ok(())
    }

    fn destroy(&self, id: &str) -> Result<()> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.remove(id);
        Ok(())
    }
}

/// ファイルに書く形。
#[derive(serde::Serialize, serde::Deserialize)]
struct Stored {
    expires_at: u64,
    data: Data,
}

/// 1970 年からの秒数。
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 期限切れのセッションを消す（`session:gc` から呼びます）。
pub(crate) fn sweep(dir: &Path) -> Result<usize> {
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
        let expired = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Stored>(&raw).ok())
            .map(|stored| stored.expires_at <= now())
            // 読めないファイルも捨てる。
            .unwrap_or(true);
        if expired && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(pairs: &[(&str, &str)]) -> Data {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bengara-session-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn メモリに置いて読み書きできる() {
        let store = MemoryStore::new(60);
        assert!(store.read("missing").unwrap().is_none());

        store.write("abc", &data(&[("a", "1")])).unwrap();
        assert_eq!(store.read("abc").unwrap(), Some(data(&[("a", "1")])));

        store.destroy("abc").unwrap();
        assert!(store.read("abc").unwrap().is_none());
    }

    #[test]
    fn メモリの期限が切れたら読めない() {
        let store = MemoryStore::new(0);
        store.write("abc", &data(&[("a", "1")])).unwrap();
        assert!(store.read("abc").unwrap().is_none(), "すぐ期限切れ");
    }

    #[test]
    fn ファイルに置いて読み書きできる() {
        let dir = temp_dir("rw");
        let store = FileStore::new(&dir, 60);

        assert!(store.read("abc").unwrap().is_none());
        store
            .write("abc", &data(&[("a", "1"), ("b", "2")]))
            .unwrap();
        assert!(dir.join("abc").is_file());
        assert_eq!(
            store.read("abc").unwrap(),
            Some(data(&[("a", "1"), ("b", "2")]))
        );

        store.destroy("abc").unwrap();
        assert!(store.read("abc").unwrap().is_none());
        // 2回消しても落ちない。
        store.destroy("abc").unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ファイルの期限が切れたら読めずに消える() {
        let dir = temp_dir("expire");
        let store = FileStore::new(&dir, 0);
        store.write("abc", &data(&[("a", "1")])).unwrap();
        assert!(store.read("abc").unwrap().is_none());
        assert!(!dir.join("abc").exists(), "期限切れのファイルは消す");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 壊れたファイルは無かったことにする() {
        let dir = temp_dir("broken");
        let store = FileStore::new(&dir, 60);
        std::fs::write(dir.join("abc"), "これは JSON ではない").unwrap();
        assert!(store.read("abc").unwrap().is_none());
        assert!(!dir.join("abc").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 危ないidは断る() {
        let dir = temp_dir("unsafe-id");
        let store = FileStore::new(&dir, 60);
        for bad in ["", "../etc/passwd", "a/b", "a\\b", "a.b", &"x".repeat(129)] {
            assert!(store.read(bad).is_err(), "`{bad}` は断るはず");
            assert!(store.write(bad, &data(&[])).is_err());
        }
        // 正しい形は通る。
        assert!(store.read("abc123").is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 期限切れだけを掃除する() {
        let dir = temp_dir("sweep");
        FileStore::new(&dir, 0)
            .write("old", &data(&[("a", "1")]))
            .unwrap();
        FileStore::new(&dir, 3600)
            .write("new", &data(&[("a", "1")]))
            .unwrap();

        let removed = sweep(&dir).unwrap();
        assert_eq!(removed, 1);
        assert!(!dir.join("old").exists());
        assert!(dir.join("new").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 無い場所を掃除しても落ちない() {
        let dir = std::env::temp_dir().join("bengara-session-test-missing");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(sweep(&dir).unwrap(), 0);
    }

    #[test]
    fn 書き換えても一時ファイルが残らない() {
        let dir = temp_dir("tmp");
        let store = FileStore::new(&dir, 60);
        store.write("abc", &data(&[("a", "1")])).unwrap();
        store.write("abc", &data(&[("a", "2")])).unwrap();
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["abc".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
