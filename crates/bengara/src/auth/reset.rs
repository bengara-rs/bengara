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
    ///
    /// **消すのと入れるのを 1 つのトランザクションで囲みます。** 別の文にすると、
    /// 同じアドレスで同時に 2 本来たときに主キー違反で 500 になります。
    /// 登録済みのアドレスだけが 500 になりえるので、
    /// 「登録の有無を漏らさない」という配慮が崩れます。
    pub async fn create(email: &str) -> Result<String> {
        let token = crypto::random_token();
        let hashed = crypto::to_hex(&crypto::sha256(token.as_bytes()));

        // 入れ替えなので、先に消してから入れる。
        let tx = DB::begin().await?;
        tx.table(TABLE).where_("email", email).delete().await?;
        tx.table(TABLE)
            .insert(&[
                ("email", Value::Text(email.to_string())),
                ("token", Value::Text(hashed)),
                ("created_at", Value::Text(crate::database::now())),
            ])
            .await?;
        tx.commit().await?;
        Ok(token)
    }

    /// 照合して、合っていれば**その場で消す**。1 回しか使えません。
    ///
    /// **パスワードの再設定は、必ずこれを通してください。** 再設定の正しい入口は
    /// これだけです。[`verify`](Self::verify) で真を見てから書き換えると、
    /// 同じリンクを 2 回使えます。
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

    /// **照合だけです。再設定には使わないでください。**
    ///
    /// トークンは消しません。ここで真を見てからパスワードを書き換えるまでの間に
    /// 別のリクエストが入れば、**同じトークンが 2 回使えます。**
    /// 再設定の正しい入口は [`consume_if_valid`](Self::consume_if_valid) だけです。
    ///
    /// 使い道は「リンクを開いた時点で入力欄を出してよいか」の下調べです。
    /// 送信を受け取る側では、必ず `consume_if_valid` で判定し直してください。
    ///
    /// 期限切れ・不一致・記録なしは、すべて偽。
    /// **理由は区別しません。** どれなのかが分かると、総当たりの手がかりになります。
    ///
    /// 3 つの経路（記録なし・期限切れ・不一致）は、どれも同じだけ `sha256` を通ります。
    /// ただし**応答時間の差を消しきれるわけではありません。** 支配しているのは
    /// DB の往復で、記録が無いときと有るときで待ち時間が違います。
    /// 気になる場面では `consume_if_valid` だけを入口にしてください。
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

        // 期限の判定は `sha256` の**あと**に置く。先に return すると、
        // 期限切れの経路だけ計算を飛ばしてしまい、速さで見分けられる。
        let hashed = crypto::to_hex(&crypto::sha256(token.as_bytes()));
        let same = crypto::constant_time_eq(hashed.as_bytes(), stored.as_bytes());
        Ok(same && !Self::expired(&created_at))
    }

    /// 使い終わったトークンを消す。**照合はしません。**
    ///
    /// 再設定の正しい入口は [`consume_if_valid`](Self::consume_if_valid) だけです。
    /// `verify` と `consume` を並べる形にはしないでください
    /// （その間に別のリクエストが入れば、同じトークンが 2 回使えます）。
    ///
    /// 使い道は「再設定をやめた」ときの片付けです。
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
        async fn 同じアドレスで何度作っても一本だけ残る() {
            let _db = setup().await;
            let email = "twice@example.com";

            // `delete` → `insert` をトランザクションで囲んでいるので、
            // 主キー違反にならず、最後の 1 本だけが残る。
            let first = PasswordReset::create(email).await.unwrap();
            let second = PasswordReset::create(email).await.unwrap();
            assert_ne!(first, second, "トークンは毎回変わる");

            assert!(!PasswordReset::verify(email, &first).await.unwrap(), "古い");
            assert!(PasswordReset::verify(email, &second).await.unwrap());

            // 残っているのは 1 本だけ。
            let rows = DB::table(TABLE).where_("email", email).get().await.unwrap();
            assert_eq!(rows.len(), 1);
        }

        #[tokio::test]
        async fn 期限切れのトークンは偽になる() {
            let _db = setup().await;
            let email = "old@example.com";
            let token = PasswordReset::create(email).await.unwrap();
            assert!(PasswordReset::verify(email, &token).await.unwrap());

            // 作った時刻を 61 分前に書き換える（期限は 60 分）。
            let old = time::format_timestamp(time::now_seconds() - 61 * 60);
            DB::table(TABLE)
                .where_("email", email)
                .update(&[("created_at", old.clone().into())])
                .await
                .unwrap();

            // 合っているトークンでも、期限切れなら偽。
            assert!(!PasswordReset::verify(email, &token).await.unwrap());
            // 合わないトークンも偽（理由は区別しない）。
            assert!(!PasswordReset::verify(email, "ちがう").await.unwrap());
            assert!(!PasswordReset::consume_if_valid(email, &token)
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
