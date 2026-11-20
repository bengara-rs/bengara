//! SQLite の下回り（sqlx）。
//!
//! **sqlx を知っているのはこのファイルだけです。** 上の層には SQL の文字列と
//! `Value` / `Row` しか渡しません。

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use sqlx::sqlite::{
    SqliteArguments, SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions,
    SqliteQueryResult, SqliteRow,
};
use sqlx::{Column, Row as SqlxRow, Sqlite, TypeInfo, ValueRef};

use super::backend::{Backend, DbFuture, RawTx};
use super::failure::{failed, failed_with_hint};
use super::grammar::Driver;
use super::value::{Affected, Row, Value};
use super::ConnectionConfig;
use crate::error::{Error, Result};

/// つなぐ。
pub(crate) async fn connect(settings: &ConnectionConfig) -> Result<Arc<dyn Backend>> {
    Ok(Arc::new(open(settings).await?))
}

/// 接続プールを作る。テストで中身を見られるよう、`connect` から分けています。
async fn open(settings: &ConnectionConfig) -> Result<SqliteBackend> {
    let memory = settings.is_memory();
    let options = build_options(settings)?;

    let mut pool = SqlitePoolOptions::new().max_connections(if memory {
        // メモリの上のデータベースは、接続ごとに別物になる。1本に固定する。
        1
    } else {
        settings.max_connections
    });
    if memory {
        // 接続が切れると中身も消えるので、ずっと持っておく。
        // 接続は 1 本なので、空くのは使っている処理が終わったときだけです。
        // トランザクションの途中で外の接続を使うと、終わりが来ないまま待ち続けます。
        // 既定の 30 秒ではなく、早めに諦めて理由を添えます（`SqliteBackend::fail`）。
        pool = pool
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .acquire_timeout(MEMORY_ACQUIRE_TIMEOUT);
    }

    let pool = pool.connect_with(options).await.map_err(|e| {
        // `url` でつないでいるときに `database` を出すと、使っていない場所を見せる。
        Error::msg(format!(
            "SQLite につなげません（{}）: {e}",
            super::connection_target(settings)
        ))
    })?;
    Ok(SqliteBackend { pool, memory })
}

/// メモリの SQLite で、接続が空くのを待つ長さ。
const MEMORY_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// トランザクションを始める文。
///
/// `BEGIN IMMEDIATE` は、始めた時点で書き込みの権利を取ります。
/// 既定の `BEGIN`（DEFERRED）は、最初に書くときに取ります。WAL で
/// 「読んでから書く」トランザクションが 2 つ同時に走ると、後から書く側は
/// `SQLITE_BUSY_SNAPSHOT` ですぐ失敗し、`busy_timeout` で待ちません。
/// IMMEDIATE なら、始めるところで `busy_timeout` の間だけ順番を待ちます。
///
/// メモリの SQLite は接続が 1 本で競り合わないので、同じ文でも困りません。
/// 経路を分けないために、どちらも同じ文にします。
const BEGIN: &str = "BEGIN IMMEDIATE";

/// どの経路でも同じ設定を付ける。
///
/// `url` で指したときと `database` で指したときの挙動をそろえるためです。
fn tune(options: SqliteConnectOptions, settings: &ConnectionConfig) -> SqliteConnectOptions {
    let options = options
        .foreign_keys(settings.foreign_keys)
        // 他のプロセスが書いている間、すぐに諦めない。
        .busy_timeout(Duration::from_secs(5));
    if settings.is_memory() {
        // メモリの上にはファイルが無いので、作成も WAL も要らない。
        return options;
    }
    options
        .create_if_missing(true)
        // 読みと書きが同時に起きても待たされにくくする。
        .journal_mode(SqliteJournalMode::Wal)
}

/// 接続の指定を組み立てる。
fn build_options(settings: &ConnectionConfig) -> Result<SqliteConnectOptions> {
    if !settings.url.is_empty() {
        // 接続文字列そのものは出さない（`unreadable_url` の説明を参照）。
        let options = SqliteConnectOptions::from_str(&settings.url)
            .map_err(|e| super::unreadable_url(settings, e))?;
        return Ok(tune(options, settings));
    }

    if settings.is_memory() {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .map_err(|e| Error::msg(format!("メモリ上の SQLite を用意できません: {e}")))?;
        return Ok(tune(options, settings));
    }

    // 相対パスは基準ディレクトリから見る。
    let path = std::path::Path::new(&settings.database);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        crate::paths::base_path().join(path)
    };
    if let Some(parent) = path.parent() {
        if !parent.is_dir() {
            return Err(Error::msg(format!(
                "{} がありません。SQLite のファイルを置くディレクトリを先に作ってください",
                parent.display()
            )));
        }
    }

    Ok(tune(SqliteConnectOptions::new().filename(path), settings))
}

/// SQLite の接続プール。
struct SqliteBackend {
    pool: SqlitePool,
    /// メモリの上か。接続を待ちきれなかったときの文を変えます。
    memory: bool,
}

impl SqliteBackend {
    /// 失敗を包む。メモリの SQLite で接続を待ちきれなかったときは、よくある理由を添えます。
    fn fail(&self, sql: &str, error: sqlx::Error) -> Error {
        if self.memory && matches!(error, sqlx::Error::PoolTimedOut) {
            return failed_with_hint(sql, error, MEMORY_POOL_HINT);
        }
        failed(sql, error)
    }
}

/// メモリの SQLite で、接続を待ちきれなかったときに添える文。
const MEMORY_POOL_HINT: &str = "メモリの SQLite は接続が 1 本だけです。トランザクションの途中で DB::table / save() を呼んでいませんか。トランザクションの中では、渡された tx を使ってください";

impl Backend for SqliteBackend {
    fn driver(&self) -> Driver {
        Driver::Sqlite
    }

    fn fetch_all<'a>(&'a self, sql: &'a str, bindings: &'a [Value]) -> DbFuture<'a, Vec<Row>> {
        Box::pin(async move {
            let rows = bind(sqlx::query(sql), bindings)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| self.fail(sql, e))?;
            convert_rows(&rows)
        })
    }

    fn execute<'a>(&'a self, sql: &'a str, bindings: &'a [Value]) -> DbFuture<'a, Affected> {
        Box::pin(async move {
            let result = bind(sqlx::query(sql), bindings)
                .execute(&self.pool)
                .await
                .map_err(|e| self.fail(sql, e))?;
            Ok(affected(sql, &result))
        })
    }

    fn begin(&self) -> DbFuture<'_, Box<dyn RawTx>> {
        Box::pin(async move {
            let tx = self
                .pool
                .begin_with(BEGIN)
                .await
                .map_err(|e| self.fail(BEGIN, e))?;
            Ok(Box::new(SqliteTx { tx }) as Box<dyn RawTx>)
        })
    }

    fn table_names(&self) -> DbFuture<'_, Vec<String>> {
        Box::pin(async move {
            let sql = "select name from sqlite_master where type = 'table' \
                       and name not like 'sqlite_%' order by name";
            let rows = sqlx::query(sql)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| self.fail(sql, e))?;
            convert_rows(&rows)?
                .iter()
                .map(|row| row.get::<String>("name"))
                .collect()
        })
    }
}

/// 始まっているトランザクション。
struct SqliteTx {
    tx: sqlx::Transaction<'static, Sqlite>,
}

impl RawTx for SqliteTx {
    fn fetch_all<'a>(&'a mut self, sql: &'a str, bindings: &'a [Value]) -> DbFuture<'a, Vec<Row>> {
        Box::pin(async move {
            let rows = bind(sqlx::query(sql), bindings)
                .fetch_all(&mut *self.tx)
                .await
                .map_err(|e| failed(sql, e))?;
            convert_rows(&rows)
        })
    }

    fn execute<'a>(&'a mut self, sql: &'a str, bindings: &'a [Value]) -> DbFuture<'a, Affected> {
        Box::pin(async move {
            let result = bind(sqlx::query(sql), bindings)
                .execute(&mut *self.tx)
                .await
                .map_err(|e| failed(sql, e))?;
            Ok(affected(sql, &result))
        })
    }

    fn commit(self: Box<Self>) -> DbFuture<'static, ()> {
        Box::pin(async move {
            let this = *self;
            // `SqlFailure` に包む。`is_unique_violation()` が見るのは原因の連鎖だけで、
            // `Error::msg` では種類が分からなくなるためです（遅延制約で実際に当たります）。
            this.tx.commit().await.map_err(|e| failed("commit", e))
        })
    }

    fn rollback(self: Box<Self>) -> DbFuture<'static, ()> {
        Box::pin(async move {
            let this = *self;
            this.tx.rollback().await.map_err(|e| failed("rollback", e))
        })
    }
}

/// 更新系の結果を `Affected` に直す。
///
/// `last_insert_rowid()` は接続ごとに「最後に入れた行」を覚えています。
/// update / delete / DDL では意味が無く、**同じ接続で前に入れた古い番号**が返ります。
/// 取り違えないよう、番号を入れるのは次の両方が成り立つときだけです。
///
/// - 行を入れる文（`insert` / `replace`）である
/// - 1行以上が変わった（`insert ... select` で0件のときを外す）
///
/// MySQL と PostgreSQL も、update / delete では `None` になります。
fn affected(sql: &str, result: &SqliteQueryResult) -> Affected {
    let rows = result.rows_affected();
    Affected {
        rows,
        last_insert_id: (rows > 0 && inserts(sql)).then(|| result.last_insert_rowid()),
    }
}

/// 行を入れる文か。先頭の語を、大文字小文字の違いを無視して見ます。
///
/// 枠組みはコメント付きの SQL を組み立てないので、`/* ... */ insert ...` の形は
/// 考えません。
fn inserts(sql: &str) -> bool {
    let head = sql.trim_start();
    ["insert", "replace"].iter().any(|word| {
        head.get(..word.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(word))
    })
}

/// 値をプレースホルダへ渡す。
fn bind<'q>(
    mut query: sqlx::query::Query<'q, Sqlite, SqliteArguments<'q>>,
    bindings: &'q [Value],
) -> sqlx::query::Query<'q, Sqlite, SqliteArguments<'q>> {
    for value in bindings {
        query = match value {
            Value::Null => query.bind(Option::<i64>::None),
            Value::Bool(v) => query.bind(*v),
            Value::Int(v) => query.bind(*v),
            Value::Float(v) => query.bind(*v),
            Value::Text(v) => query.bind(v.as_str()),
            Value::Bytes(v) => query.bind(v.as_slice()),
        };
    }
    query
}

/// sqlx の行をまとめて `Row` に直す。
///
/// 列の名前は最初の行から1回だけ作り、`Arc` で全部の行に分けます。
/// 1行ごとに `Vec<String>` を複製すると、列の数だけ無駄に確保するためです。
fn convert_rows(rows: &[SqliteRow]) -> Result<Vec<Row>> {
    let Some(first) = rows.first() else {
        return Ok(Vec::new());
    };
    let columns: Arc<[String]> = first
        .columns()
        .iter()
        .map(|column| column.name().to_string())
        .collect();
    rows.iter().map(|row| convert_row(row, &columns)).collect()
}

/// sqlx の行を `Row` に直す。列の名前は呼ぶ側から受け取ります。
fn convert_row(row: &SqliteRow, columns: &Arc<[String]>) -> Result<Row> {
    let mut values = Vec::with_capacity(columns.len());
    for (index, name) in columns.iter().enumerate() {
        values.push(convert_value(row, index, name)?);
    }
    Ok(Row::with_columns(Arc::clone(columns), values))
}

/// 1つの値を `Value` に直す。
///
/// SQLite は値ごとに型が決まります。その型の名前で**1回だけ**分かれます。
/// 取り出せる順に試すと、失敗するたび sqlx がエラーの文を組み立てるので、
/// 1セルあたり2〜3回の無駄な確保が起きます。
/// 知らない型の名前のときだけ、順に試す経路に落とします。
fn convert_value(row: &SqliteRow, index: usize, name: &str) -> Result<Value> {
    let raw = row
        .try_get_raw(index)
        .map_err(|e| Error::msg(format!("列 `{name}` を読めません: {e}")))?;
    if raw.is_null() {
        return Ok(Value::Null);
    }
    let info = raw.type_info();
    match info.name() {
        "INTEGER" => read::<i64>(row, index, name).map(Value::Int),
        "REAL" => read::<f64>(row, index, name).map(Value::Float),
        "TEXT" => read::<String>(row, index, name).map(Value::Text),
        "BLOB" => read::<Vec<u8>>(row, index, name).map(Value::Bytes),
        "NULL" => Ok(Value::Null),
        other => try_in_order(row, index, name, other),
    }
}

/// 型が分かっている値を取り出す。
fn read<'r, T>(row: &'r SqliteRow, index: usize, name: &str) -> Result<T>
where
    T: sqlx::Decode<'r, Sqlite> + sqlx::Type<Sqlite>,
{
    row.try_get::<T, _>(index)
        .map_err(|e| Error::msg(format!("列 `{name}` を読めません: {e}")))
}

/// 型の名前が分からないときに、取り出せる順に試す。
fn try_in_order(row: &SqliteRow, index: usize, name: &str, type_name: &str) -> Result<Value> {
    if let Ok(v) = row.try_get::<i64, _>(index) {
        return Ok(Value::Int(v));
    }
    if let Ok(v) = row.try_get::<f64, _>(index) {
        return Ok(Value::Float(v));
    }
    if let Ok(v) = row.try_get::<String, _>(index) {
        return Ok(Value::Text(v));
    }
    if let Ok(v) = row.try_get::<Vec<u8>, _>(index) {
        return Ok(Value::Bytes(v));
    }
    Err(super::failure::unsupported_column(name, type_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::{ConnectionConfig, Schema};

    async fn memory() -> Arc<dyn Backend> {
        connect(&ConnectionConfig::sqlite("test", ":memory:"))
            .await
            .expect("メモリ上の SQLite につながる")
    }

    #[tokio::test]
    async fn 表を作って読み書きできる() {
        let db = memory().await;
        let mut schema = Schema::new(Driver::Sqlite);
        schema.create("posts", |t| {
            t.id();
            t.string("title");
            t.integer("views").default(0);
        });
        for sql in schema.into_statements() {
            db.execute(&sql, &[]).await.expect("表が作れる");
        }

        let affected = db
            .execute(
                "insert into \"posts\" (\"title\") values (?)",
                &[Value::Text("やきそば".into())],
            )
            .await
            .expect("1件入る");
        assert_eq!(affected.rows, 1);
        assert_eq!(affected.last_insert_id, Some(1));

        let rows = db
            .fetch_all("select * from \"posts\"", &[])
            .await
            .expect("読める");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<String>("title").unwrap(), "やきそば");
        assert_eq!(rows[0].get::<i64>("views").unwrap(), 0);
        assert_eq!(rows[0].get::<i64>("id").unwrap(), 1);

        assert_eq!(db.table_names().await.unwrap(), vec!["posts".to_string()]);
    }

    #[tokio::test]
    async fn null_と_バイト列を往復できる() {
        let db = memory().await;
        db.execute("create table t (a text, b blob, c real)", &[])
            .await
            .unwrap();
        db.execute(
            "insert into t (a, b, c) values (?, ?, ?)",
            &[Value::Null, Value::Bytes(vec![1, 2, 3]), Value::Float(1.5)],
        )
        .await
        .unwrap();
        let rows = db.fetch_all("select * from t", &[]).await.unwrap();
        assert_eq!(rows[0].value("a"), Some(&Value::Null));
        assert_eq!(rows[0].value("b"), Some(&Value::Bytes(vec![1, 2, 3])));
        assert_eq!(rows[0].value("c"), Some(&Value::Float(1.5)));
    }

    #[tokio::test]
    async fn トランザクションは確定しないと残らない() {
        let db = memory().await;
        db.execute("create table t (a integer)", &[]).await.unwrap();

        let mut tx = db.begin().await.unwrap();
        tx.execute("insert into t (a) values (1)", &[])
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        let rows = db.fetch_all("select * from t", &[]).await.unwrap();
        assert!(rows.is_empty(), "巻き戻したので残らない");

        let mut tx = db.begin().await.unwrap();
        tx.execute("insert into t (a) values (2)", &[])
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let rows = db.fetch_all("select * from t", &[]).await.unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn 採番の番号を返すのは行を入れた文だけ() {
        let db = memory().await;
        db.execute("create table t (a integer)", &[]).await.unwrap();

        let inserted = db
            .execute("insert into t (a) values (1)", &[])
            .await
            .unwrap();
        assert_eq!(inserted.last_insert_id, Some(1), "insert では番号が返る");
        // 前後の空白と大文字小文字は見ない。
        let inserted = db
            .execute("  INSERT INTO t (a) VALUES (2)", &[])
            .await
            .unwrap();
        assert_eq!(inserted.last_insert_id, Some(2));
        // `replace` も行を入れる文。
        let replaced = db
            .execute("replace into t (rowid, a) values (1, 3)", &[])
            .await
            .unwrap();
        assert_eq!(replaced.last_insert_id, Some(1));

        // 行が変わった update。**古い rowid を返してはいけない。**
        // MySQL と PostgreSQL はここで `None` になる。
        let updated = db.execute("update t set a = 9", &[]).await.unwrap();
        assert_eq!(updated.rows, 2);
        assert_eq!(updated.last_insert_id, None, "update では番号を返さない");

        // 当てはまる行が無い update。
        let updated = db
            .execute("update t set a = 2 where a = 99", &[])
            .await
            .unwrap();
        assert_eq!(updated.rows, 0);
        assert_eq!(updated.last_insert_id, None);

        // delete も同じ。
        let deleted = db.execute("delete from t where a = 9", &[]).await.unwrap();
        assert_eq!(deleted.rows, 2);
        assert_eq!(deleted.last_insert_id, None, "delete では番号を返さない");

        // DDL も同じ。
        let created = db.execute("create table u (a integer)", &[]).await.unwrap();
        assert_eq!(created.last_insert_id, None);

        // 1件も入らない insert では、古い番号を返さない。
        let nothing = db
            .execute("insert into t (a) select a from t", &[])
            .await
            .unwrap();
        assert_eq!(nothing.rows, 0);
        assert_eq!(nothing.last_insert_id, None);
    }

    #[test]
    fn 行を入れる文かは先頭の語で見る() {
        assert!(inserts("insert into t values (1)"));
        assert!(inserts("  \n\tINSERT INTO t VALUES (1)"));
        assert!(inserts("Replace Into t values (1)"));
        assert!(!inserts("update t set a = 1"));
        assert!(!inserts("delete from t"));
        assert!(!inserts("create table t (a integer)"));
        assert!(!inserts(""));
        // 先頭が多バイト文字でも落ちない。
        assert!(!inserts("やきそば"));
    }

    #[tokio::test]
    async fn 接続文字列の経路も同じ設定を通る() {
        // `url` で指したときも `database` のときと同じ道（tune）を通ること。
        let settings = ConnectionConfig::sqlite("test", "ignored")
            .url("sqlite::memory:")
            .foreign_keys(false);
        let db = connect(&settings).await.expect("つながる");
        let rows = db.fetch_all("pragma foreign_keys", &[]).await.unwrap();
        assert_eq!(rows[0].at(0), Some(&Value::Int(0)), "外した設定が効く");

        let settings = ConnectionConfig::sqlite("test", "ignored").url("sqlite::memory:");
        let db = connect(&settings).await.expect("つながる");
        let rows = db.fetch_all("pragma foreign_keys", &[]).await.unwrap();
        assert_eq!(rows[0].at(0), Some(&Value::Int(1)), "既定では効いている");
    }

    #[test]
    fn 読めない接続文字列をエラー文に出さない() {
        // 以前は `接続文字列が読めません（{url}）` と URL をそのまま出していた。
        let url = "sqlite://secret-dir/app.db?mode=bogus";
        let settings = ConnectionConfig::sqlite("main", "ignored").url(url);
        let message = build_options(&settings).expect_err("読めない").to_string();
        assert!(message.contains("`main`"), "{message}");
        assert!(message.contains("読めません"), "{message}");
        assert!(!message.contains("secret-dir"), "{message}");
        assert!(!message.contains(url), "{message}");
    }

    #[tokio::test]
    async fn url_でつないだときの失敗は_url_の指定と出す() {
        // 以前は使っていない `database` の値を出していた。
        // 無いディレクトリの下を読み取り専用で開くので、必ず失敗する。
        let settings = ConnectionConfig::sqlite("main", "ignored-database-name")
            .url("sqlite://bengara-no-such-dir-7f3a/x.db?mode=ro");
        let message = connect(&settings)
            .await
            .err()
            .expect("つながらない")
            .to_string();
        assert!(message.contains("SQLite につなげません"), "{message}");
        assert!(message.contains("DB_URL"), "{message}");
        assert!(!message.contains("ignored-database-name"), "{message}");
    }

    #[tokio::test]
    async fn 列の_unique_で作った索引を名前で落とせる() {
        // 以前は列の指定に `unique` と書いていたので、名前を SQLite が決めていた
        // （`sqlite_autoindex_users_1`）。`drop_index_named("users", "users_email_unique")`
        // は `drop index if exists` なので、黙って何もしなかった。
        let db = memory().await;
        let mut schema = Schema::new(Driver::Sqlite);
        schema.create("users", |t| {
            t.id();
            t.string("email").unique();
            t.string("name");
        });
        for sql in schema.into_statements() {
            db.execute(&sql, &[]).await.expect("表が作れる");
        }
        let names = db
            .fetch_all(
                "select name from sqlite_master where type = 'index' and tbl_name = 'users'",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(names.len(), 1, "自動で付く索引は無い");
        assert_eq!(
            names[0].get::<String>("name").unwrap(),
            "users_email_unique"
        );

        // 重なりは断る。一意制約違反として見分けられる。
        let insert = "insert into \"users\" (\"email\", \"name\") values (?, ?)";
        let values = [Value::Text("a@example.com".into()), Value::Text("a".into())];
        db.execute(insert, &values).await.expect("1件目は入る");
        let error = db.execute(insert, &values).await.unwrap_err();
        assert!(crate::database::is_unique_violation(&error), "{error}");

        // 名前で落とす。2 つの書き方が同じ索引を指す。
        let mut by_name = Schema::new(Driver::Sqlite);
        by_name.drop_index_named("users", "users_email_unique");
        let mut by_columns = Schema::new(Driver::Sqlite);
        by_columns.drop_unique("users", &["email"]);
        assert_eq!(by_name.to_sql(), by_columns.to_sql());
        for sql in by_columns.into_statements() {
            db.execute(&sql, &[]).await.expect("落とせる");
        }
        db.execute(insert, &values)
            .await
            .expect("索引を落としたので重なっても入る");

        // 索引を落とした後なら、列も落とせる（rollback の「索引 → 列」の順）。
        // 列の指定の `unique` だった頃は、SQLite がこの列の drop を断っていた。
        let mut schema = Schema::new(Driver::Sqlite);
        schema.drop_column("users", "email");
        for sql in schema.into_statements() {
            db.execute(&sql, &[]).await.expect("列を落とせる");
        }
    }

    #[tokio::test]
    async fn 失敗したsqlは文を添えて知らせる() {
        let db = memory().await;
        let error = db.fetch_all("select * from nope", &[]).await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("SQL の実行に失敗しました"));
        assert!(message.contains("select * from nope"));
    }
    #[tokio::test]
    async fn 真偽と小数の列は整数と小数で返る() {
        // ドキュメント（database.md の「ドライバの違い」）に載せた形を固定します。
        // SQLite は値ごとに型が決まるので、MySQL / PostgreSQL と腕が違います。
        let db = memory().await;
        let mut schema = Schema::new(Driver::Sqlite);
        schema.create("t", |t| {
            t.boolean("published");
            t.decimal("total", 8, 2);
        });
        for sql in schema.into_statements() {
            db.execute(&sql, &[]).await.unwrap();
        }
        db.execute(
            "insert into \"t\" (\"published\", \"total\") values (?, ?)",
            &[Value::Bool(true), Value::Text("1234.56".into())],
        )
        .await
        .unwrap();

        let rows = db.fetch_all("select * from \"t\"", &[]).await.unwrap();
        let row = &rows[0];
        // 真偽は整数で返る。`get::<bool>()` ならどのドライバでも同じ結果になる。
        assert_eq!(row.value("published"), Some(&Value::Int(1)));
        assert!(row.get::<bool>("published").unwrap());
        // 小数は小数で返る（SQLite に小数専用の型が無いため）。
        assert_eq!(row.value("total"), Some(&Value::Float(1234.56)));
        assert_eq!(row.get::<f64>("total").unwrap(), 1234.56);
    }

    /// 一時ディレクトリに、テストごとに別のファイル名を作る。
    fn temp_database(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        std::env::temp_dir().join(format!("bengara-{label}-{}-{nanos}.db", std::process::id()))
    }

    /// 本体と WAL の付属ファイルを消す。
    fn remove_database(path: &std::path::Path) {
        for suffix in ["", "-wal", "-shm"] {
            let mut name = path.as_os_str().to_owned();
            name.push(suffix);
            let _ = std::fs::remove_file(std::path::PathBuf::from(name));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ファイルのsqliteで読んでから書くトランザクションが同時に走っても両方成功する() {
        // 以前は `BEGIN`（DEFERRED）だった。WAL で両方が先に読むと、後から書く側が
        // `SQLITE_BUSY_SNAPSHOT` ですぐ失敗し、`busy_timeout` で待たなかった。
        let path = temp_database("immediate");
        let settings = ConnectionConfig::sqlite("test", path.to_string_lossy().as_ref());
        let db = connect(&settings)
            .await
            .expect("ファイルの SQLite につながる");
        db.execute("create table t (a integer)", &[]).await.unwrap();

        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut tasks = Vec::new();
        for n in 0..2_i64 {
            let db = Arc::clone(&db);
            let barrier = Arc::clone(&barrier);
            tasks.push(tokio::spawn(async move {
                // 2 つが同時に始めるよう、そろえてから begin する。
                barrier.wait().await;
                let mut tx = db.begin().await?;
                let rows = tx.fetch_all("select count(*) as c from t", &[]).await?;
                let count = rows[0].get::<i64>("c")?;
                tokio::time::sleep(Duration::from_millis(50)).await;
                tx.execute(
                    "insert into t (a) values (?)",
                    &[Value::Int(count * 10 + n)],
                )
                .await?;
                tx.commit().await
            }));
        }
        let mut results = Vec::new();
        for task in tasks {
            results.push(task.await.expect("タスクが終わる"));
        }
        let rows = db.fetch_all("select a from t", &[]).await.unwrap();
        drop(db);
        remove_database(&path);

        for result in results {
            result.expect("どちらのトランザクションも確定できる");
        }
        assert_eq!(rows.len(), 2);
    }

    #[tokio::test]
    async fn メモリのsqliteは接続を早めに諦める() {
        let memory = open(&ConnectionConfig::sqlite("test", ":memory:"))
            .await
            .unwrap();
        assert!(memory.memory);
        assert_eq!(
            memory.pool.options().get_acquire_timeout(),
            MEMORY_ACQUIRE_TIMEOUT
        );
        // 待ちきれなかったときは、よくある理由を添える。
        let message = memory
            .fail("select 1", sqlx::Error::PoolTimedOut)
            .to_string();
        assert!(message.contains("DB::table / save()"), "{message}");
        // ほかの失敗には添えない。
        let message = memory
            .fail("select 1", sqlx::Error::RowNotFound)
            .to_string();
        assert!(!message.contains("DB::table / save()"), "{message}");
    }

    #[tokio::test]
    async fn ファイルのsqliteは待ち時間を変えず理由も添えない() {
        let path = temp_database("timeout");
        let settings = ConnectionConfig::sqlite("test", path.to_string_lossy().as_ref());
        let file = open(&settings).await.unwrap();
        assert!(!file.memory);
        assert_ne!(
            file.pool.options().get_acquire_timeout(),
            MEMORY_ACQUIRE_TIMEOUT
        );
        let message = file.fail("select 1", sqlx::Error::PoolTimedOut).to_string();
        assert!(!message.contains("DB::table / save()"), "{message}");
        file.pool.close().await;
        drop(file);
        remove_database(&path);
    }
}
