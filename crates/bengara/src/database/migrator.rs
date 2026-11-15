//! マイグレーションとシーダーの実行。
//!
//! 一覧（`MIGRATIONS` / `SEEDERS`）は `bengara-build` がビルド時に作ります。
//! ここはそれを順に実行する側です。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use super::backend::{Backend, RawTx};
use super::grammar::Driver;
use super::schema::Schema;
use super::transaction::Transaction;
use super::value::Value;
use super::Source;
use crate::error::{Error, Result};

/// 記録を残す表の名前。
const TABLE: &str = "migrations";

/// 札（実行中の目印）を置く表の名前。
pub(crate) const LOCK_TABLE: &str = "migration_locks";

/// 札の主キー。1行しか入れないので固定します。
const LOCK_ID: i64 = 1;

/// 札に書く「誰が取ったか」。
///
/// プロセス番号と、分かればホスト名を入れます。残った札の持ち主を探せるようにするためです。
fn lock_owner() -> String {
    let pid = std::process::id();
    match std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")) {
        Ok(host) if !host.trim().is_empty() => format!("{host}:{pid}"),
        _ => format!("pid {pid}"),
    }
}

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

/// 札が取れなかったときのエラー文。
///
/// `held` に持ち主が入っているときだけ「他のプロセスが実行中」と言います。
/// 入っていないときは、入らなかった本当の理由（`detail`）をそのまま見せます。
fn lock_error(held: Option<(String, String)>, detail: &str) -> Error {
    match held {
        Some((owner, at)) => Error::msg(format!(
            "別のプロセスがマイグレーションを実行中です（{owner} が {at} に開始）。\n\
             終わってからもう一度実行してください。\n\
             途中で落ちて札が残っているときは、`migrate:unlock` で消せます。"
        )),
        None => Error::msg(format!(
            "実行中の札（{LOCK_TABLE}）を置けませんでした: {detail}"
        )),
    }
}

/// 組み立ての誤りを見てから SQL を取り出す。誤りがあれば 1 本も流しません。
///
/// `up` / `down` は同期なので `Result` を返せません。実行すれば必ず壊れるものは
/// `Schema` に溜まるので、流す**前**にここで落とします。
fn take_statements(name: &str, mut schema: Schema) -> Result<Vec<String>> {
    let errors = schema.take_errors();
    if !errors.is_empty() {
        return Err(Error::msg(format!(
            "マイグレーション `{name}` の組み立てに誤りがあります:\n  - {}",
            errors.join("\n  - ")
        )));
    }
    Ok(schema.into_statements())
}

/// マイグレーションを実行する側。
pub(crate) struct Migrator {
    backend: Arc<dyn Backend>,
    source: Source,
    /// 札の表を作ったという印。
    ///
    /// 1つのコマンドで `lock` / `lock_holder` / `unlock` と2〜3回通るので、
    /// 作るのは最初の1回だけにします。
    lock_table_ready: std::sync::OnceLock<()>,
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
        Ok(Self {
            backend,
            source,
            lock_table_ready: std::sync::OnceLock::new(),
        })
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

    /// 札を置く表が無ければ作る。この接続では1回だけ流します。
    async fn ensure_lock_table(&self) -> Result<()> {
        if self.lock_table_ready.get().is_some() {
            return Ok(());
        }
        let mut schema = Schema::new(self.driver());
        schema.create_if_not_exists(LOCK_TABLE, |t| {
            // 1行しか入らないように、主キーを固定の値にする。
            t.integer("id").primary();
            t.string("owner");
            t.string("acquired_at");
        });
        for sql in schema.into_statements() {
            self.source.execute(&sql, &[]).await?;
        }
        // 流せたときだけ印を付ける。失敗したら次も作りに行く。
        let _ = self.lock_table_ready.set(());
        Ok(())
    }

    /// 表を変えるコマンドの札を取る。
    ///
    /// すでに誰かが取っていれば、 **取らずにエラー**にします。
    /// 複数のプロセスから同時に `migrate` すると、同じものが二重に走るためです。
    pub(crate) async fn lock(&self) -> Result<()> {
        self.ensure_lock_table().await?;

        let owner = lock_owner();
        let now = super::now();
        let inserted = super::QueryBuilder::new(self.source.clone(), LOCK_TABLE)
            .insert(&[
                ("id", Value::Int(LOCK_ID)),
                ("owner", Value::Text(owner)),
                ("acquired_at", Value::Text(now)),
            ])
            .await;

        let error = match inserted {
            Ok(_) => return Ok(()),
            Err(e) => e,
        };

        // 入らなかった理由を確かめる。札が実在するときだけ「実行中」と言う。
        // 列が合わない古い表が残っていたときやディスクの問題でも insert は失敗するので、
        // 一律に「実行中」と断定すると、直し方を間違えます。
        Err(match self.lock_holder().await {
            Ok(held) => lock_error(held, &error.to_string()),
            // 札を読むこともできなかった。持ち主が居る証拠は無い。
            Err(read) => lock_error(
                None,
                &format!("{error}\n  札を読もうとしても失敗しました: {read}"),
            ),
        })
    }

    /// 札を返す。取れていなくても失敗にしません。
    pub(crate) async fn unlock(&self) -> Result<u64> {
        self.ensure_lock_table().await?;
        let affected = super::QueryBuilder::new(self.source.clone(), LOCK_TABLE)
            .where_("id", LOCK_ID)
            .delete()
            .await?;
        Ok(affected)
    }

    /// 残った札を外す。`migrate:unlock` が使います。返るのは外した札の持ち主です。
    ///
    /// **表を作りません。** 札の表（`migration_locks`）が無ければ、札は誰も持って
    /// いないので、そのまま `None` を返します。`migrate:status` と同じ考え方で、
    /// 一度も `migrate` していないデータベースに表を残しません。
    pub(crate) async fn clear_lock(&self) -> Result<Option<(String, String)>> {
        let exists = self
            .backend
            .table_names()
            .await?
            .iter()
            .any(|name| name == LOCK_TABLE);
        if !exists {
            return Ok(None);
        }
        // 表はあるので、この先で作る文を流さない。
        let _ = self.lock_table_ready.set(());
        let held = self.lock_holder().await?;
        if held.is_some() {
            self.unlock().await?;
        }
        Ok(held)
    }

    /// 札を持っている相手（名前と取った時刻）。
    pub(crate) async fn lock_holder(&self) -> Result<Option<(String, String)>> {
        self.ensure_lock_table().await?;
        let row = super::QueryBuilder::new(self.source.clone(), LOCK_TABLE)
            .where_("id", LOCK_ID)
            .first()
            .await?;
        match row {
            Some(row) => Ok(Some((
                row.get::<String>("owner")?,
                row.get::<String>("acquired_at")?,
            ))),
            None => Ok(None),
        }
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
    ///
    /// **MySQL / MariaDB では巻き戻りません。** 表を作る・変える文（DDL）が
    /// トランザクションを勝手に確定するので、失敗した文より前の DDL は残ります。
    /// 記録（`migrations` 表の行）は入らないので、直して流し直すと同じ文をもう一度流します。
    /// SQLite と PostgreSQL は DDL もトランザクションに入るので、1本まるごと戻ります。
    async fn apply(&self, migration: &Migration, batch: i64) -> Result<()> {
        let mut schema = Schema::new(self.driver());
        (migration.up)(&mut schema);
        let statements = take_statements(migration.name, schema)?;

        let tx = self.begin().await?;
        for sql in &statements {
            tx.statement_raw(sql).await?;
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

        // 戻すものを先に全部そろえる。途中でファイルの無いものに当たって止まると、
        // バッチの半分だけ戻した状態が残る。
        let mut planned: Vec<(&String, &Migration)> = Vec::new();
        let mut missing: Vec<&str> = Vec::new();
        // 新しく入れたものから戻す。
        for (name, batch) in ran.iter().rev() {
            if !targets.contains(batch) {
                continue;
            }
            match migrations.iter().find(|m| m.name == name) {
                Some(migration) => planned.push((name, migration)),
                None => missing.push(name),
            }
        }
        if !missing.is_empty() {
            return Err(Error::msg(format!(
                "次のファイルがいまのバイナリに入っていないので、このバッチは戻せません: {}\n\
                 何も戻していません。",
                missing.join(", ")
            )));
        }

        let mut done = Vec::new();
        for (name, migration) in planned {
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
        let statements = take_statements(migration.name, schema)?;

        let tx = self.begin().await?;
        for sql in &statements {
            tx.statement_raw(sql).await?;
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

    /// 記録を残す表があるか。
    async fn has_table(&self) -> Result<bool> {
        Ok(self
            .backend
            .table_names()
            .await?
            .iter()
            .any(|name| name == TABLE))
    }

    /// 実行済みかどうかの一覧。
    ///
    /// **表を作りません。** 読むだけのコマンドなので、一度も `migrate` していない
    /// データベースに `migrations` 表を残しません。読み取り専用の利用者でも動きます。
    /// 表が無ければ、全部を「未実行」として返します。
    pub(crate) async fn status(&self, migrations: &[Migration]) -> Result<Vec<Status>> {
        let ran = if self.has_table().await? {
            self.ran().await?
        } else {
            Vec::new()
        };
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
        let mut tables = self.backend.table_names().await?;
        // 札の表は残す。いま自分が札を持っているので、消すと排他が切れる。
        tables.retain(|t| t != LOCK_TABLE);
        if tables.is_empty() {
            return Ok(Vec::new());
        }
        let statements = super::grammar::drop_all(self.driver(), &tables);
        let (before, after) = super::grammar::defer_foreign_keys(self.driver());

        // 1つのトランザクションの中で流す。
        // 外部キーの確かめを止める指定は**接続ごと**なので、プールから取り直すと
        // 別の接続に当たりえます。
        //
        // MySQL は DDL でトランザクションが勝手に確定します。それでも
        // トランザクションを使うのは、接続を1本に固定するためです。
        //
        // 包み（`Transaction`）ではなく下回りを直に使います。包みは 1 本失敗すると
        // 以後の文を全部断るので、「失敗しても確かめを戻す」文まで断られていました。
        // `foreign_key_checks = 0` のままの接続がプールに戻っていました。
        let mut raw = self.backend.begin().await?;
        // 失敗したときは `raw` を落とす。下回りが巻き戻します。
        drop_with_checks_off(raw.as_mut(), before, &statements, after).await?;
        raw.commit().await?;
        Ok(tables)
    }

    /// 表を全部消して、もう一度流す。
    pub(crate) async fn fresh(&self, migrations: &[Migration]) -> Result<Vec<String>> {
        self.drop_all_tables().await?;
        self.run(migrations).await
    }
}

/// 外部キーの確かめを止めて、表を消す文を順に流す。
///
/// **消す文が失敗しても、確かめを戻す文（`after`）は必ず流します。**
/// MySQL の `foreign_key_checks` は接続ごとの設定です。戻さないと、確かめを
/// 止めたままの接続がプールに戻り、次に借りた処理が外部キーを素通りします。
/// MySQL は文が 1 本失敗しても接続とトランザクションは使えるので、同じ接続で戻せます。
///
/// MySQL を 1 文の `drop table a, b, c` にして確かめを止めずに済ませる手は
/// 取りません。1 文の中でも外部キーの確かめは掛かり、親と子をまとめて消せるかが
/// サーバの種類と版（MySQL 5.7 / 8.0 / MariaDB）でそろわないためです。
///
/// SQLite と PostgreSQL は `after` がありません（SQLite の `defer_foreign_keys` は
/// 確定か巻き戻しで自動で戻り、PostgreSQL は `cascade` で消します）。
pub(crate) async fn drop_with_checks_off(
    raw: &mut dyn RawTx,
    before: Option<&str>,
    statements: &[String],
    after: Option<&str>,
) -> Result<()> {
    if let Some(sql) = before {
        raw.execute(sql, &[]).await?;
    }
    let mut result = Ok(());
    for sql in statements {
        if let Err(error) = raw.execute(sql, &[]).await {
            result = Err(error);
            break;
        }
    }
    if let Some(sql) = after {
        let restored = raw.execute(sql, &[]).await.map(|_| ());
        if result.is_ok() {
            // 消すのは成功して、戻すのだけ失敗したら、そのエラーを返す。
            result = restored;
        } else if let Err(error) = restored {
            // 両方失敗した。返すのは元のエラー。戻せなかったことは記録に残す
            // （接続が切れたときなど。切れた接続はプールに戻りません）。
            tracing::warn!("外部キーの確かめを戻せませんでした（{sql}）: {error}");
        }
    }
    result
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
    fn 札の持ち主が分かる形になる() {
        let owner = lock_owner();
        assert!(!owner.is_empty());
        // プロセス番号が入っていること（残った札の持ち主を探せるように）。
        assert!(
            owner.contains(&std::process::id().to_string()),
            "{owner} にプロセス番号が入っていない"
        );
    }

    #[test]
    fn 札の表はマイグレーションの表と別() {
        assert_ne!(LOCK_TABLE, TABLE);
        assert_eq!(LOCK_TABLE, "migration_locks");
    }

    #[test]
    fn 札の表は1行しか入らない形() {
        let mut schema = Schema::new(Driver::Sqlite);
        schema.create_if_not_exists(LOCK_TABLE, |t| {
            t.integer("id").primary();
            t.string("owner");
            t.string("acquired_at");
        });
        let sql = schema.to_sql().join("\n");
        assert!(sql.contains("primary key"), "{sql}");
    }

    #[test]
    fn 札が取れない理由で文が変わる() {
        // 札が実在するときだけ「実行中」と言う。
        let held = Some(("host:1234".to_string(), "2026-10-06 00:00:00".to_string()));
        let message = lock_error(held, "UNIQUE constraint failed").to_string();
        assert!(
            message.contains("別のプロセスがマイグレーションを実行中"),
            "{message}"
        );
        assert!(message.contains("host:1234"), "{message}");
        assert!(message.contains("migrate:unlock"), "{message}");

        // 札が無いときは、元のエラーを添えて返す。
        let message =
            lock_error(None, "table migration_locks has no column named owner").to_string();
        assert!(!message.contains("実行中です"), "{message}");
        assert!(message.contains("has no column named owner"), "{message}");
        assert!(message.contains(LOCK_TABLE), "{message}");
    }

    #[test]
    fn 表を全部消す文は_if_exists_付きになる() {
        // drop_all_tables はこの形の文を、1つのトランザクションの中で流す。
        let mut schema = Schema::new(Driver::Sqlite);
        for table in ["posts", "comments"] {
            schema.drop_if_exists(table);
        }
        assert_eq!(
            schema.to_sql(),
            [
                "drop table if exists \"posts\"".to_string(),
                "drop table if exists \"comments\"".to_string(),
            ]
        );
    }

    /// 流れてきた文を覚える、テスト用の中身。`fail_on` を含む文だけ失敗します。
    struct RecordingTx {
        log: Arc<std::sync::Mutex<Vec<String>>>,
        fail_on: &'static str,
    }

    impl RawTx for RecordingTx {
        fn fetch_all<'a>(
            &'a mut self,
            _sql: &'a str,
            _bindings: &'a [Value],
        ) -> super::super::backend::DbFuture<'a, Vec<super::super::Row>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn execute<'a>(
            &'a mut self,
            sql: &'a str,
            _bindings: &'a [Value],
        ) -> super::super::backend::DbFuture<'a, super::super::Affected> {
            Box::pin(async move {
                self.log.lock().unwrap().push(sql.to_string());
                if !self.fail_on.is_empty() && sql.contains(self.fail_on) {
                    return Err(Error::msg(format!("失敗: {sql}")));
                }
                Ok(super::super::Affected::default())
            })
        }

        fn commit(self: Box<Self>) -> super::super::backend::DbFuture<'static, ()> {
            Box::pin(async { Ok(()) })
        }

        fn rollback(self: Box<Self>) -> super::super::backend::DbFuture<'static, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn 表を消すのに失敗しても外部キーの確かめは戻す() {
        // 以前は包み（`Transaction`）越しに流していた。1 本失敗すると以後の文を
        // 全部断るので、戻す文も断られ、`foreign_key_checks = 0` の接続がプールに戻っていた。
        let (before, after) = super::super::grammar::defer_foreign_keys(Driver::MySql);
        let statements = vec![
            "drop table if exists `a`".to_string(),
            "drop table if exists `b`".to_string(),
            "drop table if exists `c`".to_string(),
        ];
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut tx = RecordingTx {
            log: Arc::clone(&log),
            fail_on: "`b`",
        };
        let error = drop_with_checks_off(&mut tx, before, &statements, after)
            .await
            .expect_err("消す文のエラーが返る");
        assert!(error.to_string().contains("`b`"), "{error}");
        assert_eq!(
            *log.lock().unwrap(),
            [
                "set session foreign_key_checks = 0",
                "drop table if exists `a`",
                "drop table if exists `b`",
                // `c` には進まない。戻す文は流す。
                "set session foreign_key_checks = 1",
            ]
        );

        // 成功したときは、全部流してから戻す。
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut tx = RecordingTx {
            log: Arc::clone(&log),
            fail_on: "",
        };
        drop_with_checks_off(&mut tx, before, &statements, after)
            .await
            .expect("通る");
        assert_eq!(log.lock().unwrap().len(), 5);
        assert_eq!(
            log.lock().unwrap().last().map(String::as_str),
            Some("set session foreign_key_checks = 1")
        );

        // 戻す文だけが失敗したときは、そのエラーを返す。
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut tx = RecordingTx {
            log,
            fail_on: "foreign_key_checks = 1",
        };
        assert!(drop_with_checks_off(&mut tx, before, &statements, after)
            .await
            .is_err());
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 状況を見るだけでは記録の表を作らない() {
        // 以前は `migrate:status` が `migrations` 表を作っていた。
        // 読むだけのコマンドが表を残し、読み取り専用の利用者では失敗していた。
        let backend = super::super::sqlite::connect(&super::super::ConnectionConfig::sqlite(
            "test", ":memory:",
        ))
        .await
        .expect("メモリ上の SQLite につながる");
        let migrator = Migrator {
            backend: Arc::clone(&backend),
            source: Source::Default,
            lock_table_ready: std::sync::OnceLock::new(),
        };

        let rows = migrator.status(LIST).await.expect("表が無くても読める");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, LIST[0].name);
        assert_eq!(rows[0].batch, None, "表が無ければ未実行");
        assert!(rows[0].present);
        assert!(
            backend.table_names().await.unwrap().is_empty(),
            "status が表を作ってはいけない"
        );
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 札を外すだけでは札の表を作らない() {
        // 以前は `migrate:unlock` が `migration_locks` 表を作っていた。
        let _db = crate::testing::refresh_database().await;
        let migrator = Migrator::connect(None).await.expect("つながる");
        // ほかのテストが残した札の表を消して、無い状態から始める。
        migrator
            .source
            .execute(&format!("drop table if exists {LOCK_TABLE}"), &[])
            .await
            .expect("消せる");
        let has_lock_table = || async {
            migrator
                .backend
                .table_names()
                .await
                .unwrap()
                .iter()
                .any(|name| name == LOCK_TABLE)
        };

        assert_eq!(migrator.clear_lock().await.expect("表が無くても通る"), None);
        assert!(!has_lock_table().await, "unlock が表を作ってはいけない");

        // 札があるときは外して、持ち主を返す。
        migrator.lock().await.expect("札が取れる");
        let (owner, _) = migrator
            .clear_lock()
            .await
            .expect("外せる")
            .expect("持ち主が返る");
        assert_eq!(owner, lock_owner());
        assert_eq!(migrator.lock_holder().await.unwrap(), None);
        // 表はあるが札が無いときも `None`。
        assert_eq!(migrator.clear_lock().await.unwrap(), None);
        assert!(has_lock_table().await, "あった表は消さない");
    }

    #[test]
    fn 組み立てに誤りがあると実行の前に落ちる() {
        // `.on(表名)` の無い外部キー。SQL を 1 本も流さずに落とす。
        let mut schema = Schema::new(Driver::Sqlite);
        schema.create("comments", |t| {
            t.foreign_id("post_id");
            t.foreign("post_id");
        });
        let message = take_statements("2026_10_05_000000_create_comments_table", schema)
            .expect_err("落ちる")
            .to_string();
        assert!(message.contains("組み立てに誤りがあります"), "{message}");
        assert!(
            message.contains("2026_10_05_000000_create_comments_table"),
            "{message}"
        );
        assert!(message.contains("comments.post_id"), "{message}");

        // 誤りが無ければそのまま取り出せる。
        let mut schema = Schema::new(Driver::Sqlite);
        (LIST[0].up)(&mut schema);
        let statements = take_statements(LIST[0].name, schema).expect("取り出せる");
        assert_eq!(statements.len(), 1);
    }

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
