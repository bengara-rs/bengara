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
//! 書きます。`pub async fn handle(Json(payload): Json<Payload>) -> Result<()>` と書けば、
//! 中身を型に読んで受け取れます（[`Json`]）。一覧は `bengara-build` が作ります。

mod worker;

pub use crate::support::payload::{FromPayload, Json};

pub(crate) use worker::{
    failed_count, failed_list, retry_failed, run as work, Options as WorkerOptions,
    DEFAULT_RETRY_AFTER, DEFAULT_TIMEOUT,
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
///     handle: |payload| bengara::queue::__call_job(crate::app::jobs::send_welcome::handle, payload),
/// }];
/// ```
#[derive(Clone, Copy)]
pub struct Job {
    /// ファイル名。`Queue::push` に渡す名前です。
    pub name: &'static str,
    /// 中身。
    pub handle: fn(String) -> JobFuture,
}

/// 生成コードがジョブを呼ぶ口。中身を `handle` の引数の型に読んでから呼びます。
///
/// 引数の型（`String` か [`Json<T>`]）は `handle` から推論します。
/// 読めなければ、その `Err` をすぐ返します（ジョブの失敗として、再挑戦と
/// `failed_jobs` に乗ります）。
#[doc(hidden)]
pub fn __call_job<P, F, Fut>(handle: F, payload: String) -> JobFuture
where
    P: FromPayload,
    F: FnOnce(P) -> Fut,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    match P::from_payload(payload) {
        Ok(payload) => Box::pin(handle(payload)),
        Err(e) => Box::pin(std::future::ready(Err(e))),
    }
}

/// キューの設定。`config/queue.rs` が返します。Laravel の `config/queue.php` に当たります。
///
/// ファイル名は自由です。見分けるのは名前ではなく、`config()` が返す型です。
///
/// ```ignore
/// use bengara::queue::QueueConfig;
///
/// pub fn config() -> QueueConfig {
///     QueueConfig::default()
///         .timeout("TranscodeVideo", 30 * 60) // 30 分
///         .timeout("SendWelcome", 30)
/// }
/// ```
///
/// 置かなければ、どのジョブも `queue:work --timeout`（既定 60 秒）で打ち切ります。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueConfig {
    /// ジョブごとの打ち切りの時間（秒）。ジョブの名前（`app/Jobs/` のファイル名）と秒の組です。
    ///
    /// Laravel のジョブの `$timeout` に当たります。決めたジョブは `--timeout` より
    /// こちらを使います。`0` は打ち切りません（`--timeout 0` と同じ）。
    ///
    /// 打ち切りの時間が `--retry-after` より長いジョブは、**予約もその分だけ延ばします。**
    /// 動いている途中に別のワーカーが取り直して、二重に動くことはありません。
    pub timeouts: Vec<(String, u64)>,
}

impl QueueConfig {
    /// ジョブの打ち切りの時間（秒）を決める。
    pub fn timeout(mut self, job: impl Into<String>, secs: u64) -> Self {
        self.timeouts.push((job.into(), secs));
        self
    }
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

    /// キューの名前を決めて入れる。`delay_secs` 秒たってから処理されます。
    ///
    /// [`Queue::later`] の、キューを選べる形です（Laravel の `laterOn`）。
    pub async fn later_on<T: serde::Serialize>(
        queue: &str,
        job: &str,
        payload: &T,
        delay_secs: u64,
    ) -> Result<i64> {
        let text = serde_json::to_string(payload)?;
        Self::push_raw_on(queue, job, &text, delay_secs).await
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

    /// **既定のキュー（`default`）**の表に残っているジョブの数。
    ///
    /// ほかのキューは数えません（[`Queue::size_on`] を使ってください）。
    /// [`Queue::clear`] は全キューを消すので、範囲が違います。
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

    /// **既定のキュー（`default`）**で、いますぐ処理できるジョブの数。
    ///
    /// 処理中・遅延待ち・再挑戦待ちを除いた数です。
    pub async fn ready_size() -> Result<i64> {
        Self::ready_size_on(DEFAULT_QUEUE).await
    }

    /// そのキューで、いますぐ処理できるジョブの数。
    ///
    /// **古い予約は数えません。** ワーカーが落ちて `reserved_at` が残った行は、
    /// `retry_after` 秒を過ぎるとワーカーが取り直します。ただ `retry_after` は
    /// ワーカーの起動時の引数（`--retry-after`）で、ここからは見えません。
    /// そのため、取り直せる行も「処理中」として数から外れます。
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

    /// 待っているジョブを全部捨てる。返るのは消した件数です。
    ///
    /// **全部のキューが対象です。** [`Queue::size`] は既定のキューだけを
    /// 数えるので、範囲が違います。1 つのキューだけ消すときは
    /// [`Queue::clear_on`] を使ってください。
    ///
    /// **処理中の行も消します。** いま動いているジョブは止まりません。
    /// 終わったときに自分の行が無いので、ワーカーは警告を出すだけです
    /// （失敗しても再挑戦されず、`failed_jobs` にも残りません）。
    /// 諦めたジョブ（`failed_jobs`）は消しません。
    pub async fn clear() -> Result<u64> {
        DB::table(TABLE).delete().await
    }

    /// そのキューで待っているジョブを捨てる。返るのは消した件数です。
    ///
    /// [`Queue::clear`] と同じく、処理中・遅延待ち・再挑戦待ちの行も消します。
    pub async fn clear_on(queue: &str) -> Result<u64> {
        DB::table(TABLE).where_("queue", queue).delete().await
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
            // 取り出しは `where queue = ? .. order by id limit 1` です。
            // `(queue, available_at)` の索引では id の順に並べ直す手間が残り、
            // 20 万件で 1 回 100ms を超えました。`(queue, id)` なら索引の順に
            // 読んで、条件に合う最初の 1 行で止まります。
            // MySQL（InnoDB）は二次索引に主キーを持つので `(queue)` と同じ働き、
            // PostgreSQL と SQLite は `id` まで書かないと並べ直しが残ります。
            t.index(&["queue", "id"]);
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

/// `available_at` に入れてよい一番後の時刻（`9999-12-31 23:59:59`）。
///
/// **4 桁の年までで止めます。** 取り出しの条件（`available_at <= now`）は
/// 文字列の大小比較です。`format_timestamp` は年を 4 桁で詰めるので、
/// 5 桁の年は `"11480-.."` のようになり、`"2026-.."` より小さい文字列に
/// なります。`Queue::later(.., 300_000_000_000)` のように遅延が極端に
/// 大きいジョブが、次の `queue:work` でいきなり実行されていました。
///
/// この値は `support/time.rs` が読み戻せる上限と同じです。
const MAX_AVAILABLE_AT: i64 = 253_402_300_799;

/// いつから処理できるかを文字列にする。
///
/// **あふれさせません。** `delay_secs as i64` だと `u64::MAX` が `-1` になり、
/// 「ずっと後に実行」と書いたジョブがすぐ実行されます（デバッグビルドでは
/// 足し算でパニックします）。上限で切り詰めてから飽和加算します
/// （`cache/store.rs` の `Entry::encode` と同じ形）。
///
/// 飽和の先は [`MAX_AVAILABLE_AT`] です。`i64::MAX` まで許すと、年が 5 桁に
/// なって文字列の比較が逆転します。
fn available_at(now: i64, delay_secs: u64) -> String {
    let delay = i64::try_from(delay_secs).unwrap_or(i64::MAX);
    let at = now.saturating_add(delay).min(MAX_AVAILABLE_AT);
    crate::support::time::format_timestamp(at)
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
    fn 遅延が大きくても年は4桁に収まる() {
        let now = 1_700_000_000;
        let soon = available_at(now, 0);
        // 取り出しは文字列の大小比較。`i64::MAX` まで許したころは年が 5 桁に
        // なり、`"11480-.." <= "2026-.."` が真で、ずっと先のジョブが
        // 次の `queue:work` でいきなり実行されていた。
        let far = available_at(now, 300_000_000_000);
        assert!(far > soon, "先の時刻として比較できる: {far}");
        assert_eq!(far, "9999-12-31 23:59:59");
        // どれだけ大きくしても同じ上限で止まる。
        assert_eq!(available_at(now, u64::MAX), far);
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
        assert!(sql.iter().any(|s| s.contains("jobs_queue_id_index")));
        // 前の索引は作らない（取り出しの並びと合わず、並べ直しが残った）。
        assert!(!sql
            .iter()
            .any(|s| s.contains("jobs_queue_available_at_index")));
        assert!(sql.iter().any(|s| s.contains("\"failed_jobs\"")));
    }

    /// テスト用にキューの表を作る。
    #[cfg(feature = "sqlite")]
    async fn 表を作る() {
        let mut schema = Schema::new(Driver::Sqlite);
        Queue::define(&mut schema);
        for sql in schema.to_sql() {
            DB::statement(sql, &[]).await.unwrap();
        }
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn キューを選んで遅らせて入れられる() {
        let _db = crate::testing::refresh_database().await;
        表を作る().await;

        // 以前は、遅らせて入れられるのは既定のキューだけだった。
        let id = Queue::later_on("mail", "SendWelcome", &serde_json::json!({ "id": 1 }), 600)
            .await
            .unwrap();
        let row = DB::table(TABLE).find(id).await.unwrap().unwrap();
        assert_eq!(row.get::<String>("queue").unwrap(), "mail");
        assert_eq!(row.get::<String>("payload").unwrap(), r#"{"id":1}"#);
        assert!(
            row.get::<String>("available_at").unwrap() > crate::database::now(),
            "先の時刻で入る"
        );

        // 数には入るが、まだ処理できない。既定のキューには入らない。
        assert_eq!(Queue::size_on("mail").await.unwrap(), 1);
        assert_eq!(Queue::ready_size_on("mail").await.unwrap(), 0);
        assert_eq!(Queue::size().await.unwrap(), 0);

        // 0 秒なら `push_on` と同じで、すぐ処理できる。
        Queue::later_on("mail", "SendWelcome", &1, 0).await.unwrap();
        assert_eq!(Queue::ready_size_on("mail").await.unwrap(), 1);

        // 名前の無いジョブは、ほかの入口と同じく断る。
        assert!(Queue::later_on("mail", " ", &1, 0).await.is_err());
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn キューを絞って捨てられる() {
        let _db = crate::testing::refresh_database().await;
        表を作る().await;

        Queue::push("A", &1).await.unwrap();
        Queue::push_on("mail", "B", &1).await.unwrap();
        Queue::later_on("mail", "C", &1, 600).await.unwrap();
        // 処理中の行（予約済み）も 1 本。
        let reserved = Queue::push_on("mail", "D", &1).await.unwrap();
        DB::table(TABLE)
            .where_("id", reserved)
            .update(&[("reserved_at", Value::Text(crate::database::now()))])
            .await
            .unwrap();

        // そのキューだけを消す。遅延待ちと処理中の行も消える。
        assert_eq!(Queue::clear_on("mail").await.unwrap(), 3);
        assert_eq!(Queue::size_on("mail").await.unwrap(), 0);
        assert_eq!(Queue::size().await.unwrap(), 1, "ほかのキューは残る");
        // 無いキューは 0 件。
        assert_eq!(Queue::clear_on("none").await.unwrap(), 0);

        // `clear()` は全部のキューを消す（`size()` は既定のキューだけ）。
        Queue::push_on("mail", "B", &1).await.unwrap();
        assert_eq!(Queue::size().await.unwrap(), 1);
        assert_eq!(Queue::clear().await.unwrap(), 2);
        assert_eq!(Queue::size_on("mail").await.unwrap(), 0);
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

    #[derive(serde::Deserialize)]
    struct Welcome {
        id: i64,
    }

    async fn handle_string(payload: String) -> Result<()> {
        assert_eq!(payload, "{壊れていてもよい");
        Ok(())
    }

    async fn handle_json(Json(payload): Json<Welcome>) -> Result<()> {
        if payload.id == 7 {
            Ok(())
        } else {
            Err(Error::msg("id が違う"))
        }
    }

    #[tokio::test]
    async fn ジョブは文字列でも型でも受け取れる() {
        // 生成コードと同じ形。型は `handle` の引数から推論される。
        let jobs = [
            Job {
                name: "A",
                handle: |payload| __call_job(handle_string, payload),
            },
            Job {
                name: "B",
                handle: |payload| __call_job(handle_json, payload),
            },
        ];
        (jobs[0].handle)("{壊れていてもよい".into()).await.unwrap();
        (jobs[1].handle)(r#"{"id":7}"#.into()).await.unwrap();
        assert!((jobs[1].handle)(r#"{"id":8}"#.into()).await.is_err());
    }

    #[tokio::test]
    async fn 型に読めない中身はジョブの失敗になる() {
        let job = Job {
            name: "B",
            handle: |payload| __call_job(handle_json, payload),
        };
        let error = (job.handle)("{壊れた".into())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("ジョブ・聞く側の中身を"), "{error}");
        assert!(error.contains("Welcome"), "{error}");
    }
}
