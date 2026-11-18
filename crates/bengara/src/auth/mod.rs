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
pub use middleware::{Authenticate, RedirectIfAuthenticated};
pub use reset::PasswordReset;

pub(crate) use hashing::install_test_iterations;
pub(crate) use middleware::{is_internal_path, INTENDED_KEY};

use crate::database::Model;
use crate::error::{Error, Result};
use crate::session::Session;
use crate::support::blocking;

/// セッションに入れる鍵の名前。
///
/// `SessionStore` が「この利用者のセッション」を探すときにも見ます
/// （`destroy_for_user`）。
pub(crate) const AUTH_ID_KEY: &str = "bengara_auth_id";

/// ログインした時刻（UNIX ミリ秒）を入れる鍵の名前。
///
/// `logout_user` などで「切った」印より前にログインしたセッションだけを
/// 捨てるために使います（`StartSession` が保存の直前に見ます）。
pub(crate) const AUTH_AT_KEY: &str = "bengara_auth_at";

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

    /// パスワードを入れている列の名前。Laravel の `getAuthPasswordName` に当たります。
    ///
    /// `attempt` がログインのついでにパスワードを作り直すとき、この列に書きます。
    /// 既定は `"password"` です。列の名前が違うときだけ書き換えてください。
    ///
    /// ```ignore
    /// impl Authenticatable for User {
    ///     fn password_hash(&self) -> &str {
    ///         &self.password_digest
    ///     }
    ///     fn auth_password_name() -> &'static str {
    ///         "password_digest"
    ///     }
    /// }
    /// ```
    fn auth_password_name() -> &'static str {
        "password"
    }
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
    ///
    /// **セッションだけを見ます。DB は引きません。** 利用者の行を消しても、
    /// その人のセッションが残っているあいだは真のままです（`Authenticate` も
    /// 同じ判定です）。利用者を消したときは [`logout_user`](Self::logout_user) で
    /// セッションも切ってください。
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
        session.put(AUTH_AT_KEY, crate::support::time::now_millis().to_string());
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
    /// - **呼んだ本人は残ります。** このあと `regenerate()` で ID を作り直しても
    ///   残るように、このセッションの「ログインした時刻」を切った時刻より後に
    ///   進めます。
    ///
    /// **非同期の処理の中では
    /// [`logout_other_devices_async`](Self::logout_other_devices_async) を使ってください。**
    pub fn logout_other_devices(&self) -> Result<usize> {
        let session = self.session()?;
        let Some(user_id) = self.id() else {
            return Ok(0);
        };
        let store = session.store()?;
        let (removed, logged_in_at) = revoke_others(&*store, &user_id, &session.id())?;
        if let Some(at) = logged_in_at {
            session.put(AUTH_AT_KEY, at.to_string());
        }
        Ok(removed)
    }

    /// [`logout_other_devices`](Self::logout_other_devices) を裏のスレッドで行う。
    ///
    /// **非同期の処理（ハンドラ・ジョブ・コマンドの `async fn`）の中では
    /// こちらを使ってください。** 同期版は置き場所の中を全部見るので、
    /// そのあいだ tokio のワーカースレッドを塞ぎます。
    ///
    /// ```ignore
    /// // パスワードを書き換えたあとで
    /// req.auth().logout_other_devices_async().await?;
    /// ```
    pub async fn logout_other_devices_async(&self) -> Result<usize> {
        let session = self.session()?;
        let Some(user_id) = self.id() else {
            return Ok(0);
        };
        let store = session.store()?;
        let keep_id = session.id();
        let (removed, logged_in_at) =
            crate::support::blocking(move || revoke_others(&*store, &user_id, &keep_id)).await??;
        if let Some(at) = logged_in_at {
            session.put(AUTH_AT_KEY, at.to_string());
        }
        Ok(removed)
    }

    /// 同じ利用者のセッションを**全部**切る。消した数を返す。
    ///
    /// このセッションも切れるので、呼んだ本人もログアウトします。
    /// 「全部の端末からログアウト」のボタンに使います。
    ///
    /// 注意は [`logout_other_devices`](Self::logout_other_devices) と同じです。
    ///
    /// **非同期の処理の中では
    /// [`logout_all_devices_async`](Self::logout_all_devices_async) を使ってください。**
    pub fn logout_all_devices(&self) -> Result<usize> {
        let session = self.session()?;
        let Some(user_id) = self.id() else {
            return Ok(0);
        };
        let removed = session.store()?.destroy_for_user(&user_id)?;
        // いまのセッションは、置き場所から消したうえで Cookie も消す（新しい ID は配らない）。
        self.logout()?;
        Ok(removed)
    }

    /// [`logout_all_devices`](Self::logout_all_devices) を裏のスレッドで行う。
    ///
    /// **非同期の処理の中ではこちらを使ってください。**
    ///
    /// ```ignore
    /// let removed = req.auth().logout_all_devices_async().await?;
    /// ```
    pub async fn logout_all_devices_async(&self) -> Result<usize> {
        let session = self.session()?;
        let Some(user_id) = self.id() else {
            return Ok(0);
        };
        let store = session.store()?;
        let removed = crate::support::blocking(move || store.destroy_for_user(&user_id)).await??;
        // いまのセッションは、置き場所から消したうえで Cookie も消す（新しい ID は配らない）。
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
    ///
    /// **非同期の処理の中では [`logout_user_async`](Self::logout_user_async) を
    /// 使ってください。**
    pub fn logout_user(&self, user_id: impl std::fmt::Display) -> Result<usize> {
        self.session()?
            .store()?
            .destroy_for_user(&user_id.to_string())
    }

    /// [`logout_user`](Self::logout_user) を裏のスレッドで行う。
    ///
    /// **非同期の処理の中ではこちらを使ってください。**
    ///
    /// ```ignore
    /// // パスワードを書き換えたあとで、その人のセッションを全部切る
    /// req.auth().logout_user_async(user.key()).await?;
    /// ```
    pub async fn logout_user_async(&self, user_id: impl std::fmt::Display) -> Result<usize> {
        let store = self.session()?.store()?;
        let user_id = user_id.to_string();
        crate::support::blocking(move || store.destroy_for_user(&user_id)).await?
    }

    /// ログイン中の利用者を DB から読む。
    ///
    /// **呼ぶたびに DB を引きます。** 何度も使うときは変数に入れてください。
    pub async fn user<U: Authenticatable>(&self) -> Result<Option<U>> {
        let Some(id) = self.id() else {
            return Ok(None);
        };
        // セッションには文字列で入っている。整数の主キーなら `find` が整数に直して
        // 束縛する（PostgreSQL は `bigint = text` を断る）。
        //
        // **整数に読めない値は、問い合わせずに `None` です。** `where_(主キー, ..)
        // .first()` と書いていたころは、`where 1 = 0` を 1 往復ぶん流していました。
        U::find(id).await
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
    /// - 合ったとき、保存されている値の回数が設定より少なければ
    ///   （[`Hash::needs_rehash`]）、**その場で作り直して保存します**
    ///   （Laravel の `rehashOnLogin`）。書く列は
    ///   [`Authenticatable::auth_password_name`] です。
    ///   作り直しに失敗しても、ログインは成功させます（警告をログに出します）。
    pub async fn attempt<U: Authenticatable>(
        &self,
        column: &str,
        value: &str,
        password: &str,
    ) -> Result<bool> {
        let found = U::query().where_(column, value).first().await?;
        let plain = password;
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
            if Hash::needs_rehash(user.password_hash()) {
                if let Err(e) = rehash_password(&user, plain).await {
                    tracing::warn!("ログインのついでにパスワードを作り直せませんでした: {e}");
                }
            }
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

/// パスワードをいまの設定で作り直し、主キーで引いた行に書く。
///
/// 書くのは [`Authenticatable::auth_password_name`] の列だけです。`save()` を使わないのは、
/// 呼ぶ側が持っている構造体（読んだあとに変わっているかもしれない）を丸ごと
/// 書き戻さないためです。
async fn rehash_password<U: Authenticatable>(user: &U, password: &str) -> Result<()> {
    let hashed = Hash::make_async(password).await?;
    let affected = U::query()
        .where_(U::PRIMARY_KEY, user.key())
        .update(&[(
            U::auth_password_name(),
            crate::database::Value::Text(hashed),
        )])
        .await?;
    if affected == 0 {
        return Err(Error::msg("当てはまる行がありませんでした"));
    }
    Ok(())
}

/// 同じ利用者のセッションのうち `keep_id` 以外を切り、
/// 「消した数」と「残したセッションに付け直すログインの時刻（UNIX ミリ秒）」を返す。
///
/// # なぜ時刻を付け直すのか
///
/// 置き場所は「切った時刻・除外は `keep_id`」の印を残します。`StartSession` は
/// 保存の直前に、印がログインの時刻より新しければ切られたものとして捨てます。
/// 除外は **ID で** 覚えているので、呼んだ本人がこのあと `regenerate()` で ID を
/// 作り直すと、新しい ID は除外に当たりません。ログインの時刻が古いままだと、
/// **次のリクエストの保存で本人が切られます**（パスワードを変える → 他の端末を
/// 切る → ID を作り直す、という勧めている流れで起きていました）。
///
/// そこで、残すセッションのログインの時刻を**印より後**に進めます。
/// 「印より後にログインし直したセッションは巻き込まない」という、もとからある
/// 規則にそのまま乗る形です。ID が何度変わっても残ります。
///
/// # 印と同じミリ秒にならないこと
///
/// 比較は「印 > ログインの時刻」です。時計が戻ったときにも負けないよう、
/// **自分が付けた印の時刻 + 1 と、いまの時刻の大きいほう**にします。
///
/// 印の時刻は、どのセッションとも重ならない空の ID で置き場所に聞きます
/// （除外した ID で聞くと `None` が返るため）。印を持たない置き場所
/// （`mark_revoked` を実装していないもの）では、いまの時刻になります。
///
/// # 別の所が続けて切ったとき（返す時刻が `None`）
///
/// 印は利用者ごとに 1 つで、新しいもので置き換わります。自分が印を付けてから
/// 読み直すまでのあいだに、別の所が `logout_user`（除外なし）などで印を付け直すと、
/// 読んだ印は**相手のもの**です。それより後に時刻を進めると、本人まで切るはずの
/// 操作から本人だけが逃げてしまいます。
///
/// そこで、**残す ID（`keep_id`）で印を聞き直します。** 自分の印なら除外に当たって
/// `None` が返ります。値が返るなら、最新の印は本人も切る相手の印です。
/// このときは時刻を進めず `None` を返し、本人のセッションは次の保存で切られます。
///
/// 聞き直したあとに相手の印が付いた場合は、同じミリ秒に重なったときだけ逃げられます。
/// 印を比べる仕組み（`SessionStore` の形）を変えずに塞げるのはここまでです。
fn revoke_others(
    store: &dyn crate::session::SessionStore,
    user_id: &str,
    keep_id: &str,
) -> Result<(usize, Option<i64>)> {
    let removed = store.destroy_for_user_except(user_id, keep_id)?;
    let mark = store.revoked_at(user_id, "");
    // 最新の印が本人にも効くなら、自分の印ではない（上の説明を参照）。
    if store.revoked_at(user_id, keep_id).is_some() {
        tracing::info!("他の端末を切っている間に、別の所がこの利用者を切りました");
        return Ok((removed, None));
    }
    let logged_in_at = crate::support::time::now_millis().max(mark.unwrap_or(0).saturating_add(1));
    Ok((removed, Some(logged_in_at)))
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
    // 「`features = ["encryption"]` にしてください」と書いていたころは、そのまま
    // 写すと `sqlite` などのほかの機能フラグが落ちた。足す形で案内する。
    Error::msg(
        "暗号化を使うには、Cargo.toml の bengara の features に \
         `\"encryption\"` を足してください（いまある機能フラグは残します）",
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

    /// `user()` を試すための利用者。整数の主キー（既定）です。
    struct Member {
        id: i64,
        password: String,
    }

    impl Model for Member {
        const TABLE: &'static str = "members";
        const PRIMARY_KEY: &'static str = "id";
        const COLUMNS: &'static [&'static str] = &["id", "password"];

        fn from_row(row: &crate::database::Row) -> Result<Self> {
            Ok(Self {
                id: row.get("id")?,
                password: row.get("password")?,
            })
        }

        fn attributes(&self) -> Vec<(&'static str, crate::database::Value)> {
            vec![(
                "password",
                crate::database::Value::Text(self.password.clone()),
            )]
        }

        fn key(&self) -> crate::database::Value {
            crate::database::Value::Int(self.id)
        }

        fn set_key(&mut self, _value: crate::database::Value) {}
    }

    impl Authenticatable for Member {
        fn password_hash(&self) -> &str {
            &self.password
        }
    }

    #[test]
    fn パスワードの列の名前は既定でpassword() {
        assert_eq!(Member::auth_password_name(), "password");
    }

    /// ログインのついでの作り直しを、メモリ上の SQLite で確かめる。
    #[cfg(feature = "sqlite")]
    mod rehash_on_login {
        use super::*;
        use crate::database::{Value, DB};

        /// パスワードの列が `secret` の利用者。列の名前を書き換える形を試す。
        struct Login {
            id: i64,
            secret: String,
        }

        impl Model for Login {
            const TABLE: &'static str = "logins";
            const PRIMARY_KEY: &'static str = "id";
            const COLUMNS: &'static [&'static str] = &["id", "email", "secret"];

            fn from_row(row: &crate::database::Row) -> Result<Self> {
                Ok(Self {
                    id: row.get("id")?,
                    secret: row.get("secret")?,
                })
            }

            fn attributes(&self) -> Vec<(&'static str, Value)> {
                vec![("secret", Value::Text(self.secret.clone()))]
            }

            fn key(&self) -> Value {
                Value::Int(self.id)
            }

            fn set_key(&mut self, _value: Value) {}
        }

        impl Authenticatable for Login {
            fn password_hash(&self) -> &str {
                &self.secret
            }
            fn auth_password_name() -> &'static str {
                "secret"
            }
        }

        /// 同じ表で、書く列の名前を間違えた利用者。作り直しが必ず失敗する。
        struct BrokenLogin(Login);

        impl Model for BrokenLogin {
            const TABLE: &'static str = "logins";
            const PRIMARY_KEY: &'static str = "id";
            const COLUMNS: &'static [&'static str] = &["id", "email", "secret"];

            fn from_row(row: &crate::database::Row) -> Result<Self> {
                Login::from_row(row).map(Self)
            }

            fn attributes(&self) -> Vec<(&'static str, Value)> {
                self.0.attributes()
            }

            fn key(&self) -> Value {
                self.0.key()
            }

            fn set_key(&mut self, _value: Value) {}
        }

        impl Authenticatable for BrokenLogin {
            fn password_hash(&self) -> &str {
                &self.0.secret
            }
            fn auth_password_name() -> &'static str {
                "no_such_column"
            }
        }

        async fn setup(secret: &str) -> crate::testing::DatabaseGuard {
            let guard = crate::testing::refresh_database().await;
            DB::statement(
                "CREATE TABLE logins (id INTEGER PRIMARY KEY, email TEXT NOT NULL, secret TEXT NOT NULL)",
                &[],
            )
            .await
            .expect("表が作れる");
            DB::statement(
                "INSERT INTO logins (email, secret) VALUES (?, ?)",
                &[
                    Value::Text("a@example.com".to_string()),
                    Value::Text(secret.to_string()),
                ],
            )
            .await
            .expect("行が入る");
            guard
        }

        async fn stored_secret() -> String {
            DB::table("logins")
                .where_("email", "a@example.com")
                .value::<String>("secret")
                .await
                .unwrap()
                .expect("行がある")
        }

        #[tokio::test]
        async fn 回数が古ければログインのついでに作り直す() {
            install_test_iterations();
            let old = Hash::make_with("ひみつ", 1);
            let _db = setup(&old).await;
            assert!(Hash::needs_rehash(&old), "回数 1 は作り直す対象");

            // 合わないときは作り直さない。
            let session = Session::empty();
            let auth = Auth::new(Some(&session));
            assert!(!auth
                .attempt::<Login>("email", "a@example.com", "ちがう")
                .await
                .unwrap());
            assert_eq!(stored_secret().await, old, "外れたら書かない");

            // 合えば作り直して `secret` 列に書く。ログインもする。
            assert!(auth
                .attempt::<Login>("email", "a@example.com", "ひみつ")
                .await
                .unwrap());
            assert!(auth.check());
            let new = stored_secret().await;
            assert_ne!(new, old, "作り直した");
            assert!(!Hash::needs_rehash(&new), "いまの設定の回数になった");
            assert!(Hash::check("ひみつ", &new), "作り直した値でも照合できる");

            // もう一度ログインしても、作り直す必要はないので変わらない。
            let session = Session::empty();
            let auth = Auth::new(Some(&session));
            assert!(auth
                .attempt::<Login>("email", "a@example.com", "ひみつ")
                .await
                .unwrap());
            assert_eq!(stored_secret().await, new, "要らなければ書かない");
        }

        #[tokio::test]
        async fn 作り直しに失敗してもログインは成功する() {
            install_test_iterations();
            let old = Hash::make_with("ひみつ", 1);
            let _db = setup(&old).await;

            let session = Session::empty();
            let auth = Auth::new(Some(&session));
            assert!(
                auth.attempt::<BrokenLogin>("email", "a@example.com", "ひみつ")
                    .await
                    .unwrap(),
                "書けない列でもログインは通す"
            );
            assert!(auth.check());
            assert_eq!(stored_secret().await, old, "書けなかったので元のまま");
        }
    }

    #[tokio::test]
    async fn 整数に読めないidのuserは問い合わせずにnoneを返す() {
        // 既定の接続には流れない（流れれば、つなぐところで別のエラーになる）。
        let session = Session::empty();
        let auth = Auth::new(Some(&session));
        for id in ["abc", "12abc", "99999999999999999999999"] {
            session.put(AUTH_ID_KEY, id);
            let user = auth.user::<Member>().await.expect("エラーにしない");
            assert!(user.is_none(), "{id}");
            // `user_or_fail` は、これまでどおり 401。
            let error = auth.user_or_fail::<Member>().await.err().expect("401");
            assert_eq!(error.status(), 401, "{id}");
        }

        // ログインしていなければ、これまでどおり `None`。
        let guest = Session::empty();
        let user = Auth::new(Some(&guest)).user::<Member>().await.unwrap();
        assert!(user.is_none());
        assert!(Auth::new(None).user::<Member>().await.unwrap().is_none());
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

    /// セッションに入っているログインの時刻。
    fn logged_in_at(session: &Session) -> i64 {
        session
            .get(AUTH_AT_KEY)
            .and_then(|v| v.parse().ok())
            .expect("ログインの時刻が入っている")
    }

    #[test]
    fn 他の端末を切るとログインの時刻が印より後になる() {
        // 印の除外は ID で覚えている。呼んだ本人が ID を作り直すと除外に
        // 当たらなくなるので、ログインの時刻のほうを印より後に進めておく。
        use crate::session::{MemoryStore, SessionStore};
        use std::sync::Arc;

        let store = Arc::new(MemoryStore::new(60));
        let session = Session::empty().with_store(store.clone());
        let auth = Auth::new(Some(&session));
        auth.login_using_id(7).unwrap();
        let before = logged_in_at(&session);

        assert_eq!(auth.logout_other_devices().unwrap(), 0);
        let mark = store
            .revoked_at("7", "ほかの端末")
            .expect("切った印が付いている");
        let after = logged_in_at(&session);
        assert!(after > mark, "印より後: {after} > {mark}");
        assert!(after >= before, "時刻は戻さない");

        // ID を作り直すと除外には当たらないが、時刻の比較で残る
        // （`StartSession` の判定は「印 > ログインの時刻」）。
        session.regenerate();
        let at = store
            .revoked_at("7", &session.id())
            .expect("新しい ID は除外に当たらない");
        assert!(at <= logged_in_at(&session), "切られた扱いにならない");
        assert!(auth.check(), "ログインしたまま");
    }

    #[test]
    fn 付け直す時刻は印と同じミリ秒にならない() {
        use crate::session::SessionStore;
        use std::collections::BTreeMap;

        /// 印の時刻だけを決められる置き場所。
        struct Marked(Option<i64>);
        impl SessionStore for Marked {
            fn read(&self, _id: &str) -> Result<Option<BTreeMap<String, String>>> {
                Ok(None)
            }
            fn write(&self, _id: &str, _data: &BTreeMap<String, String>) -> Result<()> {
                Ok(())
            }
            fn destroy(&self, _id: &str) -> Result<()> {
                Ok(())
            }
            fn destroy_for_user_except(&self, _user_id: &str, _keep_id: &str) -> Result<usize> {
                Ok(3)
            }
            fn revoked_at(&self, _user_id: &str, session_id: &str) -> Option<i64> {
                // 除外した ID には `None`（`FileStore` / `MemoryStore` と同じ約束）。
                if session_id == "mine" {
                    return None;
                }
                self.0
            }
        }

        let now = crate::support::time::now_millis();

        // 印がいまより未来（時計が戻った・別のプロセスの時計が進んでいる）でも、
        // 必ず印より後になる。
        let future = now + 60_000;
        let (removed, at) = revoke_others(&Marked(Some(future)), "7", "mine").unwrap();
        assert_eq!(removed, 3);
        assert_eq!(at, Some(future + 1), "印 + 1 ミリ秒");

        // 印がいまと同じミリ秒でも、同じ値にはならない。
        let (_, at) = revoke_others(&Marked(Some(now)), "7", "mine").unwrap();
        assert!(at.unwrap() > now, "{at:?} > {now}");

        // 印を持たない置き場所では、いまの時刻。
        let (_, at) = revoke_others(&Marked(None), "7", "mine").unwrap();
        let at = at.unwrap();
        assert!(at >= now);
        assert!(at < now + 60_000);

        // 端までいってもあふれない。
        let (_, at) = revoke_others(&Marked(Some(i64::MAX)), "7", "mine").unwrap();
        assert_eq!(at, Some(i64::MAX));
    }

    #[test]
    fn 切っている間に別の所が全部切ったら本人も逃げない() {
        // 印を付けてから読み直すまでに `logout_user`（除外なし）が割り込むと、
        // 読み直した印は相手のもの。以前はその後に時刻を進めたので、本人まで切る
        // はずの操作から本人だけが逃げていた。
        use crate::session::{MemoryStore, SessionStore};
        use std::collections::BTreeMap;
        use std::sync::Arc;

        /// 自分の印を付けた直後に、別の所の「全部切る」が割り込む置き場所。
        struct Interrupted(MemoryStore);
        impl SessionStore for Interrupted {
            fn read(&self, id: &str) -> Result<Option<BTreeMap<String, String>>> {
                self.0.read(id)
            }
            fn write(&self, id: &str, data: &BTreeMap<String, String>) -> Result<()> {
                self.0.write(id, data)
            }
            fn destroy(&self, id: &str) -> Result<()> {
                self.0.destroy(id)
            }
            fn destroy_for_user_except(&self, user_id: &str, keep_id: &str) -> Result<usize> {
                let removed = self.0.destroy_for_user_except(user_id, keep_id)?;
                // 別のリクエストの `logout_user`。印は除外なしで置き換わる。
                std::thread::sleep(std::time::Duration::from_millis(2));
                self.0.mark_revoked(user_id, None);
                Ok(removed)
            }
            fn mark_revoked(&self, user_id: &str, except: Option<&str>) {
                self.0.mark_revoked(user_id, except);
            }
            fn revoked_at(&self, user_id: &str, session_id: &str) -> Option<i64> {
                self.0.revoked_at(user_id, session_id)
            }
        }

        let store = Arc::new(Interrupted(MemoryStore::new(60)));
        let session = Session::empty().with_store(store.clone());
        let auth = Auth::new(Some(&session));
        auth.login_using_id(7).unwrap();
        let before = logged_in_at(&session);

        auth.logout_other_devices().unwrap();
        assert_eq!(logged_in_at(&session), before, "時刻を進めない");
        let mark = store
            .revoked_at("7", &session.id())
            .expect("本人にも効く印");
        assert!(mark > before, "次の保存で本人も切られる: {mark} > {before}");
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

    #[tokio::test]
    async fn 非同期版も同じ件数を返す() {
        use crate::session::{MemoryStore, SessionStore};
        use std::collections::BTreeMap;
        use std::sync::Arc;

        let data = |user_id: &str| {
            let mut data = BTreeMap::new();
            data.insert(AUTH_ID_KEY.to_string(), user_id.to_string());
            data
        };

        // ログインしていなければ、同期版と同じく 0。
        let session = Session::empty();
        let auth = Auth::new(Some(&session));
        assert_eq!(auth.logout_other_devices_async().await.unwrap(), 0);
        assert_eq!(auth.logout_all_devices_async().await.unwrap(), 0);

        // 他の端末だけ切る。
        let store = Arc::new(MemoryStore::new(60));
        let session = Session::empty().with_store(store.clone());
        let auth = Auth::new(Some(&session));
        auth.login_using_id(7).unwrap();
        let mine = session.id();
        store.write(&mine, &data("7")).unwrap();
        store.write("sameuser", &data("7")).unwrap();
        store.write("otheruser", &data("8")).unwrap();
        assert_eq!(auth.logout_other_devices_async().await.unwrap(), 1);
        assert!(store.read(&mine).unwrap().is_some(), "自分は残る");
        assert!(store.read("otheruser").unwrap().is_some(), "他の利用者");
        // 同期版と同じく、ログインの時刻を印より後に進める。
        let mark = store.revoked_at("7", "ほかの端末").expect("切った印");
        assert!(logged_in_at(&session) > mark, "印より後");

        // 全部切ると自分も切れる。
        assert_eq!(auth.logout_all_devices_async().await.unwrap(), 1);
        assert!(store.read(&mine).unwrap().is_none());
        assert!(!auth.check(), "呼んだ本人もログアウトする");

        // 利用者を指定して切る。
        let store = Arc::new(MemoryStore::new(60));
        let session = Session::empty().with_store(store.clone());
        let auth = Auth::new(Some(&session));
        store.write("a", &data("7")).unwrap();
        store.write("b", &data("7")).unwrap();
        store.write("c", &data("8")).unwrap();
        assert_eq!(auth.logout_user_async(7).await.unwrap(), 2);
        assert!(store.read("c").unwrap().is_some(), "他の利用者は残る");

        // セッションが無ければ、同期版と同じくエラー。
        let auth = Auth::new(None);
        assert!(auth.logout_other_devices_async().await.is_err());
        assert!(auth.logout_all_devices_async().await.is_err());
        assert!(auth.logout_user_async(7).await.is_err());
    }

    #[cfg(not(feature = "encryption"))]
    #[test]
    fn 機能フラグが無ければ直し方を知らせる() {
        // 「`features = ["encryption"]` にしてください」ではなく、足す形で案内する
        // （そのまま写すと、ほかの機能フラグが落ちるため）。
        for error in [encrypt("x").unwrap_err(), decrypt("x").unwrap_err()] {
            let text = error.to_string();
            assert!(
                text.contains("features に `\"encryption\"` を足して"),
                "{text}"
            );
            assert!(!text.contains("features = ["), "{text}");
        }
    }
}
