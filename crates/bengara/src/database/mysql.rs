//! MySQL / MariaDB の下回り（sqlx）。
//!
//! **sqlx を知っているのはこのファイルだけです。** 上の層には SQL の文字列と
//! `Value` / `Row` しか渡しません。
//!
//! MariaDB も同じドライバでつながります。通信の手順が同じためです。

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use sqlx::mysql::{
    MySqlArguments, MySqlConnectOptions, MySqlPool, MySqlPoolOptions, MySqlQueryResult, MySqlRow,
};
use sqlx::{Column, MySql, Row as SqlxRow, TypeInfo, ValueRef};

use super::backend::{Backend, DbFuture, RawTx};
use super::failure::{failed, unsupported_column};
use super::grammar::Driver;
use super::temporal;
use super::value::{Affected, Row, Value};
use super::ConnectionConfig;
use crate::error::{Error, Result};

/// つなぐ。
pub(crate) async fn connect(settings: &ConnectionConfig) -> Result<Arc<dyn Backend>> {
    let options = build_options(settings)?;
    let pool = MySqlPoolOptions::new()
        .max_connections(settings.max_connections)
        // つながらないまま長く待たない。設定の間違いに早く気づけるようにする。
        .acquire_timeout(Duration::from_secs(30))
        .connect_with(options)
        .await
        .map_err(|e| {
            Error::msg(format!(
                "MySQL につなげません（{}:{} / {}）: {e}",
                settings.host, settings.port, settings.database
            ))
        })?;
    Ok(Arc::new(MySqlBackend { pool }))
}

/// 接続の指定を組み立てる。
///
/// `url` が空でなければそちらを使います。空のときは host / port / username /
/// password / database から組み立てます。
fn build_options(settings: &ConnectionConfig) -> Result<MySqlConnectOptions> {
    if !settings.url.is_empty() {
        return MySqlConnectOptions::from_str(&settings.url)
            .map_err(|e| Error::msg(format!("接続文字列が読めません（{}）: {e}", settings.url)));
    }
    let mut options = MySqlConnectOptions::new()
        .host(&settings.host)
        .port(settings.port)
        .username(&settings.username)
        .database(&settings.database);
    if !settings.password.is_empty() {
        options = options.password(&settings.password);
    }
    Ok(options)
}

/// MySQL の接続プール。
struct MySqlBackend {
    pool: MySqlPool,
}

impl Backend for MySqlBackend {
    fn driver(&self) -> Driver {
        Driver::MySql
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
            Ok(Box::new(MySqlTx { tx }) as Box<dyn RawTx>)
        })
    }

    fn table_names(&self) -> DbFuture<'_, Vec<String>> {
        Box::pin(async move {
            // `database()` はいまつないでいるデータベース。他のデータベースは見ません。
            let sql = "select table_name as name from information_schema.tables \
                       where table_schema = database() and table_type = 'BASE TABLE' \
                       order by table_name";
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
struct MySqlTx {
    tx: sqlx::Transaction<'static, MySql>,
}

impl RawTx for MySqlTx {
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
/// `last_insert_id()` は自動採番が動かなかったときに 0 を返します。
/// 0 は番号として使われないので、そのまま「無し」と見なせます。
///
/// 番号が `i64` に収まらないときも「無し」にします。**黙って丸めません。**
fn affected(result: &MySqlQueryResult) -> Affected {
    let id = result.last_insert_id();
    Affected {
        rows: result.rows_affected(),
        last_insert_id: (id > 0).then(|| i64::try_from(id).ok()).flatten(),
    }
}

/// 値をプレースホルダへ渡す。
fn bind<'q>(
    mut query: sqlx::query::Query<'q, MySql, MySqlArguments>,
    bindings: &'q [Value],
) -> sqlx::query::Query<'q, MySql, MySqlArguments> {
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

/// sqlx の行をまとめて `Row` に直す。列の名前は最初の行から1回だけ作ります。
fn convert_rows(rows: &[MySqlRow]) -> Result<Vec<Row>> {
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

fn convert_row(row: &MySqlRow, columns: &Arc<[String]>) -> Result<Row> {
    let mut values = Vec::with_capacity(columns.len());
    for (index, name) in columns.iter().enumerate() {
        values.push(convert_value(row, index, name)?);
    }
    Ok(Row::with_columns(Arc::clone(columns), values))
}

/// 1つの値を `Value` に直す。
///
/// MySQL は列の型が決まっているので、その型名で分かれます。
/// 知らない型名のときは、列の名前を添えて断ります。**黙って別の値にしません。**
fn convert_value(row: &MySqlRow, index: usize, name: &str) -> Result<Value> {
    let raw = row
        .try_get_raw(index)
        .map_err(|e| Error::msg(format!("列 `{name}` を読めません: {e}")))?;
    if raw.is_null() {
        return Ok(Value::Null);
    }
    let type_name = raw.type_info().name().to_string();
    match type_name.as_str() {
        // `tinyint(1)` は MySQL では真偽値の置き場所。sqlx が BOOLEAN として伝えます。
        "BOOLEAN" => read::<bool>(row, index, name).map(Value::Bool),
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" | "YEAR" | "TINYINT UNSIGNED"
        | "SMALLINT UNSIGNED" | "MEDIUMINT UNSIGNED" | "INT UNSIGNED" => {
            read::<i64>(row, index, name).map(Value::Int)
        }
        // `bigint unsigned` は `i64` の上限を超えられます。
        // 超えたときは丸めずに文字列で返します。
        "BIGINT UNSIGNED" => {
            let v = read::<u64>(row, index, name)?;
            Ok(match i64::try_from(v) {
                Ok(fits) => Value::Int(fits),
                Err(_) => Value::Text(v.to_string()),
            })
        }
        "FLOAT" | "DOUBLE" => read::<f64>(row, index, name).map(Value::Float),
        // `decimal` は MySQL が文字列で送ってきます。桁を落とさないため、
        // 浮動小数にせず文字列のまま渡します。
        // `String` は `Type<MySql>` の照合では decimal と合わないので、
        // 照合をしない `try_get_unchecked` で取り出します。
        "DECIMAL" => read_unchecked::<String>(row, index, name)
            .map(|v| Value::Text(super::decimal::normalize(&v))),
        "CHAR" | "VARCHAR" | "TINYTEXT" | "TEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM" | "SET" => {
            read::<String>(row, index, name).map(Value::Text)
        }
        // JSON も文字列で送られます。`Value` に JSON 用の腕は作りません。
        "JSON" => read_unchecked::<String>(row, index, name).map(Value::Text),
        "DATE" => read::<sqlx::types::time::Date>(row, index, name)
            .map(|v| Value::Text(temporal::date_string(v))),
        "TIME" => read::<sqlx::types::time::Time>(row, index, name)
            .map(|v| Value::Text(temporal::time_string(v))),
        "DATETIME" | "TIMESTAMP" => read::<sqlx::types::time::PrimitiveDateTime>(row, index, name)
            .map(|v| Value::Text(temporal::date_time_string(v))),
        "BINARY" | "VARBINARY" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB" | "BIT"
        | "GEOMETRY" => read::<Vec<u8>>(row, index, name).map(Value::Bytes),
        "NULL" => Ok(Value::Null),
        other => Err(unsupported_column(name, other)),
    }
}

/// 型が分かっている値を取り出す。
fn read<'r, T>(row: &'r MySqlRow, index: usize, name: &str) -> Result<T>
where
    T: sqlx::Decode<'r, MySql> + sqlx::Type<MySql>,
{
    row.try_get::<T, _>(index)
        .map_err(|e| Error::msg(format!("列 `{name}` を読めません: {e}")))
}

/// 型の照合をせずに取り出す。`decimal` と `json` だけで使います。
///
/// どちらも中身は文字列で送られてきますが、`String` の型照合では弾かれます。
fn read_unchecked<'r, T>(row: &'r MySqlRow, index: usize, name: &str) -> Result<T>
where
    T: sqlx::Decode<'r, MySql>,
{
    row.try_get_unchecked::<T, _>(index)
        .map_err(|e| Error::msg(format!("列 `{name}` を読めません: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Schema;

    /// つなぎ先。`BENGARA_TEST_MYSQL_URL` が入っているときだけテストが走ります。
    ///
    /// **サーバが無い所では何もしません。** CI に MySQL を置いていないためです。
    /// 手元で確かめるときは、例えば次のように書きます。
    ///
    /// ```sh
    /// BENGARA_TEST_MYSQL_URL=mysql://bengara:secret@127.0.0.1:13306/bengara \
    ///   cargo test -p bengara --features mysql
    /// ```
    fn url() -> Option<String> {
        std::env::var("BENGARA_TEST_MYSQL_URL")
            .ok()
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty())
    }

    async fn db() -> Option<Arc<dyn Backend>> {
        let url = url()?;
        let settings = ConnectionConfig::mysql("test", "").url(url);
        Some(connect(&settings).await.expect("MySQL につながる"))
    }

    /// 表を作り直す。テストごとに別の名前を使うので、並行して走っても混ざりません。
    async fn fresh(
        db: &Arc<dyn Backend>,
        table: &str,
        build: impl FnOnce(&mut crate::database::Blueprint),
    ) {
        db.execute(&format!("drop table if exists `{table}`"), &[])
            .await
            .expect("消せる");
        let mut schema = Schema::new(Driver::MySql);
        schema.create(table, build);
        for sql in schema.into_statements() {
            db.execute(&sql, &[]).await.expect("表が作れる");
        }
    }

    #[tokio::test]
    async fn 型をひととおり往復できる() {
        let Some(db) = db().await else { return };
        let table = "bengara_types";
        fresh(&db, table, |t| {
            t.id();
            t.string("title");
            t.text("body").nullable();
            t.integer("views").default(0);
            t.big_integer("big").nullable();
            t.boolean("published").default(false);
            t.double("ratio").nullable();
            t.decimal("total", 8, 2).nullable();
            t.date("day").nullable();
            t.date_time("at").nullable();
            t.json("meta").nullable();
            t.binary("blob").nullable();
        })
        .await;

        let sql = format!(
            "insert into `{table}` \
             (`title`, `body`, `views`, `big`, `published`, `ratio`, `total`, \
              `day`, `at`, `meta`, `blob`) \
             values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        );
        let affected = db
            .execute(
                &sql,
                &[
                    Value::Text("やきそば".into()),
                    Value::Null,
                    Value::Int(7),
                    Value::Int(i64::from(i32::MAX) + 1),
                    Value::Bool(true),
                    Value::Float(1.5),
                    // decimal には文字列で渡す。桁を落とさないため。
                    Value::Text("1234.56".into()),
                    Value::Text("2026-10-09".into()),
                    Value::Text("2026-10-09 12:34:56".into()),
                    Value::Text("{\"a\": 1}".into()),
                    Value::Bytes(vec![1, 2, 3]),
                ],
            )
            .await
            .expect("1件入る");
        assert_eq!(affected.rows, 1);
        assert_eq!(affected.last_insert_id, Some(1), "自動採番の番号が返る");

        let rows = db
            .fetch_all(&format!("select * from `{table}`"), &[])
            .await
            .expect("読める");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.value("id"), Some(&Value::Int(1)));
        assert_eq!(row.value("title"), Some(&Value::Text("やきそば".into())));
        assert_eq!(row.value("body"), Some(&Value::Null));
        assert_eq!(row.value("views"), Some(&Value::Int(7)));
        assert_eq!(
            row.value("big"),
            Some(&Value::Int(i64::from(i32::MAX) + 1)),
            "bigint が int に丸まらない"
        );
        assert_eq!(
            row.value("published"),
            Some(&Value::Bool(true)),
            "tinyint(1) は真偽値になる"
        );
        assert_eq!(row.value("ratio"), Some(&Value::Float(1.5)));
        assert_eq!(
            row.value("total"),
            Some(&Value::Text("1234.56".into())),
            "decimal は桁を落とさず文字列で返る"
        );
        // 末尾の 0 は落ちます。PostgreSQL と同じ文字列にするためです。
        let rows = db
            .fetch_all(
                &format!("select cast(12.5 as decimal(8,2)) as n from `{table}`"),
                &[],
            )
            .await
            .unwrap();
        assert_eq!(rows[0].value("n"), Some(&Value::Text("12.5".into())));
        assert_eq!(row.value("day"), Some(&Value::Text("2026-10-09".into())));
        assert_eq!(
            row.value("at"),
            Some(&Value::Text("2026-10-09 12:34:56".into())),
            "datetime は now() と同じ形で返る"
        );
        // JSON は `row.get::<String>()` で読めます。
        // **生の `Value` の腕はサーバで違います。** MySQL は `Text`、MariaDB は
        // `Bytes` になります。MariaDB の `json` は照合順序が `_bin` の
        // `longtext` で、MySQL の通信の決まりでは「バイナリ」と伝わるためです。
        assert_eq!(row.get::<String>("meta").unwrap(), "{\"a\": 1}");
        assert_eq!(row.value("blob"), Some(&Value::Bytes(vec![1, 2, 3])));

        assert!(db.table_names().await.unwrap().contains(&table.to_string()));
    }

    #[tokio::test]
    async fn 行が変わらないときは採番の番号を返さない() {
        let Some(db) = db().await else { return };
        let table = "bengara_affected";
        fresh(&db, table, |t| {
            t.id();
            t.integer("a").default(0);
        })
        .await;

        let inserted = db
            .execute(&format!("insert into `{table}` (`a`) values (1)"), &[])
            .await
            .unwrap();
        assert_eq!(inserted.last_insert_id, Some(1));

        let updated = db
            .execute(&format!("update `{table}` set `a` = 2 where `a` = 99"), &[])
            .await
            .unwrap();
        assert_eq!(updated.rows, 0);
        assert_eq!(updated.last_insert_id, None);
    }

    #[tokio::test]
    async fn トランザクションは確定しないと残らない() {
        let Some(db) = db().await else { return };
        let table = "bengara_tx";
        fresh(&db, table, |t| {
            t.integer("a");
        })
        .await;
        let count = format!("select count(*) as n from `{table}`");

        let mut tx = db.begin().await.unwrap();
        tx.execute(&format!("insert into `{table}` (`a`) values (1)"), &[])
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        let rows = db.fetch_all(&count, &[]).await.unwrap();
        assert_eq!(rows[0].value("n"), Some(&Value::Int(0)), "巻き戻した");

        let mut tx = db.begin().await.unwrap();
        tx.execute(&format!("insert into `{table}` (`a`) values (2)"), &[])
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let rows = db.fetch_all(&count, &[]).await.unwrap();
        assert_eq!(rows[0].value("n"), Some(&Value::Int(1)));
    }

    #[tokio::test]
    async fn 一意制約違反が分かる() {
        let Some(db) = db().await else { return };
        let table = "bengara_unique";
        fresh(&db, table, |t| {
            t.string("email").unique();
        })
        .await;

        let sql = format!("insert into `{table}` (`email`) values (?)");
        let value = [Value::Text("a@example.com".into())];
        db.execute(&sql, &value).await.expect("1件目は入る");
        let error = db.execute(&sql, &value).await.unwrap_err();
        assert!(
            crate::database::is_unique_violation(&error),
            "2件目は一意制約違反: {error}"
        );
    }

    #[tokio::test]
    async fn 失敗したsqlは文を添えて知らせる() {
        let Some(db) = db().await else { return };
        let error = db
            .fetch_all("select * from bengara_nope", &[])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("SQL の実行に失敗しました"), "{error}");
        assert!(error.contains("bengara_nope"), "{error}");
    }
    #[tokio::test]
    async fn 外部キーでつながった表も全部消せる() {
        let Some(db) = db().await else { return };
        // `migrator.rs` の drop_all_tables と同じ道（grammar）を通します。
        let parent = "bengara_drop_parent";
        let child = "bengara_drop_child";
        for table in [child, parent] {
            db.execute(&format!("drop table if exists `{table}`"), &[])
                .await
                .unwrap();
        }
        db.execute(
            &format!("create table `{parent}` (`id` bigint unsigned not null primary key)"),
            &[],
        )
        .await
        .unwrap();
        db.execute(
            &format!(
                "create table `{child}` (`parent_id` bigint unsigned not null, \
                 constraint `fk_{child}` foreign key (`parent_id`) references `{parent}` (`id`))"
            ),
            &[],
        )
        .await
        .unwrap();

        // 親から先に消す。確かめを止めないと外部キーに引っかかります。
        let tables = vec![parent.to_string(), child.to_string()];
        let statements = super::super::grammar::drop_all(Driver::MySql, &tables);
        let (before, after) = super::super::grammar::defer_foreign_keys(Driver::MySql);

        let mut tx = db.begin().await.unwrap();
        tx.execute(before.expect("MySQL は止める文がある"), &[])
            .await
            .unwrap();
        for sql in &statements {
            tx.execute(sql, &[]).await.expect("消せる");
        }
        tx.execute(after.expect("MySQL は戻す文がある"), &[])
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let names = db.table_names().await.unwrap();
        assert!(!names.contains(&parent.to_string()), "{names:?}");
        assert!(!names.contains(&child.to_string()), "{names:?}");
    }
}
