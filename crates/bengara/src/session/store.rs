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

    /// ある利用者のセッションを**全部**消す。消した数を返す。
    ///
    /// 利用者 ID は、セッションの中の `bengara_auth_id`（`Auth::login` が入れる値）です。
    /// `Auth::logout_all_devices()` から呼ばれます。
    ///
    /// **既定では何もせず 0 を返します。** 自分で作った置き場所が
    /// 急に壊れないようにするためです。利用者で引く方法は置き場所ごとに違うので、
    /// 必要になったときに実装してください。
    fn destroy_for_user(&self, user_id: &str) -> Result<usize> {
        let _ = user_id;
        warn_not_implemented("destroy_for_user");
        Ok(0)
    }

    /// ある利用者のセッションのうち、`keep_id` **以外**を消す。消した数を返す。
    ///
    /// `Auth::logout_other_devices()` から呼ばれます。いま操作している人を
    /// ログアウトさせずに、ほかの端末だけ切るための形です。
    ///
    /// 既定では何もせず 0 を返します（`destroy_for_user` と同じ理由）。
    fn destroy_for_user_except(&self, user_id: &str, keep_id: &str) -> Result<usize> {
        let _ = (user_id, keep_id);
        warn_not_implemented("destroy_for_user_except");
        Ok(0)
    }
}

/// 既定のままの実装を呼ばれたことを知らせる。
///
/// 黙って 0 を返すと、「他の端末を切ったつもりで切れていない」ことに気づけません。
fn warn_not_implemented(method: &str) {
    tracing::warn!(
        "この SessionStore は {method} を実装していないため、\
         ほかの端末のセッションは切れていません"
    );
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
    ///
    /// 通すのは**英数字とハイフンだけ**です。ここで**ドットを通さない**ことが、
    /// `write` の一時ファイル（`<ID>.tmp`）がセッションのファイル名と
    /// ぶつからない根拠になっています。この条件を緩めるときは `write` も見直してください。
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
        restrict_dir(&self.dir);
        Ok(())
    }

    /// 利用者 ID が一致するセッションのファイルを消す。`keep` に渡した ID は残す。
    ///
    /// **ディレクトリの中を全部読んで中身を見ます。** 索引は持ちません。
    /// 呼ぶのはパスワードの変更やログアウトのときだけで、回数が少ないからです。
    /// 索引（利用者 ID → セッション ID のファイル）を作ると、
    /// 保存のたびに2つのファイルを書くことになり、整合も自分で守らなければなりません。
    fn destroy_matching(&self, user_id: &str, keep: Option<&str>) -> Result<usize> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(Error::Io(e)),
        };

        let mut removed = 0;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // 書きかけの `<ID>.tmp` などは触らない（消すと rename が失敗する）。
            if self.path_of(&name).is_err() {
                continue;
            }
            if keep == Some(name.as_str()) {
                continue;
            }
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let found = std::fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<Stored>(&raw).ok());
            // 読めないファイルはここでは触らない（`sweep` の仕事）。
            let Some(stored) = found else { continue };
            if stored
                .data
                .get(crate::auth::AUTH_ID_KEY)
                .map(String::as_str)
                != Some(user_id)
            {
                continue;
            }
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
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
        // 一時ファイルも本体と同じ権限で作る（途中だけ読めるのでは意味がない）。
        // `path_of` が ID にドットを通さないので、この名前はぶつからない。
        let temp = path.with_extension("tmp");
        write_private(&temp, body.as_bytes())?;
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

    fn destroy_for_user(&self, user_id: &str) -> Result<usize> {
        self.destroy_matching(user_id, None)
    }

    fn destroy_for_user_except(&self, user_id: &str, keep_id: &str) -> Result<usize> {
        self.destroy_matching(user_id, Some(keep_id))
    }
}

/// 所有者だけが読み書きできるファイルとして書く。
///
/// セッションのファイルには認証 ID と、検査に落ちた入力（`flash_input`）が入ります。
/// `std::fs::write` は Unix で 0644 になり、同じホストの他の利用者から読めてしまいます。
///
/// Windows では何もしません（`ops.rs` の `restrict` と同じ考え方で、既定の継承に任せます）。
#[cfg(unix)]
fn write_private(path: &Path, body: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // `mode` は**作るときだけ**効きます。前のバージョンが 0644 で作ったファイルが
    // 残っている場合もあるので、開いたあとに念のため付け直します。
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(body)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, body: &[u8]) -> Result<()> {
    std::fs::write(path, body)?;
    Ok(())
}

/// 置き場所のディレクトリを、所有者だけが入れるようにする（Unix のみ）。
///
/// 失敗しても止めません。権限を変えられない置き場所（共有ディスクなど）もあります。
#[cfg(unix)]
fn restrict_dir(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
        tracing::warn!("{} の権限を変えられませんでした: {e}", dir.display());
    }
}

#[cfg(not(unix))]
fn restrict_dir(_dir: &Path) {}

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

    fn destroy_for_user(&self, user_id: &str) -> Result<usize> {
        self.destroy_matching(user_id, None)
    }

    fn destroy_for_user_except(&self, user_id: &str, keep_id: &str) -> Result<usize> {
        self.destroy_matching(user_id, Some(keep_id))
    }
}

impl MemoryStore {
    /// 利用者 ID が一致する組を消す。`keep` に渡した ID は残す。
    ///
    /// 全部を見ますが、メモリの上なので安いです。
    fn destroy_matching(&self, user_id: &str, keep: Option<&str>) -> Result<usize> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let before = entries.len();
        entries.retain(|id, (_, data)| {
            if keep == Some(id.as_str()) {
                return true;
            }
            data.get(crate::auth::AUTH_ID_KEY).map(String::as_str) != Some(user_id)
        });
        Ok(before - entries.len())
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

    /// 認証 ID だけを入れた中身を作る。
    fn with_user(user_id: &str) -> Data {
        data(&[(crate::auth::AUTH_ID_KEY, user_id), ("cart", "1")])
    }

    #[test]
    fn メモリで同じ利用者のセッションがまとめて消える() {
        let store = MemoryStore::new(60);
        store.write("a1", &with_user("7")).unwrap();
        store.write("a2", &with_user("7")).unwrap();
        store.write("b1", &with_user("8")).unwrap();
        // 認証 ID が入っていないセッションも混ぜる。
        store.write("anon", &data(&[("cart", "1")])).unwrap();

        assert_eq!(store.destroy_for_user("7").unwrap(), 2);
        assert!(store.read("a1").unwrap().is_none());
        assert!(store.read("a2").unwrap().is_none());
        assert!(store.read("b1").unwrap().is_some(), "他の利用者は残る");
        assert!(store.read("anon").unwrap().is_some(), "匿名も残る");
    }

    #[test]
    fn メモリで自分のセッションだけ残せる() {
        let store = MemoryStore::new(60);
        store.write("mine", &with_user("7")).unwrap();
        store.write("other", &with_user("7")).unwrap();
        store.write("b1", &with_user("8")).unwrap();

        assert_eq!(store.destroy_for_user_except("7", "mine").unwrap(), 1);
        assert!(store.read("mine").unwrap().is_some(), "自分は残る");
        assert!(store.read("other").unwrap().is_none(), "他の端末は切れる");
        assert!(store.read("b1").unwrap().is_some(), "他の利用者は残る");
    }

    #[test]
    fn ファイルで同じ利用者のセッションがまとめて消える() {
        let dir = temp_dir("destroy-user");
        let store = FileStore::new(&dir, 60);
        store.write("a1", &with_user("7")).unwrap();
        store.write("a2", &with_user("7")).unwrap();
        store.write("b1", &with_user("8")).unwrap();

        assert_eq!(store.destroy_for_user("7").unwrap(), 2);
        assert!(!dir.join("a1").exists());
        assert!(!dir.join("a2").exists());
        assert!(dir.join("b1").exists(), "他の利用者は残る");

        // 2回目は何も消さない。
        assert_eq!(store.destroy_for_user("7").unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ファイルで自分のセッションだけ残せる() {
        let dir = temp_dir("destroy-others");
        let store = FileStore::new(&dir, 60);
        store.write("mine", &with_user("7")).unwrap();
        store.write("other", &with_user("7")).unwrap();

        assert_eq!(store.destroy_for_user_except("7", "mine").unwrap(), 1);
        assert!(dir.join("mine").exists(), "自分は残る");
        assert!(!dir.join("other").exists(), "他の端末は切れる");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 無い場所で利用者を消しても落ちない() {
        let dir = std::env::temp_dir().join("bengara-session-test-no-dir");
        let _ = std::fs::remove_dir_all(&dir);
        let store = FileStore::new(&dir, 60);
        assert_eq!(store.destroy_for_user("7").unwrap(), 0);
    }

    #[test]
    fn 既定の実装は何も消さない() {
        // 利用者が自分で作った置き場所が、足した2つのメソッドで壊れないこと。
        struct Bare;
        impl SessionStore for Bare {
            fn read(&self, _id: &str) -> Result<Option<Data>> {
                Ok(None)
            }
            fn write(&self, _id: &str, _data: &Data) -> Result<()> {
                Ok(())
            }
            fn destroy(&self, _id: &str) -> Result<()> {
                Ok(())
            }
        }
        let store = Bare;
        assert_eq!(store.destroy_for_user("7").unwrap(), 0);
        assert_eq!(store.destroy_for_user_except("7", "mine").unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn ファイルは所有者だけが読める() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("mode");
        let store = FileStore::new(&dir, 60);
        store.write("abc", &with_user("7")).unwrap();
        let mode = std::fs::metadata(dir.join("abc"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "0{:o}", mode & 0o777);

        // 0644 で残っていたファイルも、書き直せば 0600 になる。
        let other = dir.join("def");
        std::fs::write(&other, "{}").unwrap();
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o644)).unwrap();
        store.write("def", &with_user("7")).unwrap();
        let mode = std::fs::metadata(&other).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "0{:o}", mode & 0o777);

        let _ = std::fs::remove_dir_all(&dir);
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
