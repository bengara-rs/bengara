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
    ///
    /// # パニック
    ///
    /// 同じ名前を 2 回登録するとパニックします。名前は前回の時刻と錠の鍵なので、
    /// 2 件目は 1 件目が書いた時刻を見て、`Every::Minute` 以外は**二度と動きません。**
    /// 起動時に止めるのは、未登録のミドルウェア名と同じ考え方です（決定記録 #087）。
    pub fn job<F, Fut>(&mut self, name: impl Into<String>, every: Every, run: F) -> &mut Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let name = name.into();
        if let Some(existing) = self.tasks.iter().find(|t| t.name == name) {
            panic!(
                "定期処理 `{name}` は既に登録されています（{}）。\
                 名前は前回の時刻を覚える鍵なので、重ねると 2 件目が動きません。\
                 routes/console.rs で別の名前にしてください",
                existing.every.label()
            );
        }
        self.tasks.push(Task {
            name,
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

/// 分の粒度で許す遅れ（秒）。
///
/// cron は 1 分ごとに呼ぶので、動いた秒はぶれます。ある回が 10:00:05 に
/// 動くと、`Every::Minutes(5)` の次は 10:05:05 以降＝ cron が呼ぶのは 10:06:00
/// なので、1 回ぶん飛んで 6 分ごとになります。間隔が 1 分以上のものだけ、
/// この秒数だけ早くても動かします。
///
/// **ずれがこの秒数を超えると、やはり 1 回ぶん飛びます。** 30 秒より長く
/// 遅れて動く環境では、`Every::Seconds` で間隔を指定してください。
const DUE_SLACK_SECS: i64 = 30;

/// いま動かすべきか。前回の時刻（UNIX 秒）と間隔から決めます。
///
/// 間隔が 1 分以上のときは [`DUE_SLACK_SECS`] だけ早くても動かします。
/// `Every::Seconds(n)` には許容を付けません（秒を指定した意図を崩さないため）。
///
/// **前回が未来なら動かします。** 時計が一時的に先へ飛ぶと（NTP の補正、
/// スナップショットからの復元、RTC の狂ったコンテナ）未来の時刻が書かれます。
/// 差だけを見ていたころは、時計が直った後に差が大きな負の値になり、
/// そのタスクが二度と動きませんでした。ログも出ないので、`Every::Day` の
/// 掃除が止まっても気づけませんでした。
pub(crate) fn is_due(last_run: Option<i64>, every: Every, now: i64) -> bool {
    match last_run {
        // 1 度も動いていなければ動かす。
        None => true,
        Some(last) if last > now => {
            tracing::warn!(
                "前回の時刻 {last} がいま {now} より後です（時計がずれています）。動かします"
            );
            true
        }
        Some(last) => {
            // `as i64` だと `Every::Seconds(u64::MAX)` が `-1` になり、
            // 「ほぼ動かない」つもりが毎回動いていました。あふれは上限で
            // 切り詰めます（`queue/mod.rs` の `available_at` と同じ形）。
            let seconds = i64::try_from(every.seconds()).unwrap_or(i64::MAX);
            // 秒を指定したものは、秒のまま守る。
            let exact = matches!(every, Every::Seconds(_)) || seconds < 60;
            let need = if exact {
                seconds
            } else {
                seconds.saturating_sub(DUE_SLACK_SECS)
            };
            now.saturating_sub(last) >= need
        }
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
/// 作るときは `storage:init` と同じく所有者だけに絞ります（Unix で 0700）。
/// 錠の置き場所（`+locks/`）もここで作って同じ権限にします。
/// `FileLock` に任せると、`create_dir_all` の既定の権限（umask まかせ）になります。
fn ensure_state_dir(dir: &Path) -> Result<()> {
    let locks = dir.join(LOCK_DIR);
    if dir.is_dir() && locks.is_dir() {
        return Ok(());
    }
    let storage = crate::paths::storage_path("");
    if !storage.is_dir() {
        return Err(Error::msg(format!(
            "{} がありません。デプロイのときに storage/ を作ってください",
            storage.display()
        )));
    }
    for path in [dir, locks.as_path()] {
        if !path.is_dir() {
            std::fs::create_dir_all(path)?;
            crate::ops::restrict(path)?;
        }
    }
    Ok(())
}

/// 前回動いた時刻（UNIX 秒）を読む。無い・壊れていれば `None`。
fn read_last_run(dir: &Path, task: &Task) -> Option<i64> {
    read_last_run_file(&dir.join(task.state_file()))
}

/// 覚えるファイルを直接指定して読む（錠の中から使います）。
fn read_last_run_file(path: &Path) -> Option<i64> {
    let raw = std::fs::read_to_string(path).ok()?;
    // 1行目は名前（人が見るため）、2行目が時刻。
    raw.lines().nth(1)?.trim().parse().ok()
}

/// 前回動いた時刻を書く。
///
/// **名前の改行は空白に潰します。** 1 行目が名前、2 行目が時刻という形なので、
/// 名前に改行が入ると 2 行目が名前の続きになり、読み戻しが必ず `None` に
/// なります。`is_due(None, ..)` は真なので、`Every::Day` のタスクが黙って
/// 毎分走っていました。ファイルの中身は内部の形なので、公開 API は変わりません。
///
/// **一時ファイルに書いてから置き換えます。** `std::fs::write` は truncate
/// してから書くので原子的ではありません。途中で落ちると 0 バイトか途中までの
/// ファイルが残り、`read_last_run` が `None` を返して毎分走り出しました。
/// 改行を潰して直した形が、書き込みの非原子性で開き直っていました。
/// 作りは `cache/store.rs` の `FileCache::put` にそろえます。
#[cfg(test)]
fn write_last_run(dir: &Path, task: &Task, now: i64) -> Result<()> {
    write_last_run_file(dir, &task.state_file(), &task.name, now)
}

/// 覚えるファイルの名前を直接指定して書く（錠の中から使います）。
fn write_last_run_file(dir: &Path, state_file: &str, task_name: &str, now: i64) -> Result<()> {
    let path = dir.join(state_file);
    let name = task_name.replace(['\n', '\r'], " ");
    let seq = TEMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // 一時ファイルの名前は、覚えるファイル（SHA-256 の16進）とぶつからない
    // ように印を入れる。プロセス番号と連番で、同時に走っても重ならない。
    let temp = dir.join(format!("{state_file}+tmp.{}.{seq}", std::process::id()));
    std::fs::write(&temp, format!("{name}\n{now}\n"))?;
    if let Err(e) = std::fs::rename(&temp, &path) {
        // 置き換えに失敗したら、書きかけを残さない。
        let _ = std::fs::remove_file(&temp);
        return Err(Error::Io(e));
    }
    Ok(())
}

/// 一時ファイルの名前が重ならないようにする連番。
static TEMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 錠ファイルを置くディレクトリの名前。
///
/// 前回の時刻を覚えるファイルは SHA-256 の16進なので、`+` を含む名前とは
/// ぶつかりません。
const LOCK_DIR: &str = "+locks";

/// 錠を待つ回数（1 回 10ms なので約 1 秒）。
///
/// 守るのは「読む → 判定 → 書く」だけで、1 秒も掛かりません。待ち続けずに
/// 諦めます。取れなければ次の 1 分の `schedule:run` で動けます
/// （落ちたプロセスの錠も、そのときには古い錠として外れます）。
const CLAIM_LOCK_TRIES: u32 = 100;

/// そのタスクを守る錠ファイルの場所（テストから使います）。
#[cfg(test)]
fn lock_path(dir: &Path, task: &Task) -> PathBuf {
    lock_path_for(dir, &task.state_file())
}

/// 覚えるファイルの名前から錠ファイルの場所を決める。
fn lock_path_for(dir: &Path, state_file: &str) -> PathBuf {
    dir.join(LOCK_DIR).join(format!("{state_file}.lock"))
}

/// 前回時刻として**覚える**値。判定に使う `now` とは分けます。
///
/// 間隔が 1 分以上のタスクは、**分の頭に丸めて**覚えます。判定の時刻を
/// そのまま覚えると、先行タスクにかかった時間が記録に積まれます。
/// 10:00:00 に始まった `schedule:run` の 3 本目が 10:00:40 に `claim` すると
/// 10:00:40 が残り、`Every::Minutes(5)` の次は 10:05:10 以降（許容は
/// [`DUE_SLACK_SECS`] の 30 秒）＝ cron が呼ぶ 10:06:00 まで待って
/// **6 分ごと**になります。丸めればずれが積まないので、30 秒の幅が効き
/// 続けます（決定記録 #121 が直した現象です）。
///
/// 判定のほうは取りたての `now` のままにします。先頭で 1 回だけ取った時刻で
/// 判定すると、後ろのタスクが「前回が未来」と誤検知して二重に動きました。
///
/// `Every::Seconds(_)` と間隔 60 秒未満のタスクは丸めません。秒を指定した
/// 意図を崩さないためです（[`is_due`] の `exact` と同じ考え方）。
fn remembered_at(every: Every, now: i64) -> i64 {
    if matches!(every, Every::Seconds(_)) || every.seconds() < 60 {
        return now;
    }
    now - now.rem_euclid(60)
}

/// 錠を取って「読む → 判定 → 書く」を行う。動かすべきなら真を返す。
///
/// 錠が守るのは**前回時刻の読み書きだけ**です。2 台が同じ `storage/` を
/// 共有していても、読んだ直後に別の側が書く、という食い違いは起きません。
///
/// **いまの時刻はこの中で取ります。** `schedule:run` の先頭で 1 回だけ取って
/// いたころは、前のタスクが長引くと後ろのタスクの判定が古い時刻で行われました。
/// 10:00 に始まった A の 1 本目が 90 秒かかる間に、10:01 の B が 2 本目を動かして
/// 10:01 を書きます。A が 2 本目に来ると「前回（10:01）がいま（10:00）より後」と
/// 見て、時計ずれの扱いで**もう一度動かしていました。** `Every::Hour` が 30 秒内に
/// 2 回走る形です。
///
/// **書く時刻は丸めます。** 判定の時刻をそのまま覚えると、先行タスクに
/// かかった時間が記録に積まれ、`Every::Minutes(5)` の次回が 6 分後に
/// なりました（決定記録 #121）。判定は取りたての `now`、書くのは
/// [`remembered_at`] が丸めた値、と分けます。
///
/// **間隔より長くかかる処理の重なりは防ぎません。** 処理そのものは錠を
/// 手放してから動かします（長い処理の間ずっと握ると、古い錠と見なされて
/// 外されてしまうため）。`Every::Minute` は `seconds()` が 0 で `is_due` が
/// 常に真なので、2 つの `schedule:run` が同時に走れば毎回両方が動きます。
/// 重なりの防止は未実装で、時間での錠の自動解除は却下済みです（決定記録 #076）。
///
/// 錠の取得は待ち（`sleep`）を含むので、`spawn_blocking` の中で行います。
/// 非同期の土台で待つと、同じスレッドのほかの処理を止めます。
async fn claim(dir: &Path, task: &Task) -> Result<bool> {
    let dir = dir.to_path_buf();
    let state_file = task.state_file();
    let name = task.name.clone();
    let every = task.every;
    crate::support::blocking(move || {
        let _lock = crate::support::lock::FileLock::acquire_tries(
            &lock_path_for(&dir, &state_file),
            CLAIM_LOCK_TRIES,
        )?;
        let now = crate::support::time::now_seconds();
        if !is_due(read_last_run_file(&dir.join(&state_file)), every, now) {
            return Ok(false);
        }
        // 先に時刻を書く。処理が失敗しても、次の1分でまた走らないようにするため。
        // 書く値は分の頭に丸める（ずれを積まないため。[`remembered_at`]）。
        write_last_run_file(&dir, &state_file, &name, remembered_at(every, now))?;
        Ok(true)
    })
    .await?
}

/// 動かすべきものを動かす。返るのは動かした名前です。
pub(crate) async fn run_due(schedule: &Schedule) -> Result<Vec<String>> {
    let dir = state_dir();
    ensure_state_dir(&dir)?;
    run_due_in(&dir, schedule).await
}

/// 置き場所を指定して動かす（テストから使います）。
async fn run_due_in(dir: &Path, schedule: &Schedule) -> Result<Vec<String>> {
    let mut done = Vec::new();

    for task in &schedule.tasks {
        match claim(dir, task).await {
            Ok(true) => {}
            // まだ動かすときではない。
            Ok(false) => continue,
            // 錠が取れなかった。別のプロセスが見ているので任せる。
            // 1 つ飛ばしても、ほかの処理は動かす。
            Err(e) => {
                eprintln!("  {} は飛ばします（錠を取れませんでした）: {e}", task.name);
                continue;
            }
        }

        let started = std::time::Instant::now();
        match run_task(task).await {
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

/// タスクを 1 つ動かす。パニックも失敗として扱います。
///
/// `tokio::spawn` で包むのは、キューの `run_one` と同じ考え方です。
/// 包まないと 1 件のパニックで `schedule:run` 全体が落ち、残りのタスクが
/// 動かないまま終了コードも異常になります。時刻は先に書いてあるので、
/// 次の 1 分でも再試行されず、**黙って 1 回ぶん飛んでいました。**
async fn run_task(task: &Task) -> std::result::Result<(), String> {
    let future = (task.run)();
    match tokio::spawn(future).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(e) if e.is_panic() => Err("処理がパニックしました".to_string()),
        Err(e) => Err(format!("処理を動かせませんでした: {e}")),
    }
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
        assert!(!is_due(Some(now - 3_569), Every::Hour, now), "まだ早い");

        // 毎分は、前回がいつでも動かす（1分ごとに呼ばれる前提）。
        assert!(is_due(Some(now), Every::Minute, now));
    }

    #[test]
    fn 分の粒度は少しの遅れを許す() {
        let now = 100_000;
        // 前回が秒の単位でずれても、次の回を飛ばさない。
        // 許容が無いと 10:00:05 → 10:05:05 以降＝ cron の 10:06:00 まで待ち、
        // `Every::Minutes(5)` が 1 回ぶん飛んで 6 分ごとになっていた。
        assert!(is_due(Some(now - 300 + 30), Every::Minutes(5), now));
        assert!(is_due(Some(now - 3_600 + 30), Every::Hour, now));
        assert!(is_due(Some(now - 86_400 + 30), Every::Day, now));
        // 許容（30 秒）を超えて早いものは動かさない。
        assert!(!is_due(Some(now - 300 + 31), Every::Minutes(5), now));
        assert_eq!(DUE_SLACK_SECS, 30);
    }

    #[test]
    fn 秒の指定には許容を付けない() {
        let now = 100_000;
        assert!(is_due(Some(now - 30), Every::Seconds(30), now));
        assert!(!is_due(Some(now - 29), Every::Seconds(30), now));
        // 60 秒以上でも、秒を指定したものは秒のまま守る。
        assert!(!is_due(Some(now - 299), Every::Seconds(300), now));
        assert!(is_due(Some(now - 300), Every::Seconds(300), now));
    }

    #[test]
    fn 前回が未来なら動かす() {
        let now = 100_000;
        // 時計が先に飛んで未来の時刻が書かれた形。差だけを見ていたころは、
        // 差が大きな負の値になってそのタスクが二度と動かなかった。
        assert!(is_due(Some(now + 1), Every::Day, now));
        assert!(is_due(Some(now + 86_400 * 365), Every::Hour, now));
        assert!(is_due(Some(i64::MAX), Every::Day, now));
        // 前回がちょうど今なら、間隔を守る。
        assert!(!is_due(Some(now), Every::Day, now));
    }

    #[test]
    fn 秒の指定はあふれない() {
        let now = 100_000;
        // `seconds as i64` だと `u64::MAX` が `-1` になり、
        // 「ほぼ動かない」つもりが毎回動いていた。
        assert!(!is_due(Some(now), Every::Seconds(u64::MAX), now));
        assert!(!is_due(Some(0), Every::Seconds(u64::MAX), now));
        assert!(!is_due(
            Some(1),
            Every::Seconds(i64::MAX as u64 + 1),
            i64::MAX
        ));
        // 前回が i64::MIN でも引き算であふれない。
        assert!(is_due(Some(i64::MIN), Every::Day, now));
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
    #[should_panic(expected = "既に登録されています")]
    fn 同じ名前は登録できない() {
        // 名前は前回の時刻と錠の鍵。重ねると 2 件目が 1 件目の時刻を見て、
        // `Every::Minute` 以外は二度と動かなかった。
        let mut schedule = Schedule::new();
        schedule
            .job("掃除", Every::Hour, || async { Ok(()) })
            .job("掃除", Every::Day, || async { Ok(()) });
    }

    #[tokio::test]
    async fn パニックしても残りのタスクは動く() {
        let dir = temp_dir("panic");
        let mut schedule = Schedule::new();
        schedule
            .job("落ちる処理", Every::Hour, || async {
                panic!("わざと落ちる")
            })
            .job("次の処理", Every::Hour, || async { Ok(()) });

        // 以前は 1 件目のパニックで `schedule:run` 全体が落ち、2 件目は
        // 動かないまま時刻だけが残っていた（次の 1 分でも再試行されない）。
        let done = run_due_in(&dir, &schedule).await.unwrap();
        assert_eq!(done, vec!["次の処理".to_string()]);
        // 落ちたほうも時刻は残る（次の 1 分で繰り返し落ちないため）。
        assert!(read_last_run(&dir, &schedule.tasks()[0]).is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn 判定の時刻は処理の直前に取る() {
        let dir = temp_dir("fresh-now");
        let mut schedule = Schedule::new();
        schedule.job("1時間ごと", Every::Hour, || async { Ok(()) });
        let task = &schedule.tasks()[0];

        // 別の `schedule:run` が「いま」より後の時刻を書いた形（前の実行が
        // 長引いて、判定の時刻が古くなっていた）。先頭で 1 回だけ取った時刻で
        // 判定していたころは「前回が未来」と見て、警告つきで二重に動かした。
        let later = crate::support::time::now_seconds() + 5;
        write_last_run(&dir, task, later).unwrap();

        // 時刻を取り直すので、5 秒後の前回は「まだ早い」ではなく未来と見る。
        // ここで確かめたいのは、動かさないこと**ではなく**、前回の時刻が
        // 判定の瞬間の `now` と比べられること。
        assert!(
            claim(&dir, task).await.unwrap(),
            "未来の前回は動かす（時計ずれの扱い）"
        );
        // 書くのは判定の瞬間の時刻を分の頭に丸めた値。未来の時刻は残らない。
        let written = read_last_run(&dir, task).unwrap();
        let now = crate::support::time::now_seconds();
        assert!(written <= now, "未来の時刻は残らない: {written}");
        assert!(written > now - 120, "判定の瞬間の時刻: {written}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 覚える時刻は分の頭に丸める() {
        // 1_700_000_000 は秒が 20（分の頭 + 20 秒）。
        let now: i64 = 1_700_000_000;
        assert_eq!(now.rem_euclid(60), 20);

        // 間隔が 1 分以上のものは丸める。先行タスクにかかった時間を記録に
        // 積まないため（決定記録 #121）。
        assert_eq!(remembered_at(Every::Minutes(5), now), now - 20);
        assert_eq!(remembered_at(Every::Hour, now), now - 20);
        assert_eq!(remembered_at(Every::Day, now), now - 20);

        // 秒を指定したものと 1 分未満は丸めない（秒の意図を崩さない）。
        assert_eq!(remembered_at(Every::Seconds(30), now), now);
        assert_eq!(remembered_at(Every::Seconds(300), now), now);
        assert_eq!(remembered_at(Every::Minute, now), now);
    }

    #[test]
    fn 丸めればずれが積まない() {
        // cron は 10:00:00・10:05:00… に呼ぶ。先行タスクで 40 秒食われて
        // `claim` が 10:00:40 になった形。
        let late: i64 = 1_700_000_020;
        assert_eq!(late.rem_euclid(60), 40);
        let remembered = remembered_at(Every::Minutes(5), late);
        assert_eq!(remembered.rem_euclid(60), 0, "分の頭");

        // 丸めた値を覚えれば、5 分後の呼び出しでちょうど動く。
        assert!(is_due(
            Some(remembered),
            Every::Minutes(5),
            remembered + 300
        ));
        // そのままの秒を覚えると、経つのは 260 秒で許容（30 秒）を超え、
        // 1 回ぶん飛んで 6 分ごとになっていた。
        assert!(!is_due(Some(late), Every::Minutes(5), remembered + 300));
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
    async fn 改行入りの名前でも前回の時刻を読める() {
        let dir = temp_dir("newline");
        let mut schedule = Schedule::new();
        schedule.job("1行目\n2行目", Every::Day, || async { Ok(()) });

        // 1 回目は動く。
        assert_eq!(run_due_in(&dir, &schedule).await.unwrap().len(), 1);
        // 名前の改行で 2 行目がずれると、前回の時刻が読めず毎分走っていた。
        assert!(read_last_run(&dir, &schedule.tasks()[0]).is_some());
        assert!(run_due_in(&dir, &schedule).await.unwrap().is_empty());

        // 1 行目は名前（改行は空白に潰す）、2 行目が時刻。
        let raw = std::fs::read_to_string(dir.join(schedule.tasks()[0].state_file())).unwrap();
        assert_eq!(raw.lines().next(), Some("1行目 2行目"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 時刻は一時ファイル経由で置き換える() {
        let dir = temp_dir("atomic");
        let mut schedule = Schedule::new();
        schedule.job("1日ごとの処理", Every::Day, || async { Ok(()) });
        let task = &schedule.tasks()[0];

        // 途中で落ちた形（0 バイト）。読み戻しは `None` になり毎分走る。
        std::fs::write(dir.join(task.state_file()), "").unwrap();
        assert!(read_last_run(&dir, task).is_none());

        write_last_run(&dir, task, 1_700_000_000).unwrap();
        assert_eq!(read_last_run(&dir, task), Some(1_700_000_000));

        // 書きかけ（`+tmp`）が残らない。残ると掃除されずに溜まる。
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| *name != task.state_file())
            .collect();
        assert!(left.is_empty(), "残っている: {left:?}");

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

    #[tokio::test]
    async fn 錠を取れないタスクは動かさない() {
        let dir = temp_dir("lock");
        let mut schedule = Schedule::new();
        schedule.job("重なった処理", Every::Hour, || async { Ok(()) });

        // 別のプロセスが錠を持っている状態を作る。
        let held = crate::support::lock::FileLock::acquire(&lock_path(&dir, &schedule.tasks()[0]))
            .unwrap();

        // 動かさない。時刻も書かない（次の 1 分で動けるように）。
        assert!(run_due_in(&dir, &schedule).await.unwrap().is_empty());
        assert!(read_last_run(&dir, &schedule.tasks()[0]).is_none());

        // 錠が空けば動く。
        drop(held);
        assert_eq!(run_due_in(&dir, &schedule).await.unwrap().len(), 1);
        // 錠は手放したら残らない。
        assert!(!lock_path(&dir, &schedule.tasks()[0]).exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 錠の置き場所は覚えるファイルとぶつからない() {
        let mut schedule = Schedule::new();
        schedule.job("毎日の集計", Every::Day, || async { Ok(()) });
        let task = &schedule.tasks()[0];
        let dir = PathBuf::from("どこか");

        // 覚えるファイルは SHA-256 の16進なので、`+` を含む名前とは重ならない。
        assert!(!task.state_file().contains('+'));
        assert!(lock_path(&dir, task).starts_with(dir.join(LOCK_DIR)));
        assert_ne!(lock_path(&dir, task), dir.join(task.state_file()));
    }

    #[test]
    fn 置き場所はstorageの下() {
        assert!(state_dir().ends_with("schedule"));
        assert!(state_dir().starts_with(crate::paths::storage_path("")));
    }
}
