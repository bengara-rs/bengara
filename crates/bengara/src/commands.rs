//! 周辺機能のコマンド（`cache:clear` / `queue:work` / `schedule:run` など）。
//!
//! DB のコマンド（`migrate` 系）は `database/commands.rs` にあります。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::Hooks;

/// このファイルが受け持つコマンドの一覧（ヘルプに出す）。
pub(crate) const COMMANDS: &[(&str, &str)] = &[
    ("cache:clear", "キャッシュを全部消す"),
    ("cache:prune", "期限切れのキャッシュだけ消す"),
    (
        "queue:work",
        "キューのジョブを処理する（--once / --tries=3 / --sleep=1 / --timeout=60 / --retry-after=90 / --queue=名前）",
    ),
    ("queue:failed", "諦めたジョブの一覧"),
    ("queue:retry", "諦めたジョブをキューへ戻す（--id=1 で1件）"),
    ("queue:flush", "諦めたジョブの記録を捨てる"),
    ("schedule:run", "いま動かすべき定期処理を動かす"),
    ("schedule:list", "定期処理の一覧"),
    ("lang:list", "読み込まれている言語の一覧"),
];

/// この名前を受け持つか。
pub(crate) fn is_command(name: &str) -> bool {
    COMMANDS.iter().any(|(command, _)| *command == name)
}

/// コマンドを実行する。
pub(crate) fn run(name: &str, args: &[String], hooks: Hooks) -> Result<()> {
    let options = Options::parse(name, args)?;
    let runtime = runtime()?;
    let result = runtime.block_on(execute(name, &options, hooks));
    if name == "queue:work" {
        // 打ち切っても止まらないジョブがあると、ワーカーはエラーで終わります。
        // 普通に `drop` すると、そのジョブのスレッドが終わるまで待ってしまい、
        // プロセスが終わりません。待たずに手放します（`queue/worker.rs` の `run_one`）。
        runtime.shutdown_background();
    }
    result
}

/// 自作コマンドが終わったあと、残った裏の処理を待つ上限。
///
/// 普通に `drop` すると、`spawn_blocking` で始めて待たなかった処理が終わるまで
/// 待つので、止まらない処理が 1 つ残るとプロセスが終わりません。
/// かといって待たずに手放すと、書きかけのファイルなどが途中で切れます。
/// 少しだけ待ってから手放します（Laravel でも `handle` が返れば終わります）。
const CUSTOM_SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(1);

/// 自作コマンドを実行する。
pub(crate) fn run_custom(name: &str, args: &[String], hooks: Hooks) -> Result<()> {
    let commands = (hooks.commands)();
    let command = crate::console::find(commands, name)
        .ok_or_else(|| Error::msg(format!("`{name}` というコマンドはありません")))?;
    let args = args.to_vec();
    let runtime = runtime()?;
    let result = runtime.block_on((command.handle)(args));
    runtime.shutdown_timeout(CUSTOM_SHUTDOWN_GRACE);
    result
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(Error::Io)
}

async fn execute(name: &str, options: &Options, hooks: Hooks) -> Result<()> {
    match name {
        "cache:clear" => {
            crate::cache::Cache::flush().await?;
            println!("キャッシュを消しました。");
        }
        "cache:prune" => {
            // 置き場所の都合はキャッシュ側に任せる。ここでパスを組み立てると、
            // `install_store` で場所を変えたアプリで無関係なディレクトリを見ます。
            match crate::cache::Cache::prune().await? {
                Some(removed) => match crate::cache::Cache::location() {
                    Some(place) => {
                        println!("期限切れのキャッシュを {removed} 件消しました（{place}）")
                    }
                    None => println!("期限切れのキャッシュを {removed} 件消しました。"),
                },
                None => println!(
                    "このキャッシュには期限切れの掃除がありません（CACHE_DRIVER を確かめてください）"
                ),
            }
        }
        "queue:work" => {
            let stop = stop_signal();
            let worker = crate::queue::WorkerOptions {
                queue: options.queue.clone(),
                tries: options.tries,
                sleep: options.sleep,
                once: options.once,
                retry_after: options.retry_after,
                timeout: options.timeout,
                // ジョブごとの打ち切り（`config/queue.rs`）。置いていなければ空。
                timeouts: crate::try_config::<crate::queue::QueueConfig>()
                    .map(|config| config.timeouts.clone())
                    .unwrap_or_default(),
            };
            crate::queue::work((hooks.jobs)(), &worker, stop).await?;
        }
        "queue:failed" => print_failed().await?,
        "queue:retry" => {
            let moved = crate::queue::Queue::retry(options.id).await?;
            println!("{moved} 件をキューへ戻しました。");
        }
        "queue:flush" => {
            let removed = crate::queue::Queue::flush_failed().await?;
            println!("諦めたジョブの記録を {removed} 件捨てました。");
        }
        "schedule:run" => {
            let schedule = build_schedule(hooks);
            if schedule.is_empty() {
                println!(
                    "登録されている定期処理はありません（routes/console.rs を確かめてください）"
                );
                return Ok(());
            }
            let outcome = crate::schedule::run_due(&schedule).await?;
            if let Some(line) = schedule_summary(&outcome) {
                println!("{line}");
            }
            // 失敗が 1 件でもあれば、残りを動かし終えたいま、終了コードを
            // 0 以外にする。cron や監視の側で拾えるようにするため。
            if let Some(error) = outcome.failure() {
                return Err(error);
            }
        }
        "schedule:list" => {
            let schedule = build_schedule(hooks);
            crate::schedule::print_list(&schedule);
        }
        "lang:list" => {
            // 言語の表は `kernel_impl::main` が起動のときに固定してある。
            let available = crate::lang::Lang::available();
            if available.is_empty() {
                println!("言語ファイルがありません（resources/lang/*.toml を置いてください）");
            } else {
                println!("いまの言語: {}", crate::lang::Lang::current());
                for name in available {
                    println!("  {name}");
                }
            }
        }
        other => return Err(Error::msg(format!("`{other}` は受け持っていません"))),
    }
    Ok(())
}

/// `routes/console.rs` の `schedule()` を呼んで一覧を作る。
fn build_schedule(hooks: Hooks) -> crate::schedule::Schedule {
    let mut schedule = crate::schedule::Schedule::new();
    (hooks.schedule)(&mut schedule);
    schedule
}

/// `schedule:run` の最後に出す 1 行。出すものが無ければ `None`。
///
/// 失敗したタスクがあるときは「いま動かすものはありません」と言いません。
/// 動かそうとして失敗したので、その文は事実と違います（失敗の件数は、
/// このあと返すエラーの文に出ます）。
fn schedule_summary(outcome: &crate::schedule::Outcome) -> Option<String> {
    if !outcome.done.is_empty() {
        return Some(format!("{} 件動かしました。", outcome.done.len()));
    }
    if outcome.failed.is_empty() {
        return Some("いま動かすものはありません。".to_string());
    }
    None
}

/// 停止の合図を受け取ったら真になる札。
///
/// `queue:work` は、いま処理中のジョブを終えてから止まります。
///
/// **見張りは `serve` と同じものを使います**（`server::watch_shutdown`）。
/// ここで Ctrl+C（SIGINT）だけを見ていたころは、`systemctl stop` や
/// `docker stop` が送る SIGTERM を受け取れず、処理中のジョブごと即死して
/// いました。切れたジョブは `retry_after` の後に取り直され、試行回数が 1 つ減ります。
///
/// **2 回目も受け取ります。** 1 回待って終わると、2 回目が握り潰されて
/// しまい、ジョブが固まったときに `kill` が必要になります。
/// 2 回目は待たずに落とします（終了コードは 130。Ctrl+C の慣わしです）。
fn stop_signal() -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let handle = stop.clone();
    tokio::spawn(crate::server::watch_shutdown(move || {
        handle.store(true, Ordering::Relaxed);
        println!(
            "\n停止の合図を受けました。いまのジョブを終えてから止まります\
             （もう一度合図を送ると強制終了します。Ctrl+C なら、もう一度押します）。"
        );
    }));
    stop
}

/// `queue:failed` の表の桁幅（ジョブ名、失敗した時刻）。
///
/// **文字数ではなく端末での幅で数えます。** `{job:<20}` は文字数で詰めるので、
/// ジョブ名に日本語が入ると表が崩れます（`schedule::print_list` と同じ考え方）。
fn failed_widths(rows: &[(i64, String, String, String)]) -> (usize, usize) {
    use crate::support::text;

    let job = rows
        .iter()
        .map(|(_, job, _, _)| text::width(job))
        .max()
        .unwrap_or(0)
        .max(text::width("ジョブ"));
    let time = rows
        .iter()
        .map(|(_, _, failed_at, _)| text::width(failed_at))
        .max()
        .unwrap_or(0)
        .max(text::width("失敗した時刻"));
    (job, time)
}

async fn print_failed() -> Result<()> {
    let rows = crate::queue::failed_list().await?;
    if rows.is_empty() {
        println!("諦めたジョブはありません。");
        return Ok(());
    }
    use crate::support::text;
    let (job_width, time_width) = failed_widths(&rows);

    println!(
        "ID   {}  {}  理由",
        text::pad("ジョブ", job_width),
        text::pad("失敗した時刻", time_width)
    );
    for (id, job, failed_at, error) in &rows {
        let error = error.lines().next().unwrap_or("");
        println!(
            "{id:<4} {}  {}  {error}",
            text::pad(job, job_width),
            text::pad(failed_at, time_width)
        );
    }
    println!("\n{} 件", rows.len());
    Ok(())
}

/// コマンドに渡せる指定。
#[derive(Debug, PartialEq, Eq)]
struct Options {
    queue: String,
    tries: u32,
    sleep: u64,
    once: bool,
    retry_after: i64,
    /// `queue:work` が 1 本のジョブを待つ上限（秒）。0 は無制限。
    timeout: u64,
    id: Option<i64>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            queue: crate::queue::DEFAULT_QUEUE.to_string(),
            tries: 3,
            sleep: 1,
            once: false,
            retry_after: crate::queue::DEFAULT_RETRY_AFTER,
            timeout: crate::queue::DEFAULT_TIMEOUT,
            id: None,
        }
    }
}

/// そのコマンドが受け付ける旗。
///
/// **コマンドごとに分けます。** 全コマンドで共通にすると、
/// `cache:clear --tries=9` のような書き間違いが黙って通ります。
/// 値を取る旗は `=` まで書いておきます。
fn allowed_flags(command: &str) -> &'static [&'static str] {
    match command {
        "queue:work" => &[
            "--queue=",
            "--tries=",
            "--sleep=",
            "--timeout=",
            "--retry-after=",
            "--once",
        ],
        "queue:retry" => &["--id="],
        // 残りは旗を取りません。
        _ => &[],
    }
}

impl Options {
    fn parse(command: &str, args: &[String]) -> Result<Self> {
        let allowed = allowed_flags(command);
        let mut options = Self::default();

        for arg in args {
            // `--tries=5` は `--tries=`、`--once` は `--once` として見分ける。
            let flag = match arg.split_once('=') {
                Some((name, _)) => format!("{name}="),
                None => arg.clone(),
            };
            if !allowed.contains(&flag.as_str()) {
                return Err(Error::msg(format!(
                    "`{command}` は `{arg}` を受け付けません（{}）",
                    describe_flags(allowed)
                )));
            }

            if let Some(value) = arg.strip_prefix("--queue=") {
                options.queue = value.to_string();
            } else if let Some(value) = arg.strip_prefix("--tries=") {
                options.tries = parse_number(value, "--tries")?;
            } else if let Some(value) = arg.strip_prefix("--sleep=") {
                options.sleep = parse_number(value, "--sleep")?;
            } else if let Some(value) = arg.strip_prefix("--timeout=") {
                // 0 は「無制限」なので断らない（Laravel の `--timeout=0` と同じ）。
                options.timeout = parse_number(value, "--timeout")?;
            } else if let Some(value) = arg.strip_prefix("--retry-after=") {
                options.retry_after = parse_number(value, "--retry-after")?;
            } else if let Some(value) = arg.strip_prefix("--id=") {
                options.id = Some(parse_number(value, "--id")?);
            } else if arg == "--once" {
                options.once = true;
            }
        }

        if options.tries == 0 {
            return Err(Error::msg("--tries は 1 以上にしてください"));
        }
        if options.retry_after <= 0 {
            return Err(Error::msg("--retry-after は 1 以上にしてください"));
        }
        Ok(options)
    }
}

/// 旗も引数も取らないコマンドの引数を確かめる。1 つでもあれば断ります。
///
/// このファイルの外のコマンド（`route:list` / `session:gc` / `storage:init` /
/// `about`）が使います。以前は引数を見ておらず、`route:list --json` や
/// `about --verbose` のような存在しない旗が黙って通っていました。
///
/// **仕組みは [`allowed_flags`] の許可表と同じです。** 表に無い名前は
/// 「旗を取らない」扱いなので、同じ文面で断れます。
pub(crate) fn no_flags(command: &str, args: &[String]) -> Result<()> {
    debug_assert!(
        allowed_flags(command).is_empty(),
        "`{command}` は旗を取るコマンドです。`Options::parse` を使ってください"
    );
    Options::parse(command, args).map(|_| ())
}

/// 受け付ける旗を人が読む形にする。
fn describe_flags(allowed: &[&str]) -> String {
    if allowed.is_empty() {
        return "このコマンドは旗を取りません".to_string();
    }
    format!("使えるのは {}", allowed.join(" / "))
}

fn parse_number<T: std::str::FromStr>(value: &str, name: &str) -> Result<T> {
    value
        .parse()
        .map_err(|_| Error::msg(format!("{name} の値 `{value}` が数値ではありません")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn 既定の指定() {
        let options = Options::parse("queue:work", &[]).unwrap();
        assert_eq!(options.queue, "default");
        assert_eq!(options.tries, 3);
        assert_eq!(options.sleep, 1);
        assert!(!options.once);
        assert_eq!(options.retry_after, 90);
        assert_eq!(options.id, None);
    }

    #[test]
    fn queue_workのtimeoutを読める() {
        // 既定は Laravel と同じ 60 秒。
        let options = Options::parse("queue:work", &[]).unwrap();
        assert_eq!(options.timeout, 60);

        let options = Options::parse("queue:work", &args(&["--timeout=120"])).unwrap();
        assert_eq!(options.timeout, 120);
        // 0 は無制限。`--tries=0` と違って断らない。
        let options = Options::parse("queue:work", &args(&["--timeout=0"])).unwrap();
        assert_eq!(options.timeout, 0);

        // 数でない値と負の値は断る。ほかのコマンドでは受け付けない。
        assert!(Options::parse("queue:work", &args(&["--timeout=x"])).is_err());
        assert!(Options::parse("queue:work", &args(&["--timeout=-1"])).is_err());
        assert!(Options::parse("queue:retry", &args(&["--timeout=5"])).is_err());
    }

    #[test]
    fn 指定を読める() {
        let options = Options::parse(
            "queue:work",
            &args(&[
                "--queue=mail",
                "--tries=5",
                "--sleep=3",
                "--retry-after=300",
                "--once",
            ]),
        )
        .unwrap();
        assert_eq!(options.queue, "mail");
        assert_eq!(options.tries, 5);
        assert_eq!(options.sleep, 3);
        assert_eq!(options.retry_after, 300);
        assert!(options.once);

        let options = Options::parse("queue:retry", &args(&["--id=12"])).unwrap();
        assert_eq!(options.id, Some(12));
    }

    #[test]
    fn 知らない指定と0回は断る() {
        assert!(Options::parse("queue:work", &args(&["--nope"])).is_err());
        assert!(Options::parse("queue:work", &args(&["--tries=x"])).is_err());
        assert!(Options::parse("queue:work", &args(&["--tries=0"])).is_err());
        assert!(Options::parse("queue:work", &args(&["--retry-after=0"])).is_err());
    }

    #[test]
    fn 旗はコマンドごとに分かれている() {
        // 前は全コマンド共通だったので、これが黙って通っていた。
        let error = Options::parse("cache:clear", &args(&["--tries=9"]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("cache:clear"), "{error}");
        assert!(error.contains("旗を取りません"), "{error}");

        // `--id=` は queue:retry だけ。
        assert!(Options::parse("queue:retry", &args(&["--id=1"])).is_ok());
        assert!(Options::parse("queue:work", &args(&["--id=1"])).is_err());
        assert!(Options::parse("queue:retry", &args(&["--once"])).is_err());

        // 旗を取らないコマンドは、旗が無ければ通る。
        assert!(Options::parse("cache:prune", &[]).is_ok());
    }

    #[test]
    fn 諦めたジョブの表は文字の幅でそろえる() {
        use crate::support::text;

        let rows = vec![
            (
                1i64,
                "日報の送信".to_string(),
                "2024-01-01 00:00:00".to_string(),
                "理由".to_string(),
            ),
            (
                2i64,
                "SendWelcome".to_string(),
                "2024-01-02 00:00:00".to_string(),
                "理由".to_string(),
            ),
        ];
        let (job, time) = failed_widths(&rows);

        // `日報の送信` は 10 文字分。`SendWelcome` は 11 文字分。
        assert_eq!(job, 11);
        assert_eq!(time, 19);
        // そろえた後の幅が同じになる（`{job:<20}` だとここがずれる）。
        assert_eq!(text::width(&text::pad(&rows[0].1, job)), job);
        assert_eq!(text::width(&text::pad(&rows[1].1, job)), job);

        // 1 件も無いときは見出しの幅になる。
        let (job, time) = failed_widths(&[]);
        assert_eq!(job, text::width("ジョブ"));
        assert_eq!(time, text::width("失敗した時刻"));
    }

    #[test]
    fn コマンドの名前を見分ける() {
        assert!(is_command("cache:clear"));
        assert!(is_command("queue:work"));
        assert!(is_command("schedule:run"));
        assert!(!is_command("migrate"));
        assert!(!is_command("serve"));
        assert_eq!(COMMANDS.len(), 9);
    }

    #[test]
    fn 旗を取らないコマンドは旗も余分な引数も断る() {
        // 以前は引数を見ておらず、存在しない旗が黙って通っていた。
        for command in ["route:list", "session:gc", "storage:init", "about"] {
            assert!(no_flags(command, &[]).is_ok(), "{command}");

            let error = no_flags(command, &args(&["--json"]))
                .unwrap_err()
                .to_string();
            assert!(error.contains(command), "{error}");
            assert!(error.contains("--json"), "{error}");
            assert!(error.contains("旗を取りません"), "{error}");

            // 旗の形をしていない余分な引数も同じ。
            let error = no_flags(command, &args(&["extra"]))
                .unwrap_err()
                .to_string();
            assert!(error.contains("extra"), "{error}");
            // ほかのコマンドの旗も通さない。
            assert!(no_flags(command, &args(&["--force"])).is_err());
            assert!(no_flags(command, &args(&["--once"])).is_err());
        }
    }

    /// テスト用の結果。
    fn outcome(done: &[&str], failed: &[&str], busy: &[&str]) -> crate::schedule::Outcome {
        crate::schedule::Outcome {
            done: args(done),
            failed: args(failed),
            busy: args(busy),
        }
    }

    #[test]
    fn 定期処理の最後の1行() {
        assert_eq!(
            schedule_summary(&outcome(&["a", "b"], &[], &[])).as_deref(),
            Some("2 件動かしました。")
        );
        assert_eq!(
            schedule_summary(&outcome(&[], &[], &[])).as_deref(),
            Some("いま動かすものはありません。")
        );
        // 錠が取れずに飛ばしただけなら、これまでどおり。
        assert_eq!(
            schedule_summary(&outcome(&[], &[], &["a"])).as_deref(),
            Some("いま動かすものはありません。")
        );
        // 成功と失敗が混ざったら、成功した件数だけを言う。
        assert_eq!(
            schedule_summary(&outcome(&["a"], &["b"], &[])).as_deref(),
            Some("1 件動かしました。")
        );
        // 失敗だけのときは「動かすものはありません」と言わない。
        assert_eq!(schedule_summary(&outcome(&[], &["a"], &[])), None);
    }

    #[test]
    fn 定期処理の失敗は終了コードに出す() {
        // `run` はこのエラーをそのまま返し、`main` が終了コード 1 で終わる。
        assert!(outcome(&["a"], &[], &[]).failure().is_none());
        assert!(
            outcome(&[], &[], &["a"]).failure().is_none(),
            "錠は失敗ではない"
        );
        let error = outcome(&["a"], &["b"], &[]).failure().unwrap().to_string();
        assert!(error.contains("1 件失敗"), "{error}");
        assert!(error.contains('b'), "{error}");
    }
}
