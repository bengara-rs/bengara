//! ジョブを取り出して動かす側（`queue:work`）。
//!
//! - **同じジョブを2つのワーカーが同時に取らない**ように、取り出しはトランザクションの中で行います。
//! - 失敗したら回数を数え、待ち時間を伸ばして戻します。上限を超えたら `failed_jobs` へ移します。
//! - 停止の合図（Ctrl+C）を受けたら、**いま処理中のジョブを終えてから**止まります。
//! - ワーカーが強制終了しても、`retry_after` 秒たてば別のワーカーが取り直します
//!   （Laravel の `retry_after` と同じ考え方）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::{Job, DEFAULT_QUEUE, FAILED_TABLE, TABLE};
use crate::database::{Value, DB};
use crate::error::{Error, Result};
use crate::support::time;

/// `queue:work` に渡せる指定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    /// 処理するキューの名前。
    pub queue: String,
    /// 失敗を許す回数。
    pub tries: u32,
    /// 空のときに待つ秒数。
    pub sleep: u64,
    /// 1 本だけ処理して終わるか。
    pub once: bool,
    /// 処理中のまま放置されたジョブを取り直すまでの秒数。
    ///
    /// **1 本のジョブにかかる最長の時間より長くしてください。** 短いと、
    /// まだ動いているジョブを別のワーカーが二重に処理します。
    pub retry_after: i64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            queue: DEFAULT_QUEUE.to_string(),
            tries: 3,
            sleep: 1,
            once: false,
            retry_after: DEFAULT_RETRY_AFTER,
        }
    }
}

/// 予約を取り直すまでの既定の秒数。Laravel の `retry_after` に当たります。
pub(crate) const DEFAULT_RETRY_AFTER: i64 = 90;

/// 失敗の回数から次に試すまでの待ち時間（秒）を決める。
///
/// 1 回目 10 秒、2 回目 20 秒、3 回目 40 秒…と伸ばします。
/// すぐ試し直すと、同じ理由で失敗し続けて DB を叩くだけになるためです。
pub(crate) fn backoff_secs(attempts: u32) -> i64 {
    let shift = attempts.min(6);
    10 * (1_i64 << shift) / 2
}

/// 予約を取り直す境目の時刻。
///
/// これ以前に予約された（`reserved_at <= cutoff`）ものは、持ち主が死んだと見なします。
pub(crate) fn reclaim_cutoff(now: i64, retry_after: i64) -> String {
    time::format_timestamp(now - retry_after.max(1))
}

/// 取り直したあとの失敗回数。
///
/// 目印が残っていた（`reserved_at` がある）ときだけ +1 します。同じジョブを
/// 永久に回し続けず、`tries` に達したら諦められるようにするためです。
pub(crate) fn next_attempts(reserved_at: Option<&str>, attempts: u32) -> u32 {
    if reserved_at.is_some() {
        attempts + 1
    } else {
        attempts
    }
}

/// 取り直してよいか。`reserve` の `where` と同じ判定です。
///
/// 時刻は `YYYY-MM-DD HH:MM:SS` なので、文字の大小が時刻の前後と一致します。
#[cfg(test)]
pub(crate) fn is_reclaimable(reserved_at: Option<&str>, cutoff: &str) -> bool {
    match reserved_at {
        None => true,
        Some(at) => at <= cutoff,
    }
}

/// 取り出した 1 件。
struct Reserved {
    id: i64,
    job: String,
    payload: String,
    attempts: u32,
    queue: String,
}

/// 取り出しの結果。
///
/// **「空だった」と「ほかのワーカーに取られた」を分けます。** 一緒にすると、
/// 競り合いに負けただけで `--sleep` 秒待つことになります。
enum Reservation {
    /// 1 件取れた。
    Taken(Reserved),
    /// 待っているジョブが無かった。
    Empty,
    /// ほかのワーカーが先に取った。待たずに取り直してよい。
    Lost,
}

/// 1 件取り出して、処理中の目印を付ける。
///
/// **トランザクションの中で選んで更新します。** 2 つのワーカーが同じジョブを
/// 取らないようにするためです。
///
/// 目印（`reserved_at`）が `retry_after` 秒より古いものも取り直します。
/// ワーカーが強制終了・電源断で死ぬと目印が残ったままになるためです。
/// 取り直したときは `attempts` を +1 して、同じジョブを永久に回し続けないようにします。
async fn reserve(queue: &str, retry_after: i64) -> Result<Reservation> {
    let now_secs = time::now_seconds();
    let now = time::format_timestamp(now_secs);
    let cutoff = reclaim_cutoff(now_secs, retry_after);
    let tx = DB::begin().await?;

    let row = tx
        .table(TABLE)
        .where_("queue", queue)
        .where_op("available_at", "<=", now.clone())
        .where_group(|q| {
            q.where_null("reserved_at")
                .or_where_op("reserved_at", "<=", cutoff.clone())
        })
        .order_by("id")
        .limit(1)
        .get()
        .await?
        .into_iter()
        .next();

    let Some(row) = row else {
        tx.commit().await?;
        return Ok(Reservation::Empty);
    };

    let id: i64 = row.get("id")?;
    let reserved_at: Option<String> = row.get("reserved_at")?;
    let was_stale = reserved_at.is_some();
    let attempts = next_attempts(
        reserved_at.as_deref(),
        row.get::<i64>("attempts")?.max(0) as u32,
    );

    let affected = tx
        .table(TABLE)
        .where_("id", id)
        .where_group(|q| {
            q.where_null("reserved_at")
                .or_where_op("reserved_at", "<=", cutoff.clone())
        })
        .update(&[
            ("reserved_at", Value::Text(now)),
            ("attempts", Value::Int(attempts as i64)),
        ])
        .await?;
    tx.commit().await?;

    if affected == 0 {
        // ほかのワーカーが先に取った。待たずに取り直す。
        return Ok(Reservation::Lost);
    }

    if was_stale {
        tracing::warn!(
            "ジョブ #{id} は {retry_after} 秒以上処理中のままだったので取り直します（{attempts} 回目）"
        );
    }

    Ok(Reservation::Taken(Reserved {
        id,
        job: row.get("job")?,
        payload: row.get("payload")?,
        attempts,
        queue: row.get("queue")?,
    }))
}

/// 成功したので消す。
async fn finish(id: i64) -> Result<()> {
    DB::table(TABLE).where_("id", id).delete().await?;
    Ok(())
}

/// 失敗したので戻す。上限を超えていたら諦める。
async fn fail(reserved: &Reserved, tries: u32, error: &str) -> Result<bool> {
    let attempts = reserved.attempts + 1;
    if attempts >= tries {
        // 入れるのと消すのを 1 つのトランザクションにする。
        // 分けると、間でプロセスが死んだときに両方の表に残って再実行されます。
        let tx = DB::begin().await?;
        tx.table(FAILED_TABLE)
            .insert(&[
                ("queue", Value::Text(reserved.queue.clone())),
                ("job", Value::Text(reserved.job.clone())),
                ("payload", Value::Text(reserved.payload.clone())),
                ("error", Value::Text(error.to_string())),
                ("failed_at", Value::Text(crate::database::now())),
            ])
            .await?;
        tx.table(TABLE).where_("id", reserved.id).delete().await?;
        tx.commit().await?;
        return Ok(true);
    }

    let next = time::format_timestamp(time::now_seconds() + backoff_secs(attempts));
    DB::table(TABLE)
        .where_("id", reserved.id)
        .update(&[
            ("attempts", Value::Int(attempts as i64)),
            ("available_at", Value::Text(next)),
            ("reserved_at", Value::Null),
        ])
        .await?;
    Ok(false)
}

/// ジョブを 1 件動かす。パニックも失敗として扱います。
async fn run_one(jobs: &[Job], reserved: &Reserved) -> std::result::Result<(), String> {
    let Some(job) = jobs.iter().find(|j| j.name == reserved.job) else {
        return Err(format!(
            "`{}` というジョブはありません（app/Jobs/ を確かめてください）",
            reserved.job
        ));
    };

    let handle = job.handle;
    let payload = reserved.payload.clone();
    // パニックでワーカーを落とさない（ハンドラと同じ考え方。決定記録 #008）。
    let task = tokio::spawn(async move { handle(payload).await });
    match task.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(e) if e.is_panic() => Err("ジョブがパニックしました".to_string()),
        Err(e) => Err(format!("ジョブを動かせませんでした: {e}")),
    }
}

/// `queue:work` の本体。止まるまでジョブを処理し続けます。
pub(crate) async fn run(jobs: &[Job], options: &Options, stop: Arc<AtomicBool>) -> Result<()> {
    println!(
        "キュー `{}` のジョブを処理します（Ctrl+C で終了）",
        options.queue
    );

    let mut processed = 0u64;
    loop {
        if stop.load(Ordering::Relaxed) {
            println!("\n停止の合図を受けました。{processed} 件処理しました。");
            return Ok(());
        }

        let reserved = match reserve(&options.queue, options.retry_after).await? {
            Reservation::Taken(reserved) => reserved,
            // 競り合いに負けただけなので待たない。
            Reservation::Lost => continue,
            Reservation::Empty => {
                if options.once {
                    println!("待っているジョブはありません。");
                    return Ok(());
                }
                // 空なら少し待つ。ここで止まらないよう、細かく区切って合図を見る。
                for _ in 0..options.sleep.max(1) * 10 {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                continue;
            }
        };

        let started = std::time::Instant::now();
        match run_one(jobs, &reserved).await {
            Ok(()) => {
                finish(reserved.id).await?;
                processed += 1;
                println!(
                    "  {} #{} 成功（{} ms）",
                    reserved.job,
                    reserved.id,
                    started.elapsed().as_millis()
                );
            }
            Err(message) => {
                let gave_up = fail(&reserved, options.tries, &message).await?;
                if gave_up {
                    eprintln!(
                        "  {} #{} 失敗（{} 回目で諦めました）: {message}",
                        reserved.job,
                        reserved.id,
                        reserved.attempts + 1
                    );
                } else {
                    eprintln!(
                        "  {} #{} 失敗（{} 回目、{} 秒後に再挑戦）: {message}",
                        reserved.job,
                        reserved.id,
                        reserved.attempts + 1,
                        backoff_secs(reserved.attempts + 1)
                    );
                }
            }
        }

        if options.once {
            return Ok(());
        }
    }
}

/// 諦めたジョブの数。
pub(crate) async fn failed_count() -> Result<i64> {
    DB::table(FAILED_TABLE).count().await
}

/// 諦めたジョブをキューへ戻す。
pub(crate) async fn retry_failed(id: Option<i64>) -> Result<u64> {
    let query = match id {
        Some(id) => DB::table(FAILED_TABLE).where_("id", id),
        None => DB::table(FAILED_TABLE),
    };
    let rows = query.get().await?;
    if rows.is_empty() {
        return Ok(0);
    }

    let now = crate::database::now();
    let mut moved = 0;
    // 入れるのと消すのを 1 つのトランザクションにする。
    // 分けると、間でプロセスが死んだときに二重投入になります。
    let tx = DB::begin().await?;
    for row in &rows {
        tx.table(TABLE)
            .insert(&[
                ("queue", Value::Text(row.get("queue")?)),
                ("job", Value::Text(row.get("job")?)),
                ("payload", Value::Text(row.get("payload")?)),
                ("attempts", Value::Int(0)),
                ("available_at", Value::Text(now.clone())),
                ("created_at", Value::Text(now.clone())),
            ])
            .await?;
        let failed_id: i64 = row.get("id")?;
        tx.table(FAILED_TABLE)
            .where_("id", failed_id)
            .delete()
            .await?;
        moved += 1;
    }
    tx.commit().await?;
    Ok(moved)
}

/// 諦めたジョブの一覧（`queue:failed` が使う）。
pub(crate) async fn failed_list() -> Result<Vec<(i64, String, String, String)>> {
    let rows = DB::table(FAILED_TABLE).order_by("id").get().await?;
    rows.iter()
        .map(|row| {
            Ok((
                row.get::<i64>("id")?,
                row.get::<String>("job")?,
                row.get::<String>("failed_at")?,
                row.get::<String>("error")?,
            ))
        })
        .collect::<Result<Vec<_>>>()
        .map_err(|e: Error| e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 待ち時間は回数で伸びる() {
        assert_eq!(backoff_secs(1), 10);
        assert_eq!(backoff_secs(2), 20);
        assert_eq!(backoff_secs(3), 40);
        assert_eq!(backoff_secs(4), 80);
        // 伸ばしすぎないよう上限を付ける。
        assert_eq!(backoff_secs(6), 320);
        assert_eq!(backoff_secs(100), 320);
    }

    #[test]
    fn 既定の指定() {
        let options = Options::default();
        assert_eq!(options.queue, "default");
        assert_eq!(options.tries, 3);
        assert_eq!(options.sleep, 1);
        assert!(!options.once);
        assert_eq!(options.retry_after, 90);
    }

    #[test]
    fn 期限を過ぎた予約は取り直す() {
        let now = time::now_seconds();
        let cutoff = reclaim_cutoff(now, 90);

        // 100 秒前に予約されたまま（ワーカーが死んだ）。
        let dead = time::format_timestamp(now - 100);
        assert!(is_reclaimable(Some(&dead), &cutoff));
        // まだ予約されていないものも取れる。
        assert!(is_reclaimable(None, &cutoff));
    }

    #[test]
    fn 期限内の予約は取らない() {
        let now = time::now_seconds();
        let cutoff = reclaim_cutoff(now, 90);

        // 10 秒前に予約されたばかり（まだ処理中）。
        let working = time::format_timestamp(now - 10);
        assert!(!is_reclaimable(Some(&working), &cutoff));
        // ちょうど境目は取り直す側に入れる。
        assert!(is_reclaimable(Some(&cutoff), &cutoff));
    }

    #[test]
    fn 取り直すと失敗回数が増える() {
        // 目印が残っていた＝前のワーカーが死んだので、1 回ぶん数える。
        assert_eq!(next_attempts(Some("2024-01-01 00:00:00"), 0), 1);
        assert_eq!(next_attempts(Some("2024-01-01 00:00:00"), 2), 3);
        // 初めて取るときは増やさない。
        assert_eq!(next_attempts(None, 0), 0);
        assert_eq!(next_attempts(None, 2), 2);
    }

    #[test]
    fn 取り直しの境目は0秒にしない() {
        let now = time::now_seconds();
        // 0 や負の値を渡しても、いま予約したものを取り直さない。
        assert!(reclaim_cutoff(now, 0) < time::format_timestamp(now));
        assert!(reclaim_cutoff(now, -5) < time::format_timestamp(now));
    }
}
