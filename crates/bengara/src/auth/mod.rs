//! 認証と認可。
//!
//! 入口は `req.auth()` です。
//!
//! ```ignore
//! let ok = req.auth().attempt::<User>("email", email, password).await?;
//! let user = req.auth().user_or_fail::<User>().await?;
//! ```
//!
//! ログイン中の利用者を**どこからでも読める形**（Laravel の `Auth::user()`）は作りません。
//! ハンドラは `tokio::spawn` の中で動くため、タスクに紐づけた値が引き継がれないからです
//! （決定記録 #008、#047）。

mod hashing;
mod middleware;
mod reset;

#[cfg(feature = "encryption")]
mod encryption;

pub use hashing::{Hash, HashConfig};
pub use middleware::Authenticate;
pub use reset::PasswordReset;

pub(crate) use hashing::install_test_iterations;
pub(crate) use middleware::INTENDED_KEY;

use crate::database::Model;
use crate::error::{Error, Result};
use crate::session::Session;
use crate::support::blocking;

/// セッションに入れる鍵の名前。
///
/// `SessionStore` が「この利用者のセッション」を探すときにも見ます
/// （`destroy_for_user`）。
pub(crate) const AUTH_ID_KEY: &str = "bengara_auth_id";

/// 表と結びついた「ログインできる人」。
///
/// `Model` を継承しているので、主キーと表の名前は `#[derive(Model)]` から取れます。
/// 足すのはパスワードの取り出し方だけです。
///
/// ```ignore
/// impl Authenticatable for User {
///     fn password_hash(&self) -> &str {
///         &self.password
///     }
/// }
/// ```
pub trait Authenticatable: Model {
    /// 保存してあるパスワード（`Hash::make` が作った文字列）。
    fn password_hash(&self) -> &str;
}

/// ログインの状態を扱う。`req.auth()` が返します。
///
/// 中身はセッションへの参照だけなので、作るのは安いです。
pub struct Auth<'a> {
    session: Option<&'a Session>,
}

impl<'a> Auth<'a> {
    pub(crate) fn new(session: Option<&'a Session>) -> Self {
        Self { session }
    }

    /// セッションを取り出す。無ければ、登録し忘れとして知らせる。
    fn session(&self) -> Result<&Session> {
        self.session.ok_or_else(|| {
            Error::msg(
                "セッションが用意されていません。bootstrap/app.rs の with_middleware で \
                 StartSession を登録してください",
            )
        })
    }

    /// ログインしているか。
    pub fn check(&self) -> bool {
        self.id().is_some()
    }

    /// ログインしていないか。
    pub fn guest(&self) -> bool {
        !self.check()
    }

    /// ログイン中の主キーの値。
    pub fn id(&self) -> Option<String> {
        self.session?.get(AUTH_ID_KEY).filter(|v| !v.is_empty())
    }

    /// 照合せずにログインする。登録の直後などに使います。
    ///
    /// **セッション ID を作り直します。**
    pub fn login<U: Authenticatable>(&self, user: &U) -> Result<()> {
        self.login_using_id(user.key())
    }

    /// 主キーの値だけでログインする。
    pub fn login_using_id(&self, id: impl std::fmt::Display) -> Result<()> {
        let session = self.session()?;
        // 他人のセッション ID を押し付ける攻撃（セッション固定化）を防ぐ。
        session.regenerate();
        session.put(AUTH_ID_KEY, id.to_string());
        Ok(())
    }

    /// ログアウトする。セッションの中身を捨て、ID も作り直します。
    ///
    /// **このセッションだけを切ります。** 同じ利用者が別のブラウザや別の端末で
    /// ログインしていても、そちらは切れません。
    /// 他の端末も切るときは [`logout_other_devices`](Self::logout_other_devices) を
    /// 使ってください。
    pub fn logout(&self) -> Result<()> {
        self.session()?.invalidate();
        Ok(())
    }

    /// 同じ利用者のセッションのうち、**このセッション以外**を切る。消した数を返す。
    ///
    /// パスワードを変えたあとに呼びます。変えただけでは、盗まれたセッションが
    /// 期限まで生き残ります。
    ///
    /// ```ignore
    /// // パスワードを書き換えたあとで
    /// req.auth().logout_other_devices()?;
    /// ```
    ///
    /// - ログインしていなければ何もせず `0` を返します。
    /// - 置き場所の中を全部見ます。**時間のかかる操作です**が、呼ぶ回数は少ないので
    ///   索引は持ちません。
    /// - 自分で作った `SessionStore` が `destroy_for_user_except` を実装していなければ、
    ///   警告を出して `0` を返します。
    pub fn logout_other_devices(&self) -> Result<usize> {
        let session = self.session()?;
        let Some(user_id) = self.id() else {
            return Ok(0);
        };
        session
            .store()?
            .destroy_for_user_except(&user_id, &session.id())
    }

    /// 同じ利用者のセッションを**全部**切る。消した数を返す。
    ///
    /// このセッションも切れるので、呼んだ本人もログアウトします。
    /// 「全部の端末からログアウト」のボタンに使います。
    ///
    /// 注意は [`logout_other_devices`](Self::logout_other_devices) と同じです。
    pub fn logout_all_devices(&self) -> Result<usize> {
        let session = self.session()?;
        let Some(user_id) = self.id() else {
            return Ok(0);
        };
        let removed = session.store()?.destroy_for_user(&user_id)?;
        // いまのセッションは、置き場所から消したうえで Cookie も作り直す。
        self.logout()?;
        Ok(removed)
    }

    /// **指定した利用者**のセッションを全部切る。消した数を返す。
    ///
    /// パスワードの再設定のように、**本人がログインしていない場面**で使います。
    /// ログイン中の人が自分の他の端末を切るときは
    /// [`logout_other_devices`](Self::logout_other_devices) を使ってください。
    ///
    /// ```ignore
    /// // パスワードを書き換えたあとで、その人のセッションを全部切る
    /// req.auth().logout_user(user.key())?;
    /// ```
    ///
    /// 注意は [`logout_other_devices`](Self::logout_other_devices) と同じです。
    pub fn logout_user(&self, user_id: impl std::fmt::Display) -> Result<usize> {
        self.session()?
            .store()?
            .destroy_for_user(&user_id.to_string())
    }

    /// ログイン中の利用者を DB から読む。
    ///
    /// **呼ぶたびに DB を引きます。** 何度も使うときは変数に入れてください。
    pub async fn user<U: Authenticatable>(&self) -> Result<Option<U>> {
        let Some(id) = self.id() else {
            return Ok(None);
        };
        U::query().where_(U::PRIMARY_KEY, id).first().await
    }

    /// ログイン中の利用者を読む。いなければ 401 のエラー。
    pub async fn user_or_fail<U: Authenticatable>(&self) -> Result<U> {
        self.user::<U>()
            .await?
            .ok_or_else(|| Error::http(401, "ログインしてください"))
    }

    /// 列の値とパスワードで照合し、合えばログインする。
    ///
    /// ```ignore
    /// let ok = req.auth().attempt::<User>("email", &email, &password).await?;
    /// ```
    ///
    /// - 合わなければ `false` を返します。**エラーにはしません**（理由を外に出さないため）。
    /// - 利用者がいない場合も、**照合と同じだけ時間をかけます**（登録の有無が漏れないため）。
    /// - 照合は裏のスレッドで行うので、ほかのリクエストを待たせません。
    pub async fn attempt<U: Authenticatable>(
        &self,
        column: &str,
        value: &str,
        password: &str,
    ) -> Result<bool> {
        let found = U::query().where_(column, value).first().await?;
        let password = password.to_string();

        let Some(user) = found else {
            // 見つからないときも時間を合わせる。
            // **送られてきたパスワードをそのまま渡します。** 固定のダミーだと、
            // 長い入力のときに登録済みの側だけが重くなり、登録の有無が漏れます。
            blocking(move || {
                Hash::waste_time(&password);
                false
            })
            .await?;
            return Ok(false);
        };

        let stored = user.password_hash().to_string();
        let ok = blocking(move || Hash::check(&password, &stored)).await?;
        if ok {
            self.login(&user)?;
        }
        Ok(ok)
    }

    /// パスワードだけを確かめる。ログインはしません。
    ///
    /// 「パスワードの再入力」を求める画面で使います。
    pub async fn validate_password<U: Authenticatable>(
        &self,
        user: &U,
        password: &str,
    ) -> Result<bool> {
        let stored = user.password_hash().to_string();
        let password = password.to_string();
        blocking(move || Hash::check(&password, &stored)).await
    }
}

/// 許されていなければ 403 で止める。
///
/// ```ignore
/// authorize(PostPolicy::update(&user, &post))?;
/// ```
///
/// 判定そのものは `app/Policies/` に書きます。フレームワークは表を持ちません
/// （決定記録 #049）。
pub fn authorize(allowed: bool) -> Result<()> {
    authorize_with(allowed, "この操作は許可されていません")
}

/// メッセージを指定して 403 で止める。
pub fn authorize_with(allowed: bool, message: impl Into<String>) -> Result<()> {
    if allowed {
        return Ok(());
    }
    Err(Error::http(403, message))
}

/// 暗号化（機能フラグ `encryption`）。
///
/// 値を他人に読めない形にします。署名（改ざんの検出）は `signed_url` 側です。
pub fn encrypt(plain: &str) -> Result<String> {
    #[cfg(feature = "encryption")]
    {
        encryption::encrypt(plain)
    }
    #[cfg(not(feature = "encryption"))]
    {
        let _ = plain;
        Err(missing_encryption())
    }
}

/// 暗号化した値を元に戻す。改ざんされていればエラー。
pub fn decrypt(cipher: &str) -> Result<String> {
    #[cfg(feature = "encryption")]
    {
        encryption::decrypt(cipher)
    }
    #[cfg(not(feature = "encryption"))]
    {
        let _ = cipher;
        Err(missing_encryption())
    }
}

#[cfg(not(feature = "encryption"))]
fn missing_encryption() -> Error {
    Error::msg(
        "暗号化を使うには Cargo.toml を \
         `bengara = { version = \"0.1\", features = [\"encryption\"] }` にしてください",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 許可されていなければ403になる() {
        assert!(authorize(true).is_ok());
        let error = authorize(false).unwrap_err();
        assert_eq!(error.status(), 403);
        assert_eq!(error.public_message(), "この操作は許可されていません");

        let error = authorize_with(false, "あなたの記事ではありません").unwrap_err();
        assert_eq!(error.status(), 403);
        assert_eq!(error.public_message(), "あなたの記事ではありません");
    }

    #[test]
    fn セッションが無ければログインできない() {
        let auth = Auth::new(None);
        assert!(!auth.check());
        assert!(auth.guest());
        assert_eq!(auth.id(), None);
        assert!(auth.login_using_id(1).is_err());
        assert!(auth.logout().is_err());
        assert!(auth.logout_other_devices().is_err());
        assert!(auth.logout_all_devices().is_err());
    }

    #[test]
    fn ログインしていなければ他の端末は切らない() {
        let session = Session::empty();
        let auth = Auth::new(Some(&session));
        assert_eq!(auth.logout_other_devices().unwrap(), 0);
        assert_eq!(auth.logout_all_devices().unwrap(), 0);
    }

    #[test]
    fn 他の端末のセッションだけ切れる() {
        use crate::session::{MemoryStore, SessionStore};
        use std::collections::BTreeMap;
        use std::sync::Arc;

        let store = Arc::new(MemoryStore::new(60));

        // 置き場所はセッションに結びつく。`StartSession` がこれをやります。
        let session = Session::empty().with_store(store.clone());
        let auth = Auth::new(Some(&session));
        auth.login_using_id(7).unwrap();
        let mine = session.id();

        let data = |user_id: &str| {
            let mut data = BTreeMap::new();
            data.insert(AUTH_ID_KEY.to_string(), user_id.to_string());
            data
        };
        store.write(&mine, &data("7")).unwrap();
        store.write("sameuser", &data("7")).unwrap();
        store.write("otheruser", &data("8")).unwrap();

        assert_eq!(auth.logout_other_devices().unwrap(), 1);
        assert!(store.read(&mine).unwrap().is_some(), "自分は残る");
        assert!(
            store.read("sameuser").unwrap().is_none(),
            "他の端末は切れる"
        );
        assert!(
            store.read("otheruser").unwrap().is_some(),
            "他の利用者は残る"
        );

        // 全部切ると自分も切れ、ログアウトもする。
        assert_eq!(auth.logout_all_devices().unwrap(), 1);
        assert!(store.read(&mine).unwrap().is_none());
        assert!(!auth.check(), "呼んだ本人もログアウトする");
    }

    #[test]
    fn 利用者を指定して切れる() {
        use crate::session::{MemoryStore, SessionStore};
        use std::collections::BTreeMap;
        use std::sync::Arc;

        let store = Arc::new(MemoryStore::new(60));
        // パスワードの再設定の場面。本人はログインしていない。
        let session = Session::empty().with_store(store.clone());
        let auth = Auth::new(Some(&session));
        assert!(!auth.check());

        let data = |user_id: &str| {
            let mut data = BTreeMap::new();
            data.insert(AUTH_ID_KEY.to_string(), user_id.to_string());
            data
        };
        store.write("a", &data("7")).unwrap();
        store.write("b", &data("7")).unwrap();
        store.write("c", &data("8")).unwrap();

        assert_eq!(auth.logout_user(7).unwrap(), 2);
        assert!(store.read("a").unwrap().is_none());
        assert!(store.read("b").unwrap().is_none());
        assert!(store.read("c").unwrap().is_some(), "他の利用者は残る");
    }

    #[cfg(not(feature = "encryption"))]
    #[test]
    fn 機能フラグが無ければ直し方を知らせる() {
        let error = encrypt("x").unwrap_err();
        assert!(error.to_string().contains("features = [\"encryption\"]"));
        let error = decrypt("x").unwrap_err();
        assert!(error.to_string().contains("features = [\"encryption\"]"));
    }
}
