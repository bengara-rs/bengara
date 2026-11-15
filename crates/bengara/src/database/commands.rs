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
    // つなぐ前に見る。効かない指定のまま表を消したり流したりしないため。
    options.check(command)?;
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
/// `migrate:rollback` と `migrate:reset` と `migrate:refresh` も入れます。
/// `down` を流すので、名前に「消す」と書いていなくても中身は消えます。
const DESTRUCTIVE: &[&str] = &[
    "migrate:rollback",
    "migrate:reset",
    "migrate:refresh",
    "migrate:fresh",
    "db:wipe",
];

/// コマンドごとに使える指定。ここに無い指定はエラーにします。
///
/// 以前はどのコマンドも全部の指定を受けていました。`migrate:refresh --step=2` は
/// 黙って全部を巻き戻し、`migrate:status --seed` は何も流さずに成功していました。
/// 「知らない指定はエラー」と同じ考え方で、**効かない指定もエラー**にします。
///
/// `migrate` と `db:seed` の `--force` は受けるだけです。Laravel では本番で流すのに
/// 要る指定で、デプロイの手順に `migrate --force` と書く習慣があります。bengara は
/// 足すだけのコマンドを本番でも止めないので、付けても付けなくても同じ動きです。
const ALLOWED: &[(&str, &[&str])] = &[
    ("migrate", &["--database", "--seed", "--class", "--force"]),
    ("migrate:status", &["--database"]),
    ("migrate:rollback", &["--database", "--step", "--force"]),
    ("migrate:reset", &["--database", "--force"]),
    (
        "migrate:refresh",
        &["--database", "--seed", "--class", "--force"],
    ),
    (
        "migrate:fresh",
        &["--database", "--seed", "--class", "--force"],
    ),
    ("db:seed", &["--database", "--class", "--force"]),
    ("db:wipe", &["--database", "--force"]),
    ("migrate:unlock", &["--database"]),
];

/// シーダーを、既定ではない接続に向けて流そうとしていたら止める。
///
/// シーダーの中の `DB::table(..)` は**既定の接続**に書きます。`--database=` は
/// マイグレーションの流し先しか変えません。以前は
/// `migrate:fresh --database=pgsql --seed` が pgsql を作り直したうえで、
/// シーダーを黙って既定のデータベースに流していました。
///
/// つなぐ前・表を消す前に呼びます。
fn guard_seed_connection(command: &str, options: &Options, default: &str) -> Result<()> {
    let seeds = command == "db:seed" || options.seed;
    let Some(name) = options.database.as_deref() else {
        return Ok(());
    };
    if !seeds || name == default {
        return Ok(());
    }
    Err(Error::msg(format!(
        "シーダーは既定の接続（`{default}`）に書きます。`--database={name}` には流せません。\n\
         シーダーの中の `DB::table(..)` が既定の接続を使うためです。\
         このまま流すと、`{name}` ではなく `{default}` に書いてしまいます。\n\
         `{name}` に流すときは、`DB_CONNECTION={name}` にして `--database=` を外してください。\
         何も実行していません。"
    )))
}

async fn execute(
    command: &str,
    options: &Options,
    migrations: &[Migration],
    seeders: &[Seeder],
) -> Result<()> {
    guard_seed_connection(command, options, &super::database_config().default)?;
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
        // 札の表が無ければ作らない（読むだけで終わる）。
        "migrate:unlock" => match migrator.clear_lock().await? {
            Some((owner, at)) => {
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
    /// 実際に書かれた指定の名前（`--step` など）。コマンドごとの確かめに使います。
    ///
    /// 値だけでは「書かれたか」が分かりません（`--step=1` は既定値と同じ）。
    given: Vec<&'static str>,
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
                options.given.push("--database");
            } else if let Some(value) = arg.strip_prefix("--class=") {
                options.class = Some(value.to_string());
                options.given.push("--class");
            } else if let Some(value) = arg.strip_prefix("--step=") {
                options.step = value.parse().map_err(|_| {
                    Error::msg(format!("--step の値 `{value}` が数値ではありません"))
                })?;
                options.given.push("--step");
            } else if arg == "--seed" {
                options.seed = true;
                options.given.push("--seed");
            } else if arg == "--force" {
                options.force = true;
                options.given.push("--force");
            } else {
                return Err(Error::msg(format!(
                    "`{arg}` は受け付けません（使えるのは --database= / --class= / --step= / --seed / --force）"
                )));
            }
        }
        Ok(options)
    }

    /// このコマンドで使えない指定が書かれていないか確かめる。
    ///
    /// 一覧（[`ALLOWED`]）に無いコマンドは見ません。あとで
    /// 「DB のコマンドではありません」のエラーになります。
    fn check(&self, command: &str) -> Result<()> {
        let Some((_, allowed)) = ALLOWED.iter().find(|(name, _)| *name == command) else {
            return Ok(());
        };
        if let Some(flag) = self.given.iter().find(|flag| !allowed.contains(*flag)) {
            return Err(Error::msg(format!(
                "`{command}` に {flag} は使えません（使えるのは {}）。何も実行していません",
                allowed.join(" / ")
            )));
        }
        // `db:seed` 以外は、`--seed` が無いとシーダーを流しません。
        if self.class.is_some() && !self.seed && command != "db:seed" {
            return Err(Error::msg(format!(
                "`{command}` の --class は --seed と一緒に使います\
                 （--seed が無いとシーダーを流さないので効きません）。何も実行していません"
            )));
        }
        Ok(())
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
        // 書かれた指定の名前を、書かれた順に覚える。
        assert_eq!(options.given, ["--seed", "--step", "--database", "--force"]);
    }

    #[test]
    fn 知らない指定は断る() {
        assert!(Options::parse(&args(&["--nope"])).is_err());
        assert!(Options::parse(&args(&["--step=x"])).is_err());
    }

    #[test]
    fn コマンドごとに使えない指定は断る() {
        let check =
            |command: &str, list: &[&str]| Options::parse(&args(list)).unwrap().check(command);

        // 以前は黙って受けていた組み合わせ。
        let error = check("migrate:refresh", &["--step=2"])
            .unwrap_err()
            .to_string();
        assert!(error.contains("migrate:refresh"), "{error}");
        assert!(error.contains("--step"), "{error}");
        assert!(check("migrate:status", &["--seed"]).is_err());
        assert!(check("migrate:status", &["--force"]).is_err());
        assert!(
            check("migrate", &["--step=1"]).is_err(),
            "既定値と同じでも断る"
        );
        assert!(check("migrate:reset", &["--seed"]).is_err());
        assert!(check("db:wipe", &["--class=X"]).is_err());
        assert!(check("db:seed", &["--seed"]).is_err());
        assert!(check("migrate:unlock", &["--force"]).is_err());

        // `--class` は、シーダーを流すときだけ。
        let error = check("migrate", &["--class=UserSeeder"])
            .unwrap_err()
            .to_string();
        assert!(error.contains("--seed"), "{error}");
        assert!(check("migrate:fresh", &["--class=UserSeeder"]).is_err());
        check("migrate", &["--seed", "--class=UserSeeder"]).expect("--seed と一緒なら通る");
        check("db:seed", &["--class=UserSeeder"]).expect("db:seed は --seed が要らない");

        // 使える組み合わせ。
        check("migrate", &[]).unwrap();
        check("migrate", &["--database=pgsql", "--seed", "--force"]).unwrap();
        check("migrate:status", &["--database=pgsql"]).unwrap();
        check("migrate:rollback", &["--step=2", "--force"]).unwrap();
        check("migrate:refresh", &["--seed", "--force"]).unwrap();
        check("migrate:fresh", &["--seed", "--class=X", "--force"]).unwrap();
        check("db:wipe", &["--force", "--database=pgsql"]).unwrap();
        check("migrate:unlock", &["--database=pgsql"]).unwrap();
    }

    #[test]
    fn 使える指定の表は全部のコマンドを持つ() {
        // コマンドを足したときに表を足し忘れると、そのコマンドだけ何でも受ける。
        for (command, _) in COMMANDS {
            assert!(
                ALLOWED.iter().any(|(name, _)| name == command),
                "{command} が ALLOWED に無い"
            );
        }
        assert_eq!(ALLOWED.len(), COMMANDS.len());
        // 中身が消えるコマンドは、本番で通すための --force を受ける。
        for command in DESTRUCTIVE {
            let (_, allowed) = ALLOWED.iter().find(|(name, _)| name == command).unwrap();
            assert!(allowed.contains(&"--force"), "{command}");
        }
    }

    #[test]
    fn 既定ではない接続にシーダーは流さない() {
        let parse = |list: &[&str]| Options::parse(&args(list)).unwrap();

        // 以前は pgsql を作り直して、シーダーを黙って既定の接続に流していた。
        let error = guard_seed_connection(
            "migrate:fresh",
            &parse(&["--database=pgsql", "--seed"]),
            "sqlite",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("pgsql"), "{error}");
        assert!(error.contains("sqlite"), "{error}");
        assert!(error.contains("DB_CONNECTION=pgsql"), "{error}");
        assert!(guard_seed_connection("db:seed", &parse(&["--database=pgsql"]), "sqlite").is_err());
        assert!(guard_seed_connection(
            "migrate",
            &parse(&["--database=pgsql", "--seed"]),
            "sqlite"
        )
        .is_err());

        // 既定の接続を名指ししたとき、--database が無いとき、シーダーを流さないときは通る。
        guard_seed_connection("db:seed", &parse(&["--database=sqlite"]), "sqlite").unwrap();
        guard_seed_connection("db:seed", &parse(&[]), "sqlite").unwrap();
        guard_seed_connection("migrate", &parse(&["--seed"]), "sqlite").unwrap();
        guard_seed_connection("migrate", &parse(&["--database=pgsql"]), "sqlite").unwrap();
        guard_seed_connection("migrate:fresh", &parse(&["--database=pgsql"]), "sqlite").unwrap();
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
        // `migrate:rollback` と `migrate:reset` と `migrate:refresh` は
        // down を流すので中身が消える。
        for command in [
            "migrate:rollback",
            "migrate:reset",
            "migrate:refresh",
            "migrate:fresh",
            "db:wipe",
        ] {
            assert!(DESTRUCTIVE.contains(&command), "{command} が一覧に無い");
            assert!(is_command(command), "{command} が一覧に無い");
        }
        // 足すだけのもの、読むだけのものは入れない。
        assert!(!DESTRUCTIVE.contains(&"migrate"));
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
