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

async fn execute(
    command: &str,
    options: &Options,
    migrations: &[Migration],
    seeders: &[Seeder],
) -> Result<()> {
    let migrator = Migrator::connect(options.database.as_deref()).await?;

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
            guard_destructive(options)?;
            report("消した表", migrator.drop_all_tables().await?);
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
            guard_destructive(options)?;
            report("消した表", migrator.drop_all_tables().await?);
        }
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
    fn コマンドの名前を見分ける() {
        assert!(is_command("migrate"));
        assert!(is_command("db:seed"));
        assert!(!is_command("serve"));
        assert_eq!(COMMANDS.len(), 8);
    }
}
