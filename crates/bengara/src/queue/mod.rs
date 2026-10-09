//! キューとジョブ。時間のかかる処理を後回しにします。
//!
//! ```ignore
//! Queue::push("SendWelcome", &payload).await?;   // 入れる
//! ```
//!
//! ```sh
//! cargo artisan queue:work                       # 動かす
//! ```
//!
//! ジョブは `app/Jobs/` に `pub async fn handle(payload: String) -> Result<()>` として
//! 書きます。一覧は `bengara-build` が作ります。

mod worker;

pub(crate) use worker::{
    failed_count, failed_list, retry_failed, run as work, Options as WorkerOptions,
    DEFAULT_RETRY_AFTER,
};

use std::future::Future;
use std::pin::Pin;

use crate::database::{Schema, Value, DB};
use crate::error::{Error, Result};

/// 待っているジョブを置く表。
pub const TABLE: &str = "jobs";

/// 諦めたジョブを置く表。
pub const FAILED_TABLE: &str = "failed_jobs";

/// 既定のキューの名前。
pub const DEFAULT_QUEUE: &str = "default";

/// ジョブが返す非同期の結果。
pub type JobFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

/// ジョブ1つ。`bengara-build` が生成します。
///
/// ```ignore
/// pub const JOBS: &[Job] = &[Job {
///     name: "SendWelcome",
///     handle: |payload| Box::pin(crate::app::jobs::send_welcome::handle(payload)),
/// }];
/// ```
#[derive(Clone, Copy)]
pub struct Job {
    /// ファイル名。`Queue::push` に渡す名前です。
    pub name: &'static str,
    /// 中身。
    pub handle: fn(String) -> JobFuture,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job").field("name", &self.name).finish()
    }
}

/// キューの入口。
pub struct Queue;

impl Queue {
    /// ジョブを入れる。すぐ処理されます。
    pub async fn push<T: serde::Serialize>(job: &str, payload: &T) -> Result<i64> {
        Self::later(job, payload, 0).await
    }

    /// ジョブを入れる。`delay_secs` 秒たってから処理されます。
    pub async fn later<T: serde::Serialize>(
        job: &str,
        payload: &T,
        delay_secs: u64,
    ) -> Result<i64> {
        let text = serde_json::to_string(payload)?;
        Self::push_raw_on(DEFAULT_QUEUE, job, &text, delay_secs).await
    }

    /// 文字列をそのまま入れる。
    pub async fn push_raw(job: &str, payload: &str) -> Result<i64> {
        Self::push_raw_on(DEFAULT_QUEUE, job, payload, 0).await
    }

    /// キューの名前を決めて入れる。
    pub async fn push_on<T: serde::Serialize>(queue: &str, job: &str, payload: &T) -> Result<i64> {
        let text = serde_json::to_string(payload)?;
        Self::push_raw_on(queue, job, &text, 0).await
    }

    async fn push_raw_on(queue: &str, job: &str, payload: &str, delay_secs: u64) -> Result<i64> {
        if job.trim().is_empty() {
            return Err(Error::msg("ジョブの名前がありません"));
        }
        let now = crate::support::time::now_seconds();
        let available_at = available_at(now, delay_secs);

        DB::table(TABLE)
            .insert_get_id(&[
                ("queue", Value::Text(queue.to_string())),
                ("job", Value::Text(job.to_string())),
                ("payload", Value::Text(payload.to_string())),
                ("attempts", Value::Int(0)),
                ("available_at", Value::Text(available_at)),
                ("created_at", Value::Text(crate::database::now())),
            ])
            .await
    }

    /// 表に残っているジョブの数。
    ///
    /// **すぐ処理できる数ではありません。** 下の `size_on` と同じで、処理中
    /// （予約済み）・遅延待ち・再挑戦待ちも数に入ります。すぐ処理できる数は
    /// [`Queue::ready_size`] を使ってください。
    pub async fn size() -> Result<i64> {
        Self::size_on(DEFAULT_QUEUE).await
    }

    /// そのキューの表に残っているジョブの数。
    ///
    /// `queue` の列だけで数えます。つまり次の 3 つも含みます。
    ///
    /// - 処理中のもの（`reserved_at` が入っている）
    /// - 遅延待ちのもの（`Queue::later` で入れた、`available_at` が未来）
    /// - 再挑戦待ちのもの（失敗して待ち時間を伸ばしたもの）
    pub async fn size_on(queue: &str) -> Result<i64> {
        DB::table(TABLE).where_("queue", queue).count().await
    }

    /// いますぐ処理できるジョブの数。
    ///
    /// 処理中・遅延待ち・再挑戦待ちを除いた数です。
    pub async fn ready_size() -> Result<i64> {
        Self::ready_size_on(DEFAULT_QUEUE).await
    }

    /// そのキューで、いますぐ処理できるジョブの数。
    pub async fn ready_size_on(queue: &str) -> Result<i64> {
        let now = crate::support::time::format_timestamp(crate::support::time::now_seconds());
        DB::table(TABLE)
            .where_("queue", queue)
            .where_null("reserved_at")
            .where_op("available_at", "<=", now)
            .count()
            .await
    }

    /// 諦めたジョブの数。
    pub async fn failed_size() -> Result<i64> {
        failed_count().await
    }

    /// 待っているジョブを全部捨てる。
    pub async fn clear() -> Result<u64> {
        DB::table(TABLE).delete().await
    }

    /// この 2 つの表を作るマイグレーションの中身。
    ///
    /// ```ignore
    /// pub fn up(schema: &mut Schema) {
    ///     Queue::define(schema);
    /// }
    /// ```
    pub fn define(schema: &mut Schema) {
        schema.create_if_not_exists(TABLE, |t| {
            t.id();
            t.string("queue").default(DEFAULT_QUEUE);
            t.string("job");
            t.text("payload");
            t.integer("attempts").default(0);
            t.date_time("available_at");
            t.date_time("reserved_at").nullable();
            t.date_time("created_at");
            t.index(&["queue", "available_at"]);
        });
        schema.create_if_not_exists(FAILED_TABLE, |t| {
            t.id();
            t.string("queue");
            t.string("job");
            t.text("payload");
            t.text("error");
            t.date_time("failed_at");
        });
    }

    /// 諦めたジョブをもう一度キューへ戻す。
    ///
    /// `id` が `None` なら全部戻します。返るのは戻した件数です。
    pub async fn retry(id: Option<i64>) -> Result<u64> {
        retry_failed(id).await
    }

    /// 諦めたジョブの記録を捨てる。
    pub async fn flush_failed() -> Result<u64> {
        DB::table(FAILED_TABLE).delete().await
    }
}

/// いつから処理できるかを文字列にする。
///
/// **あふれさせません。** `delay_secs as i64` だと `u64::MAX` が `-1` になり、
/// 「ずっと後に実行」と書いたジョブがすぐ実行されます（デバッグビルドでは
/// 足し算でパニックします）。上限で切り詰めてから飽和加算します
/// （`cache/store.rs` の `Entry::encode` と同じ形）。
fn available_at(now: i64, delay_secs: u64) -> String {
    let delay = i64::try_from(delay_secs).unwrap_or(i64::MAX);
    crate::support::time::format_timestamp(now.saturating_add(delay))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Driver;

    #[test]
    fn 遅延秒数はあふれない() {
        let now = 1_000_000;
        let soon = available_at(now, 0);
        // `as i64` だと `u64::MAX` が `-1` になり、すぐ実行されてしまった。
        assert!(available_at(now, u64::MAX) > soon, "過去にならない");
        assert!(available_at(now, i64::MAX as u64 + 1) > soon);
        // 普通の値はそのまま足す。
        assert_eq!(
            available_at(now, 60),
            crate::support::time::format_timestamp(now + 60)
        );
    }

    #[test]
    fn 表の定義が作れる() {
        let mut schema = Schema::new(Driver::Sqlite);
        Queue::define(&mut schema);
        let sql = schema.to_sql();

        assert!(sql[0].contains("\"jobs\""), "{}", sql[0]);
        assert!(sql[0].contains("\"attempts\" integer not null default 0"));
        assert!(sql[0].contains("\"reserved_at\" datetime null"));
        // 索引は別の文。
        assert!(sql
            .iter()
            .any(|s| s.contains("jobs_queue_available_at_index")));
        assert!(sql.iter().any(|s| s.contains("\"failed_jobs\"")));
    }

    #[test]
    fn 表の名前() {
        assert_eq!(TABLE, "jobs");
        assert_eq!(FAILED_TABLE, "failed_jobs");
        assert_eq!(DEFAULT_QUEUE, "default");
    }

    #[test]
    fn ジョブのデバッグ表示に名前が出る() {
        let job = Job {
            name: "SendWelcome",
            handle: |_| Box::pin(async { Ok(()) }),
        };
        assert!(format!("{job:?}").contains("SendWelcome"));
    }
}
