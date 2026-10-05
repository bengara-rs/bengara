//! セッション。リクエストをまたいで値を覚えておく仕組みです。
//!
//! ```ignore
//! pub async fn store(req: Request) -> Result<Response> {
//!     req.session().put("cart", "12");
//!     req.session().flash("status", "保存しました");
//!     redirect().route("home")
//! }
//! ```
//!
//! # 置き場所
//!
//! 中身はサーバー側（既定は `storage/framework/sessions/`）に置き、
//! ブラウザには**鍵となる ID だけ**を署名つきの Cookie で渡します。
//! 中身がブラウザに出ないので、Cookie の大きさ（4 KB）にも縛られません。
//!
//! プロセスを2つ以上動かすときは、全プロセスが同じ `storage/` を見るようにしてください
//! （`APP_STORAGE_PATH`）。

mod middleware;
mod store;

pub use middleware::{SessionConfig, StartSession, VerifyCsrfToken};
pub use store::{FileStore, MemoryStore, SessionStore};

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::support::crypto;

/// セッションに入れられる中身。
type Data = BTreeMap<String, String>;

/// 1リクエストぶんのセッション。
///
/// `Request` から `req.session()` で取り出します。クローンしても同じ中身を指します。
#[derive(Clone)]
pub struct Session {
    inner: Arc<Mutex<Inner>>,
    /// このセッションを置いてある場所。`StartSession` が入れます。
    ///
    /// 同じ利用者のほかのセッションを消すとき（`Auth::logout_other_devices()` など）に使います。
    /// プロセス全体の置き場所を1つ覚える形にすると、アプリを2つ組み立てたとき
    /// （テストが並んで走るときなど）に別のアプリの置き場所を触ってしまうため、
    /// **リクエストに結びつけて持ち歩きます。**
    store: Option<Arc<dyn SessionStore>>,
}

struct Inner {
    id: String,
    data: Data,
    /// 次のリクエストまでだけ残す値。
    flash: Data,
    /// 前のリクエストから持ち越した、今回だけ読める値。
    old_flash: Data,
    /// 書き換えたか（変わっていなければ保存しない）。
    dirty: bool,
    /// ID を作り直したか。
    regenerated: bool,
    /// セッションごと捨てたか。
    flushed: bool,
}

impl Session {
    pub(crate) fn new(id: String, data: Data, old_flash: Data) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                id,
                data,
                flash: Data::new(),
                old_flash,
                dirty: false,
                regenerated: false,
                flushed: false,
            })),
            store: None,
        }
    }

    /// 空のセッションを作る（初めて来た人）。
    pub(crate) fn empty() -> Self {
        Self::new(new_id(), Data::new(), Data::new())
    }

    /// 置き場所を結びつける。`StartSession` が呼びます。
    pub(crate) fn with_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// 結びついている置き場所。無ければ、直し方を書いたエラーを返す。
    pub(crate) fn store(&self) -> crate::error::Result<Arc<dyn SessionStore>> {
        self.store.clone().ok_or_else(|| {
            crate::Error::msg(
                "セッションの置き場所が結びついていません。bootstrap/app.rs の \
                 with_middleware で StartSession を登録してください",
            )
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // 中身は Mutex だが、1リクエストの中でしか触らないので取り合いにならない。
        // 他のスレッドがパニックしても読めるようにしておく。
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// セッションの ID。
    pub fn id(&self) -> String {
        self.lock().id.clone()
    }

    /// 値を読む。1 回だけの値（flash）も読めます。
    pub fn get(&self, key: &str) -> Option<String> {
        let inner = self.lock();
        inner
            .data
            .get(key)
            .or_else(|| inner.old_flash.get(key))
            .or_else(|| inner.flash.get(key))
            .cloned()
    }

    /// 値を読む。無ければ既定値。
    pub fn get_or(&self, key: &str, default: &str) -> String {
        self.get(key).unwrap_or_else(|| default.to_string())
    }

    /// 値を入れる。次のリクエストでも読めます。
    pub fn put(&self, key: &str, value: impl Into<String>) {
        let mut inner = self.lock();
        inner.data.insert(key.to_string(), value.into());
        inner.dirty = true;
    }

    /// **次のリクエストまでだけ**残る値を入れる。
    ///
    /// 「保存しました」のような、1回見せたら消えてよいものに使います。
    pub fn flash(&self, key: &str, value: impl Into<String>) {
        let mut inner = self.lock();
        inner.flash.insert(key.to_string(), value.into());
        inner.dirty = true;
    }

    /// 値があるか。
    pub fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// 値を消す。
    pub fn forget(&self, key: &str) {
        let mut inner = self.lock();
        let removed = inner.data.remove(key).is_some();
        let removed_flash = inner.flash.remove(key).is_some();
        let removed_old = inner.old_flash.remove(key).is_some();
        if removed || removed_flash || removed_old {
            inner.dirty = true;
        }
    }

    /// 値を読んでから消す。
    pub fn pull(&self, key: &str) -> Option<String> {
        let value = self.get(key);
        if value.is_some() {
            self.forget(key);
        }
        value
    }

    /// 中身を全部消す。ID は変えません。
    pub fn flush(&self) {
        let mut inner = self.lock();
        inner.data.clear();
        inner.flash.clear();
        inner.old_flash.clear();
        inner.dirty = true;
        inner.flushed = true;
    }

    /// ID を作り直す。**ログインの直後に必ず呼んでください。**
    ///
    /// 人のブラウザにあらかじめ ID を仕込んでおく攻撃（セッション固定）を防ぎます。
    /// 中身は引き継ぎますが、**CSRF のトークンは忘れます**（Laravel の
    /// `Session::regenerate()` が `regenerateToken()` も呼ぶのと同じ）。
    /// ログイン前にトークンを知られていると、作り直さないかぎりログイン後も通ってしまいます。
    ///
    /// ここでは消すだけです。次に必要になったときに作り直されます。
    pub fn regenerate(&self) {
        let mut inner = self.lock();
        inner.id = new_id();
        inner.dirty = true;
        inner.regenerated = true;
        // `forget()` は同じ Mutex を取るので、ここで直に消す。
        inner.data.remove(middleware::CSRF_KEY);
        inner.flash.remove(middleware::CSRF_KEY);
        inner.old_flash.remove(middleware::CSRF_KEY);
    }

    /// ログアウト用。中身を捨てて ID も作り直します。
    pub fn invalidate(&self) {
        self.flush();
        self.regenerate();
    }

    /// 中身の組を全部返す（持ち越し中の値も含む）。
    pub fn all(&self) -> BTreeMap<String, String> {
        let inner = self.lock();
        let mut out = inner.old_flash.clone();
        out.extend(inner.data.clone());
        out.extend(inner.flash.clone());
        out
    }

    /// 前のリクエストの入力を覚えておく（入力し直しを避けるため）。
    pub(crate) fn flash_input(&self, pairs: &[(String, String)]) {
        let json = serde_json::to_string(&pairs.iter().cloned().collect::<BTreeMap<_, _>>())
            .unwrap_or_else(|_| "{}".to_string());
        self.flash(OLD_INPUT_KEY, json);
    }

    /// 前のリクエストの入力を読む。
    pub fn old(&self, key: &str) -> Option<String> {
        let raw = self.get(OLD_INPUT_KEY)?;
        let map: BTreeMap<String, String> = serde_json::from_str(&raw).ok()?;
        map.get(key).cloned()
    }

    /// 保存が要るか。
    ///
    /// 書き換えたときだけでなく、**持ち越しの値を抱えているとき**も保存します。
    /// 保存しないと、1回だけのはずの値がいつまでも残ってしまいます。
    pub(crate) fn should_save(&self) -> bool {
        let inner = self.lock();
        inner.dirty || !inner.old_flash.is_empty()
    }

    /// 書き換えたか（テストと内部の確認用）。
    #[cfg(test)]
    pub(crate) fn is_dirty(&self) -> bool {
        self.lock().dirty
    }

    /// 中身を捨てたか。
    pub(crate) fn was_flushed(&self) -> bool {
        self.lock().flushed
    }

    /// 保存する中身と ID を取り出す。
    ///
    /// 今回入れた flash は「次回ぶん」として持ち越し、前回ぶんは捨てます。
    pub(crate) fn take_for_save(&self) -> (String, Data) {
        let inner = self.lock();
        let mut data = inner.data.clone();
        for (k, v) in &inner.flash {
            data.insert(format!("{FLASH_PREFIX}{k}"), v.clone());
        }
        (inner.id.clone(), data)
    }

    /// 読み込んだ中身を、本体ぶんと持ち越しぶんに分ける。
    pub(crate) fn split_flash(stored: Data) -> (Data, Data) {
        let mut data = Data::new();
        let mut flash = Data::new();
        for (k, v) in stored {
            match k.strip_prefix(FLASH_PREFIX) {
                Some(name) => {
                    flash.insert(name.to_string(), v);
                }
                None => {
                    data.insert(k, v);
                }
            }
        }
        (data, flash)
    }
}

/// 期限切れのセッションを消す（`session:gc` から呼びます）。
pub(crate) fn sweep_expired(dir: &std::path::Path) -> crate::error::Result<usize> {
    store::sweep(dir)
}

/// CSRF のトークンを取り出す（無ければ作る）。
pub(crate) fn csrf_token(session: &Session) -> String {
    middleware::ensure_token(session)
}

/// 持ち越しの値に付ける目印。
const FLASH_PREFIX: &str = "__flash:";

/// 前の入力を入れておく場所。
const OLD_INPUT_KEY: &str = "__old_input";

/// 推測できない ID を作る。
fn new_id() -> String {
    crypto::random_token()
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // ID と中身はログに出さない。漏れるとなりすましに使われる。
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session::empty()
    }

    #[test]
    fn 入れた値を読める() {
        let s = session();
        assert!(s.get("a").is_none());
        s.put("a", "1");
        assert_eq!(s.get("a").as_deref(), Some("1"));
        assert_eq!(s.get_or("b", "既定"), "既定");
        assert!(s.has("a"));
        assert!(s.is_dirty());
    }

    #[test]
    fn 読まないうちは保存が要らない() {
        let s = session();
        let _ = s.get("a");
        assert!(!s.is_dirty(), "読むだけなら保存しない");
    }

    #[test]
    fn 消せる() {
        let s = session();
        s.put("a", "1");
        s.forget("a");
        assert!(s.get("a").is_none());
    }

    #[test]
    fn 読んでから消せる() {
        let s = session();
        s.put("a", "1");
        assert_eq!(s.pull("a").as_deref(), Some("1"));
        assert!(s.get("a").is_none());
        assert!(s.pull("a").is_none());
    }

    #[test]
    fn idを作り直すと値は変わらない() {
        let s = session();
        s.put("a", "1");
        let before = s.id();
        s.regenerate();
        assert_ne!(s.id(), before, "ID は変わる");
        assert_eq!(s.get("a").as_deref(), Some("1"), "中身は残る");
    }

    #[test]
    fn idを作り直すとcsrfトークンも変わる() {
        let s = session();
        let before = csrf_token(&s);
        s.regenerate();
        assert!(
            s.get(super::middleware::CSRF_KEY).is_none(),
            "いったん忘れる"
        );

        // 次に必要になったときに作り直される。
        let after = csrf_token(&s);
        assert_ne!(after, before, "ログイン前のトークンは通らなくなる");
        assert_eq!(after.len(), 64);
    }

    #[test]
    fn 持ち越し中のcsrfトークンも忘れる() {
        let mut old_flash = Data::new();
        old_flash.insert(
            super::middleware::CSRF_KEY.to_string(),
            "もれたトークン".into(),
        );
        let s = Session::new("id".into(), Data::new(), old_flash);
        s.flash(super::middleware::CSRF_KEY, "これも消える");

        s.regenerate();
        assert!(s.get(super::middleware::CSRF_KEY).is_none());
    }

    #[test]
    fn 無効にすると中身が消えてidも変わる() {
        let s = session();
        s.put("a", "1");
        let before = s.id();
        s.invalidate();
        assert!(s.get("a").is_none());
        assert_ne!(s.id(), before);
        assert!(s.was_flushed());
    }

    #[test]
    fn flashは今回も読める() {
        let s = session();
        s.flash("status", "保存しました");
        assert_eq!(s.get("status").as_deref(), Some("保存しました"));
    }

    #[test]
    fn flashは次の回まで残りその次で消える() {
        // 1回目: flash を入れて保存する。
        let s = session();
        s.flash("status", "ok");
        s.put("keep", "always");
        let (_, saved) = s.take_for_save();
        assert!(saved.contains_key("__flash:status"));
        assert_eq!(saved.get("keep").map(String::as_str), Some("always"));

        // 2回目: 読み込むと flash は「持ち越し」として入り、読める。
        let (data, old_flash) = Session::split_flash(saved);
        let s2 = Session::new("id".into(), data, old_flash);
        assert_eq!(s2.get("status").as_deref(), Some("ok"));
        assert_eq!(s2.get("keep").as_deref(), Some("always"));

        // 2回目で何も足さずに保存すると、持ち越しは消える。
        let (_, saved2) = s2.take_for_save();
        assert!(!saved2.contains_key("__flash:status"), "3回目には残らない");
        assert!(!saved2.contains_key("status"));
        assert_eq!(saved2.get("keep").map(String::as_str), Some("always"));
    }

    #[test]
    fn 持ち越しを抱えていたら読むだけでも保存する() {
        // 保存しないと、1回だけのはずの値が次の次まで残ってしまう。
        let mut old_flash = Data::new();
        old_flash.insert("status".into(), "ok".into());
        let s = Session::new("id".into(), Data::new(), old_flash);
        assert!(!s.is_dirty(), "書き換えてはいない");
        assert!(s.should_save(), "それでも保存は要る");

        // 何も持ち越していなければ、読むだけで保存はしない。
        let s = Session::empty();
        assert!(!s.should_save());
    }

    #[test]
    fn 前の入力を読める() {
        let s = session();
        s.flash_input(&[("title".to_string(), "下書き".to_string())]);
        assert_eq!(s.old("title").as_deref(), Some("下書き"));
        assert!(s.old("missing").is_none());
    }

    #[test]
    fn 全部の値を取れる() {
        let s = session();
        s.put("a", "1");
        s.flash("b", "2");
        let all = s.all();
        assert_eq!(all.get("a").map(String::as_str), Some("1"));
        assert_eq!(all.get("b").map(String::as_str), Some("2"));
    }

    #[test]
    fn idは毎回違う() {
        assert_ne!(Session::empty().id(), Session::empty().id());
        assert_eq!(Session::empty().id().len(), 64);
    }

    #[test]
    fn デバッグ出力に中身を出さない() {
        let s = session();
        s.put("secret", "とても大事");
        let text = format!("{s:?}");
        assert!(!text.contains("とても大事"));
        assert!(!text.contains(&s.id()));
    }
}
