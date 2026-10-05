//! マイグレーションとシーダーの実行。
//!
//! 一覧（`MIGRATIONS` / `SEEDERS`）は `bengara-build` がビルド時に作ります。
//! ここはそれを順に実行する側です。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use super::backend::Backend;
use super::grammar::Driver;
use super::schema::Schema;
use super::transaction::Transaction;
use super::value::Value;
use super::Source;
use crate::error::{Error, Result};

/// 記録を残す表の名前。
const TABLE: &str = "migrations";

/// マイグレーション1本。`bengara-build` が生成します。
///
/// ```ignore
/// pub const MIGRATIONS: &[Migration] = &[Migration {
///     name: "2026_10_05_000000_create_posts_table",
///     up: crate::database::migrations::m2026_10_05_000000_create_posts_table::up,
///     down: crate::database::migrations::m2026_10_05_000000_create_posts_table::down,
/// }];
/// ```
#[derive(Clone, Copy)]
pub struct Migration {
    /// ファイル名（拡張子なし）。実行の順番と記録の鍵になります。
    pub name: &'static str,
    /// 表を作る側。
    pub up: fn(&mut Schema),
    /// 元に戻す側。
    pub down: fn(&mut Schema),
}

impl std::fmt::Debug for Migration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Migration")
            .field("name", &self.name)
            .finish()
    }
}

/// シーダーが返す非同期の結果。
pub type SeederFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

/// シーダー1本。`bengara-build` が生成します。
#[derive(Clone, Copy)]
pub struct Seeder {
    /// ファイル名（拡張子なし）。`db:seed --class=` で指す名前です。
    pub name: &'static str,
    /// 中身。
    pub run: fn() -> SeederFuture,
}

impl std::fmt::Debug for Seeder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Seeder").field("name", &self.name).finish()
    }
}

/// 1本ぶんの状況。`migrate:status` が使います。
#[derive(Debug, Clone)]
pub(crate) struct Status {
    pub name: String,
    /// 実行済みならバッチ番号。
    pub batch: Option<i64>,
    /// いまのバイナリにそのファイルが入っているか。
    pub present: bool,
}

/// マイグレーションを実行する側。
pub(crate) struct Migrator {
    backend: Arc<dyn Backend>,
    source: Source,
}

impl Migrator {
    /// 接続を決めて作る。
    pub(crate) async fn connect(name: Option<&str>) -> Result<Self> {
        let backend = super::backend(name).await?;
        let source = match name {
            // テスト用の接続が入っているときは、名前ではなく既定の経路を使う。
            Some(name) if super::test_backend().is_none() => Source::Named(name.to_string()),
            _ => Source::Default,
        };
        Ok(Self { backend, source })
    }

    fn driver(&self) -> Driver {
        self.backend.driver()
    }

    /// 記録を残す表が無ければ作る。
    pub(crate) async fn ensure_table(&self) -> Result<()> {
        let mut schema = Schema::new(self.driver());
        schema.create_if_not_exists(TABLE, |t| {
            t.id();
            t.string("migration");
            t.integer("batch");
        });
        for sql in schema.into_statements() {
            self.source.execute(&sql, &[]).await?;
        }
        Ok(())
    }

    /// 実行済みの一覧（名前とバッチ番号）。古い順。
    pub(crate) async fn ran(&self) -> Result<Vec<(String, i64)>> {
        let rows = self
            .query()
            .select(&["migration", "batch"])
            .order_by("id")
            .get()
            .await?;
        rows.iter()
            .map(|row| Ok((row.get::<String>("migration")?, row.get::<i64>("batch")?)))
            .collect()
    }

    fn query(&self) -> super::QueryBuilder {
        super::QueryBuilder::new(self.source.clone(), TABLE)
    }

    async fn next_batch(&self) -> Result<i64> {
        let max = self.query().max::<i64>("batch").await?.unwrap_or(0);
        Ok(max + 1)
    }

    /// まだ実行していないものを順に実行する。返るのは実行した名前。
    pub(crate) async fn run(&self, migrations: &[Migration]) -> Result<Vec<String>> {
        self.ensure_table().await?;
        let ran = self.ran().await?;
        let batch = self.next_batch().await?;
        let mut done = Vec::new();

        for migration in migrations {
            if ran.iter().any(|(name, _)| name == migration.name) {
                continue;
            }
            self.apply(migration, batch)
                .await
                .map_err(|e| Error::msg(format!("{} の実行に失敗しました: {e}", migration.name)))?;
            done.push(migration.name.to_string());
        }
        Ok(done)
    }

    /// 1本を実行して記録する。失敗したら巻き戻す。
    async fn apply(&self, migration: &Migration, batch: i64) -> Result<()> {
        let mut schema = Schema::new(self.driver());
        (migration.up)(&mut schema);
        let statements = schema.into_statements();

        let tx = self.begin().await?;
        for sql in &statements {
            tx.statement(sql, &[]).await?;
        }
        tx.table(TABLE)
            .insert(&[
                ("migration", Value::Text(migration.name.to_string())),
                ("batch", Value::Int(batch)),
            ])
            .await?;
        tx.commit().await
    }

    /// 最後のバッチから数えて `steps` 回分を巻き戻す。返るのは戻した名前。
    pub(crate) async fn rollback(
        &self,
        migrations: &[Migration],
        steps: u32,
    ) -> Result<Vec<String>> {
        self.ensure_table().await?;
        let ran = self.ran().await?;
        if ran.is_empty() {
            return Ok(Vec::new());
        }
        let mut batches: Vec<i64> = ran.iter().map(|(_, batch)| *batch).collect();
        batches.sort_unstable();
        batches.dedup();
        let targets: Vec<i64> = batches
            .into_iter()
            .rev()
            .take(steps.max(1) as usize)
            .collect();

        let mut done = Vec::new();
        // 新しく入れたものから戻す。
        for (name, batch) in ran.iter().rev() {
            if !targets.contains(batch) {
                continue;
            }
            let Some(migration) = migrations.iter().find(|m| m.name == name) else {
                return Err(Error::msg(format!(
                    "`{name}` のファイルがいまのバイナリに入っていないので戻せません"
                )));
            };
            self.revert(migration)
                .await
                .map_err(|e| Error::msg(format!("{name} の巻き戻しに失敗しました: {e}")))?;
            done.push(name.clone());
        }
        Ok(done)
    }

    /// 全部巻き戻す。
    pub(crate) async fn reset(&self, migrations: &[Migration]) -> Result<Vec<String>> {
        self.rollback(migrations, u32::MAX).await
    }

    async fn revert(&self, migration: &Migration) -> Result<()> {
        let mut schema = Schema::new(self.driver());
        (migration.down)(&mut schema);
        let statements = schema.into_statements();

        let tx = self.begin().await?;
        for sql in &statements {
            tx.statement(sql, &[]).await?;
        }
        tx.table(TABLE)
            .where_("migration", migration.name)
            .delete()
            .await?;
        tx.commit().await
    }

    async fn begin(&self) -> Result<Transaction> {
        Transaction::start(Arc::clone(&self.backend)).await
    }

    /// 実行済みかどうかの一覧。
    pub(crate) async fn status(&self, migrations: &[Migration]) -> Result<Vec<Status>> {
        self.ensure_table().await?;
        let ran = self.ran().await?;
        let mut out: Vec<Status> = migrations
            .iter()
            .map(|migration| Status {
                name: migration.name.to_string(),
                batch: ran
                    .iter()
                    .find(|(name, _)| name == migration.name)
                    .map(|(_, batch)| *batch),
                present: true,
            })
            .collect();
        // 記録にはあるが、ファイルが無いもの。
        for (name, batch) in &ran {
            if !migrations.iter().any(|m| m.name == name) {
                out.push(Status {
                    name: name.clone(),
                    batch: Some(*batch),
                    present: false,
                });
            }
        }
        Ok(out)
    }

    /// 表を全部消す。
    pub(crate) async fn drop_all_tables(&self) -> Result<Vec<String>> {
        let tables = self.backend.table_names().await?;
        if tables.is_empty() {
            return Ok(Vec::new());
        }
        // 外部キーの順番を気にせず消せるように、いったん外す。
        if self.driver() == Driver::Sqlite {
            self.source
                .execute("pragma foreign_keys = off", &[])
                .await?;
        }
        let mut schema = Schema::new(self.driver());
        for table in &tables {
            schema.drop_if_exists(table);
        }
        for sql in schema.into_statements() {
            self.source.execute(&sql, &[]).await?;
        }
        if self.driver() == Driver::Sqlite {
            self.source.execute("pragma foreign_keys = on", &[]).await?;
        }
        Ok(tables)
    }

    /// 表を全部消して、もう一度流す。
    pub(crate) async fn fresh(&self, migrations: &[Migration]) -> Result<Vec<String>> {
        self.drop_all_tables().await?;
        self.run(migrations).await
    }
}

/// シーダーを流す。`class` が `None` なら `DatabaseSeeder`、
/// それが無ければ全部を名前順に流します。
pub(crate) async fn seed(seeders: &[Seeder], class: Option<&str>) -> Result<Vec<String>> {
    if seeders.is_empty() {
        return Ok(Vec::new());
    }
    let selected: Vec<&Seeder> = match class {
        Some(name) => {
            let found = seeders.iter().find(|s| s.name == name).ok_or_else(|| {
                Error::msg(format!(
                    "`{name}` というシーダーはありません（あるもの: {}）",
                    seeders
                        .iter()
                        .map(|s| s.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
            vec![found]
        }
        None => match seeders.iter().find(|s| s.name == "DatabaseSeeder") {
            Some(found) => vec![found],
            None => seeders.iter().collect(),
        },
    };

    let mut done = Vec::new();
    for seeder in selected {
        (seeder.run)()
            .await
            .map_err(|e| Error::msg(format!("{} の実行に失敗しました: {e}", seeder.name)))?;
        done.push(seeder.name.to_string());
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(schema: &mut Schema) {
        schema.create("posts", |t| {
            t.id();
            t.string("title");
        });
    }

    fn down(schema: &mut Schema) {
        schema.drop_if_exists("posts");
    }

    const LIST: &[Migration] = &[Migration {
        name: "2026_10_05_000000_create_posts_table",
        up,
        down,
    }];

    #[test]
    fn 一覧は名前とsqlを持つ() {
        assert_eq!(LIST.len(), 1);
        let mut schema = Schema::new(Driver::Sqlite);
        (LIST[0].up)(&mut schema);
        assert_eq!(schema.to_sql().len(), 1);
        assert!(schema.to_sql()[0].starts_with("create table \"posts\""));

        let mut schema = Schema::new(Driver::Sqlite);
        (LIST[0].down)(&mut schema);
        assert_eq!(schema.to_sql()[0], "drop table if exists \"posts\"");
        assert!(format!("{:?}", LIST[0]).contains("create_posts_table"));
    }
}
