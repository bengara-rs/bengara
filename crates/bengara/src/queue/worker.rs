//! ジョブを取り出して動かす側（`queue:work`）。
//!
//! - **同じジョブを2つのワーカーが同時に取らない**ように、取り出しは 2 文の
//!   比較交換（select → 条件付き update）で行います。
//! - 失敗したら回数を数え、待ち時間を伸ばして戻します。上限を超えたら `failed_jobs` へ移します。
//! - 停止の合図（Ctrl+C）を受けたら、**いま処理中のジョブを終えてから**止まります。
//! - ワーカーが強制終了しても、`retry_after` 秒たてば別のワーカーが取り直します
//!   （Laravel の `retry_after` と同じ考え方）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::{Job, DEFAULT_QUEUE, FAILED_TABLE, TABLE};
use crate::database::{QueryBuilder, Row, Value, DB};
use crate::error::Result;
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
///
/// `attempts` は `jobs.attempts` の実値なので、負の値も来ます
/// （アプリが `DB::table("jobs").update(..)` で書けます）。`clamp` で
/// 0〜6 に収め、ずらし幅が負になることを防ぎます。
pub(crate) fn backoff_secs(attempts: i64) -> i64 {
    let shift = attempts.clamp(0, 6);
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
///
/// 値は `jobs.attempts` の実値（`i64`）です。丸めないので負の値も来ます。
/// あふれないよう飽和加算にします。
pub(crate) fn next_attempts(reserved_at: Option<&str>, attempts: i64) -> i64 {
    if reserved_at.is_some() {
        attempts.saturating_add(1)
    } else {
        attempts
    }
}

/// 取り直してよい行だけに絞る条件。
///
/// **`reserve` の 2 つの文が同じものを使います。** 片方だけ直すと、
/// 候補の選び方と更新の条件がずれて、同じジョブを 2 つのワーカーが取ります。
///
/// 時刻は `YYYY-MM-DD HH:MM:SS` なので、文字の大小が時刻の前後と一致します。
fn reclaimable(query: QueryBuilder, cutoff: &str) -> QueryBuilder {
    let cutoff = cutoff.to_string();
    query.where_group(move |q| {
        q.where_null("reserved_at")
            .or_where_op("reserved_at", "<=", cutoff)
    })
}

/// いま取れるジョブだけに絞る条件（`reserve` の 1 文目）。
fn reservable(query: QueryBuilder, queue: &str, now: &str, cutoff: &str) -> QueryBuilder {
    reclaimable(
        query
            .where_("queue", queue)
            .where_op("available_at", "<=", now.to_string()),
        cutoff,
    )
}

/// **自分が予約したままの行**だけに絞る条件。
///
/// `id` だけで絞ると、`retry_after` で取り直された後の行を触ってしまいます。
/// 成功したときは別のワーカーが動かしている行を消し、失敗したときは
/// 予約を外して 3 つ目のワーカーに取らせてしまいました。
fn mine(query: QueryBuilder, reserved: &Reserved) -> QueryBuilder {
    query
        .where_("id", reserved.id)
        .where_("reserved_at", reserved.reserved_at.clone())
}

/// 自分の予約ではなくなっていたことを知らせる。
fn warn_not_mine(id: i64) {
    tracing::warn!(
        "ジョブ #{id} は自分の予約ではなくなっていたので、何もしません（\
         処理に retry_after より長くかかって、別のワーカーが取り直しました。\
         --retry-after を長くしてください）"
    );
}

/// 取り出した 1 件。
struct Reserved {
    id: i64,
    job: String,
    payload: String,
    /// 予約のときに書いた `attempts`。列の実値なので負の値も来ます。
    attempts: i64,
    queue: String,
    /// 予約したときに書いた `reserved_at`。
    ///
    /// `finish` と `fail` の `where` に入れて、**自分の予約のままか**を確かめます。
    reserved_at: String,
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
/// **トランザクションを使いません。2 文の比較交換（compare-and-set）です。**
///
/// 1. `select` で候補を 1 件取る。
/// 2. `update ... where id = ? and <1 文目と同じ条件> and attempts = <1 文目で見た値>`
///    を投げ、0 件なら負けたと見なす（[`Reservation::Lost`]、[`claim_row`]）。
///
/// 2 つのワーカーが同じ行を見ても、更新が通るのは 1 つだけです。
///
/// トランザクションの中で読んでから書くと、WAL では読み取りのスナップショットを
/// 持ったまま書き込みへ昇格します。これは `SQLITE_BUSY_SNAPSHOT` になり、
/// `busy_timeout` の再試行対象ではないので、ワーカーが丸ごと終了しました。
///
/// 目印（`reserved_at`）が `retry_after` 秒より古いものも取り直します。
/// ワーカーが強制終了・電源断で死ぬと目印が残ったままになるためです。
/// 取り直したときは `attempts` を +1 して、同じジョブを永久に回し続けないようにします。
///
/// **`tries` に達した行は予約せずに `failed_jobs` へ移します。** 諦める判定が
/// [`fail`] だけだったころ、ワーカープロセスごと落とすジョブ（OOM kill・
/// SIGKILL）は `fail` を通らず、`attempts` が `tries` を超えてもずっと
/// 回収され続けました。`failed_jobs` に入らないので `queue:failed` にも出ず、
/// 取り出しは `order_by("id") limit 1` なので、そのジョブ 1 件でキュー全体が
/// 止まりました。
async fn reserve(queue: &str, retry_after: i64, tries: u32) -> Result<Reservation> {
    let now_secs = time::now_seconds();
    let now = time::format_timestamp(now_secs);
    let cutoff = reclaim_cutoff(now_secs, retry_after);

    let row = reservable(DB::table(TABLE), queue, &now, &cutoff)
        .order_by("id")
        .limit(1)
        .get()
        .await?
        .into_iter()
        .next();

    let Some(row) = row else {
        return Ok(Reservation::Empty);
    };

    let id: i64 = row.get("id")?;
    let reserved_at: Option<String> = row.get("reserved_at")?;
    let was_stale = reserved_at.is_some();
    // **丸めません。** 2 文目は列の実値と突き合わせるので、`max(0)` で丸めると
    // 負の `attempts` が入った行で `where attempts = ?` が永久に 0 件になり、
    // `Reservation::Lost` を待たずに繰り返して全力ループになりました。
    let done = row.get::<i64>("attempts")?;
    let attempts = next_attempts(reserved_at.as_deref(), done);

    // 回収しようとした行が、もう `tries` ぶん試されていたら諦める。
    // ここで止めないと、ワーカーごと落とすジョブが永久に回り続けます。
    // `Options.tries == 0` は `commands.rs` が弾くので、無制限はありません。
    //
    // 見るのは**数え直した `attempts`** です。列から読んだ `done` で見ていた
    // ころは、落ちた進行中の 1 回が数えられないまま 1 回ぶん多く動きました
    //（`--tries 3` で 4 回走り、`failed_jobs` の文面だけ「3 回」と出る）。
    if let (true, Some(reserved_at)) = (attempts >= i64::from(tries), reserved_at.as_deref()) {
        give_up_stale(&row, id, reserved_at, attempts).await?;
        // 自分は予約していないので、待たずに次の 1 件を見る。
        return Ok(Reservation::Lost);
    }

    let affected = claim_row(queue, id, done, &now, &cutoff, attempts).await?;

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
        reserved_at: now,
    }))
}

/// `reserve` の 2 文目。**1 文目で見たとおりの行だけ**を予約する。
///
/// `id` と取り直しの条件（[`reclaimable`]）だけで絞っていたころは、
/// 1 文目と 2 文目の間に別のワーカーが同じ行を取って**速く失敗**すると、
/// こちらの更新が通ってしまいました。[`fail`] は `reserved_at` を外して
/// `attempts` を増やし、`available_at` を先へ送ります。それを知らないまま
/// 上書きするので、(a) 待ち時間を無視して即再実行し、(b) `attempts` を
/// 1 文目で見た古い値に巻き戻して `--tries` より多く試していました。
///
/// 1 文目と同じ条件（[`reservable`]）に `attempts` の一致を足し、本当の
/// 比較交換にします。どれか 1 つでも変わっていれば 0 件になり、負けた側は
/// 待たずに取り直します。
/// `seen_attempts` は**列から読んだ値そのまま**です。丸めた値を渡すと、
/// 条件と実値がずれて永久に 0 件になります。
async fn claim_row(
    queue: &str,
    id: i64,
    seen_attempts: i64,
    now: &str,
    cutoff: &str,
    attempts: i64,
) -> Result<u64> {
    reservable(DB::table(TABLE).where_("id", id), queue, now, cutoff)
        .where_("attempts", seen_attempts)
        .update(&[
            ("reserved_at", Value::Text(now.to_string())),
            ("attempts", Value::Int(attempts)),
        ])
        .await
}

/// 回収しようとした行を、予約せずに `failed_jobs` へ移す。
///
/// ワーカープロセスごと落とすジョブ（OOM kill・SIGKILL・`panic = "abort"`）は
/// [`fail`] を通りません。以前はこの形で `attempts` が `tries` を超えても
/// 回収され続け、`queue:failed` にも出ないまま、そのジョブ 1 件でキュー全体が
/// 止まりました。
///
/// `fail` と同じ「**先に消して、1 件だった側だけ入れる**」形です。0 件なら
/// 別のワーカーが先に動かしたので、`failed_jobs` にも入れません。
async fn give_up_stale(row: &Row, id: i64, reserved_at: &str, attempts: i64) -> Result<()> {
    let error = format!(
        "{attempts} 回試しましたが、ワーカーが応答しなくなったため諦めました\
         （処理の途中でワーカープロセスが落ちています）"
    );
    // 入れるのと消すのを 1 つのトランザクションにする。
    let tx = DB::begin().await?;
    let affected = tx
        .table(TABLE)
        .where_("id", id)
        .where_("reserved_at", reserved_at.to_string())
        .delete()
        .await?;
    if affected == 0 {
        tx.commit().await?;
        return Ok(());
    }
    tx.table(FAILED_TABLE)
        .insert(&[
            ("queue", Value::Text(row.get("queue")?)),
            ("job", Value::Text(row.get("job")?)),
            ("payload", Value::Text(row.get("payload")?)),
            ("error", Value::Text(error)),
            ("failed_at", Value::Text(crate::database::now())),
        ])
        .await?;
    tx.commit().await?;
    tracing::warn!(
        "ジョブ #{id} は {attempts} 回で諦めました（ワーカーが応答しなくなりました）。\
         queue:failed で見られます"
    );
    Ok(())
}

/// 成功したので消す。
///
/// **自分が予約したままの行だけ**を消します。`id` だけで消すと、
/// `retry_after` で取り直した別のワーカーが動かしている行を消してしまい、
/// そのワーカーの失敗が `failed_jobs` にも残りません。
async fn finish(reserved: &Reserved) -> Result<()> {
    let affected = mine(DB::table(TABLE), reserved).delete().await?;
    if affected == 0 {
        warn_not_mine(reserved.id);
    }
    Ok(())
}

/// 失敗したので戻す。上限を超えていたら諦める。
///
/// こちらも**自分が予約したままの行だけ**を触ります。`reserved_at` を
/// 戻すだけだと、まだ動いている別のワーカーがいるのに 3 つ目のワーカーが
/// 同じジョブを取れてしまいます。
async fn fail(reserved: &Reserved, tries: u32, error: &str) -> Result<bool> {
    let attempts = reserved.attempts.saturating_add(1);
    if attempts >= i64::from(tries) {
        give_up(reserved, error).await?;
        return Ok(true);
    }

    let next = time::format_timestamp(time::now_seconds() + backoff_secs(attempts));
    let affected = mine(DB::table(TABLE), reserved)
        .update(&[
            ("attempts", Value::Int(attempts)),
            ("available_at", Value::Text(next)),
            ("reserved_at", Value::Null),
        ])
        .await?;
    if affected == 0 {
        warn_not_mine(reserved.id);
    }
    Ok(false)
}

/// 諦めて `failed_jobs` へ移す（[`fail`] の上限到達と、名前の無いジョブで使う）。
///
/// 入れるのと消すのを 1 つのトランザクションにします。
/// 分けると、間でプロセスが死んだときに両方の表に残って再実行されます。
///
/// **先に消します。** 0 件なら自分の予約ではないので、
/// `failed_jobs` にも入れません（別のワーカーの結果を踏まない）。
async fn give_up(reserved: &Reserved, error: &str) -> Result<()> {
    let tx = DB::begin().await?;
    let affected = mine(tx.table(TABLE), reserved).delete().await?;
    if affected == 0 {
        tx.commit().await?;
        warn_not_mine(reserved.id);
        return Ok(());
    }
    tx.table(FAILED_TABLE)
        .insert(&[
            ("queue", Value::Text(reserved.queue.clone())),
            ("job", Value::Text(reserved.job.clone())),
            ("payload", Value::Text(reserved.payload.clone())),
            ("error", Value::Text(error.to_string())),
            ("failed_at", Value::Text(crate::database::now())),
        ])
        .await?;
    tx.commit().await
}

/// 名前の無いジョブの説明文。
fn unknown_job_message(name: &str) -> String {
    format!("`{name}` というジョブはありません（app/Jobs/ を確かめてください）")
}

/// ジョブを 1 件動かす。パニックも失敗として扱います。
async fn run_one(jobs: &[Job], reserved: &Reserved) -> std::result::Result<(), String> {
    let Some(job) = jobs.iter().find(|j| j.name == reserved.job) else {
        return Err(unknown_job_message(&reserved.job));
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

/// 続けて競り合いに負けてよい回数。
///
/// [`Reservation::Lost`] は普通ならすぐ解消するので待ちません。ですが
/// 2 文目の条件が**永久に合わない**行が先頭にあると、同じ行を選び直して
/// 全力で回り続けます（CPU を使い切り、キューは 1 件も進みません）。
/// 続けてこの回数負けたら `--sleep` の待ちに落として歯止めにします。
const MAX_CONSECUTIVE_LOST: u32 = 10;

/// 続けて負けた回数を数え、待ちに落ちるべきかを返す。
///
/// 落ちると決めたら数えた回数を 0 に戻します。待った後は、また
/// [`MAX_CONSECUTIVE_LOST`] 回まで待たずに取り直せます。
fn back_off_after_losses(lost: &mut u32) -> bool {
    *lost += 1;
    if *lost >= MAX_CONSECUTIVE_LOST {
        *lost = 0;
        return true;
    }
    false
}

/// `--sleep` 秒待つ。ここで止まらないよう、細かく区切って合図を見る。
async fn idle_sleep(sleep: u64, stop: &AtomicBool) {
    for _ in 0..sleep.max(1) * 10 {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// `queue:work` の本体。止まるまでジョブを処理し続けます。
pub(crate) async fn run(jobs: &[Job], options: &Options, stop: Arc<AtomicBool>) -> Result<()> {
    println!(
        "キュー `{}` のジョブを処理します（Ctrl+C で終了）",
        options.queue
    );

    let mut processed = 0u64;
    // 続けて競り合いに負けた回数。取れたら 0 に戻します。
    let mut lost = 0u32;
    loop {
        if stop.load(Ordering::Relaxed) {
            println!("\n停止の合図を受けました。{processed} 件処理しました。");
            return Ok(());
        }

        let reserved = match reserve(&options.queue, options.retry_after, options.tries).await? {
            Reservation::Taken(reserved) => {
                lost = 0;
                reserved
            }
            // 競り合いに負けただけなので待たない。ただし負け続けるときは、
            // 条件が永久に合わない行で全力ループになるので待ちに落ちる。
            Reservation::Lost => {
                if back_off_after_losses(&mut lost) {
                    idle_sleep(options.sleep, &stop).await;
                }
                continue;
            }
            Reservation::Empty => {
                lost = 0;
                if options.once {
                    println!("待っているジョブはありません。");
                    return Ok(());
                }
                idle_sleep(options.sleep, &stop).await;
                continue;
            }
        };

        // 名前の無いジョブは、待っても動くようにならない。再挑戦の待ち時間
        //（10 秒・20 秒…）を 3 回ぶん積むだけなので、すぐ `failed_jobs` へ移す。
        // ファイルを置き忘れたまま `queue:retry` で戻せるよう、記録は残す。
        if !jobs.iter().any(|j| j.name == reserved.job) {
            let message = unknown_job_message(&reserved.job);
            give_up(&reserved, &message).await?;
            eprintln!(
                "  {} #{} 失敗（諦めました）: {message}",
                reserved.job, reserved.id
            );
            if options.once {
                return Ok(());
            }
            continue;
        }

        let started = std::time::Instant::now();
        match run_one(jobs, &reserved).await {
            Ok(()) => {
                finish(&reserved).await?;
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
                        reserved.attempts.saturating_add(1)
                    );
                } else {
                    eprintln!(
                        "  {} #{} 失敗（{} 回目、{} 秒後に再挑戦）: {message}",
                        reserved.job,
                        reserved.id,
                        reserved.attempts.saturating_add(1),
                        backoff_secs(reserved.attempts.saturating_add(1))
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
        let failed_id: i64 = row.get("id")?;
        // **先に消します。** 行はトランザクションの外で読んでいるので、
        // `queue:retry` を 2 つ同時に流すと同じ行を両方が見ます。先に入れると
        // `insert` は両方通り、二重投入になります（`delete` だけが 0 件になる）。
        // 消せた側だけが入れ直せば、通るのは 1 つだけです
        //（`finish` / `fail` と同じ「先に消す」考え方）。
        let affected = tx
            .table(FAILED_TABLE)
            .where_("id", failed_id)
            .delete()
            .await?;
        if affected != 1 {
            continue;
        }
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
        .collect()
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

    /// `reserve` の 1 文目を組み立てて SQL と値を見る。
    ///
    /// **Rust 側に判定を写しません。** 本番で走るのは SQL なので、写しを
    /// 持つと 2 つがずれても気づけません。投げる文そのものを押さえます。
    fn reserve_sql(now: &str, cutoff: &str) -> (String, Vec<Value>) {
        reservable(DB::table(TABLE), "default", now, cutoff)
            .order_by("id")
            .limit(1)
            .to_sql()
    }

    #[test]
    fn 取り直しの条件はsqlに出る() {
        let now = time::format_timestamp(1_700_000_000);
        let cutoff = reclaim_cutoff(1_700_000_000, 90);
        let (sql, bindings) = reserve_sql(&now, &cutoff);

        // 予約が無いもの、または境目**以前**に予約されたもの。`<` では
        // ちょうど境目のものを取り直せません。
        assert!(sql.contains("\"reserved_at\" is null"), "{sql}");
        assert!(sql.contains("or \"reserved_at\" <= ?"), "{sql}");
        assert!(!sql.contains("\"reserved_at\" < ?"), "{sql}");
        // 括弧でくくられていること（`or` が外に出ると全件が当たる）。
        assert!(sql.contains("and (\"reserved_at\" is null"), "{sql}");
        // 1 件ずつ、古いものから。
        assert!(sql.contains("order by \"id\" asc limit 1"), "{sql}");

        // 渡す値は キュー → いまの時刻 → 境目 の順。
        assert_eq!(
            bindings,
            vec![
                Value::Text("default".to_string()),
                Value::Text(now),
                Value::Text(cutoff),
            ]
        );
    }

    #[test]
    fn 更新も同じ条件で絞る() {
        // 2 文目（update）の `where` は 1 文目と同じ条件に、見た `attempts` を足す。
        let now = time::format_timestamp(1_700_000_000);
        let cutoff = reclaim_cutoff(1_700_000_000, 90);
        let (sql, bindings) =
            reservable(DB::table(TABLE).where_("id", 7), "default", &now, &cutoff)
                .where_("attempts", 2_i64)
                .to_sql();

        assert!(sql.contains("\"id\" = ?"), "{sql}");
        assert!(sql.contains("\"queue\" = ?"), "{sql}");
        assert!(sql.contains("\"available_at\" <= ?"), "{sql}");
        assert!(sql.contains("and (\"reserved_at\" is null"), "{sql}");
        assert!(sql.contains("or \"reserved_at\" <= ?"), "{sql}");
        assert!(sql.contains("and \"attempts\" = ?"), "{sql}");
        assert_eq!(
            bindings,
            vec![
                Value::Int(7),
                Value::Text("default".to_string()),
                Value::Text(now),
                Value::Text(cutoff),
                Value::Int(2),
            ]
        );
    }

    /// すぐ取れるジョブを 1 本入れて、その id を返す。
    #[cfg(feature = "sqlite")]
    async fn 待っているジョブを入れる(job: &str) -> i64 {
        let now = crate::database::now();
        DB::table(TABLE)
            .insert_get_id(&[
                ("queue", Value::Text(DEFAULT_QUEUE.to_string())),
                ("job", Value::Text(job.to_string())),
                ("payload", Value::Text("{}".to_string())),
                ("attempts", Value::Int(0)),
                ("available_at", Value::Text(now.clone())),
                ("created_at", Value::Text(now)),
            ])
            .await
            .unwrap()
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 間に別のワーカーが失敗させた行は取らない() {
        let _db = crate::testing::refresh_database().await;
        表を作る().await;
        let id = 待っているジョブを入れる("SendWelcome").await;

        // 1 文目で見た状態（attempts = 0、available_at = 今）。
        let now_secs = time::now_seconds();
        let now = time::format_timestamp(now_secs);
        let cutoff = reclaim_cutoff(now_secs, DEFAULT_RETRY_AFTER);

        // 2 文目を投げる前に、別のワーカーが同じ行を取って速く失敗した形。
        // `fail` と同じく、予約を外して回数を増やし、次の時刻を先へ送る。
        let later = time::format_timestamp(now_secs + 60);
        DB::table(TABLE)
            .where_("id", id)
            .update(&[
                ("attempts", Value::Int(1)),
                ("available_at", Value::Text(later.clone())),
                ("reserved_at", Value::Null),
            ])
            .await
            .unwrap();

        // 以前は `id` と取り直しの条件だけで更新が通り、待ち時間を無視して
        // 即再実行し、`attempts` を 0 に巻き戻していた。
        let affected = claim_row(DEFAULT_QUEUE, id, 0, &now, &cutoff, 0)
            .await
            .unwrap();
        assert_eq!(affected, 0, "負けたと見なす");

        let row = DB::table(TABLE).find(id).await.unwrap().unwrap();
        assert_eq!(row.get::<i64>("attempts").unwrap(), 1, "回数を巻き戻さない");
        assert_eq!(
            row.get::<String>("available_at").unwrap(),
            later,
            "待ち時間を保つ"
        );
        assert!(row.value("reserved_at").unwrap().is_null(), "予約しない");
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 名前の無いジョブは待たずに諦める() {
        let _db = crate::testing::refresh_database().await;
        表を作る().await;
        待っているジョブを入れる("NoSuchJob").await;

        // 登録されているジョブが 1 つも無い状態で 1 本だけ処理する。
        let stop = Arc::new(AtomicBool::new(false));
        let options = Options {
            once: true,
            ..Options::default()
        };
        run(&[], &options, stop).await.unwrap();

        // 以前は 10 秒・20 秒と待って 3 回回してから諦めていた。
        assert_eq!(
            DB::table(TABLE).count().await.unwrap(),
            0,
            "キューから消える"
        );
        let failed = failed_list().await.unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].1, "NoSuchJob");
        assert!(
            failed[0].3.contains("というジョブはありません"),
            "{}",
            failed[0].3
        );
    }

    #[test]
    fn 自分の予約だけを触る条件() {
        // `finish` と `fail` は id だけで絞りません。
        let reserved = Reserved {
            id: 7,
            job: "SendWelcome".into(),
            payload: "{}".into(),
            attempts: 1,
            queue: "default".into(),
            reserved_at: "2024-01-01 00:00:00".into(),
        };
        let (sql, bindings) = mine(DB::table(TABLE), &reserved).to_sql();

        assert!(sql.contains("\"id\" = ?"), "{sql}");
        assert!(sql.contains("and \"reserved_at\" = ?"), "{sql}");
        assert_eq!(
            bindings,
            vec![
                Value::Int(7),
                Value::Text("2024-01-01 00:00:00".to_string()),
            ]
        );
    }

    #[test]
    fn 境目の時刻は予約より前になる() {
        let now = time::now_seconds();
        let cutoff = reclaim_cutoff(now, 90);
        // 100 秒前に予約されたまま（ワーカーが死んだ）＝境目より前なので取り直す。
        assert!(time::format_timestamp(now - 100) < cutoff);
        // 10 秒前に予約されたばかり（まだ処理中）＝境目より後なので取らない。
        assert!(time::format_timestamp(now - 10) > cutoff);
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

    /// テスト用にキューの表を作る。`sqlite` が無いときは使いません。
    #[cfg(feature = "sqlite")]
    async fn 表を作る() {
        let mut schema = crate::database::Schema::new(crate::database::Driver::Sqlite);
        crate::queue::Queue::define(&mut schema);
        for sql in schema.to_sql() {
            DB::statement(sql, &[]).await.unwrap();
        }
    }

    /// 諦めたジョブを 1 本入れて、その id を返す。`sqlite` が無いときは使いません。
    #[cfg(feature = "sqlite")]
    async fn 失敗を1本入れる() -> i64 {
        let now = crate::database::now();
        DB::table(FAILED_TABLE)
            .insert_get_id(&[
                ("queue", Value::Text(DEFAULT_QUEUE.to_string())),
                ("job", Value::Text("SendWelcome".to_string())),
                ("payload", Value::Text("{}".to_string())),
                ("error", Value::Text("わざと失敗".to_string())),
                ("failed_at", Value::Text(now)),
            ])
            .await
            .unwrap()
    }

    /// 実際にデータベースをつなぐテストです。
    ///
    /// 機能フラグ `sqlite` が無いとつなげないので、そのときは飛ばします。
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 同じ失敗ジョブは二重に入らない() {
        let _db = crate::testing::refresh_database().await;
        表を作る().await;
        let id = 失敗を1本入れる().await;

        // `queue:retry` を 2 つ同時に流した形。行はトランザクションの外で
        // 読むので、両方が同じ失敗ジョブを見ます。消せた側だけが入れ直すので、
        // 入るのは 1 本だけです（前は `insert` が両方通っていました）。
        let (a, b) = tokio::join!(retry_failed(Some(id)), retry_failed(Some(id)));
        let moved = a.unwrap() + b.unwrap();
        assert_eq!(moved, 1, "入れ直したのは 1 本だけ");
        assert_eq!(DB::table(TABLE).count().await.unwrap(), 1, "二重に入らない");
        assert_eq!(DB::table(FAILED_TABLE).count().await.unwrap(), 0);

        // もう 1 回流しても増えない。
        assert_eq!(retry_failed(None).await.unwrap(), 0);
        assert_eq!(DB::table(TABLE).count().await.unwrap(), 1);
    }

    /// 処理中の目印が残ったままのジョブを 1 本入れて、その id を返す。
    #[cfg(feature = "sqlite")]
    async fn 放置されたジョブを入れる(attempts: i64) -> i64 {
        let now = crate::database::now();
        // 目印は `retry_after` より十分古くする（＝回収の対象）。
        let old = time::format_timestamp(time::now_seconds() - 1_000);
        DB::table(TABLE)
            .insert_get_id(&[
                ("queue", Value::Text(DEFAULT_QUEUE.to_string())),
                ("job", Value::Text("SendWelcome".to_string())),
                ("payload", Value::Text("{}".to_string())),
                ("attempts", Value::Int(attempts)),
                ("available_at", Value::Text(now.clone())),
                ("reserved_at", Value::Text(old)),
                ("created_at", Value::Text(now)),
            ])
            .await
            .unwrap()
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn ワーカーごと落ちたジョブは諦める() {
        let _db = crate::testing::refresh_database().await;
        表を作る().await;
        // `tries` を超えて試したあと、ワーカーごと落ちて目印が残ったままの行。
        放置されたジョブを入れる(3).await;

        // 以前はここで予約し直し、`attempts` が `tries` を超えても回収され
        // 続けました。`order_by("id") limit 1` なので、この 1 件で
        // キュー全体が止まりました。
        let taken = reserve(DEFAULT_QUEUE, DEFAULT_RETRY_AFTER, 3)
            .await
            .unwrap();
        assert!(matches!(taken, Reservation::Lost), "予約しない");
        assert_eq!(
            DB::table(TABLE).count().await.unwrap(),
            0,
            "キューから消える"
        );
        assert_eq!(DB::table(FAILED_TABLE).count().await.unwrap(), 1);

        // `queue:failed` に出るので、何が起きたか分かる。
        let failed = failed_list().await.unwrap();
        assert_eq!(failed.len(), 1);
        assert!(
            failed[0].3.contains("ワーカーが応答しなくなった"),
            "{}",
            failed[0].3
        );

        // 詰まりが解けて、次は空になる。
        assert!(matches!(
            reserve(DEFAULT_QUEUE, DEFAULT_RETRY_AFTER, 3)
                .await
                .unwrap(),
            Reservation::Empty
        ));
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 落ちた1回も数えて諦める() {
        let _db = crate::testing::refresh_database().await;
        表を作る().await;
        // 2 回ぶん記録されたあと、3 回目の途中でワーカーごと落ちた行。
        // 落ちた 1 回はまだ `attempts` に数えられていない。
        放置されたジョブを入れる(2).await;

        // 列の値（2）で見ていたころは `2 >= 3` が偽で予約し直し、
        // `--tries 3` でも 4 回走っていた。数え直した値（3）で諦める。
        let taken = reserve(DEFAULT_QUEUE, DEFAULT_RETRY_AFTER, 3)
            .await
            .unwrap();
        assert!(matches!(taken, Reservation::Lost), "予約しない");
        assert_eq!(
            DB::table(TABLE).count().await.unwrap(),
            0,
            "キューから消える"
        );

        // 文面の回数も、実際に動いた回数とそろう。
        let failed = failed_list().await.unwrap();
        assert_eq!(failed.len(), 1);
        assert!(failed[0].3.starts_with("3 回試しました"), "{}", failed[0].3);
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 回数が負の行でも予約できる() {
        let _db = crate::testing::refresh_database().await;
        表を作る().await;
        let id = 待っているジョブを入れる("SendWelcome").await;
        // アプリが `DB::table("jobs").update(..)` で負の値を入れた形。
        DB::table(TABLE)
            .where_("id", id)
            .update(&[("attempts", Value::Int(-1))])
            .await
            .unwrap();

        // 読んだ値を `max(0)` で丸めていたころは、2 文目の
        // `where attempts = 0` が永久に 0 件になり、`Lost` を待たずに
        // 繰り返して全力ループになった（キューは 1 件も進まない）。
        match reserve(DEFAULT_QUEUE, DEFAULT_RETRY_AFTER, 3)
            .await
            .unwrap()
        {
            Reservation::Taken(reserved) => {
                assert_eq!(reserved.id, id);
                assert_eq!(reserved.attempts, -1, "読んだ値をそのまま持つ");
            }
            _ => panic!("予約できるはずです"),
        }
        // 回数は `attempts + 1` で進むので、いずれ `tries` に達する。
        assert_eq!(next_attempts(Some("2024-01-01 00:00:00"), -1), 0);
        // ずらし幅が負にならない。
        assert_eq!(backoff_secs(-1), 5);
    }

    #[test]
    fn 負けが続いたら待ちに落ちる() {
        // 2 文目の条件が永久に合わない行があると、`Lost` の繰り返しで
        // 全力ループになる。ある回数で `--sleep` の待ちに落として歯止めにする。
        let mut lost = 0;
        for _ in 1..MAX_CONSECUTIVE_LOST {
            assert!(!back_off_after_losses(&mut lost), "数回なら待たない");
        }
        assert!(back_off_after_losses(&mut lost), "続けて負けたら待つ");
        // 数え直すので、次はまた待たずに取り直せる。
        assert!(!back_off_after_losses(&mut lost));
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 回数が残っていれば取り直す() {
        let _db = crate::testing::refresh_database().await;
        表を作る().await;
        let id = 放置されたジョブを入れる(1).await;

        // `tries` に届いていない行は、今までどおり取り直す（`attempts` は +1）。
        match reserve(DEFAULT_QUEUE, DEFAULT_RETRY_AFTER, 3)
            .await
            .unwrap()
        {
            Reservation::Taken(reserved) => {
                assert_eq!(reserved.id, id);
                assert_eq!(reserved.attempts, 2);
            }
            _ => panic!("取り直すはずです"),
        }
        assert_eq!(DB::table(FAILED_TABLE).count().await.unwrap(), 0);
        assert_eq!(DB::table(TABLE).count().await.unwrap(), 1);
    }
}
