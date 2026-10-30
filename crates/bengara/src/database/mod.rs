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
/// 小数の文字列をそろえる。SQLite には小数の型が無いので要りません。
#[cfg(any(feature = "mysql", feature = "postgres"))]
mod decimal;
/// SQL の失敗の包み方。3つのドライバで共通です。
#[cfg(feature = "database")]
mod failure;
mod grammar;
mod model;
#[cfg(feature = "mysql")]
mod mysql;
#[cfg(feature = "postgres")]
mod postgres;
mod query;
mod schema;
#[cfg(feature = "sqlite")]
mod sqlite;
/// 日時の列を文字列に直す。SQLite は値がそのまま文字列なので要りません。
#[cfg(any(feature = "mysql", feature = "postgres"))]
mod temporal;
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
    ///
    /// 見る環境変数は次のとおりです。
    ///
    /// | 環境変数           | 既定値                        | 使うドライバ   |
    /// |--------------------|-------------------------------|----------------|
    /// | `DB_CONNECTION`    | `sqlite`                      | すべて         |
    /// | `DB_DATABASE`      | ドライバごと（下の表）        | すべて         |
    /// | `DB_URL`           | 空（他の値から組み立てる）    | すべて         |
    /// | `DB_HOST`          | `127.0.0.1`                   | MySQL / pgsql  |
    /// | `DB_PORT`          | 3306 / 5432                   | MySQL / pgsql  |
    /// | `DB_USERNAME`      | `root` / `postgres`           | MySQL / pgsql  |
    /// | `DB_PASSWORD`      | 空                            | MySQL / pgsql  |
    /// | `DB_MAX_CONNECTIONS` | 5                           | すべて         |
    ///
    /// `DB_DATABASE` の既定値は、SQLite では `database/database.sqlite`、
    /// MySQL と PostgreSQL では `bengara` です。
    ///
    /// 接続は `init` が作る `config/database.rs` と同じ 4 本（`sqlite` / `mysql` /
    /// `mariadb` / `pgsql`）を作ります。`DB_CONNECTION` がその 4 つに無い名前のときは、
    /// その名前の接続を 1 本足します（ドライバは名前から読み、読めなければ SQLite）。
    ///
    /// `DB_URL` は既定の接続にだけ効きます。ほかの接続にまで同じ接続文字列を
    /// 入れると、`DB::connection("mysql")` が PostgreSQL の URL を持つことになるためです。
    fn default() -> Self {
        let default: String = crate::env("DB_CONNECTION", "sqlite");
        let file: String = crate::env("DB_DATABASE", "database/database.sqlite");
        let name: String = crate::env("DB_DATABASE", "bengara");
        let mut connections = vec![
            ConnectionConfig::sqlite("sqlite", file),
            from_env(ConnectionConfig::mysql("mysql", name.clone())),
            from_env(ConnectionConfig::mysql("mariadb", name.clone())),
            from_env(ConnectionConfig::postgres("pgsql", name.clone())),
        ];
        if !connections.iter().any(|c| c.name == default) {
            // 名前が読めないときは SQLite とみなします。本当のエラーは接続のときに出ます。
            let extra = match Driver::parse(&default).unwrap_or(Driver::Sqlite) {
                Driver::Sqlite => ConnectionConfig::sqlite(
                    default.clone(),
                    crate::env::<String>("DB_DATABASE", "database/database.sqlite"),
                ),
                Driver::MySql => from_env(ConnectionConfig::mysql(default.clone(), name)),
                Driver::Postgres => from_env(ConnectionConfig::postgres(default.clone(), name)),
            };
            connections.push(extra);
        }
        let url: String = crate::env("DB_URL", "");
        let max: u32 = crate::env("DB_MAX_CONNECTIONS", 5u32);
        for connection in &mut connections {
            connection.max_connections = max.max(1);
            if connection.name == default && !url.is_empty() {
                connection.url = url.clone();
            }
        }
        Self {
            connections,
            default,
        }
    }
}

/// 環境変数でつなぎ先と資格情報を上書きする。
///
/// 空の文字列と 0 は「指定なし」として無視します（[`ConnectionConfig::host`] ほか）。
fn from_env(base: ConnectionConfig) -> ConnectionConfig {
    base.host(crate::env::<String>("DB_HOST", ""))
        .port(crate::env::<u16>("DB_PORT", 0u16))
        .username(crate::env::<String>("DB_USERNAME", ""))
        .password(crate::env::<String>("DB_PASSWORD", ""))
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
    /// つなぎ先のホスト。SQLite では使いません。
    pub host: String,
    /// つなぎ先のポート。SQLite では使いません。
    pub port: u16,
    /// 利用者名。SQLite では使いません。
    pub username: String,
    /// パスワード。空のときは渡しません。SQLite では使いません。
    pub password: String,
    /// 接続文字列。空でなければ、他の値より優先します。
    pub url: String,
    /// 同時に張る接続の数の上限。
    pub max_connections: u32,
    /// SQLite で外部キーの制約を効かせるか。
    ///
    /// MySQL と PostgreSQL では外部キーは常に効きます。この値は見ません。
    pub foreign_keys: bool,
}

impl ConnectionConfig {
    /// SQLite の接続を作る。
    pub fn sqlite(name: impl Into<String>, database: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            driver: Driver::Sqlite,
            database: database.into(),
            host: String::new(),
            port: 0,
            username: String::new(),
            password: String::new(),
            url: String::new(),
            max_connections: 5,
            foreign_keys: true,
        }
    }

    /// MySQL / MariaDB の接続を作る。
    ///
    /// ホストは `127.0.0.1`、ポートは 3306、利用者名は `root` から始まります。
    /// 変えるときは [`host`](Self::host) / [`port`](Self::port) /
    /// [`username`](Self::username) / [`password`](Self::password) を続けます。
    ///
    /// ```ignore
    /// ConnectionConfig::mysql("mysql", env("DB_DATABASE", "bengara"))
    ///     .host(env("DB_HOST", "127.0.0.1"))
    ///     .username(env("DB_USERNAME", "root"))
    ///     .password(env("DB_PASSWORD", ""))
    /// ```
    pub fn mysql(name: impl Into<String>, database: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            driver: Driver::MySql,
            database: database.into(),
            host: "127.0.0.1".to_string(),
            port: 3306,
            username: "root".to_string(),
            password: String::new(),
            url: String::new(),
            max_connections: 5,
            foreign_keys: true,
        }
    }

    /// PostgreSQL の接続を作る。
    ///
    /// ホストは `127.0.0.1`、ポートは 5432、利用者名は `postgres` から始まります。
    pub fn postgres(name: impl Into<String>, database: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            driver: Driver::Postgres,
            database: database.into(),
            host: "127.0.0.1".to_string(),
            port: 5432,
            username: "postgres".to_string(),
            password: String::new(),
            url: String::new(),
            max_connections: 5,
            foreign_keys: true,
        }
    }

    /// つなぎ先のホストを決める。空の文字列は無視します。
    pub fn host(mut self, host: impl Into<String>) -> Self {
        let host = host.into();
        if !host.is_empty() {
            self.host = host;
        }
        self
    }

    /// つなぎ先のポートを決める。0 は無視します。
    pub fn port(mut self, port: u16) -> Self {
        if port != 0 {
            self.port = port;
        }
        self
    }

    /// 利用者名を決める。空の文字列は無視します。
    pub fn username(mut self, username: impl Into<String>) -> Self {
        let username = username.into();
        if !username.is_empty() {
            self.username = username;
        }
        self
    }

    /// パスワードを決める。空のままならパスワード無しでつなぎます。
    pub fn password(mut self, password: impl Into<String>) -> Self {
        self.password = password.into();
        self
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
    /// 真になるのは次の 2 とおりです。接続は1本に固定されます。
    ///
    /// | 形                                             | 例                                           |
    /// |------------------------------------------------|----------------------------------------------|
    /// | `:memory:` を含む                              | `:memory:` / `sqlite::memory:`               |
    /// | 接続文字列のクエリに `mode=memory` がある      | `sqlite://file:app?mode=memory&cache=shared` |
    ///
    /// 見るのはこの 2 つだけです。`memory` という語を含むだけの
    /// ファイル名（`data/memory_2026.db` など）を取り違えないためです。
    pub fn is_memory(&self) -> bool {
        self.database.contains(":memory:")
            || self.url.contains(":memory:")
            || has_memory_mode(&self.url)
    }
}

/// 接続文字列のクエリの部分に `mode=memory` があるか。
///
/// `sqlite://file:app?mode=memory&cache=shared` の形を拾います。
/// 見るのは `?` より後ろだけなので、ファイル名は取り違えません。
fn has_memory_mode(url: &str) -> bool {
    let Some((_, query)) = url.split_once('?') else {
        return false;
    };
    query
        .split('&')
        .any(|pair| pair.trim().eq_ignore_ascii_case("mode=memory"))
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
///
/// ドライバを選ぶのはここだけです。機能フラグが無いドライバは、つなぐ前に止めます。
async fn connect(settings: &ConnectionConfig) -> Result<Arc<dyn Backend>> {
    // 機能フラグが無いドライバは、ここで止める。
    if !settings.driver.is_available() {
        return Err(unavailable(settings.driver));
    }
    // 3つのフラグを全部付けると最後の腕に届かなくなるので、警告を外す。
    #[allow(unreachable_patterns)]
    match settings.driver {
        #[cfg(feature = "sqlite")]
        Driver::Sqlite => sqlite::connect(settings).await,
        #[cfg(feature = "mysql")]
        Driver::MySql => mysql::connect(settings).await,
        #[cfg(feature = "postgres")]
        Driver::Postgres => postgres::connect(settings).await,
        other => Err(unavailable(other)),
    }
}

/// 機能フラグが無いドライバを使おうとしたときのエラー。
///
/// 付けるフラグの名前をそのまま出します。MySQL は `mariadb` でも同じものが入ります。
fn unavailable(driver: Driver) -> Error {
    let (flag, note) = match driver {
        Driver::Sqlite => ("sqlite", ""),
        // MariaDB を指した人にも分かるように、同じものだと添えます。
        Driver::MySql => (
            "mysql",
            "（MariaDB なら \"mariadb\" でも同じものが入ります）",
        ),
        Driver::Postgres => ("postgres", ""),
    };
    Error::msg(format!(
        "{driver} につなぐには Cargo.toml を \
         `bengara = {{ version = \"0.1\", features = [\"{flag}\"] }}` にしてください{note}"
    ))
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
    // SQLite はファイルを指すので、既定でメモリの上に逃がせます。
    // MySQL と PostgreSQL は**既にあるデータベース**を指すので、逃がせません。
    // `refresh_database()` は表を全部消すので、指定が無ければ**つなぎません。**
    let database: String = if base.driver == Driver::Sqlite {
        crate::env("DB_TEST_DATABASE", ":memory:")
    } else {
        let name: String = crate::env("DB_TEST_DATABASE", "");
        if name.is_empty() {
            return Err(Error::msg(format!(
                "{} のテストには DB_TEST_DATABASE が必要です。\
                 テスト用のデータベースを別に作って、その名前を .env の DB_TEST_DATABASE に書いてください\
                 （テストは表を全部消すので、開発用のデータベースには触りません）",
                base.driver
            )));
        }
        name
    };
    // 接続文字列で書いている人は、host / user / password をそちらに持っている。
    // 捨てると既定の `127.0.0.1` と `root` / `postgres` に落ちて、別の DB を向く。
    // データベース名の部分だけをテスト用に差し替える。SQLite は URL がファイルを
    // 指すだけなので、これまでどおり `database` で置き換える。
    let url = if base.driver == Driver::Sqlite || base.url.is_empty() {
        String::new()
    } else {
        replace_url_database(&base.url, &database)
    };
    let settings = ConnectionConfig {
        database,
        url,
        ..base
    };
    let created = connect(&settings).await?;
    let _ = TEST_BACKEND.set(created);
    Ok(())
}

/// 接続文字列のデータベース名（パスの部分）だけを差し替える。
///
/// `mysql://u:p@host:3306/app?ssl-mode=required` の `app` を `name` にします。
/// 認証情報・ホスト・ポート・クエリはそのまま残します。
/// `://` が無い文字列は接続文字列と見なさず、そのまま返します。
fn replace_url_database(url: &str, name: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let (before_query, query) = match rest.find(['?', '#']) {
        Some(at) => rest.split_at(at),
        None => (rest, ""),
    };
    let authority = before_query
        .split_once('/')
        .map_or(before_query, |(authority, _)| authority);
    format!("{scheme}://{authority}/{name}{query}")
}

/// 主キーの列に渡す値を、列の型に合わせて直す。
///
/// 整数に読める文字列（パス引数・セッションの ID）は `Value::Int` にします。
/// PostgreSQL は `bigint = text` を断るので、文字列のまま渡すと
/// `operator does not exist` になります。SQLite と MySQL は暗黙に変換するので、
/// そちらでは気づけません。
///
/// 整数に読めない文字列はそのまま返します。外から来た文字列をモデルの主キーに
/// 渡すときは、この関数ではなく [`Model::key_value`] を通してください。
/// 文字列の主キーを自分で実装したモデル（`INTEGER_KEY = false`）にはそちらだけが対応します。
pub(crate) fn normalize_key(value: Value) -> Value {
    match value {
        Value::Text(text) => match text.trim().parse::<i64>() {
            Ok(number) => Value::Int(number),
            Err(_) => Value::Text(text),
        },
        other => other,
    }
}

/// 利用者が直接書いた SQL を、投げる前にそろえる。
///
/// `?` の数と値の数を確かめ、PostgreSQL では `?` を `$1` `$2` … に振り直します。
/// `DB::select` / `DB::statement` / `Transaction::select` / `statement` が通ります。
/// クエリビルダが組み立てた SQL は、組み立ての時点で済んでいるのでここを通りません。
///
/// PostgreSQL で最初から `$1` と書いてあるときはそのまま通します。
pub(crate) fn prepare_raw_sql(driver: Driver, sql: &str, bindings: &[Value]) -> Result<String> {
    let holes = grammar::count_placeholders(sql);
    if holes != bindings.len() {
        if driver == Driver::Postgres && holes == 0 && sql.contains('$') {
            return Ok(sql.to_string());
        }
        return Err(Error::msg(format!(
            "SQL に書いた `?` の数（{holes}）と、渡した値の数（{}）が合っていません。\
             `?` は値の置き場所としてのみ使えます",
            bindings.len()
        )));
    }
    Ok(grammar::number_placeholders(driver, sql, 0))
}

/// テスト用の接続が入っているか。
pub(crate) fn test_backend() -> Option<Arc<dyn Backend>> {
    TEST_BACKEND.get().map(Arc::clone)
}

/// エラーが一意制約違反（unique / primary key の重なり）か。
///
/// 同じ値をもう一度入れたときに真になります。エラーの文を `contains` で見るのを
/// やめるための口です。
///
/// ```ignore
/// use bengara::database::is_unique_violation;
///
/// if let Err(error) = DB::table("users").insert(&[("email", email.into())]).await {
///     if is_unique_violation(&error) {
///         return Err(Error::http(409, "そのメールアドレスは使われています"));
///     }
///     return Err(error);
/// }
/// ```
///
/// `Error` の列挙に腕は足しません。利用者が書いた `match` を壊さないためです。
pub fn is_unique_violation(error: &Error) -> bool {
    #[cfg(feature = "database")]
    {
        failure::is_unique_violation(error)
    }
    #[cfg(not(feature = "database"))]
    {
        let _ = error;
        false
    }
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

/// プールへ投げるときの、SQL の組み立てに使うドライバを決める。
///
/// `test` にテスト用の接続のドライバが入っていれば、**そちらを最優先**します。
/// 設定を見るのは、テスト用の接続が無いときだけです。
fn pool_driver(test: Option<Driver>, name: Option<&str>) -> Result<Driver> {
    if let Some(driver) = test {
        return Ok(driver);
    }
    let config = database_config();
    let name = name.unwrap_or(config.default.as_str());
    config.get(name).map(|c| c.driver).ok_or_else(|| {
        Error::msg(format!(
            "`{name}` という接続は設定にありません（ある接続: {}）",
            config.names().join(", ")
        ))
    })
}

impl Source {
    /// SQL の組み立てに使うドライバ。接続はしません。
    ///
    /// テスト用の接続が入っているときは、そちらのドライバを返します。
    /// `backend()` は接続名を無視してテスト用の 1 本に投げるので、設定の側を見ると
    /// SQL だけ別の書き方で組み立ててしまいます。SQLite のテストで
    /// `DB::connection("pgsql")` と書くと、`$1` の SQL を SQLite に投げる形です。
    pub(crate) fn driver(&self) -> Result<Driver> {
        // 投げ先（`target()`）と同じ決め方にそろえる。
        let test = TEST_BACKEND.get().map(|found| found.driver());
        match self {
            Source::Tx(shared) => Ok(shared.driver()),
            Source::Default => pool_driver(test, None),
            Source::Named(name) => pool_driver(test, Some(name.as_str())),
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
    ///
    /// 値の置き場所は `?` で書きます。PostgreSQL でも `?` のままでよく、
    /// 投げる前に `$1` `$2` … へ振り直します。`?` の数と値の数が合わないときは
    /// エラーです。
    pub async fn select(sql: &str, bindings: &[Value]) -> Result<Vec<Row>> {
        let source = Source::Default;
        let sql = prepare_raw_sql(source.driver()?, sql, bindings)?;
        source.fetch_all(&sql, bindings).await
    }

    /// SQL を直接投げる（`insert` / `update` / `delete` / DDL）。
    ///
    /// 値の置き場所の決まりは [`DB::select`] と同じです。
    pub async fn statement(sql: &str, bindings: &[Value]) -> Result<Affected> {
        let source = Source::Default;
        let sql = prepare_raw_sql(source.driver()?, sql, bindings)?;
        source.execute(&sql, bindings).await
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

    /// SQL を直接投げて行を受け取る。値の置き場所の決まりは [`DB::select`] と同じです。
    pub async fn select(&self, sql: &str, bindings: &[Value]) -> Result<Vec<Row>> {
        let source = Source::Named(self.name.clone());
        let sql = prepare_raw_sql(source.driver()?, sql, bindings)?;
        source.fetch_all(&sql, bindings).await
    }

    /// SQL を直接投げる。値の置き場所の決まりは [`DB::select`] と同じです。
    pub async fn statement(&self, sql: &str, bindings: &[Value]) -> Result<Affected> {
        let source = Source::Named(self.name.clone());
        let sql = prepare_raw_sql(source.driver()?, sql, bindings)?;
        source.execute(&sql, bindings).await
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
    fn 既定の設定はinitの雛形と同じ4本を持つ() {
        // `config/database.rs` を置かなくても、`DB::connection("mysql")` が引けること。
        let config = DatabaseConfig::default();
        assert_eq!(config.names(), vec!["sqlite", "mysql", "mariadb", "pgsql"]);
        assert_eq!(config.get("mysql").unwrap().driver, Driver::MySql);
        assert_eq!(config.get("mariadb").unwrap().driver, Driver::MySql);
        assert_eq!(config.get("pgsql").unwrap().driver, Driver::Postgres);
        assert_eq!(config.get("pgsql").unwrap().port, 5432);
        assert!(config.get("oracle").is_none());
    }

    #[test]
    fn 接続文字列はデータベース名だけ差し替える() {
        assert_eq!(
            replace_url_database("mysql://u:p@dbhost:3307/app", "app_test"),
            "mysql://u:p@dbhost:3307/app_test"
        );
        // クエリは残す。
        assert_eq!(
            replace_url_database("postgres://u:p@h/app?sslmode=require", "t"),
            "postgres://u:p@h/t?sslmode=require"
        );
        // パスが無ければ足す。
        assert_eq!(replace_url_database("postgres://h", "t"), "postgres://h/t");
        // 接続文字列でないものは触らない。
        assert_eq!(replace_url_database("not a url", "t"), "not a url");
    }

    #[test]
    fn 整数に読める主キーは整数に直す() {
        // パス引数とセッションの ID は文字列で届く。PostgreSQL は bigint = text を断る。
        let text = |s: &str| Value::Text(s.to_string());
        assert_eq!(normalize_key(text("12")), Value::Int(12));
        assert_eq!(normalize_key(text(" 7 ")), Value::Int(7));
        assert_eq!(normalize_key(text("-3")), Value::Int(-3));
        // 読めないものと、文字列以外はそのまま。
        assert_eq!(normalize_key(text("abc")), text("abc"));
        assert_eq!(normalize_key(text("12abc")), text("12abc"));
        assert_eq!(normalize_key(text("")), text(""));
        assert_eq!(normalize_key(Value::Int(5)), Value::Int(5));
        assert_eq!(normalize_key(Value::Null), Value::Null);
    }

    #[test]
    fn 生のsqlはpostgresで番号に直し数も確かめる() {
        let bindings = [Value::Int(1), Value::Int(2)];
        assert_eq!(
            prepare_raw_sql(
                Driver::Postgres,
                "select * from t where a = ? and b = ?",
                &bindings
            )
            .unwrap(),
            "select * from t where a = $1 and b = $2"
        );
        assert_eq!(
            prepare_raw_sql(Driver::Sqlite, "select ? , ?", &bindings).unwrap(),
            "select ? , ?"
        );
        // 最初から `$1` と書いてあるときは、そのまま通す。
        assert_eq!(
            prepare_raw_sql(Driver::Postgres, "select $1, $2", &bindings).unwrap(),
            "select $1, $2"
        );
        // 数が合わなければ断る。
        let error = prepare_raw_sql(Driver::Sqlite, "select ?", &bindings)
            .unwrap_err()
            .to_string();
        assert!(error.contains("合っていません"), "{error}");
        assert!(prepare_raw_sql(Driver::Postgres, "select 1", &bindings).is_err());
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

        // `mode=memory` の形もメモリ上。接続が5本に増えると、
        // `create table` の後で `no such table` になることがある。
        assert!(ConnectionConfig::sqlite("main", "x")
            .url("sqlite://file:x?mode=memory&cache=shared")
            .is_memory());
        assert!(ConnectionConfig::sqlite("main", "x")
            .url("sqlite://file:x?cache=shared&MODE=Memory")
            .is_memory());

        // `memory` という語を含むだけのファイル名は、メモリ上ではない。
        // 取り違えると接続1本・WAL なしになってしまう。
        assert!(!ConnectionConfig::sqlite("main", "data/memory_2026.db").is_memory());
        assert!(!ConnectionConfig::sqlite("main", "x")
            .url("sqlite://data/memory_2026.db")
            .is_memory());
        // `?` より前に書いてあるだけのものは拾わない。
        assert!(!ConnectionConfig::sqlite("main", "x")
            .url("sqlite://data/mode=memory.db")
            .is_memory());
        assert!(!ConnectionConfig::sqlite("main", "x")
            .url("sqlite://file:x?mode=rwc")
            .is_memory());
    }

    #[test]
    fn テスト用の接続が入っていれば接続名より優先する() {
        // `backend()` は接続名を無視してテスト用の1本に投げる。
        // SQL の組み立ても同じドライバにそろえないと、`$1` の SQL を SQLite に投げる。
        assert_eq!(
            pool_driver(Some(Driver::Sqlite), Some("pgsql")).unwrap(),
            Driver::Sqlite
        );
        assert_eq!(
            pool_driver(Some(Driver::Sqlite), None).unwrap(),
            Driver::Sqlite
        );
        // 設定に無い名前でも、テスト用の接続が入っていれば通る（投げ先も同じ1本）。
        assert_eq!(
            pool_driver(Some(Driver::Postgres), Some("typo")).unwrap(),
            Driver::Postgres
        );
        // テスト用の接続が無いときは、これまでどおり設定を見る。
        assert_eq!(
            pool_driver(None, Some("pgsql")).unwrap(),
            Driver::Postgres,
            "設定の pgsql は PostgreSQL"
        );
        assert_eq!(pool_driver(None, None).unwrap(), Driver::Sqlite);
        let error = pool_driver(None, Some("typo")).unwrap_err().to_string();
        assert!(error.contains("設定にありません"), "{error}");
    }

    #[test]
    fn 鍵で束ねられる() {
        let grouped = group_by(vec![(1, "a"), (1, "b"), (2, "c")], |item| item.0);
        assert_eq!(grouped[&1].len(), 2);
        assert_eq!(grouped[&2].len(), 1);
    }
}
