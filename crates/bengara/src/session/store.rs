//! セッションの置き場所。
//!
//! 既定は `FileStore`（`storage/framework/sessions/`）です。
//! テストや、ディスクを使いたくないときは `MemoryStore` を使えます。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};
use crate::support::crypto;

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

    /// 中身を変えずに**期限だけ延ばす**。
    ///
    /// 読むだけのリクエスト（GET の画面）でも呼ばれます。延ばさないと、
    /// 置き場所の期限が「最後に書いたとき」から数えられるので、
    /// 画面を見て回っていた人が突然ログアウトされます。
    ///
    /// **既定は `read` して、同じ中身で `write` し直すだけです。**
    /// 置き場所が期限を知っているなら、残りが半分を切ったときだけ書く形にして
    /// ください（`FileStore` と `MemoryStore` はそうしています）。
    /// 置き場所に無い ID のときは何もしません。
    ///
    /// **期限切れを延命してはいけません。** 残りが 0 秒のものは「残りが半分を切った」
    /// にも当たるので、期限を確かめずに書き直すと、切れたセッションに満額の寿命を
    /// 与え直すことになります（`FileStore` が実際にそうなっていました）。
    /// 既定の実装は `read` が期限切れを捨てるので、ここは守れています。
    fn touch(&self, id: &str) -> Result<()> {
        if let Some(data) = self.read(id)? {
            self.write(id, &data)?;
        }
        Ok(())
    }

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

/// セッション ID として通す形か。
///
/// ID は自分で作った16進の文字列ですが、外から来た値でもあるので念のため確かめます。
/// `..` や `/` が混じったファイル名を作らせないためです。
///
/// 通すのは**英数字とハイフンだけ**です。ここで**ドットを通さない**ことが、
/// `write` の一時ファイル（`<ID>.<乱数>.tmp`）がセッションのファイル名と
/// ぶつからない根拠になっています。この条件を緩めるときは `write` も見直してください。
///
/// `destroy_matching` と `sweep` も、ディレクトリの中のファイルを触るかどうかを
/// **この判定で**決めます。書きかけの一時ファイルを消すと `rename` が失敗します。
fn is_session_id(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// `storage/framework/sessions/` にファイルとして置く。既定の置き場所です。
///
/// 1つのセッションが1つのファイルになります。
/// プロセスを2つ以上動かすときは、全プロセスで同じ場所を指してください。
pub struct FileStore {
    dir: PathBuf,
    lifetime_secs: u64,
    /// ディレクトリの権限を直したか（`ensure_dir` が1回だけ呼びます）。
    restricted: std::sync::Once,
}

impl FileStore {
    /// 置き場所と、放っておいて消えるまでの秒数を決めて作る。
    pub fn new(dir: impl Into<PathBuf>, lifetime_secs: u64) -> Self {
        Self {
            dir: dir.into(),
            lifetime_secs,
            restricted: std::sync::Once::new(),
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
    fn path_of(&self, id: &str) -> Result<PathBuf> {
        if !is_session_id(id) {
            return Err(Error::msg("セッション ID の形が正しくありません"));
        }
        Ok(self.dir.join(id))
    }

    fn ensure_dir(&self) -> Result<()> {
        if self.dir.is_dir() {
            // 既にあるときも権限を直す。tar の展開などで 0755 のまま置かれると、
            // ファイルが 0600 でも**ディレクトリが読めるので、
            // ファイル名＝セッション ID を一覧できてしまいます。**
            // 書き込みごとに直す意味は無いので、1回だけにします。
            self.restricted.call_once(|| restrict_dir(&self.dir));
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
        self.restricted.call_once(|| restrict_dir(&self.dir));
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
            // 書きかけの一時ファイルなどは触らない（消すと rename が失敗する）。
            if !is_session_id(&name) {
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
        //
        // 名前に**乱数を混ぜます。** ID だけで決めると、同じ Cookie の2本が同時に
        // 書き込んだときに同じ一時ファイルを `truncate` で開き、混ざった JSON が
        // 本体に置き換わります。次の `read` が失敗してファイルを消すので、
        // 利用者は突然ログアウトされ、次の POST が 419 になります。
        //
        // `is_session_id` がドットを通さないので、この名前は本体とぶつからず、
        // `destroy_matching` と `sweep` も触りません。
        let temp = self
            .dir
            .join(format!("{id}.{}.tmp", crypto::random_token()));
        write_private(&temp, body.as_bytes())?;
        if let Err(e) = std::fs::rename(&temp, &path) {
            // 置き換えられなかった一時ファイルを残さない。
            // 残すとディスクを食い、`sweep` も触らないので永遠に消えません。
            if let Err(cleanup) = std::fs::remove_file(&temp) {
                tracing::warn!("一時ファイルを片付けられませんでした: {cleanup}");
            }
            return Err(Error::Io(e));
        }
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

    fn touch(&self, id: &str) -> Result<()> {
        let path = self.path_of(id)?;
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Ok(());
        };
        let Ok(stored) = serde_json::from_str::<Stored>(&raw) else {
            // 壊れたファイルは `read` と `sweep` の仕事。ここでは触らない。
            return Ok(());
        };
        // 期限切れは延命しない。`needs_touch` だけを見ていたころは、
        // 残り 0 秒が「半分を切った」に当たり、満額の寿命を与え直していた。
        // `MemoryStore::touch` には最初からこの確認がある。
        if stored.expires_at <= now() {
            return Ok(());
        }
        if !needs_touch(stored.expires_at, self.lifetime_secs) {
            return Ok(());
        }
        self.write(id, &stored.data)
    }

    fn destroy_for_user(&self, user_id: &str) -> Result<usize> {
        self.destroy_matching(user_id, None)
    }

    fn destroy_for_user_except(&self, user_id: &str, keep_id: &str) -> Result<usize> {
        self.destroy_matching(user_id, Some(keep_id))
    }
}

/// 期限を延ばし直すか。**残りが半分を切ったときだけ**書きます。
///
/// 毎リクエスト書くと、GET の画面を開くたびにファイルを置き換えることになります。
/// 半分を境にすれば、書く回数は多くても期限の半分に1回で済みます。
fn needs_touch(expires_at: u64, lifetime_secs: u64) -> bool {
    let remaining = expires_at.saturating_sub(now());
    remaining <= lifetime_secs / 2
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

    fn touch(&self, id: &str) -> Result<()> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let Some((expires_at, _)) = entries.get_mut(id) else {
            return Ok(());
        };
        if *expires_at <= now() || !needs_touch(*expires_at, self.lifetime_secs) {
            return Ok(());
        }
        *expires_at = now() + self.lifetime_secs;
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
        // 名前がセッション ID の形でないファイルは触らない。
        // 書きかけの一時ファイル（`<ID>.<乱数>.tmp`）は、まだ JSON として読めないので
        // 「読めない＝捨てる」の判定に当たり、`rename` の直前に消してしまいます。
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_session_id(&name) {
            continue;
        }
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
    fn セッションidの形だけを通す() {
        assert!(is_session_id("abc123"));
        assert!(is_session_id("a-b"));
        assert!(!is_session_id(""));
        assert!(
            !is_session_id("a.b"),
            "ドットは通さない（一時ファイルの根拠）"
        );
        assert!(!is_session_id("a/b"));
        assert!(!is_session_id(".."));
        assert!(!is_session_id(&"x".repeat(129)));
    }

    #[test]
    fn 掃除は一時ファイルを触らない() {
        let dir = temp_dir("sweep-tmp");
        FileStore::new(&dir, 0)
            .write("old", &data(&[("a", "1")]))
            .unwrap();
        // 書きかけの一時ファイルを置いておく。JSON としては読めない。
        let temp = dir.join("abc.0123456789abcdef.tmp");
        std::fs::write(&temp, "{\"expires_at\":").unwrap();

        let removed = sweep(&dir).unwrap();
        assert_eq!(removed, 1, "期限切れの本体だけ消す");
        assert!(!dir.join("old").exists());
        assert!(
            temp.exists(),
            "書きかけは触らない（消すと rename が失敗する）"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 同時に書き込んでも中身が混ざらない() {
        // 一時ファイル名が ID だけで決まっていたころは、同じ ID の2本が
        // 同じ `<ID>.tmp` を `truncate` で開き、混ざった JSON が本体になった。
        let dir = temp_dir("concurrent");
        let store = std::sync::Arc::new(FileStore::new(&dir, 60));

        let handles: Vec<_> = (0..8)
            .map(|n| {
                let store = store.clone();
                std::thread::spawn(move || {
                    for _ in 0..20 {
                        // Windows では、同じ本体への `rename` が同時に走ると
                        // 「別のプロセスが使用中」で失敗することがあります。
                        // ここで確かめたいのは**中身が混ざらないこと**なので、
                        // 失敗そのものは見ません。
                        let _ = store.write("abc", &data(&[("n", "値")]));
                    }
                    n
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("スレッドが落ちていない");
        }

        // 本体は読める（混ざった JSON になっていれば `read` が消して `None`）。
        assert_eq!(store.read("abc").unwrap(), Some(data(&[("n", "値")])));
        // 一時ファイルも残っていない。
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["abc".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 置き場所のファイルから `expires_at` を読む。
    fn expires_of(dir: &Path, id: &str) -> u64 {
        let raw = std::fs::read_to_string(dir.join(id)).unwrap();
        serde_json::from_str::<Stored>(&raw).unwrap().expires_at
    }

    #[test]
    fn 残りが半分を切ったらファイルの期限を延ばす() {
        let dir = temp_dir("touch-file");
        let store = FileStore::new(&dir, 60);
        store.write("abc", &data(&[("a", "1")])).unwrap();

        // 書いた直後は残り 60 秒。半分（30 秒）を切っていないので書かない。
        let before = expires_of(&dir, "abc");
        store.touch("abc").unwrap();
        assert_eq!(expires_of(&dir, "abc"), before, "まだ延ばさない");

        // 残り 10 秒の形に差し替える。
        let stored = Stored {
            expires_at: now() + 10,
            data: data(&[("a", "1")]),
        };
        std::fs::write(dir.join("abc"), serde_json::to_string(&stored).unwrap()).unwrap();
        store.touch("abc").unwrap();
        let after = expires_of(&dir, "abc");
        assert!(after > now() + 50, "期限が延びている");
        // 中身は変えない。
        assert_eq!(store.read("abc").unwrap(), Some(data(&[("a", "1")])));

        // 置き場所に無い ID なら何もしない（新しいファイルを作らない）。
        store.touch("missing").unwrap();
        assert!(!dir.join("missing").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 期限切れのファイルは延命しない() {
        // `needs_touch` だけを見ていたころは、残り 0 秒が「半分を切った」に
        // 当たり、期限切れのセッションに満額の寿命を与え直していた。
        let dir = temp_dir("touch-expired");
        let store = FileStore::new(&dir, 60);
        let stored = Stored {
            expires_at: now().saturating_sub(10),
            data: data(&[("a", "1")]),
        };
        std::fs::write(dir.join("abc"), serde_json::to_string(&stored).unwrap()).unwrap();

        store.touch("abc").unwrap();
        assert!(
            expires_of(&dir, "abc") <= now(),
            "期限は延びない: {}",
            expires_of(&dir, "abc")
        );
        assert!(store.read("abc").unwrap().is_none(), "読めば消える");

        // メモリの置き場所も同じ（こちらには最初から確認があった）。
        let store = MemoryStore::new(60);
        store.write("abc", &data(&[("a", "1")])).unwrap();
        store.entries.lock().unwrap().get_mut("abc").unwrap().0 = now().saturating_sub(10);
        store.touch("abc").unwrap();
        assert!(store.read("abc").unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 残りが半分を切ったらメモリの期限も延ばす() {
        let store = MemoryStore::new(60);
        store.write("abc", &data(&[("a", "1")])).unwrap();

        let before = store.entries.lock().unwrap().get("abc").unwrap().0;
        store.touch("abc").unwrap();
        assert_eq!(
            store.entries.lock().unwrap().get("abc").unwrap().0,
            before,
            "まだ延ばさない"
        );

        // 残り 10 秒にする。
        store.entries.lock().unwrap().get_mut("abc").unwrap().0 = now() + 10;
        store.touch("abc").unwrap();
        assert!(
            store.entries.lock().unwrap().get("abc").unwrap().0 > now() + 50,
            "期限が延びている"
        );
        assert_eq!(store.read("abc").unwrap(), Some(data(&[("a", "1")])));

        // 置き場所に無い ID なら何もしない。
        store.touch("missing").unwrap();
        assert!(store.read("missing").unwrap().is_none());
    }

    #[test]
    #[cfg(unix)]
    fn 既にあるディレクトリの権限も直す() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("perm");
        // tar の展開などで 0755 のまま置かれた形。
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        let store = FileStore::new(&dir, 60);
        store.write("abc", &data(&[("a", "1")])).unwrap();

        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "ディレクトリを他人から読めないようにする");

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
