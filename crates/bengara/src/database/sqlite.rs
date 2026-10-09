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
use super::failure::failed;
use super::grammar::Driver;
use super::value::{Affected, Row, Value};
use super::ConnectionConfig;
use crate::error::{Error, Result};

/// つなぐ。
pub(crate) async fn connect(settings: &ConnectionConfig) -> Result<Arc<dyn Backend>> {
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
        pool = pool
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None);
    }

    let pool = pool.connect_with(options).await.map_err(|e| {
        Error::msg(format!(
            "SQLite につなげません（{}）: {e}",
            settings.database
        ))
    })?;
    Ok(Arc::new(SqliteBackend { pool }))
}

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
        let options = SqliteConnectOptions::from_str(&settings.url)
            .map_err(|e| Error::msg(format!("接続文字列が読めません（{}）: {e}", settings.url)))?;
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
}

impl Backend for SqliteBackend {
    fn driver(&self) -> Driver {
        Driver::Sqlite
    }

    fn fetch_all<'a>(&'a self, sql: &'a str, bindings: &'a [Value]) -> DbFuture<'a, Vec<Row>> {
        Box::pin(async move {
            let rows = bind(sqlx::query(sql), bindings)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| failed(sql, e))?;
            convert_rows(&rows)
        })
    }

    fn execute<'a>(&'a self, sql: &'a str, bindings: &'a [Value]) -> DbFuture<'a, Affected> {
        Box::pin(async move {
            let result = bind(sqlx::query(sql), bindings)
                .execute(&self.pool)
                .await
                .map_err(|e| failed(sql, e))?;
            Ok(affected(&result))
        })
    }

    fn begin(&self) -> DbFuture<'_, Box<dyn RawTx>> {
        Box::pin(async move {
            let tx = self
                .pool
                .begin()
                .await
                .map_err(|e| Error::msg(format!("トランザクションを始められません: {e}")))?;
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
                .map_err(|e| failed(sql, e))?;
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
            Ok(affected(&result))
        })
    }

    fn commit(self: Box<Self>) -> DbFuture<'static, ()> {
        Box::pin(async move {
            let this = *self;
            this.tx
                .commit()
                .await
                .map_err(|e| Error::msg(format!("トランザクションを確定できません: {e}")))
        })
    }

    fn rollback(self: Box<Self>) -> DbFuture<'static, ()> {
        Box::pin(async move {
            let this = *self;
            this.tx
                .rollback()
                .await
                .map_err(|e| Error::msg(format!("トランザクションを巻き戻せません: {e}")))
        })
    }
}

/// 更新系の結果を `Affected` に直す。
///
/// `last_insert_rowid()` は接続ごとに「最後に入れた行」を覚えています。
/// update / delete / DDL では意味が無く、行が入らなかったときは
/// **同じ接続で前に入れた古い番号**が返ります。
/// そのまま渡すと取り違えるので、1行も変わらなかったときは入れません。
fn affected(result: &SqliteQueryResult) -> Affected {
    let rows = result.rows_affected();
    Affected {
        rows,
        last_insert_id: (rows > 0).then(|| result.last_insert_rowid()),
    }
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
    async fn 行が変わらないときは採番の番号を返さない() {
        let db = memory().await;
        db.execute("create table t (a integer)", &[]).await.unwrap();

        let inserted = db
            .execute("insert into t (a) values (1)", &[])
            .await
            .unwrap();
        assert_eq!(inserted.last_insert_id, Some(1), "insert では番号が返る");

        // 当てはまる行が無い update。古い rowid を返してはいけない。
        let updated = db
            .execute("update t set a = 2 where a = 99", &[])
            .await
            .unwrap();
        assert_eq!(updated.rows, 0);
        assert_eq!(updated.last_insert_id, None);

        // DDL も同じ。
        let created = db.execute("create table u (a integer)", &[]).await.unwrap();
        assert_eq!(created.last_insert_id, None);
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
}
