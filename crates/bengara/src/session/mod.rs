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
//! （`APP_STORAGE_PATH`）。同じセッションのリクエストを順番にする錠は
//! **プロセスの中だけ**です（[`StartSession`] の説明を参照）。
//!
//! # 鍵の名前
//!
//! `__` で始まる鍵は、フレームワークが中で使います（CSRF のトークン、検査に落ちた
//! 入力、flash の持ち越しの目印）。`put` / `flash` に渡すと警告が出て、
//! `all()` には入りません。

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
    /// 置き場所から**実際に読めた**セッションか。`StartSession` が入れます。
    ///
    /// 偽のときは、このリクエストで新しく作ったセッションです。
    /// `Request::validate()` が「落ちた入力を覚えるか」を決めるのに使います。
    /// 読めていない相手に覚えさせると、Cookie を持たない相手が壊れた本文を
    /// 投げるだけでセッションのファイルが1つできます。
    loaded: bool,
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
            loaded: false,
        }
    }

    /// 空のセッションを作る（初めて来た人）。
    pub(crate) fn empty() -> Self {
        Self::new(new_id(), Data::new(), Data::new())
    }

    /// 「置き場所から読めた」目印を付ける。`StartSession` が呼びます。
    pub(crate) fn mark_loaded(mut self) -> Self {
        self.loaded = true;
        self
    }

    /// 置き場所から読めたセッションか。
    ///
    /// 偽のときは、このリクエストで新しく作ったセッションです。
    pub(crate) fn was_loaded(&self) -> bool {
        self.loaded
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
    ///
    /// **入れた順番に関係なく、`flash` の値が勝ちます。** 今回の `flash` →
    /// 持ち越しの `flash` → `put` の値、の順で探します。
    /// `flash("status", "2")` のあとに `put("status", "1")` しても、読めるのは `"2"` です。
    /// [`all`](Self::all) は、持ち越しの `flash` と `put` が同じ鍵のときだけ逆（`put`）になります。
    /// 逆にすると、`put("status", "1")` のあとの `flash("status", "2")` が
    /// `get` から永久に見えなくなります。
    pub fn get(&self, key: &str) -> Option<String> {
        let inner = self.lock();
        inner
            .flash
            .get(key)
            .or_else(|| inner.old_flash.get(key))
            .or_else(|| inner.data.get(key))
            .cloned()
    }

    /// 値を読む。無ければ既定値。
    pub fn get_or(&self, key: &str, default: &str) -> String {
        self.get(key).unwrap_or_else(|| default.to_string())
    }

    /// 値を入れる。次のリクエストでも読めます。
    ///
    /// **`__` で始まる鍵は使わないでください。** フレームワークが中で使う名前です
    /// （CSRF のトークン、検査に落ちた入力、持ち越しの目印）。たとえば
    /// `put("__flash:x", ..)` は次の回に flash として読まれ、その次で消えます。
    /// 断りはしませんが、**警告を出します。**
    pub fn put(&self, key: &str, value: impl Into<String>) {
        warn_reserved_key("put", key);
        self.put_internal(key, value.into());
    }

    /// `serde` の型を JSON の文字列にして入れる。`Cache::put_json` と同じ形です。
    ///
    /// ```ignore
    /// req.session().put_json("cart", &cart)?;
    /// let cart: Option<Cart> = req.session().get_json("cart")?;
    /// ```
    ///
    /// JSON にできなければ `Err` です。鍵の決まりは [`put`](Self::put) と同じです。
    pub fn put_json<T: serde::Serialize + ?Sized>(
        &self,
        key: &str,
        value: &T,
    ) -> crate::error::Result<()> {
        let text = serde_json::to_string(value)?;
        self.put(key, text);
        Ok(())
    }

    /// [`put_json`](Self::put_json) で入れた値を型に読む。
    ///
    /// 無ければ `Ok(None)`、型に読めなければ `Err` です。
    /// `Cache::get_json` と違って、読めない値を黙って捨てません。セッションは
    /// 作り直せる値の置き場所ではないためです。flash の値も読めます（[`get`](Self::get) と同じ）。
    pub fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        key: &str,
    ) -> crate::error::Result<Option<T>> {
        let Some(text) = self.get(key) else {
            return Ok(None);
        };
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| crate::Error::msg(format!("セッションの `{key}` を読めません: {e}")))
    }

    /// 鍵の名前を確かめずに入れる。フレームワークの中から内部の鍵を入れる口です。
    pub(crate) fn put_internal(&self, key: &str, value: String) {
        let mut inner = self.lock();
        inner.data.insert(key.to_string(), value);
        inner.dirty = true;
    }

    /// **次のリクエストまでだけ**残る値を入れる。
    ///
    /// 「保存しました」のような、1回見せたら消えてよいものに使います。
    ///
    /// [`put`](Self::put) と同じく、`__` で始まる鍵には警告を出します。
    pub fn flash(&self, key: &str, value: impl Into<String>) {
        warn_reserved_key("flash", key);
        self.flash_internal(key, value.into());
    }

    /// 鍵の名前を確かめずに flash する（中だけで使います）。
    fn flash_internal(&self, key: &str, value: String) {
        let mut inner = self.lock();
        inner.flash.insert(key.to_string(), value);
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
    ///
    /// **`__` で始まる鍵は入りません。** フレームワークが中で使う値
    /// （CSRF のトークン `__csrf_token`、検査に落ちた入力 `__old_input`）です。
    /// 画面やログにそのまま出すと、トークンや入力し直し用の値が漏れます。
    /// 要るときは `req.csrf_token()` と [`old`](Self::old) で取り出してください。
    pub fn all(&self) -> BTreeMap<String, String> {
        let inner = self.lock();
        // 後ろに並べたほうが勝つ（持ち越し → `put` → 今回の flash）。
        // 今回の flash が一番強いのは `get` と同じ。持ち越しと `put` が同じ鍵のときは
        // `get`（持ち越しが勝つ）と逆になる。
        inner
            .old_flash
            .iter()
            .chain(&inner.data)
            .chain(&inner.flash)
            .filter(|(key, _)| !key.starts_with(RESERVED_PREFIX))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    /// 前のリクエストの入力を覚えておく（入力し直しを避けるため）。
    ///
    /// 合計が [`MAX_OLD_INPUT_BYTES`] を超える入力は覚えません。
    /// 本文の上限（2 MiB）いっぱいの失敗 POST のたびに、その大きさを
    /// セッションの置き場所に書くのを避けるためです。
    pub(crate) fn flash_input(&self, pairs: &[(String, String)]) {
        let total: usize = pairs.iter().map(|(k, v)| k.len() + v.len()).sum();
        if total > MAX_OLD_INPUT_BYTES {
            tracing::warn!(
                "検査に落ちた入力が {total} バイトあり、上限（{MAX_OLD_INPUT_BYTES} バイト）を\
                 超えているので覚えません"
            );
            return;
        }
        let json = serde_json::to_string(&pairs.iter().cloned().collect::<BTreeMap<_, _>>())
            .unwrap_or_else(|_| "{}".to_string());
        self.flash_internal(OLD_INPUT_KEY, json);
    }

    /// ログイン前に見ようとしていたパスを取り出す。**読むと消えます。**
    ///
    /// `Authenticate` ミドルウェアが転送するときに覚えた値です。
    /// ログインできたあとの転送先に使います。
    ///
    /// ```ignore
    /// let back = req.session().intended().unwrap_or_else(|| "/".to_string());
    /// redirect().to(back)
    /// ```
    ///
    /// 覚えてあるのは自分のサイトの中を指すパスだけです（`Authenticate` が選びます）。
    /// 覚えていなければ `None` を返すので、呼ぶ側で既定の行き先を決めてください。
    ///
    /// **読み出しのときにもう一度確かめます。** 入れるときだけ見ていると、
    /// 置き場所の値が別の経路で書かれた場合に外のサイトへ飛ばせます。
    pub fn intended(&self) -> Option<String> {
        let found = self.pull(crate::auth::INTENDED_KEY)?;
        if crate::auth::is_internal_path(&found) {
            return Some(found);
        }
        tracing::warn!("セッションに入っていた戻り先が自分のサイトの中を指していません");
        None
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

    /// 書き換えたか。
    ///
    /// ルートに当たらなかったリクエストは、これが真のときだけ保存します
    /// （持ち越しの値を抱えているだけでは保存しません。`StartSession` を参照）。
    pub(crate) fn is_dirty(&self) -> bool {
        self.lock().dirty
    }

    /// 中身を捨てたか。
    pub(crate) fn was_flushed(&self) -> bool {
        self.lock().flushed
    }

    /// 保存する中身を写し取る。
    ///
    /// 今回入れた flash は「次回ぶん」として持ち越し、前回ぶんは捨てます。
    ///
    /// **中身をクローンします。** 保存が要るとき（[`should_save`](Self::should_save) が
    /// 真のとき）だけ呼んでください。
    pub(crate) fn snapshot_for_save(&self) -> Data {
        let inner = self.lock();
        let mut data = inner.data.clone();
        for (k, v) in &inner.flash {
            data.insert(format!("{FLASH_PREFIX}{k}"), v.clone());
        }
        data
    }

    /// 保存する中身を、**持ち越しの値を消費せずに**写し取る。
    ///
    /// ルートに当たらなかったリクエスト（静的ファイル・404・405）が書き換えたときに
    /// 使います。[`snapshot_for_save`](Self::snapshot_for_save) で保存すると、
    /// 画面に出していない「保存しました」が、CSS を 1 本返しただけで消えます。
    ///
    /// 前回ぶんも「次回ぶん」の形のまま残します。同じ鍵に今回 flash したときは
    /// 今回のほうが勝ちます。
    pub(crate) fn snapshot_keeping_flash(&self) -> Data {
        let inner = self.lock();
        let mut data = inner.data.clone();
        for (k, v) in inner.old_flash.iter().chain(&inner.flash) {
            data.insert(format!("{FLASH_PREFIX}{k}"), v.clone());
        }
        data
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
///
/// 「切った」印を残す秒数は、置き場所が読むときと同じ（1 時間と
/// `SESSION_LIFETIME` の長いほう）にそろえます。
pub(crate) fn sweep_expired(dir: &std::path::Path) -> crate::error::Result<usize> {
    store::sweep(dir, middleware::lifetime_secs_from_env())
}

/// CSRF のトークンを取り出す（無ければ作る）。
pub(crate) fn csrf_token(session: &Session) -> String {
    middleware::ensure_token(session)
}

/// フレームワークが中で使う鍵の頭。
///
/// [`FLASH_PREFIX`]・[`OLD_INPUT_KEY`]・CSRF のトークンの鍵は、どれもこれで始まります。
/// 利用者の鍵と同じ地図に入るので、`put` / `flash` に来たら警告し、
/// `all()` からは外します。
const RESERVED_PREFIX: &str = "__";

/// `__` で始まる鍵を利用者が入れようとしたら知らせる。
///
/// **断りません。** これまで通っていた呼び出しを落とさないためです。
fn warn_reserved_key(method: &str, key: &str) {
    if key.starts_with(RESERVED_PREFIX) {
        tracing::warn!(
            "Session::{method}(\"{key}\", ..): `__` で始まる鍵はフレームワークが中で使います。\
             思わぬ消え方をするので、別の名前にしてください"
        );
    }
}

/// 持ち越しの値に付ける目印。
const FLASH_PREFIX: &str = "__flash:";

/// 前の入力を入れておく場所。
const OLD_INPUT_KEY: &str = "__old_input";

/// 検査に落ちた入力として覚える合計の上限（バイト）。
const MAX_OLD_INPUT_BYTES: usize = 64 * 1024;

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
    fn jsonで入れて型で読める() {
        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct Cart {
            items: Vec<i64>,
        }
        let s = session();
        s.put_json("cart", &Cart { items: vec![1, 2] }).unwrap();
        assert_eq!(s.get("cart").as_deref(), Some(r#"{"items":[1,2]}"#));
        assert_eq!(
            s.get_json::<Cart>("cart").unwrap(),
            Some(Cart { items: vec![1, 2] })
        );
        // `?Sized` なので、文字列やスライスもそのまま渡せる。
        s.put_json("tags", ["a", "b"].as_slice()).unwrap();
        assert_eq!(
            s.get_json::<Vec<String>>("tags").unwrap().unwrap(),
            ["a", "b"]
        );

        // 無い鍵は `None`。
        assert_eq!(s.get_json::<Cart>("none").unwrap(), None);

        // 壊れた JSON は `Err`。黙って捨てない。
        s.put("broken", "{壊れた");
        let error = s.get_json::<Cart>("broken").unwrap_err().to_string();
        assert!(
            error.contains("セッションの `broken` を読めません"),
            "{error}"
        );
        assert!(s.has("broken"), "値は残す");
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
        let saved = s.snapshot_for_save();
        assert!(saved.contains_key("__flash:status"));
        assert_eq!(saved.get("keep").map(String::as_str), Some("always"));

        // 2回目: 読み込むと flash は「持ち越し」として入り、読める。
        let (data, old_flash) = Session::split_flash(saved);
        let s2 = Session::new("id".into(), data, old_flash);
        assert_eq!(s2.get("status").as_deref(), Some("ok"));
        assert_eq!(s2.get("keep").as_deref(), Some("always"));

        // 2回目で何も足さずに保存すると、持ち越しは消える。
        let saved2 = s2.snapshot_for_save();
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
    fn 戻り先は一度だけ取り出せる() {
        let s = session();
        assert!(s.intended().is_none(), "覚えていなければ None");

        // `Authenticate` が入れる形。
        s.put(crate::auth::INTENDED_KEY, "/profile?page=2");
        assert_eq!(s.intended().as_deref(), Some("/profile?page=2"));
        assert!(s.intended().is_none(), "読むと消える");
    }

    #[test]
    fn 外を指す戻り先は読み出しのときも断る() {
        // 入れるときだけ見ていると、置き場所の値が別の経路で書かれたときに
        // ログイン後に外のサイトへ飛ばせる。
        for bad in ["//evil.example", "/\\evil.example", "https://evil.example"] {
            let s = session();
            s.put(crate::auth::INTENDED_KEY, bad);
            assert!(s.intended().is_none(), "`{bad}` は断るはず");
            assert!(
                s.get(crate::auth::INTENDED_KEY).is_none(),
                "断っても消しておく"
            );
        }
    }

    #[test]
    fn 置き場所から読めたかを覚えている() {
        // 読めていないセッションには、落ちた入力を覚えさせない。
        assert!(!Session::empty().was_loaded());
        assert!(Session::empty().mark_loaded().was_loaded());
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
    fn 全部の値に内部の鍵は入らない() {
        // CSRF のトークンと検査に落ちた入力は、`all()` をそのまま画面やログに
        // 出したときに漏れる。持ち越しの目印つきの鍵も出さない。
        let mut old_flash = Data::new();
        old_flash.insert("status".into(), "ok".into());
        old_flash.insert(OLD_INPUT_KEY.into(), "{}".into());
        let s = Session::new("id".into(), Data::new(), old_flash);
        s.put("a", "1");
        let _ = csrf_token(&s);
        s.flash_input(&[("title".to_string(), "下書き".to_string())]);
        // 利用者が入れてしまった `__` 始まりも、内部の鍵と同じ扱い。
        s.put("__mine", "x");
        s.flash("__flash:y", "z");

        let all = s.all();
        let keys: Vec<&str> = all.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["a", "status"], "内部の鍵は出さない");

        // 外すのは `all()` だけ。個別には今までどおり読める。
        assert!(s.get(super::middleware::CSRF_KEY).is_some());
        assert_eq!(s.old("title").as_deref(), Some("下書き"));
        assert_eq!(s.get("__mine").as_deref(), Some("x"), "警告は出すが入る");
    }

    #[test]
    fn 内部の鍵かどうかを頭で見分ける() {
        // 警告を出す条件。内部の 3 つの鍵がどれも当たること。
        for key in [FLASH_PREFIX, OLD_INPUT_KEY, super::middleware::CSRF_KEY] {
            assert!(key.starts_with(RESERVED_PREFIX), "{key}");
        }
        // 認証の鍵は `__` で始まらない（`put` で入れても警告しない）。
        assert!(!crate::auth::INTENDED_KEY.starts_with(RESERVED_PREFIX));
        // 呼んでも落ちない（警告を出すだけ）。
        warn_reserved_key("put", "__x");
        warn_reserved_key("put", "cart");
    }

    #[test]
    fn 持ち越しを消費せずに写し取れる() {
        // ルートに当たらなかったリクエストが書き換えたときの保存の形。
        let mut old_flash = Data::new();
        old_flash.insert("status".into(), "保存しました".into());
        old_flash.insert("both".into(), "前回".into());
        let s = Session::new("id".into(), Data::new(), old_flash);
        s.put("keep", "always");
        s.flash("fresh", "今回");
        s.flash("both", "今回");

        let kept = s.snapshot_keeping_flash();
        assert_eq!(
            kept.get("__flash:status").map(String::as_str),
            Some("保存しました"),
            "前回ぶんも次回ぶんとして残す"
        );
        assert_eq!(kept.get("__flash:fresh").map(String::as_str), Some("今回"));
        assert_eq!(
            kept.get("__flash:both").map(String::as_str),
            Some("今回"),
            "同じ鍵は今回が勝つ"
        );
        assert_eq!(kept.get("keep").map(String::as_str), Some("always"));

        // ふつうの保存では前回ぶんを捨てる（こちらは変えていない）。
        assert!(!s.snapshot_for_save().contains_key("__flash:status"));

        // 読み込み直すと、持ち越しとしてもう一度読める。
        let (_, flash) = Session::split_flash(kept);
        assert_eq!(
            flash.get("status").map(String::as_str),
            Some("保存しました")
        );
    }

    #[test]
    fn 同じ鍵にflashしたら新しいほうを読む() {
        // `get` と `all` の順をそろえる。逆だと、あとから入れた flash が
        // `get` から永久に見えなくなる。
        let s = session();
        s.put("status", "1");
        s.flash("status", "2");
        assert_eq!(s.get("status").as_deref(), Some("2"));
        assert_eq!(s.all().get("status").map(String::as_str), Some("2"));

        // 持ち越しの値より、今回入れた flash が勝つ。
        let mut old_flash = Data::new();
        old_flash.insert("status".into(), "古い".into());
        let s = Session::new("id".into(), Data::new(), old_flash);
        assert_eq!(s.get("status").as_deref(), Some("古い"));
        s.flash("status", "新しい");
        assert_eq!(s.get("status").as_deref(), Some("新しい"));
        assert_eq!(s.all().get("status").map(String::as_str), Some("新しい"));
    }

    #[test]
    fn flashのあとにputしてもflashを読む() {
        // 「新しく入れたほうが勝つ」ではない。順番に関係なく flash が勝つ。
        let s = session();
        s.flash("status", "2");
        s.put("status", "1");
        assert_eq!(s.get("status").as_deref(), Some("2"));
        assert_eq!(s.all().get("status").map(String::as_str), Some("2"));
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

    #[test]
    fn 大きすぎる入力は覚えない() {
        let s = Session::empty();
        let big = "x".repeat(MAX_OLD_INPUT_BYTES + 1);
        s.flash_input(&[("body".to_string(), big)]);
        assert!(s.old("body").is_none(), "上限を超える入力は覚えない");

        s.flash_input(&[("title".to_string(), "小さい".to_string())]);
        assert_eq!(s.old("title").as_deref(), Some("小さい"));
    }
}
