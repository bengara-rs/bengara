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

use crate::database::Model;
use crate::error::{Error, Result};
use crate::session::Session;

/// セッションに入れる鍵の名前。
const KEY: &str = "bengara_auth_id";

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
        self.session?.get(KEY).filter(|v| !v.is_empty())
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
        session.put(KEY, id.to_string());
        Ok(())
    }

    /// ログアウトする。セッションの中身を捨て、ID も作り直します。
    pub fn logout(&self) -> Result<()> {
        self.session()?.invalidate();
        Ok(())
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
            blocking(move || {
                Hash::waste_time();
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

/// 時間のかかる計算を裏のスレッドで行う。
///
/// ハッシュの計算は 0.2 秒ほどかかります。そのまま待つと、同じスレッドで動いている
/// ほかのリクエストが止まります。
pub(crate) async fn blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::msg(format!("パスワードの計算に失敗しました: {e}")))
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
