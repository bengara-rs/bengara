//! キャッシュの置き場所。
//!
//! 既定は `FileCache`（`storage/framework/cache/`）です。
//! テストやディスクを使いたくないときは `MemoryCache` を使えます。
//!
//! 形は `SessionStore`（Phase 2）にそろえてあります。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::error::{Error, Result};
use crate::support::lock::{is_stale, FileLock};
use crate::support::time;

/// キャッシュの置き場所。
///
/// 自分で作るときは、次の3つを満たしてください。
///
/// - **複数のプロセスから同時に使われます。** プロセスのメモリだけに置かないでください。
/// - 期限切れは読み込みのときに捨ててください。
/// - 読み書きはブロックしてかまいません（`spawn_blocking` の中で呼びます）。
///
/// `increment` 以下には既定の実装が付いています。足しても、既にある実装は壊れません。
pub trait CacheStore: Send + Sync + 'static {
    /// 入れる。`seconds` が `None` なら期限なし。
    fn put(&self, key: &str, value: &str, seconds: Option<u64>) -> Result<()>;

    /// 読む。無い・期限切れは `None`。
    fn get(&self, key: &str) -> Result<Option<String>>;

    /// 消す。
    fn forget(&self, key: &str) -> Result<()>;

    /// 全部消す。
    fn flush(&self) -> Result<()>;

    /// あるか。
    ///
    /// 既定の実装は値を読んで捨てます。値が大きいときは自分で実装してください。
    fn has(&self, key: &str) -> Result<bool> {
        Ok(self.get(key)?.is_some())
    }

    /// 数を足して、足した後の値を返す。無ければ 0 から始めます。
    ///
    /// **既定の実装は「読む → 足す → 書く」の3手に分かれているので、
    /// 同時に呼ばれると数を落とします。** 正しく数えたい置き場所は、
    /// 自分で排他を取って実装してください。
    ///
    /// **期限は引き継ぎません**（期限なしで書き直します）。回数の記録に使う前提です。
    fn increment(&self, key: &str, by: i64) -> Result<i64> {
        let current = parse_number(key, self.get(key)?)?;
        let next = current + by;
        self.put(key, &next.to_string(), None)?;
        Ok(next)
    }

    /// 読み書きがブロックするか。
    ///
    /// `true`（既定）のときだけ、裏のスレッド（`spawn_blocking`）を経由します。
    /// メモリだけで済む置き場所は `false` にすると、無駄な往復がなくなります。
    fn blocking(&self) -> bool {
        true
    }

    /// 期限切れのものだけ消して、消した件数を返す（`cache:prune`）。
    ///
    /// 既定の実装は何もしません。[`CacheStore::prunable`] も合わせて `true` にしてください。
    fn prune(&self) -> Result<usize> {
        Ok(0)
    }

    /// 期限切れの掃除ができるか。`cache:prune` の表示を変えるために見ます。
    fn prunable(&self) -> bool {
        false
    }

    /// 置き場所を人に見せる言い方（コマンドの表示用）。言えないときは `None`。
    fn location(&self) -> Option<String> {
        None
    }
}

/// キャッシュに入っている文字を数として読む。
fn parse_number(key: &str, raw: Option<String>) -> Result<i64> {
    match raw {
        Some(found) => found
            .trim()
            .parse::<i64>()
            .map_err(|_| Error::msg(format!("`{key}` の値は数ではありません"))),
        None => Ok(0),
    }
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
            // **あふれさせません。** `as i64` だと `u64::MAX` が `-1` になって
            // 即時失効し、`i64::MAX` の近くはデバッグビルドでパニックします。
            // 上限で切り詰めてから飽和加算します。
            Some(seconds) => {
                let seconds = i64::try_from(seconds).unwrap_or(i64::MAX);
                time::now_seconds().saturating_add(seconds)
            }
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

/// 書き込み途中の一時ファイルに付ける連番。
///
/// 鍵が違えば一時ファイルも違う名前になるように、プロセス番号と合わせて使います。
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// 一時ファイルの印。**`normalize_key` が絶対に作らない形**にします。
///
/// `normalize_key` が通すのは小文字英数字と `. _ - ~` だけなので、`+` は出てきません。
/// 「後ろ2つが数字」で見分けていたときは、`report.2024.12` のような鍵の本体を
/// 一時ファイルと取り違えていました（`cache:clear` で消えず、`cache:prune` では
/// 期限が残っていても消えていました）。
const TEMP_MARK: &str = ".+tmp.";

/// 錠ファイルを置くディレクトリの名前。**`normalize_key` が絶対に作らない形**にします。
///
/// `locks` のような普通の名前だと、鍵 `locks` の本体とぶつかって
/// `Cache::get("locks")` がディレクトリを読もうとして必ずエラーになりました。
/// 大文字（`LOCKS`）では直りません。NTFS と APFS は大文字小文字を区別しないので、
/// Windows と macOS では鍵 `locks` と同じ名前のままです。
const LOCK_DIR: &str = "+locks";

/// 一時ファイルがこの秒数より古ければ、書き込みに失敗した残骸と見なして消す。
const TEMP_STALE_SECS: u64 = 300;

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
        Self::new(Self::default_dir())
    }

    /// 既定の置き場所。**パスの定義はここ1か所だけです。**
    fn default_dir() -> PathBuf {
        // join を 2 回に分けて、Windows でも区切りが混ざらないようにする。
        crate::paths::storage_path("framework").join("cache")
    }

    fn path_of(&self, key: &str) -> PathBuf {
        self.dir.join(key)
    }

    /// 書き込み途中に使う名前。
    ///
    /// `<鍵>.+tmp.<プロセス番号>.<連番>` にします。`with_extension("tmp")` は
    /// 最後の `.` 以降を置き換えるので、`test.put` と `test.count` が同じ
    /// `test.tmp` を共有してしまいます。プロセス番号と連番を足して、
    /// 鍵をまたいでも重ならない名前にします。
    ///
    /// 印（[`TEMP_MARK`]）を入れるのは、利用者の鍵と確実に見分けるためです。
    fn temp_path_of(&self, key: &str) -> PathBuf {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        self.dir
            .join(format!("{key}{TEMP_MARK}{}.{seq}", std::process::id()))
    }

    /// 1つの鍵を守る錠ファイルの場所。
    ///
    /// 本体と混ざらないよう、別のディレクトリ（[`LOCK_DIR`]）に置きます
    /// （`read_dir` ではファイルだけを見ているので、掃除の対象にもなりません）。
    fn lock_path_of(&self, key: &str) -> PathBuf {
        self.dir.join(LOCK_DIR).join(format!("{key}.lock"))
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
        let temp = self.temp_path_of(key);
        std::fs::write(&temp, Entry::encode(value, seconds))?;
        if let Err(e) = std::fs::rename(&temp, &path) {
            // 置き換えに失敗したら、書きかけを残さない。
            let _ = std::fs::remove_file(&temp);
            return Err(Error::Io(e));
        }
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
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            // 別のプロセスが書いている途中の一時ファイルは消さない。
            // 消すと、向こうの `rename` が NotFound で失敗します。
            if is_temp_path(&path) && !is_stale(&path, TEMP_STALE_SECS) {
                continue;
            }
            let _ = std::fs::remove_file(&path);
        }
        Ok(())
    }

    fn has(&self, key: &str) -> Result<bool> {
        // 中身を読まないと期限が分からないので、`get` と同じ手順で見る。
        Ok(self.get(key)?.is_some())
    }

    /// 錠を取ってから「読む → 足す → 書く」を行う。
    ///
    /// 錠は**鍵ごとの錠ファイル1つだけ**です。`create_new(true)` で作るので、
    /// 作れた側だけが進めます。これはプロセスをまたぐ分にも、同じプロセスの
    /// スレッド同士にも効きます。
    ///
    /// プロセス全体で1つの `Mutex` は**持ちません**。握ったまま錠ファイルを
    /// 待つので、別プロセスが錠を持つ間、無関係な鍵の `increment` まで止まりました。
    ///
    /// **期限は引き継ぎません**（期限なしで書き直します）。Laravel の
    /// `increment` は元の期限を保つので、そこは挙動が違います。
    fn increment(&self, key: &str, by: i64) -> Result<i64> {
        self.ensure_dir()?;
        let _file = FileLock::acquire(&self.lock_path_of(key))?;

        let current = parse_number(key, self.get(key)?)?;
        let next = current + by;
        self.put(key, &next.to_string(), None)?;
        Ok(next)
    }

    fn prune(&self) -> Result<usize> {
        sweep_expired(&self.dir)
    }

    fn prunable(&self) -> bool {
        true
    }

    fn location(&self) -> Option<String> {
        Some(self.dir.display().to_string())
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

    /// Mutex の中で読んで書く。数を落としません。
    ///
    /// **期限は引き継ぎません**（期限なしで書き直します）。
    fn increment(&self, key: &str, by: i64) -> Result<i64> {
        let mut entries = self.lock();
        let current = match entries.get(key).and_then(|raw| Entry::decode(raw)) {
            // 期限切れは 0 から数え直す。
            Some(entry) if !entry.expired() => entry
                .value
                .trim()
                .parse::<i64>()
                .map_err(|_| Error::msg(format!("`{key}` の値は数ではありません")))?,
            _ => 0,
        };
        let next = current + by;
        entries.insert(key.to_string(), Entry::encode(&next.to_string(), None));
        Ok(next)
    }

    /// メモリだけなのでブロックしません（裏のスレッドを経由しません）。
    fn blocking(&self) -> bool {
        false
    }
}

/// 書き込み途中の一時ファイルの名前か。
///
/// `put` が付ける印（[`TEMP_MARK`]）で見分けます。`normalize_key` は `+` を
/// 通さないので、利用者の鍵がこの印を含むことはありません。
///
/// 「後ろ2つが数字」で見ていたときは、`report.2024.12.tmp` や `page.1.2.tmp`
/// のような鍵の本体を一時ファイルと取り違えていました。
fn is_temp_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.contains(TEMP_MARK))
}

/// 期限切れのファイルを消す。
///
/// 呼び出し元は [`FileCache::prune`]（`cache:prune`）だけです。
/// `cache:clear` は `flush()` を通るので、ここは通りません。
fn sweep_expired(dir: &Path) -> Result<usize> {
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
        if is_temp_path(&path) {
            // 書き込み中かもしれないので**読まない**。十分古いものだけ残骸として消す。
            // 読んで「壊れている」と判断して消すと、向こうの `rename` が失敗します。
            if is_stale(&path, TEMP_STALE_SECS) && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
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

    /// テスト用の空ディレクトリ。名前が重ならないように連番を足す。
    fn temp_dir(label: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("bengara-{label}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn 拡張子だけ違う鍵は別の一時ファイルを使う() {
        let dir = temp_dir("tmpname");
        let store = FileCache::new(&dir);

        // `with_extension("tmp")` だと両方 `test.tmp` になってしまう。
        let a = store.temp_path_of("test.put");
        let b = store.temp_path_of("test.count");
        assert_ne!(a, b);
        // 印が入っているので、一時ファイルだと分かる。
        assert!(is_temp_path(&a));
        assert!(a.file_name().unwrap().to_str().unwrap().contains(TEMP_MARK));

        // 同じ鍵でも、呼ぶたびに別の名前になる。
        assert_ne!(store.temp_path_of("test.put"), a);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 数字で終わる鍵は一時ファイルと取り違えない() {
        // 「後ろ2つが数字」で見ていたときは、この 2 つを一時ファイルと見なしていた。
        assert!(!is_temp_path(Path::new("report.2024.12.tmp")));
        assert!(!is_temp_path(Path::new("page.1.2.tmp")));
        assert!(!is_temp_path(Path::new("1.2")));
        // 印が入っているものだけが一時ファイル。
        assert!(is_temp_path(Path::new("report.2024.12.+tmp.1234.5")));
        assert!(is_temp_path(Path::new("a.+tmp.1.0")));
    }

    #[test]
    fn 数字で終わる鍵は期限が残っていれば消さない() {
        let dir = temp_dir("digitkey");
        let store = FileCache::new(&dir);

        // 前は `cache:prune` で期限なしでも消えていた。
        store.put("report.2024.12", "1", None).unwrap();
        assert_eq!(sweep_expired(&dir).unwrap(), 0);
        assert_eq!(store.get("report.2024.12").unwrap().as_deref(), Some("1"));

        // `cache:clear`（flush）では消える。前は「書き込み中」として飛ばしていた。
        store.flush().unwrap();
        assert_eq!(store.get("report.2024.12").unwrap(), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 鍵locksは錠の置き場所とぶつからない() {
        let dir = temp_dir("lockskey");
        let store = FileCache::new(&dir);

        // 一度 increment を使って錠の置き場所を作る。
        store.increment("n", 1).unwrap();
        // `normalize_key("locks")` はそのまま `locks` なので、避けようがない。
        store.put("locks", "値", Some(60)).unwrap();
        assert_eq!(store.get("locks").unwrap().as_deref(), Some("値"));

        // 錠の置き場所は `normalize_key` が作れない名前。大文字では駄目（NTFS）。
        assert!(!LOCK_DIR.bytes().all(|b| b.is_ascii_alphanumeric()));
        assert_ne!(LOCK_DIR.to_ascii_lowercase(), "locks");
        assert!(dir.join(LOCK_DIR).is_dir());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 長すぎる期限でもあふれない() {
        // `as i64` だと `u64::MAX` が `-1` になって即時失効していた。
        let raw = Entry::encode("値", Some(u64::MAX));
        let entry = Entry::decode(&raw).unwrap();
        assert_eq!(entry.expires_at, i64::MAX);
        assert!(!entry.expired(), "即時失効しない");

        // i64 の上限に近い値でもパニックしない。
        let entry = Entry::decode(&Entry::encode("値", Some(i64::MAX as u64))).unwrap();
        assert_eq!(entry.expires_at, i64::MAX);
        assert!(!entry.expired());
    }

    #[test]
    fn tmpで終わる鍵も壊れない() {
        let dir = temp_dir("tmpkey");
        let store = FileCache::new(&dir);

        store.put("a.tmp", "1", Some(60)).unwrap();
        assert_eq!(store.get("a.tmp").unwrap().as_deref(), Some("1"));
        // 一時ファイルと見なされないので、本体として掃除の対象になる。
        assert!(!is_temp_path(&dir.join("a.tmp")));
        assert!(is_temp_path(&dir.join("a.tmp.+tmp.1234.5")));

        store.put("b", "2", Some(0)).unwrap();
        assert_eq!(sweep_expired(&dir).unwrap(), 1, "期限切れの b だけ");
        assert!(dir.join("a.tmp").exists());

        store.flush().unwrap();
        assert_eq!(store.get("a.tmp").unwrap(), None, "flush で消える");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 掃除は新しい一時ファイルを残す() {
        let dir = temp_dir("tmpsweep");
        let store = FileCache::new(&dir);

        // 書き込み途中に見える一時ファイル。
        let writing = store.temp_path_of("key");
        std::fs::write(&writing, "まだ書いている途中").unwrap();

        assert_eq!(sweep_expired(&dir).unwrap(), 0, "新しい一時ファイルは残す");
        assert!(writing.exists());

        store.flush().unwrap();
        assert!(writing.exists(), "flush でも消さない");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 数を足せる() {
        let dir = temp_dir("incr");
        let store = FileCache::new(&dir);

        assert_eq!(store.increment("n", 1).unwrap(), 1);
        assert_eq!(store.increment("n", 2).unwrap(), 3);
        assert_eq!(store.increment("n", -1).unwrap(), 2);
        assert_eq!(store.get("n").unwrap().as_deref(), Some("2"));
        // 錠は手放したら残らない。
        assert!(!store.lock_path_of("n").exists());

        // 数でない値はエラーにする。
        store.put("word", "あ", None).unwrap();
        assert!(store.increment("word", 1).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn メモリの置き場所も数を足せる() {
        let store = MemoryCache::new();
        assert_eq!(store.increment("n", 1).unwrap(), 1);
        assert_eq!(store.increment("n", 2).unwrap(), 3);
        assert_eq!(store.increment("n", -4).unwrap(), -1);

        store.put("word", "あ", None).unwrap();
        assert!(store.increment("word", 1).is_err());

        // 期限切れは 0 から数え直す。
        store.put("old", "5", Some(0)).unwrap();
        assert_eq!(store.increment("old", 1).unwrap(), 1);
    }

    #[test]
    fn ブロックするかを言える() {
        assert!(
            FileCache::new("どこか").blocking(),
            "ファイルはブロックする"
        );
        assert!(!MemoryCache::new().blocking());
    }

    #[test]
    fn 掃除ができるかを言える() {
        let store = FileCache::new("どこか");
        assert!(store.prunable());
        assert_eq!(store.location().as_deref(), Some("どこか"));

        let memory = MemoryCache::new();
        assert!(!memory.prunable());
        assert_eq!(memory.prune().unwrap(), 0);
        assert_eq!(memory.location(), None);
    }

    #[test]
    fn あるかを調べられる() {
        let store = MemoryCache::new();
        store.put("a", "1", Some(60)).unwrap();
        assert!(store.has("a").unwrap());
        assert!(!store.has("b").unwrap());

        let dir = temp_dir("has");
        let file = FileCache::new(&dir);
        file.put("a", "1", Some(60)).unwrap();
        assert!(file.has("a").unwrap());
        assert!(!file.has("b").unwrap());
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
