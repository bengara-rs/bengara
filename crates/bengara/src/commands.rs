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
        "キューのジョブを処理する（--once / --tries=3 / --sleep=1 / --retry-after=90 / --queue=名前）",
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
            crate::schedule::print_list(&schedule);
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
    retry_after: i64,
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
    fn コマンドの名前を見分ける() {
        assert!(is_command("cache:clear"));
        assert!(is_command("queue:work"));
        assert!(is_command("schedule:run"));
        assert!(!is_command("migrate"));
        assert!(!is_command("serve"));
        assert_eq!(COMMANDS.len(), 9);
    }
}
