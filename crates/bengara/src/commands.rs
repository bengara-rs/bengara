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
        "キューのジョブを処理する（--once / --tries=3 / --sleep=1 / --queue=名前）",
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
    let options = Options::parse(args)?;
    runtime()?.block_on(execute(name, &options, hooks))
}

/// 自作コマンドを実行する。
pub(crate) fn run_custom(name: &str, args: &[String], hooks: Hooks) -> Result<()> {
    let commands = (hooks.commands)();
    let command = crate::console::find(commands, name)
        .ok_or_else(|| Error::msg(format!("`{name}` というコマンドはありません")))?;
    let args = args.to_vec();
    runtime()?.block_on((command.handle)(args))
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
            let dir = crate::paths::storage_path("framework").join("cache");
            let removed = crate::cache::store::sweep_expired(&dir)?;
            println!(
                "期限切れのキャッシュを {removed} 件消しました（{}）",
                dir.display()
            );
        }
        "queue:work" => {
            let stop = stop_signal();
            let worker = crate::queue::WorkerOptions {
                queue: options.queue.clone(),
                tries: options.tries,
                sleep: options.sleep,
                once: options.once,
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
            let done = crate::schedule::run_due(&schedule).await?;
            if done.is_empty() {
                println!("いま動かすものはありません。");
            } else {
                println!("{} 件動かしました。", done.len());
            }
        }
        "schedule:list" => {
            let schedule = build_schedule(hooks);
            crate::schedule::print_list(&schedule).await?;
        }
        "lang:list" => {
            crate::lang::install((hooks.lang)());
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

/// Ctrl+C を受け取ったら真になる札。
///
/// `queue:work` は、いま処理中のジョブを終えてから止まります。
fn stop_signal() -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let handle = stop.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            handle.store(true, Ordering::Relaxed);
        }
    });
    stop
}

async fn print_failed() -> Result<()> {
    let rows = crate::queue::failed_list().await?;
    if rows.is_empty() {
        println!("諦めたジョブはありません。");
        return Ok(());
    }
    println!("ID   ジョブ               失敗した時刻          理由");
    for (id, job, failed_at, error) in &rows {
        let error = error.lines().next().unwrap_or("");
        println!("{id:<4} {job:<20} {failed_at:<20} {error}");
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
    id: Option<i64>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            queue: crate::queue::DEFAULT_QUEUE.to_string(),
            tries: 3,
            sleep: 1,
            once: false,
            id: None,
        }
    }
}

impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let mut options = Self::default();
        for arg in args {
            if let Some(value) = arg.strip_prefix("--queue=") {
                options.queue = value.to_string();
            } else if let Some(value) = arg.strip_prefix("--tries=") {
                options.tries = parse_number(value, "--tries")?;
            } else if let Some(value) = arg.strip_prefix("--sleep=") {
                options.sleep = parse_number(value, "--sleep")?;
            } else if let Some(value) = arg.strip_prefix("--id=") {
                options.id = Some(parse_number(value, "--id")?);
            } else if arg == "--once" {
                options.once = true;
            } else {
                return Err(Error::msg(format!(
                    "`{arg}` は受け付けません（使えるのは --queue= / --tries= / --sleep= / --id= / --once）"
                )));
            }
        }
        if options.tries == 0 {
            return Err(Error::msg("--tries は 1 以上にしてください"));
        }
        Ok(options)
    }
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
        let options = Options::parse(&[]).unwrap();
        assert_eq!(options.queue, "default");
        assert_eq!(options.tries, 3);
        assert_eq!(options.sleep, 1);
        assert!(!options.once);
        assert_eq!(options.id, None);
    }

    #[test]
    fn 指定を読める() {
        let options = Options::parse(&args(&[
            "--queue=mail",
            "--tries=5",
            "--sleep=3",
            "--once",
            "--id=12",
        ]))
        .unwrap();
        assert_eq!(options.queue, "mail");
        assert_eq!(options.tries, 5);
        assert_eq!(options.sleep, 3);
        assert!(options.once);
        assert_eq!(options.id, Some(12));
    }

    #[test]
    fn 知らない指定と0回は断る() {
        assert!(Options::parse(&args(&["--nope"])).is_err());
        assert!(Options::parse(&args(&["--tries=x"])).is_err());
        assert!(Options::parse(&args(&["--tries=0"])).is_err());
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
}
