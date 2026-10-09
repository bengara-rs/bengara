//! PostgreSQL の下回り（sqlx）。
//!
//! **sqlx を知っているのはこのファイルだけです。** 上の層には SQL の文字列と
//! `Value` / `Row` しか渡しません。
//!
//! PostgreSQL は値を渡す場所の型に厳しいデータベースです。`$1` の型と列の型が
//! 合わないと、入れる前に断られます。bengara が作る表では、日付と JSON を
//! `text` 列にしてこれを避けています（`schema.rs` の `type_sql`）。

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::{PgArguments, PgConnectOptions, PgPool, PgPoolOptions, PgQueryResult, PgRow};
use sqlx::{Column, Postgres, Row as SqlxRow, TypeInfo, ValueRef};

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
    let pool = PgPoolOptions::new()
        .max_connections(settings.max_connections)
        // つながらないまま長く待たない。設定の間違いに早く気づけるようにする。
        .acquire_timeout(Duration::from_secs(30))
        .connect_with(options)
        .await
        .map_err(|e| {
            Error::msg(format!(
                "PostgreSQL につなげません（{}:{} / {}）: {e}",
                settings.host, settings.port, settings.database
            ))
        })?;
    Ok(Arc::new(PgBackend { pool }))
}

/// 接続の指定を組み立てる。
///
/// `url` が空でなければそちらを使います。空のときは host / port / username /
/// password / database から組み立てます。
fn build_options(settings: &ConnectionConfig) -> Result<PgConnectOptions> {
    if !settings.url.is_empty() {
        return PgConnectOptions::from_str(&settings.url)
            .map_err(|e| Error::msg(format!("接続文字列が読めません（{}）: {e}", settings.url)));
    }
    let mut options = PgConnectOptions::new()
        .host(&settings.host)
        .port(settings.port)
        .username(&settings.username)
        .database(&settings.database);
    if !settings.password.is_empty() {
        options = options.password(&settings.password);
    }
    Ok(options)
}

/// PostgreSQL の接続プール。
struct PgBackend {
    pool: PgPool,
}

impl Backend for PgBackend {
    fn driver(&self) -> Driver {
        Driver::Postgres
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
            Ok(Box::new(PgTx { tx }) as Box<dyn RawTx>)
        })
    }

    fn table_names(&self) -> DbFuture<'_, Vec<String>> {
        Box::pin(async move {
            // `current_schema()` はいま使っているスキーマ（ふつうは `public`）。
            let sql = "select tablename as name from pg_catalog.pg_tables \
                       where schemaname = current_schema() order by tablename";
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
struct PgTx {
    tx: sqlx::Transaction<'static, Postgres>,
}

impl RawTx for PgTx {
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
/// **PostgreSQL には「最後に入れた行の番号」を返す仕組みがありません。**
/// `insert_get_id()` は `returning` を付けて取ります（`query.rs`）。
/// ここで作り話の番号を返さないよう、必ず `None` にします。
fn affected(result: &PgQueryResult) -> Affected {
    Affected {
        rows: result.rows_affected(),
        last_insert_id: None,
    }
}

/// 値をプレースホルダへ渡す。
fn bind<'q>(
    mut query: sqlx::query::Query<'q, Postgres, PgArguments>,
    bindings: &'q [Value],
) -> sqlx::query::Query<'q, Postgres, PgArguments> {
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
fn convert_rows(rows: &[PgRow]) -> Result<Vec<Row>> {
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

fn convert_row(row: &PgRow, columns: &Arc<[String]>) -> Result<Row> {
    let mut values = Vec::with_capacity(columns.len());
    for (index, name) in columns.iter().enumerate() {
        values.push(convert_value(row, index, name)?);
    }
    Ok(Row::with_columns(Arc::clone(columns), values))
}

/// 1つの値を `Value` に直す。
///
/// PostgreSQL は列の型が決まっているので、その型名で分かれます。
/// 知らない型名のときは、列の名前を添えて断ります。**黙って別の値にしません。**
fn convert_value(row: &PgRow, index: usize, name: &str) -> Result<Value> {
    let raw = row
        .try_get_raw(index)
        .map_err(|e| Error::msg(format!("列 `{name}` を読めません: {e}")))?;
    if raw.is_null() {
        return Ok(Value::Null);
    }
    let type_name = raw.type_info().name().to_string();
    match type_name.as_str() {
        "BOOL" => read::<bool>(row, index, name).map(Value::Bool),
        // 整数は幅ごとに型が違います。`i64` ひとつでは照合が通りません。
        "INT2" => read::<i16>(row, index, name).map(|v| Value::Int(i64::from(v))),
        "INT4" => read::<i32>(row, index, name).map(|v| Value::Int(i64::from(v))),
        "INT8" => read::<i64>(row, index, name).map(Value::Int),
        "FLOAT4" => read::<f32>(row, index, name).map(|v| Value::Float(f64::from(v))),
        "FLOAT8" => read::<f64>(row, index, name).map(Value::Float),
        // `numeric` は桁を落とさないよう文字列で渡します。
        // `t.decimal()` が作る列なので、読めないままにはできません。
        "NUMERIC" => read::<sqlx::types::BigDecimal>(row, index, name)
            .map(|v| Value::Text(super::decimal::normalize(&v.to_string()))),
        // `CHAR` は `char(n)`（bpchar）です。1バイトの内部型は `"CHAR"` という
        // 別の名前になるので、ここには来ません。
        "TEXT" | "VARCHAR" | "CHAR" | "NAME" | "UNKNOWN" => {
            read::<String>(row, index, name).map(Value::Text)
        }
        "BYTEA" => read::<Vec<u8>>(row, index, name).map(Value::Bytes),
        "DATE" => read::<sqlx::types::time::Date>(row, index, name)
            .map(|v| Value::Text(temporal::date_string(v))),
        "TIME" => read::<sqlx::types::time::Time>(row, index, name)
            .map(|v| Value::Text(temporal::time_string(v))),
        "TIMESTAMP" => read::<sqlx::types::time::PrimitiveDateTime>(row, index, name)
            .map(|v| Value::Text(temporal::date_time_string(v))),
        "TIMESTAMPTZ" => read::<sqlx::types::time::OffsetDateTime>(row, index, name)
            .map(|v| Value::Text(temporal::offset_date_time_string(v))),
        // JSON は文字列にして渡します。`Value` に JSON 用の腕は作りません。
        "JSON" | "JSONB" => {
            read::<serde_json::Value>(row, index, name).map(|v| Value::Text(v.to_string()))
        }
        other => Err(unsupported_column(name, other)),
    }
}

/// 型が分かっている値を取り出す。
fn read<'r, T>(row: &'r PgRow, index: usize, name: &str) -> Result<T>
where
    T: sqlx::Decode<'r, Postgres> + sqlx::Type<Postgres>,
{
    row.try_get::<T, _>(index)
        .map_err(|e| Error::msg(format!("列 `{name}` を読めません: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Schema;

    /// つなぎ先。`BENGARA_TEST_POSTGRES_URL` が入っているときだけテストが走ります。
    ///
    /// **サーバが無い所では何もしません。** CI に PostgreSQL を置いていないためです。
    /// 手元で確かめるときは、例えば次のように書きます。
    ///
    /// ```sh
    /// BENGARA_TEST_POSTGRES_URL=postgres://bengara:secret@127.0.0.1:15432/bengara \
    ///   cargo test -p bengara --features postgres
    /// ```
    fn url() -> Option<String> {
        std::env::var("BENGARA_TEST_POSTGRES_URL")
            .ok()
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty())
    }

    async fn db() -> Option<Arc<dyn Backend>> {
        let url = url()?;
        let settings = ConnectionConfig::postgres("test", "").url(url);
        Some(connect(&settings).await.expect("PostgreSQL につながる"))
    }

    /// 表を作り直す。テストごとに別の名前を使うので、並行して走っても混ざりません。
    async fn fresh(
        db: &Arc<dyn Backend>,
        table: &str,
        build: impl FnOnce(&mut crate::database::Blueprint),
    ) {
        db.execute(&format!("drop table if exists \"{table}\" cascade"), &[])
            .await
            .expect("消せる");
        let mut schema = Schema::new(Driver::Postgres);
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

        // PostgreSQL は自動採番の番号を `returning` で受け取ります。
        let sql = format!(
            "insert into \"{table}\" \
             (\"title\", \"body\", \"views\", \"big\", \"published\", \"ratio\", \"total\", \
              \"day\", \"at\", \"meta\", \"blob\") \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) returning \"id\""
        );
        let returned = db
            .fetch_all(
                &sql,
                &[
                    Value::Text("やきそば".into()),
                    Value::Null,
                    Value::Int(7),
                    Value::Int(i64::from(i32::MAX) + 1),
                    Value::Bool(true),
                    Value::Float(1.5),
                    // numeric には数で渡します。文字列は PostgreSQL が受け取りません。
                    Value::Float(1234.56),
                    // date と date_time と json は text 列です（`schema.rs` の type_sql）。
                    Value::Text("2026-10-09".into()),
                    Value::Text("2026-10-09 12:34:56".into()),
                    Value::Text("{\"a\": 1}".into()),
                    Value::Bytes(vec![1, 2, 3]),
                ],
            )
            .await
            .expect("1件入る");
        assert_eq!(returned[0].value("id"), Some(&Value::Int(1)));

        let rows = db
            .fetch_all(&format!("select * from \"{table}\""), &[])
            .await
            .expect("読める");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.value("id"), Some(&Value::Int(1)));
        assert_eq!(row.value("title"), Some(&Value::Text("やきそば".into())));
        assert_eq!(row.value("body"), Some(&Value::Null));
        assert_eq!(
            row.value("views"),
            Some(&Value::Int(7)),
            "int4 も Int になる"
        );
        assert_eq!(row.value("big"), Some(&Value::Int(i64::from(i32::MAX) + 1)));
        assert_eq!(row.value("published"), Some(&Value::Bool(true)));
        assert_eq!(row.value("ratio"), Some(&Value::Float(1.5)));
        assert_eq!(
            row.value("total"),
            Some(&Value::Text("1234.56".into())),
            "numeric は桁を落とさず文字列で返る"
        );
        assert_eq!(row.value("day"), Some(&Value::Text("2026-10-09".into())));
        assert_eq!(
            row.value("at"),
            Some(&Value::Text("2026-10-09 12:34:56".into()))
        );
        assert_eq!(row.value("meta"), Some(&Value::Text("{\"a\": 1}".into())));
        assert_eq!(row.value("blob"), Some(&Value::Bytes(vec![1, 2, 3])));

        assert!(db.table_names().await.unwrap().contains(&table.to_string()));
    }

    #[tokio::test]
    async fn 本物の日時と_numeric_の列も読める() {
        // bengara が作る表では使いませんが、既にあるデータベースにつなぐときに通ります。
        let Some(db) = db().await else { return };
        let table = "bengara_native";
        db.execute(&format!("drop table if exists \"{table}\""), &[])
            .await
            .unwrap();
        db.execute(
            &format!(
                "create table \"{table}\" (\
                 d date, t time, ts timestamp, tz timestamptz, n numeric(8,2), j json)"
            ),
            &[],
        )
        .await
        .unwrap();
        // 値は $1 では渡せません（text から date へは PostgreSQL が自動で直さない）。
        // 既にある列を**読める**ことだけを確かめるので、SQL に直接書きます。
        db.execute(
            &format!(
                "insert into \"{table}\" values \
                 (date '2026-10-09', time '12:34:56', \
                 timestamp '2026-10-09 12:34:56', \
                 timestamptz '2026-10-09 09:00:00+09', 12.5, '{{\"a\":1}}')"
            ),
            &[],
        )
        .await
        .unwrap();

        let rows = db
            .fetch_all(&format!("select * from \"{table}\""), &[])
            .await
            .expect("読める");
        let row = &rows[0];
        assert_eq!(row.value("d"), Some(&Value::Text("2026-10-09".into())));
        assert_eq!(row.value("t"), Some(&Value::Text("12:34:56".into())));
        assert_eq!(
            row.value("ts"),
            Some(&Value::Text("2026-10-09 12:34:56".into()))
        );
        assert_eq!(
            row.value("tz"),
            Some(&Value::Text("2026-10-09 00:00:00".into())),
            "時差付きは UTC に直して返る"
        );
        // 末尾の 0 は落ちます（MySQL と同じ文字列にするため）。
        assert_eq!(row.value("n"), Some(&Value::Text("12.5".into())));
        assert_eq!(row.value("j"), Some(&Value::Text("{\"a\":1}".into())));
    }

    #[tokio::test]
    async fn 更新系は採番の番号を返さない() {
        let Some(db) = db().await else { return };
        let table = "bengara_affected";
        fresh(&db, table, |t| {
            t.id();
            t.integer("a").default(0);
        })
        .await;

        let inserted = db
            .execute(&format!("insert into \"{table}\" (\"a\") values (1)"), &[])
            .await
            .unwrap();
        assert_eq!(inserted.rows, 1);
        assert_eq!(
            inserted.last_insert_id, None,
            "PostgreSQL には最後の番号を返す仕組みが無い"
        );

        let updated = db
            .execute(
                &format!("update \"{table}\" set \"a\" = 2 where \"a\" = 99"),
                &[],
            )
            .await
            .unwrap();
        assert_eq!(updated.rows, 0);
    }

    #[tokio::test]
    async fn トランザクションは確定しないと残らない() {
        let Some(db) = db().await else { return };
        let table = "bengara_tx";
        fresh(&db, table, |t| {
            t.integer("a");
        })
        .await;
        let count = format!("select count(*) as n from \"{table}\"");

        let mut tx = db.begin().await.unwrap();
        tx.execute(&format!("insert into \"{table}\" (\"a\") values (1)"), &[])
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        let rows = db.fetch_all(&count, &[]).await.unwrap();
        assert_eq!(rows[0].value("n"), Some(&Value::Int(0)), "巻き戻した");

        let mut tx = db.begin().await.unwrap();
        tx.execute(&format!("insert into \"{table}\" (\"a\") values (2)"), &[])
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

        let sql = format!("insert into \"{table}\" (\"email\") values ($1)");
        let value = [Value::Text("a@example.com".into())];
        db.execute(&sql, &value).await.expect("1件目は入る");
        let error = db.execute(&sql, &value).await.unwrap_err();
        assert!(
            crate::database::is_unique_violation(&error),
            "2件目は一意制約違反: {error}"
        );
    }

    #[tokio::test]
    async fn 読めない型は列名を添えて断る() {
        let Some(db) = db().await else { return };
        // uuid は `Value` に直す道を作っていません。黙って別の値にせず断ります。
        let error = db
            .fetch_all(
                "select '00000000-0000-0000-0000-000000000000'::uuid as u",
                &[],
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("`u`"), "{error}");
        assert!(error.contains("UUID"), "{error}");
        assert!(error.contains("cast"), "{error}");
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
            db.execute(&format!("drop table if exists \"{table}\" cascade"), &[])
                .await
                .unwrap();
        }
        db.execute(
            &format!("create table \"{parent}\" (\"id\" bigint primary key)"),
            &[],
        )
        .await
        .unwrap();
        db.execute(
            &format!(
                "create table \"{child}\" (\"parent_id\" bigint not null \
                 references \"{parent}\" (\"id\"))"
            ),
            &[],
        )
        .await
        .unwrap();

        // 親から先に並べる。`cascade` が無いと外部キーに引っかかります。
        let tables = vec![parent.to_string(), child.to_string()];
        let statements = super::super::grammar::drop_all(Driver::Postgres, &tables);
        assert_eq!(statements.len(), 1, "PostgreSQL は1文にまとめる");
        let (before, after) = super::super::grammar::defer_foreign_keys(Driver::Postgres);
        assert!(before.is_none() && after.is_none(), "止める文は要らない");

        let mut tx = db.begin().await.unwrap();
        for sql in &statements {
            tx.execute(sql, &[]).await.expect("消せる");
        }
        tx.commit().await.unwrap();

        let names = db.table_names().await.unwrap();
        assert!(!names.contains(&parent.to_string()), "{names:?}");
        assert!(!names.contains(&child.to_string()), "{names:?}");
    }
}
