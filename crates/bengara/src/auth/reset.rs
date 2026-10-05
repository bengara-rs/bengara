//! パスワードの再設定（トークンの作成・照合・片付け）。
//!
//! 表の名前と列は Laravel と同じ `password_resets` です。
//! **メールは本当には送れません。** ここで作るのはトークンとリンクまでです。

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
/// if PasswordReset::consume_if_valid("a@example.com", &token).await? {
///     // パスワードを書き換える
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

    /// 照合して、合っていれば**その場で消す**。1 回しか使えません。
    ///
    /// `verify` と `consume` を分けて呼ぶと、その間に別のリクエストが入れば
    /// 同じトークンを 2 回使えます。ここでは**条件つきの `delete` 1 文**にして、
    /// 消せた件数で判定します。消せるのは最初の 1 回だけです。
    ///
    /// ```ignore
    /// if !PasswordReset::consume_if_valid(&email, &token).await? {
    ///     return Err(Error::http(400, "リンクの有効期限が切れています"));
    /// }
    /// // ここから先はパスワードを書き換えてよい
    /// ```
    ///
    /// 期限切れ・不一致・記録なしは、すべて偽です。**理由は区別しません。**
    ///
    /// 照合は SQL の `=` で行います。定数時間ではありませんが、比べているのは
    /// 32 バイトの乱数の SHA-256 なので、かかった時間から手がかりは取れません。
    pub async fn consume_if_valid(email: &str, token: &str) -> Result<bool> {
        let hashed = crypto::to_hex(&crypto::sha256(token.as_bytes()));
        // `expired` と同じ境目（作ってから LIFETIME_MINUTES 以内なら有効）。
        let limit = time::format_timestamp(time::now_seconds() - LIFETIME_MINUTES * 60);
        let removed = DB::table(TABLE)
            .where_("email", email)
            .where_("token", hashed)
            .where_op("created_at", ">=", limit)
            .delete()
            .await?;
        Ok(removed > 0)
    }

    /// トークンが合っているか。期限切れ・不一致・記録なしは、すべて偽。
    ///
    /// **理由は区別しません。** どれなのかが分かると、総当たりの手がかりになります。
    ///
    /// **照合だけです。トークンは消しません。** 1 回限りにするには
    /// [`consume_if_valid`](Self::consume_if_valid) を使ってください。
    /// ここで真を見てから `consume` を呼ぶまでの間に別のリクエストが入れば、
    /// 同じトークンが 2 回使えます。
    pub async fn verify(email: &str, token: &str) -> Result<bool> {
        let Some(row) = DB::table(TABLE).where_("email", email).first().await? else {
            // 記録が無いときも、合っているときと同じだけ計算してから帰る。
            // すぐ帰ると、応答の速さから「そのアドレスは再設定を待っている」と分かる。
            // `black_box` は、計算ごと消されないようにするためのものです。
            std::hint::black_box(crypto::sha256(token.as_bytes()));
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
    ///
    /// 照合と消すのをまとめたいときは [`consume_if_valid`](Self::consume_if_valid) を
    /// 使ってください。
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

    /// ここから下は、実際にデータベースをつなぐテストです。
    ///
    /// 機能フラグ `sqlite` が無いとつなげないので、そのときは飛ばします。
    #[cfg(feature = "sqlite")]
    mod with_database {
        use super::*;

        /// メモリ上の SQLite に表を用意する。
        ///
        /// テスト用のデータベースはプロセスで 1 つなので、**必ず札を取ってから**使います。
        /// 札を取らないと、ほかのテストの `refresh_database()` が表を消してしまいます。
        /// 戻ってきた札は `let _db = setup().await;` で受け取ってください。
        async fn setup() -> crate::testing::DatabaseGuard {
            let guard = crate::testing::refresh_database().await;
            let mut schema = Schema::new(Driver::Sqlite);
            PasswordReset::define(&mut schema);
            for sql in schema.to_sql() {
                DB::statement(sql, &[]).await.expect("表が作れる");
            }
            guard
        }

        #[tokio::test]
        async fn 一度使ったトークンは二度目は通らない() {
            let _db = setup().await;
            let email = "once@example.com";
            let token = PasswordReset::create(email).await.unwrap();

            assert!(
                PasswordReset::consume_if_valid(email, &token)
                    .await
                    .unwrap(),
                "1 回目は通る"
            );
            assert!(
                !PasswordReset::consume_if_valid(email, &token)
                    .await
                    .unwrap(),
                "2 回目は通らない（その場で消している）"
            );
            // 記録も残っていない。
            assert!(!PasswordReset::verify(email, &token).await.unwrap());
        }

        #[tokio::test]
        async fn 合わないトークンでは記録が残る() {
            let _db = setup().await;
            let email = "keep@example.com";
            let token = PasswordReset::create(email).await.unwrap();

            assert!(
                !PasswordReset::consume_if_valid(email, "ちがうトークン")
                    .await
                    .unwrap(),
                "合わなければ偽"
            );
            assert!(
                !PasswordReset::consume_if_valid("other@example.com", &token)
                    .await
                    .unwrap(),
                "アドレスが違えば偽"
            );
            // 本物はまだ使える（間違えただけで消えてしまわない）。
            assert!(PasswordReset::verify(email, &token).await.unwrap());
            assert!(PasswordReset::consume_if_valid(email, &token)
                .await
                .unwrap());
        }

        #[tokio::test]
        async fn 記録が無くても同じだけ計算する() {
            let _db = setup().await;
            // 戻り値は偽。時間は測らないが、`black_box` を挟んだ計算が入っていること。
            assert!(!PasswordReset::verify("nobody@example.com", "とーくん")
                .await
                .unwrap());
        }
    }
}
