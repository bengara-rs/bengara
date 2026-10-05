//! パスワードの再設定（トークンの作成・照合・片付け）。
//!
//! 表の名前と列は Laravel と同じ `password_resets` です。
//! **メールの送信は Phase 5 です。** ここで作るのはトークンとリンクまでです。

use crate::database::{Value, DB};
use crate::error::Result;
use crate::support::{crypto, time};

/// 記録を残す表の名前。
const TABLE: &str = "password_resets";

/// トークンが使える時間（分）。Laravel の既定と同じ。
const LIFETIME_MINUTES: i64 = 60;

/// パスワード再設定のトークン。
///
/// ```ignore
/// let token = PasswordReset::create("a@example.com").await?;
/// let url = PasswordReset::link("/password/reset", "a@example.com", &token, 3600)?;
/// // ... 利用者がリンクを開く ...
/// if PasswordReset::verify("a@example.com", &token).await? {
///     // パスワードを書き換えて
///     PasswordReset::consume("a@example.com").await?;
/// }
/// ```
pub struct PasswordReset;

impl PasswordReset {
    /// 表の名前。マイグレーションを書くときに使います。
    pub const TABLE: &'static str = TABLE;

    /// トークンを作って表に入れ、**平文のトークン**を返す。
    ///
    /// 表に入れるのはハッシュです。表が漏れても、そのままでは使えません。
    /// 同じメールアドレスのトークンが残っていたら上書きします（1 アドレス 1 本）。
    pub async fn create(email: &str) -> Result<String> {
        let token = crypto::random_token();
        let hashed = crypto::to_hex(&crypto::sha256(token.as_bytes()));

        // 入れ替えなので、先に消してから入れる。
        DB::table(TABLE).where_("email", email).delete().await?;
        DB::table(TABLE)
            .insert(&[
                ("email", Value::Text(email.to_string())),
                ("token", Value::Text(hashed)),
                ("created_at", Value::Text(crate::database::now())),
            ])
            .await?;
        Ok(token)
    }

    /// トークンが合っているか。期限切れ・不一致・記録なしは、すべて偽。
    ///
    /// **理由は区別しません。** どれなのかが分かると、総当たりの手がかりになります。
    pub async fn verify(email: &str, token: &str) -> Result<bool> {
        let Some(row) = DB::table(TABLE).where_("email", email).first().await? else {
            return Ok(false);
        };
        let stored: String = row.get("token")?;
        let created_at: String = row.get("created_at")?;

        if Self::expired(&created_at) {
            return Ok(false);
        }
        let hashed = crypto::to_hex(&crypto::sha256(token.as_bytes()));
        Ok(crypto::constant_time_eq(
            hashed.as_bytes(),
            stored.as_bytes(),
        ))
    }

    /// 使い終わったトークンを消す。
    pub async fn consume(email: &str) -> Result<()> {
        DB::table(TABLE).where_("email", email).delete().await?;
        Ok(())
    }

    /// 期限切れのトークンをまとめて消す。`session:gc` と同じ用途です。
    pub async fn sweep_expired() -> Result<u64> {
        let limit = time::format_timestamp(time::now_seconds() - LIFETIME_MINUTES * 60);
        DB::table(TABLE)
            .where_op("created_at", "<", limit)
            .delete()
            .await
    }

    /// 再設定の画面へのリンク（署名付き・期限つき）を作る。
    pub fn link(path: &str, email: &str, token: &str, expires_in_secs: u64) -> Result<String> {
        crate::http::temporary_signed_url(
            path,
            &[("email", email), ("token", token)],
            expires_in_secs,
        )
    }

    /// トークンが使える時間（分）。
    pub fn lifetime_minutes() -> i64 {
        LIFETIME_MINUTES
    }

    /// 作った時刻が古すぎるか。読めない値は期限切れとして扱う。
    fn expired(created_at: &str) -> bool {
        match time::parse_timestamp(created_at) {
            Some(created) => time::now_seconds() - created > LIFETIME_MINUTES * 60,
            None => {
                tracing::warn!("password_resets.created_at を読めませんでした: {created_at}");
                true
            }
        }
    }
}

/// 表が無いときのための案内。マイグレーションの中身をそのまま返します。
impl PasswordReset {
    /// この表を作るマイグレーションの中身。
    ///
    /// ```ignore
    /// pub fn up(schema: &mut Schema) {
    ///     PasswordReset::define(schema);
    /// }
    /// ```
    pub fn define(schema: &mut crate::database::Schema) {
        schema.create_if_not_exists(TABLE, |t| {
            t.string("email").primary();
            t.string("token");
            t.date_time("created_at");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::{Driver, Schema};

    #[test]
    fn 期限の判定() {
        let now = time::format_timestamp(time::now_seconds());
        let old = time::format_timestamp(time::now_seconds() - 61 * 60);
        assert!(!PasswordReset::expired(&now), "作った直後は有効");
        assert!(PasswordReset::expired(&old), "61 分前は期限切れ");
        assert!(PasswordReset::expired("こわれた値"), "読めない値は期限切れ");
        assert_eq!(PasswordReset::lifetime_minutes(), 60);
    }

    #[test]
    fn 表の定義が作れる() {
        let mut schema = Schema::new(Driver::Sqlite);
        PasswordReset::define(&mut schema);
        let sql = &schema.to_sql()[0];
        assert!(sql.contains("\"password_resets\""), "{sql}");
        assert!(sql.contains("\"email\" varchar(255) primary key"), "{sql}");
        assert!(sql.contains("\"token\""), "{sql}");
        assert_eq!(PasswordReset::TABLE, "password_resets");
    }
}
