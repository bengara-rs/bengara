//! データベース。接続、クエリビルダ、マイグレーション、モデル。
//!
//! 入口は [`DB`] です。
//!
//! ```ignore
//! let posts = DB::table("posts").where_("status", "published").get().await?;
//! ```
//!
//! 使うには Cargo の機能フラグが必要です。
//!
//! ```toml
//! bengara = { version = "0.1", features = ["sqlite"] }
//! ```
//!
//! フラグが無い状態でもコンパイルはできます。実際に接続しようとしたときだけ、
//! 直し方を書いたエラーになります。

mod backend;
mod grammar;
mod model;
mod query;
mod schema;
#[cfg(feature = "sqlite")]
mod sqlite;
mod transaction;
mod value;

pub(crate) mod commands;
pub(crate) mod migrator;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

pub(crate) use backend::Backend;

pub use grammar::Driver;
pub use migrator::{Migration, Seeder, SeederFuture};
#[doc(hidden)]
pub use model::warn_key_not_set;
pub use model::{Model, ModelQuery};
pub use query::{Paginator, QueryBuilder};
pub use schema::{Blueprint, ColumnDefinition, ForeignKeyDefinition, Schema};
pub use transaction::Transaction;
pub use value::{Affected, FromValue, IntoValue, Row, Value};

use crate::error::{Error, Result};

/// 接続の設定。`config/database.rs` が返します。
///
/// ```ignore
/// pub fn config() -> DatabaseConfig {
///     DatabaseConfig {
///         default: env("DB_CONNECTION", "sqlite"),
///         connections: vec![
///             ConnectionConfig::sqlite("sqlite", env("DB_DATABASE", "database/database.sqlite")),
///         ],
///     }
/// }
/// ```
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    /// 既定で使う接続の名前。
    pub default: String,
    /// 接続の一覧。
    pub connections: Vec<ConnectionConfig>,
}

impl Default for DatabaseConfig {
    /// `config/database.rs` が無いときに使われる値。環境変数から組み立てます。
    fn default() -> Self {
        let default: String = crate::env("DB_CONNECTION", "sqlite");
        let database: String = crate::env("DB_DATABASE", "database/database.sqlite");
        Self {
            connections: vec![ConnectionConfig::sqlite(default.clone(), database)],
            default,
        }
    }
}

impl DatabaseConfig {
    /// 名前で接続の設定を探す。
    pub fn get(&self, name: &str) -> Option<&ConnectionConfig> {
        self.connections.iter().find(|c| c.name == name)
    }

    /// 設定されている接続の名前の一覧。
    pub fn names(&self) -> Vec<&str> {
        self.connections.iter().map(|c| c.name.as_str()).collect()
    }
}

/// 接続1本ぶんの設定。
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    /// 接続の名前。`DB::connection("名前")` で指す名前です。
    pub name: String,
    /// どのデータベースか。
    pub driver: Driver,
    /// データベース名。SQLite ではファイルの場所です。
    ///
    /// 相対パスは基準ディレクトリ（`base_path()`）から見ます。
    /// `:memory:` にすると、プロセスの中だけのデータベースになります。
    pub database: String,
    /// 接続文字列。空でなければ、他の値より優先します。
    pub url: String,
    /// 同時に張る接続の数の上限。
    pub max_connections: u32,
    /// SQLite で外部キーの制約を効かせるか。
    pub foreign_keys: bool,
}

impl ConnectionConfig {
    /// SQLite の接続を作る。
    pub fn sqlite(name: impl Into<String>, database: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            driver: Driver::Sqlite,
            database: database.into(),
            url: String::new(),
            max_connections: 5,
            foreign_keys: true,
        }
    }

    /// 接続文字列を直接指定する。
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    /// 同時に張る接続の数の上限を決める。
    pub fn max_connections(mut self, max: u32) -> Self {
        self.max_connections = max.max(1);
        self
    }

    /// SQLite で外部キーの制約を効かせるか決める。
    pub fn foreign_keys(mut self, on: bool) -> Self {
        self.foreign_keys = on;
        self
    }

    /// メモリの上のデータベースか。
    ///
    /// `:memory:` を指したときは真になります。接続は1本に固定されます。
    ///
    /// 見るのは `:memory:` だけです。`memory` という語を含むだけの
    /// ファイル名（`data/memory_2026.db` など）を取り違えないためです。
    pub fn is_memory(&self) -> bool {
        self.database.contains(":memory:") || self.url.contains(":memory:")
    }
}

/// 設定を読み出す。`config/database.rs` が無ければ既定値を使います。
pub(crate) fn database_config() -> &'static DatabaseConfig {
    static FALLBACK: OnceLock<DatabaseConfig> = OnceLock::new();
    crate::try_config::<DatabaseConfig>()
        .unwrap_or_else(|| FALLBACK.get_or_init(DatabaseConfig::default))
}

static DEFAULT_BACKEND: OnceLock<Arc<dyn Backend>> = OnceLock::new();
/// 名前で指した接続の置き場所。
///
/// 読むほうが圧倒的に多いので `RwLock` にします。2回目以降は読みのロックだけで
/// 済み、書き込みのロックを取りません。
static NAMED_BACKENDS: OnceLock<RwLock<HashMap<String, Arc<dyn Backend>>>> = OnceLock::new();
static TEST_BACKEND: OnceLock<Arc<dyn Backend>> = OnceLock::new();

/// 接続を取り出す。初回だけ実際につなぎます。
///
/// 既定の接続は `OnceLock` に入るので、2回目以降はロックを取りません。
pub(crate) async fn backend(name: Option<&str>) -> Result<Arc<dyn Backend>> {
    // テスト用の接続が入っているときは、必ずそちらを使う。
    if let Some(found) = TEST_BACKEND.get() {
        return Ok(Arc::clone(found));
    }

    let config = database_config();
    let target = name.unwrap_or(config.default.as_str());
    let is_default = target == config.default;

    if is_default {
        if let Some(found) = DEFAULT_BACKEND.get() {
            return Ok(Arc::clone(found));
        }
    } else if let Some(found) = named_backend(target) {
        return Ok(found);
    }

    let settings = config.get(target).ok_or_else(|| {
        Error::msg(format!(
            "`{target}` という接続は設定にありません（ある接続: {}）。config/database.rs を確かめてください",
            config.names().join(", ")
        ))
    })?;
    let created = connect(settings).await?;

    if is_default {
        // 同時に2つ作られたときは、先に入ったほうを使う。
        let _ = DEFAULT_BACKEND.set(Arc::clone(&created));
        return Ok(DEFAULT_BACKEND.get().map(Arc::clone).unwrap_or(created));
    }
    // 書き込みのロックを取るのは、初回だけです。
    let mut map = named_lock().write().unwrap_or_else(|e| e.into_inner());
    let stored = map.entry(target.to_string()).or_insert(created).clone();
    Ok(stored)
}

fn named_lock() -> &'static RwLock<HashMap<String, Arc<dyn Backend>>> {
    NAMED_BACKENDS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// 名前で指した接続を、読みのロックだけで探す。
fn named_backend(name: &str) -> Option<Arc<dyn Backend>> {
    named_lock()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(name)
        .map(Arc::clone)
}

/// 設定に従って実際につなぐ。
async fn connect(settings: &ConnectionConfig) -> Result<Arc<dyn Backend>> {
    // 機能フラグが無い、または、まだ作っていないドライバは、ここで止める。
    if !settings.driver.is_available() {
        return Err(unavailable(settings.driver));
    }
    #[cfg(feature = "sqlite")]
    if settings.driver == Driver::Sqlite {
        return sqlite::connect(settings).await;
    }
    Err(unavailable(settings.driver))
}

/// 機能フラグが無い、または、まだ作っていないドライバを使おうとしたときのエラー。
fn unavailable(driver: Driver) -> Error {
    match driver {
        Driver::Sqlite => Error::msg(
            "SQLite を使うには Cargo.toml を `bengara = { version = \"0.1\", features = [\"sqlite\"] }` にしてください",
        ),
        other => Error::msg(format!(
            "{other} はまだ作っていません。いまつなげるのは SQLite だけです"
        )),
    }
}

/// テストで使う接続を用意する。`testing::refresh_database()` から呼びます。
///
/// 置き場所は `DB_TEST_DATABASE`（既定は `:memory:`）です。
/// **開発用のデータベースには触りません。**
pub(crate) async fn connect_for_tests() -> Result<()> {
    if TEST_BACKEND.get().is_some() {
        return Ok(());
    }
    let config = database_config();
    let base = config
        .get(&config.default)
        .cloned()
        .unwrap_or_else(|| ConnectionConfig::sqlite("sqlite", ":memory:"));
    let settings = ConnectionConfig {
        database: crate::env("DB_TEST_DATABASE", ":memory:"),
        url: String::new(),
        ..base
    };
    let created = connect(&settings).await?;
    let _ = TEST_BACKEND.set(created);
    Ok(())
}

/// テスト用の接続が入っているか。
pub(crate) fn test_backend() -> Option<Arc<dyn Backend>> {
    TEST_BACKEND.get().map(Arc::clone)
}

/// クエリをどこへ投げるか。
#[derive(Clone)]
pub(crate) enum Source {
    /// 既定の接続。
    Default,
    /// 名前で指した接続。
    Named(String),
    /// 始まっているトランザクション。
    Tx(Arc<transaction::TxShared>),
}

impl Source {
    /// SQL の組み立てに使うドライバ。接続はしません。
    pub(crate) fn driver(&self) -> Result<Driver> {
        match self {
            Source::Tx(shared) => Ok(shared.driver()),
            Source::Default | Source::Named(_) => {
                let config = database_config();
                let name = match self {
                    Source::Named(name) => name.as_str(),
                    _ => config.default.as_str(),
                };
                config.get(name).map(|c| c.driver).ok_or_else(|| {
                    Error::msg(format!(
                        "`{name}` という接続は設定にありません（ある接続: {}）",
                        config.names().join(", ")
                    ))
                })
            }
        }
    }

    /// 実際の投げ先を決める。**3分岐はここだけです。**
    async fn target(&self) -> Result<Target> {
        Ok(match self {
            Source::Tx(shared) => Target::Tx(Arc::clone(shared)),
            Source::Default => Target::Pool(backend(None).await?),
            Source::Named(name) => Target::Pool(backend(Some(name)).await?),
        })
    }

    pub(crate) async fn fetch_all(&self, sql: &str, bindings: &[Value]) -> Result<Vec<Row>> {
        match self.target().await? {
            Target::Tx(shared) => shared.fetch_all(sql, bindings).await,
            Target::Pool(found) => found.fetch_all(sql, bindings).await,
        }
    }

    pub(crate) async fn execute(&self, sql: &str, bindings: &[Value]) -> Result<Affected> {
        match self.target().await? {
            Target::Tx(shared) => shared.execute(sql, bindings).await,
            Target::Pool(found) => found.execute(sql, bindings).await,
        }
    }
}

/// 解決した投げ先。トランザクションの中かどうかだけが違います。
enum Target {
    Tx(Arc<transaction::TxShared>),
    Pool(Arc<dyn Backend>),
}

/// データベースの入口。
///
/// ```ignore
/// let rows = DB::table("posts").where_("status", "published").get().await?;
/// let tx = DB::begin().await?;
/// ```
pub struct DB;

impl DB {
    /// 表を指してクエリを組み立てる。
    pub fn table(table: impl Into<String>) -> QueryBuilder {
        QueryBuilder::new(Source::Default, table)
    }

    /// 名前で接続を選ぶ。
    pub fn connection(name: impl Into<String>) -> ConnectionRef {
        ConnectionRef { name: name.into() }
    }

    /// SQL を直接投げて行を受け取る。
    pub async fn select(sql: &str, bindings: &[Value]) -> Result<Vec<Row>> {
        Source::Default.fetch_all(sql, bindings).await
    }

    /// SQL を直接投げる（`insert` / `update` / `delete` / DDL）。
    pub async fn statement(sql: &str, bindings: &[Value]) -> Result<Affected> {
        Source::Default.execute(sql, bindings).await
    }

    /// トランザクションを始める。
    ///
    /// ```ignore
    /// let tx = DB::begin().await?;
    /// tx.table("posts").insert(&[("title", "x".into())]).await?;
    /// tx.commit().await?;
    /// ```
    pub async fn begin() -> Result<Transaction> {
        let found = backend(None).await?;
        Transaction::start(found).await
    }

    /// つながっているデータベースの種類。
    pub async fn driver() -> Result<Driver> {
        Ok(backend(None).await?.driver())
    }

    /// 表の名前の一覧。
    pub async fn table_names() -> Result<Vec<String>> {
        backend(None).await?.table_names().await
    }
}

/// 名前で選んだ接続。
pub struct ConnectionRef {
    name: String,
}

impl ConnectionRef {
    /// 表を指してクエリを組み立てる。
    pub fn table(&self, table: impl Into<String>) -> QueryBuilder {
        QueryBuilder::new(Source::Named(self.name.clone()), table)
    }

    /// SQL を直接投げて行を受け取る。
    pub async fn select(&self, sql: &str, bindings: &[Value]) -> Result<Vec<Row>> {
        Source::Named(self.name.clone())
            .fetch_all(sql, bindings)
            .await
    }

    /// SQL を直接投げる。
    pub async fn statement(&self, sql: &str, bindings: &[Value]) -> Result<Affected> {
        Source::Named(self.name.clone())
            .execute(sql, bindings)
            .await
    }

    /// トランザクションを始める。
    pub async fn begin(&self) -> Result<Transaction> {
        let found = backend(Some(&self.name)).await?;
        Transaction::start(found).await
    }
}

/// いまの時刻を `YYYY-MM-DD HH:MM:SS`（UTC）で返す。
///
/// `created_at` / `updated_at` に入れる形です。日時専用の型は作っていません（決定記録 #040）。
pub fn now() -> String {
    crate::support::time::now_string()
}

/// 一覧を鍵で束ねる。まとめ読みした後に使います。
///
/// ```ignore
/// let comments = Comment::query().where_in("post_id", &ids).get().await?;
/// let by_post = group_by(comments, |c| c.post_id);
/// ```
pub fn group_by<T, K, F>(items: Vec<T>, key: F) -> HashMap<K, Vec<T>>
where
    K: std::hash::Hash + Eq,
    F: Fn(&T) -> K,
{
    let mut map: HashMap<K, Vec<T>> = HashMap::new();
    for item in items {
        map.entry(key(&item)).or_default().push(item);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 既定の設定はsqliteになる() {
        let config = DatabaseConfig::default();
        assert_eq!(config.default, "sqlite");
        let first = config.get("sqlite").expect("既定の接続がある");
        assert_eq!(first.driver, Driver::Sqlite);
        assert_eq!(first.max_connections, 5);
        assert!(first.foreign_keys);
    }

    #[test]
    fn 無い接続はnoneになる() {
        let config = DatabaseConfig::default();
        assert!(config.get("mysql").is_none());
        assert_eq!(config.names(), vec!["sqlite"]);
    }

    #[test]
    fn 設定は組み立て直せる() {
        let c = ConnectionConfig::sqlite("main", ":memory:")
            .max_connections(0)
            .foreign_keys(false);
        assert_eq!(c.max_connections, 1, "0 を渡しても1本は残す");
        assert!(!c.foreign_keys);
        assert!(c.is_memory());
    }

    #[test]
    fn メモリ上かどうかは_memory_の指定だけで決める() {
        assert!(ConnectionConfig::sqlite("main", ":memory:").is_memory());
        assert!(ConnectionConfig::sqlite("main", "x")
            .url("sqlite::memory:")
            .is_memory());

        // `memory` という語を含むだけのファイル名は、メモリ上ではない。
        // 取り違えると接続1本・WAL なしになってしまう。
        assert!(!ConnectionConfig::sqlite("main", "data/memory_2026.db").is_memory());
        assert!(!ConnectionConfig::sqlite("main", "x")
            .url("sqlite://data/memory_2026.db")
            .is_memory());
    }

    #[test]
    fn 鍵で束ねられる() {
        let grouped = group_by(vec![(1, "a"), (1, "b"), (2, "c")], |item| item.0);
        assert_eq!(grouped[&1].len(), 2);
        assert_eq!(grouped[&2].len(), 1);
    }
}
