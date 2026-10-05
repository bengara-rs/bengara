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
//!
//! 前回いつ動いたかは `storage/framework/schedule/` にファイルで残します。
//! **キャッシュには置きません。** `CACHE_DRIVER=memory` だと、1分ごとに起動する
//! 別のプロセスからは前回の時刻が見えず、`Every::Hour` が毎分走ってしまうためです。

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use crate::error::{Error, Result};

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

    /// 前回いつ動いたかを覚えるファイルの名前。
    ///
    /// 名前には日本語も空白も入るので、SHA-256 の16進にします。
    /// 元の名前はファイルの1行目に書くので、中を見れば分かります。
    fn state_file(&self) -> String {
        crate::support::crypto::to_hex(&crate::support::crypto::sha256(self.name.as_bytes()))
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

/// 前回いつ動いたかを置くディレクトリ。
pub(crate) fn state_dir() -> PathBuf {
    // join を 2 回に分けて、Windows でも区切りが混ざらないようにする。
    crate::paths::storage_path("framework").join("schedule")
}

/// 置き場所を用意する。
///
/// `storage/` そのものが無いなら、作らずに知らせます（決定記録 #023）。
fn ensure_state_dir(dir: &Path) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let storage = crate::paths::storage_path("");
    if !storage.is_dir() {
        return Err(Error::msg(format!(
            "{} がありません。デプロイのときに storage/ を作ってください",
            storage.display()
        )));
    }
    std::fs::create_dir_all(dir)?;
    Ok(())
}

/// 前回動いた時刻（UNIX 秒）を読む。無い・壊れていれば `None`。
fn read_last_run(dir: &Path, task: &Task) -> Option<i64> {
    let raw = std::fs::read_to_string(dir.join(task.state_file())).ok()?;
    // 1行目は名前（人が見るため）、2行目が時刻。
    raw.lines().nth(1)?.trim().parse().ok()
}

/// 前回動いた時刻を書く。
fn write_last_run(dir: &Path, task: &Task, now: i64) -> Result<()> {
    let path = dir.join(task.state_file());
    std::fs::write(&path, format!("{}\n{now}\n", task.name))?;
    Ok(())
}

/// 動かすべきものを動かす。返るのは動かした名前です。
pub(crate) async fn run_due(schedule: &Schedule) -> Result<Vec<String>> {
    let dir = state_dir();
    ensure_state_dir(&dir)?;
    run_due_in(&dir, schedule).await
}

/// 置き場所を指定して動かす（テストから使います）。
async fn run_due_in(dir: &Path, schedule: &Schedule) -> Result<Vec<String>> {
    let now = crate::support::time::now_seconds();
    let mut done = Vec::new();

    for task in &schedule.tasks {
        let last_run = read_last_run(dir, task);

        if !is_due(last_run, task.every, now) {
            continue;
        }

        // 先に時刻を書く。処理が失敗しても、次の1分でまた走らないようにするため。
        write_last_run(dir, task, now)?;

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
pub(crate) fn print_list(schedule: &Schedule) {
    if schedule.is_empty() {
        println!("登録されている処理はありません（routes/console.rs を確かめてください）");
        return;
    }
    let dir = state_dir();
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
        let last = read_last_run(&dir, task)
            .map(crate::support::time::format_timestamp)
            .unwrap_or_else(|| "まだ".to_string());
        println!(
            "{}  {}  {last}",
            text::pad(&task.name, name_width),
            text::pad(&task.every.label(), every_width)
        );
    }
    println!("\n{} 件", schedule.len());
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
    fn 覚えるファイルの名前は名前から作る() {
        let mut schedule = Schedule::new();
        schedule
            .job("毎日の集計", Every::Day, || async { Ok(()) })
            .job("別の処理", Every::Day, || async { Ok(()) });

        let first = schedule.tasks()[0].state_file();
        // 日本語でもファイル名に使える形になる。
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|b| b.is_ascii_hexdigit()));
        // 名前が違えば別のファイル。同じ名前なら同じファイル。
        assert_ne!(first, schedule.tasks()[1].state_file());
        assert_eq!(first, schedule.tasks()[0].state_file());
    }

    /// テスト用の空ディレクトリ。
    fn temp_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bengara-schedule-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn 前回の時刻はファイルに残る() {
        let dir = temp_dir("state");
        let mut schedule = Schedule::new();
        schedule.job("1時間ごとの処理", Every::Hour, || async { Ok(()) });

        // 1 回目は動く。
        let done = run_due_in(&dir, &schedule).await.unwrap();
        assert_eq!(done, vec!["1時間ごとの処理".to_string()]);

        // 2 回目は、前回の時刻がファイルに残っているので動かない。
        // （キャッシュに置いていたときは、別プロセスだと毎回動いてしまった）
        let done = run_due_in(&dir, &schedule).await.unwrap();
        assert!(done.is_empty());

        // 中身は「名前」と「UNIX 秒」の 2 行。
        let raw = std::fs::read_to_string(dir.join(schedule.tasks()[0].state_file())).unwrap();
        let mut lines = raw.lines();
        assert_eq!(lines.next(), Some("1時間ごとの処理"));
        assert!(lines.next().unwrap().parse::<i64>().unwrap() > 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn 毎分の処理は毎回動く() {
        let dir = temp_dir("minute");
        let mut schedule = Schedule::new();
        schedule.job("毎分の処理", Every::Minute, || async { Ok(()) });

        assert_eq!(run_due_in(&dir, &schedule).await.unwrap().len(), 1);
        assert_eq!(run_due_in(&dir, &schedule).await.unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn 失敗しても時刻は残る() {
        let dir = temp_dir("failed");
        let mut schedule = Schedule::new();
        schedule.job("失敗する処理", Every::Hour, || async {
            Err(crate::error::Error::msg("わざと失敗"))
        });

        // 失敗したので動いた一覧には入らない。
        assert!(run_due_in(&dir, &schedule).await.unwrap().is_empty());
        // それでも時刻は残るので、次の 1 分でまた走らない。
        assert!(read_last_run(&dir, &schedule.tasks()[0]).is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 置き場所はstorageの下() {
        assert!(state_dir().ends_with("schedule"));
        assert!(state_dir().starts_with(crate::paths::storage_path("")));
    }
}
