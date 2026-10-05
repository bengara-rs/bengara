//! スケジューラ。決まった間隔で処理を動かします。
//!
//! ```ignore
//! // routes/console.rs
//! pub fn schedule(s: &mut Schedule) {
//!     s.job("期限切れのトークンを消す", Every::Hour, || async {
//!         PasswordReset::sweep_expired().await.map(|_| ())
//!     });
//! }
//! ```
//!
//! ```sh
//! cargo artisan schedule:run     # いま動かすべきものを動かす
//! ```
//!
//! **1分ごとに外から呼んでください**（Laravel と同じ）。
//!
//! ```text
//! * * * * * cd /path/to/app && ./myapp schedule:run >> /dev/null 2>&1
//! ```

use std::future::Future;
use std::pin::Pin;

use crate::error::Result;

/// 処理が返す非同期の結果。
pub type TaskFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

/// どのくらいの間隔で動かすか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Every {
    /// 呼ばれるたびに動かす（1分ごとに呼ぶ前提）。
    Minute,
    /// n 分ごと。
    Minutes(u32),
    /// 1 時間ごと。
    Hour,
    /// 1 日ごと。
    Day,
    /// n 秒ごと。
    Seconds(u64),
}

impl Every {
    /// 間隔を秒で返す。
    pub fn seconds(&self) -> u64 {
        match self {
            // 1分ごとに呼ばれる前提なので、毎回動かす。
            Every::Minute => 0,
            Every::Minutes(n) => *n as u64 * 60,
            Every::Hour => 3_600,
            Every::Day => 86_400,
            Every::Seconds(n) => *n,
        }
    }

    /// 人が読む形。
    pub fn label(&self) -> String {
        match self {
            Every::Minute => "毎分".to_string(),
            Every::Minutes(n) => format!("{n} 分ごと"),
            Every::Hour => "1 時間ごと".to_string(),
            Every::Day => "1 日ごと".to_string(),
            Every::Seconds(n) => format!("{n} 秒ごと"),
        }
    }
}

/// 登録された処理1つ。
pub struct Task {
    name: String,
    every: Every,
    run: Box<dyn Fn() -> TaskFuture + Send + Sync>,
}

impl Task {
    /// 名前。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 間隔。
    pub fn every(&self) -> Every {
        self.every
    }

    /// キャッシュに入れる鍵（前回いつ動いたか）。
    fn cache_key(&self) -> String {
        format!("bengara_schedule:{}", self.name)
    }
}

/// 登録の入れ物。`routes/console.rs` の `schedule` が受け取ります。
#[derive(Default)]
pub struct Schedule {
    tasks: Vec<Task>,
}

impl Schedule {
    /// 空の入れ物を作る。
    pub fn new() -> Self {
        Self::default()
    }

    /// 処理を足す。
    ///
    /// 名前は**前回いつ動いたか**を覚える鍵になります。重ならない名前にしてください。
    pub fn job<F, Fut>(&mut self, name: impl Into<String>, every: Every, run: F) -> &mut Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        self.tasks.push(Task {
            name: name.into(),
            every,
            run: Box::new(move || Box::pin(run())),
        });
        self
    }

    /// 登録されている処理。
    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    /// 何件登録されているか。
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// 1件も無いか。
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
}

impl std::fmt::Debug for Schedule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<_> = self
            .tasks
            .iter()
            .map(|t| format!("{}({})", t.name, t.every.label()))
            .collect();
        f.debug_struct("Schedule").field("tasks", &names).finish()
    }
}

/// いま動かすべきか。前回の時刻（UNIX 秒）と間隔から決めます。
pub(crate) fn is_due(last_run: Option<i64>, every: Every, now: i64) -> bool {
    match last_run {
        // 1 度も動いていなければ動かす。
        None => true,
        Some(last) => now - last >= every.seconds() as i64,
    }
}

/// 動かすべきものを動かす。返るのは動かした名前です。
pub(crate) async fn run_due(schedule: &Schedule) -> Result<Vec<String>> {
    let now = crate::support::time::now_seconds();
    let mut done = Vec::new();

    for task in &schedule.tasks {
        let key = task.cache_key();
        let last_run = crate::cache::Cache::get(&key)
            .await?
            .and_then(|v| v.trim().parse::<i64>().ok());

        if !is_due(last_run, task.every, now) {
            continue;
        }

        // 先に時刻を書く。処理が失敗しても、次の1分でまた走らないようにするため。
        crate::cache::Cache::forever(&key, now.to_string()).await?;

        let started = std::time::Instant::now();
        match (task.run)().await {
            Ok(()) => {
                println!(
                    "  {} 成功（{} ms）",
                    task.name,
                    started.elapsed().as_millis()
                );
                done.push(task.name.clone());
            }
            // 1つ失敗しても、ほかの処理は動かす。
            Err(e) => eprintln!("  {} 失敗: {e}", task.name),
        }
    }
    Ok(done)
}

/// 登録されている処理を表で出す（`schedule:list`）。
pub(crate) async fn print_list(schedule: &Schedule) -> Result<()> {
    if schedule.is_empty() {
        println!("登録されている処理はありません（routes/console.rs を確かめてください）");
        return Ok(());
    }
    // 日本語は端末で 2 文字分の幅を取るので、文字数ではなく幅でそろえる。
    use crate::support::text;

    let name_width = schedule
        .tasks
        .iter()
        .map(|t| text::width(&t.name))
        .max()
        .unwrap_or(4)
        .max(4);
    let every_width = schedule
        .tasks
        .iter()
        .map(|t| text::width(&t.every.label()))
        .max()
        .unwrap_or(4)
        .max(4);

    println!(
        "{}  {}  前回",
        text::pad("名前", name_width),
        text::pad("間隔", every_width)
    );
    for task in &schedule.tasks {
        let last = crate::cache::Cache::get(&task.cache_key())
            .await?
            .and_then(|v| v.trim().parse::<i64>().ok())
            .map(crate::support::time::format_timestamp)
            .unwrap_or_else(|| "まだ".to_string());
        println!(
            "{}  {}  {last}",
            text::pad(&task.name, name_width),
            text::pad(&task.every.label(), every_width)
        );
    }
    println!("\n{} 件", schedule.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 間隔を秒にできる() {
        assert_eq!(Every::Minute.seconds(), 0);
        assert_eq!(Every::Minutes(5).seconds(), 300);
        assert_eq!(Every::Hour.seconds(), 3_600);
        assert_eq!(Every::Day.seconds(), 86_400);
        assert_eq!(Every::Seconds(30).seconds(), 30);
    }

    #[test]
    fn 間隔の表示() {
        assert_eq!(Every::Minute.label(), "毎分");
        assert_eq!(Every::Minutes(5).label(), "5 分ごと");
        assert_eq!(Every::Hour.label(), "1 時間ごと");
        assert_eq!(Every::Day.label(), "1 日ごと");
        assert_eq!(Every::Seconds(30).label(), "30 秒ごと");
    }

    #[test]
    fn 一度も動いていなければ動かす() {
        assert!(is_due(None, Every::Day, 1_000));
    }

    #[test]
    fn 間隔が過ぎていれば動かす() {
        let now = 100_000;
        assert!(is_due(Some(now - 3_600), Every::Hour, now), "ちょうど1時間");
        assert!(is_due(Some(now - 3_601), Every::Hour, now));
        assert!(!is_due(Some(now - 3_599), Every::Hour, now), "まだ早い");

        // 毎分は、前回がいつでも動かす（1分ごとに呼ばれる前提）。
        assert!(is_due(Some(now), Every::Minute, now));
    }

    #[test]
    fn 登録できる() {
        let mut schedule = Schedule::new();
        assert!(schedule.is_empty());

        schedule
            .job("a", Every::Minute, || async { Ok(()) })
            .job("b", Every::Hour, || async { Ok(()) });

        assert_eq!(schedule.len(), 2);
        assert_eq!(schedule.tasks()[0].name(), "a");
        assert_eq!(schedule.tasks()[1].every(), Every::Hour);
        assert!(format!("{schedule:?}").contains("a(毎分)"));
    }

    #[test]
    fn 鍵は名前から作る() {
        let mut schedule = Schedule::new();
        schedule.job("毎日の集計", Every::Day, || async { Ok(()) });
        assert_eq!(
            schedule.tasks()[0].cache_key(),
            "bengara_schedule:毎日の集計"
        );
    }
}
