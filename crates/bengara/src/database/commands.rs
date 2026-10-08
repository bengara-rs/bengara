//! `migrate` と `db:seed` のコマンド。
//!
//! 本体のバイナリから呼ばれます（`cargo artisan migrate` は本体に中継されます）。

use super::migrator::{self, Migration, Migrator, Seeder};
use crate::error::{Error, Result};

/// DB のコマンドの一覧（ヘルプに出す）。
pub(crate) const COMMANDS: &[(&str, &str)] = &[
    (
        "migrate",
        "まだ実行していないマイグレーションを流す（--seed でシーダーも）",
    ),
    ("migrate:status", "実行済みかどうかを一覧にする"),
    (
        "migrate:rollback",
        "最後のバッチを巻き戻す（--step=2 で2バッチ分）",
    ),
    ("migrate:reset", "全部巻き戻す"),
    (
        "migrate:refresh",
        "全部巻き戻してから流し直す（--seed でシーダーも）",
    ),
    (
        "migrate:fresh",
        "表を全部消してから流し直す（--seed でシーダーも）",
    ),
    (
        "db:seed",
        "シーダーを流す（--class=DatabaseSeeder で1本だけ）",
    ),
    ("db:wipe", "表を全部消す"),
    (
        "migrate:unlock",
        "途中で落ちて残った実行中の札を外す（誰が取ったかを表示する）",
    ),
];

/// この名前が DB のコマンドか。
pub(crate) fn is_command(name: &str) -> bool {
    COMMANDS.iter().any(|(command, _)| *command == name)
}

/// コマンドを実行する。
pub(crate) fn run(
    command: &str,
    args: &[String],
    migrations: &'static [Migration],
    seeders: &'static [Seeder],
) -> Result<()> {
    let options = Options::parse(args)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(Error::Io)?;
    runtime.block_on(execute(command, &options, migrations, seeders))
}

/// 表を変えるコマンド。実行の前に札を取ります。
///
/// `migrate:status` は読むだけなので入れません。
/// `migrate:unlock` は札を外すためのものなので、当然入れません。
const NEEDS_LOCK: &[&str] = &[
    "migrate",
    "migrate:rollback",
    "migrate:reset",
    "migrate:refresh",
    "migrate:fresh",
    "db:seed",
    "db:wipe",
];

/// 中身が消えるコマンド。本番では `--force` が無いと止めます。
///
/// `migrate:reset` と `migrate:refresh` も入れます。`down` を流すので、
/// 名前に「消す」と書いていなくても中身は消えます。
const DESTRUCTIVE: &[&str] = &[
    "migrate:reset",
    "migrate:refresh",
    "migrate:fresh",
    "db:wipe",
];

async fn execute(
    command: &str,
    options: &Options,
    migrations: &[Migration],
    seeders: &[Seeder],
) -> Result<()> {
    let migrator = Migrator::connect(options.database.as_deref()).await?;

    // 複数のプロセスから同時に流さない（決定記録 #067）。
    let locked = NEEDS_LOCK.contains(&command);
    if locked {
        migrator.lock().await?;
    }
    let result = run_one(command, options, migrations, seeders, &migrator).await;
    if locked {
        // 失敗しても札は返す。返せなかったときは、外し方を伝える。
        if let Err(e) = migrator.unlock().await {
            eprintln!("警告: 札を外せませんでした（{e}）。`migrate:unlock` で外せます");
        }
    }
    result
}

async fn run_one(
    command: &str,
    options: &Options,
    migrations: &[Migration],
    seeders: &[Seeder],
    migrator: &Migrator,
) -> Result<()> {
    if DESTRUCTIVE.contains(&command) {
        // 腕ごとに書くと足し忘れるので、一覧で見る。
        guard_destructive(options)?;
    }
    match command {
        "migrate" => {
            report("実行した", migrator.run(migrations).await?);
            if options.seed {
                report(
                    "流した",
                    migrator::seed(seeders, options.class.as_deref()).await?,
                );
            }
        }
        "migrate:status" => {
            print_status(&migrator.status(migrations).await?);
        }
        "migrate:rollback" => {
            report(
                "巻き戻した",
                migrator.rollback(migrations, options.step).await?,
            );
        }
        "migrate:reset" => {
            report("巻き戻した", migrator.reset(migrations).await?);
        }
        "migrate:refresh" => {
            report("巻き戻した", migrator.reset(migrations).await?);
            report("実行した", migrator.run(migrations).await?);
            if options.seed {
                report(
                    "流した",
                    migrator::seed(seeders, options.class.as_deref()).await?,
                );
            }
        }
        "migrate:fresh" => {
            report("消した", migrator.drop_all_tables().await?);
            report("実行した", migrator.run(migrations).await?);
            if options.seed {
                report(
                    "流した",
                    migrator::seed(seeders, options.class.as_deref()).await?,
                );
            }
        }
        "db:seed" => {
            let done = migrator::seed(seeders, options.class.as_deref()).await?;
            if done.is_empty() {
                println!("シーダーがありません（database/seeders/ を確かめてください）");
            } else {
                report("流した", done);
            }
        }
        "db:wipe" => {
            report("消した", migrator.drop_all_tables().await?);
        }
        "migrate:unlock" => match migrator.lock_holder().await? {
            Some((owner, at)) => {
                migrator.unlock().await?;
                println!("札を外しました（{owner} が {at} に取ったもの）。");
            }
            None => println!("札は誰も持っていません。"),
        },
        other => {
            return Err(Error::msg(format!(
                "`{other}` は DB のコマンドではありません"
            )))
        }
    }
    Ok(())
}

/// 消す系のコマンドを本番で止める。
fn guard_destructive(options: &Options) -> Result<()> {
    if options.force || !crate::config_registry::app_config().is_production() {
        return Ok(());
    }
    Err(Error::msg(
        "APP_ENV=production では表を消すコマンドを実行しません。本当に消すなら --force を付けてください",
    ))
}

fn report(label: &str, items: Vec<String>) {
    if items.is_empty() {
        println!("{label}ものはありません（すべて最新です）");
        return;
    }
    for item in &items {
        println!("  {item}");
    }
    println!("{label}のは {} 件です", items.len());
}

fn print_status(rows: &[migrator::Status]) {
    if rows.is_empty() {
        println!("マイグレーションは1本もありません（database/migrations/ を確かめてください）");
        return;
    }
    let width = rows.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4);
    println!("{:<width$}  状態      バッチ", "名前");
    for row in rows {
        let state = match (row.batch, row.present) {
            (Some(_), true) => "実行済み",
            (None, true) => "未実行  ",
            (_, false) => "ファイルなし",
        };
        let batch = row
            .batch
            .map(|b| b.to_string())
            .unwrap_or_else(|| "-".to_string());
        println!("{:<width$}  {state}  {batch}", row.name);
    }
    let done = rows.iter().filter(|r| r.batch.is_some()).count();
    println!("\n{} 本中 {done} 本が実行済みです", rows.len());
}

/// コマンドに渡せる指定。
#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    /// 使う接続の名前（`--database=`）。
    database: Option<String>,
    /// 巻き戻すバッチの数（`--step=`）。
    step: u32,
    /// シーダーも流すか（`--seed`）。
    seed: bool,
    /// 本番で消す系を通すか（`--force`）。
    force: bool,
    /// 流すシーダーの名前（`--class=`）。
    class: Option<String>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let mut options = Self {
            step: 1,
            ..Self::default()
        };
        for arg in args {
            if let Some(value) = arg.strip_prefix("--database=") {
                options.database = Some(value.to_string());
            } else if let Some(value) = arg.strip_prefix("--class=") {
                options.class = Some(value.to_string());
            } else if let Some(value) = arg.strip_prefix("--step=") {
                options.step = value.parse().map_err(|_| {
                    Error::msg(format!("--step の値 `{value}` が数値ではありません"))
                })?;
            } else if arg == "--seed" {
                options.seed = true;
            } else if arg == "--force" {
                options.force = true;
            } else {
                return Err(Error::msg(format!(
                    "`{arg}` は受け付けません（使えるのは --database= / --class= / --step= / --seed / --force）"
                )));
            }
        }
        Ok(options)
    }
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
        assert_eq!(options.step, 1);
        assert!(!options.seed);
        assert_eq!(options.database, None);
    }

    #[test]
    fn 指定を読める() {
        let options = Options::parse(&args(&[
            "--seed",
            "--step=3",
            "--database=other",
            "--force",
        ]))
        .unwrap();
        assert!(options.seed);
        assert_eq!(options.step, 3);
        assert_eq!(options.database.as_deref(), Some("other"));
        assert!(options.force);
    }

    #[test]
    fn 知らない指定は断る() {
        assert!(Options::parse(&args(&["--nope"])).is_err());
        assert!(Options::parse(&args(&["--step=x"])).is_err());
    }

    #[test]
    fn 表を変えるコマンドは札を取る() {
        for command in NEEDS_LOCK {
            assert!(is_command(command), "{command} が一覧に無い");
        }
        // 読むだけのものは札を取らない。
        assert!(!NEEDS_LOCK.contains(&"migrate:status"));
        // 札を外すコマンド自身は札を取らない。
        assert!(!NEEDS_LOCK.contains(&"migrate:unlock"));
    }

    #[test]
    fn 中身が消えるコマンドは本番の保護を通る() {
        // `migrate:reset` と `migrate:refresh` は down を流すので中身が消える。
        for command in [
            "migrate:reset",
            "migrate:refresh",
            "migrate:fresh",
            "db:wipe",
        ] {
            assert!(DESTRUCTIVE.contains(&command), "{command} が一覧に無い");
            assert!(is_command(command), "{command} が一覧に無い");
        }
        // 足すだけのものは入れない。
        assert!(!DESTRUCTIVE.contains(&"migrate"));
        assert!(!DESTRUCTIVE.contains(&"migrate:rollback"));
        assert!(!DESTRUCTIVE.contains(&"migrate:status"));
    }

    #[test]
    fn 本番では_force_が無いと断る() {
        // `--force` を付けたときは、本番かどうかを見ずに通る。
        let forced = Options {
            force: true,
            ..Options::default()
        };
        guard_destructive(&forced).expect("--force なら通る");
    }

    #[test]
    fn 札を外すコマンドがヘルプに出る() {
        assert!(is_command("migrate:unlock"));
        assert!(COMMANDS
            .iter()
            .any(|(name, description)| *name == "migrate:unlock" && !description.is_empty()));
    }

    #[test]
    fn コマンドの名前を見分ける() {
        assert!(is_command("migrate"));
        assert!(is_command("db:seed"));
        assert!(!is_command("serve"));
        assert_eq!(COMMANDS.len(), 9);
    }
}
