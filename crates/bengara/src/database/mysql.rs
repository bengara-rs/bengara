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
            // `url` でつないでいるときに host / port を出すと、使っていない既定値
            // （`127.0.0.1:3306`）を見せる。
            Error::msg(format!(
                "MySQL につなげません（{}）: {e}",
                super::connection_target(settings)
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
        // 接続文字列そのものは出さない（`unreadable_url` の説明を参照）。
        return MySqlConnectOptions::from_str(&settings.url)
            .map_err(|e| super::unreadable_url(settings, e));
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
            let tx = self.pool.begin().await.map_err(|e| failed("begin", e))?;
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
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" => {
            read::<i64>(row, index, name).map(Value::Int)
        }
        // 符号なしの整数は `u64` で読みます。sqlx の `i64` は UNSIGNED の旗が付いた列を
        // 断るので（sqlx-mysql `types/int.rs` の `int_compatible`）、以前はここで
        // 「列を読めません」になり、**その列を含む行ごと**読めませんでした。
        // bengara が作る表は `bigint unsigned` だけですが、既にある表
        // （Laravel の `unsignedInteger` など）で当たります。
        //
        // `bigint unsigned` は `i64` の上限を超えられます。
        // 超えたときは丸めずに文字列で返します。
        "TINYINT UNSIGNED" | "SMALLINT UNSIGNED" | "MEDIUMINT UNSIGNED" | "INT UNSIGNED"
        | "BIGINT UNSIGNED" => read::<u64>(row, index, name).map(unsigned_value),
        // `year` と `bit` も `u64` で読みます。照合はしません。
        //
        // sqlx の `u64` が受けるのは UNSIGNED の旗が付いているときだけです
        // （`types/uint.rs` の `uint_compatible`）。旗が付くかはサーバ次第なので、
        // 旗に頼らず中身だけを読みます。読み方は型で決まっています（`uint_decode`）。
        //
        // | 型     | 中身                                          | 返す値                |
        // |--------|-----------------------------------------------|-----------------------|
        // | `year` | 数（文字列なら数字、バイナリなら 2 バイト）   | `Value::Int(2026)`    |
        // | `bit`  | 上の桁から並んだバイト列（最大 64 ビット）    | `Value::Int`（数として） |
        "YEAR" | "BIT" => read_unchecked::<u64>(row, index, name).map(unsigned_value),
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
        "BINARY" | "VARBINARY" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB" => {
            read::<Vec<u8>>(row, index, name).map(Value::Bytes)
        }
        // 空間の型（`geometry` / `point` など）は、MySQL の内部の形のバイト列で届きます
        // （先頭 4 バイトが SRID、その後ろが WKB）。sqlx の `Vec<u8>` は型の照合で
        // `Geometry` を受けないので（`types/bytes.rs`）、照合をせずに取り出します。
        // 中身の解釈はしません。読みたい形があるときは `st_astext(列)` を SQL に書きます。
        "GEOMETRY" => read_unchecked::<Vec<u8>>(row, index, name).map(Value::Bytes),
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

/// 符号なしの整数を `Value` に直す。
///
/// `i64` に収まれば `Value::Int`、収まらなければ**丸めずに** `Value::Text` にします
/// （`bigint unsigned` と 64 ビットの `bit` だけが超えられます）。
fn unsigned_value(v: u64) -> Value {
    match i64::try_from(v) {
        Ok(fits) => Value::Int(fits),
        Err(_) => Value::Text(v.to_string()),
    }
}

/// 型の照合をせずに取り出す。sqlx の照合が受けない組み合わせで使います。
///
/// | 列の型             | 取り出す型 | 照合が通らない理由                       |
/// |--------------------|------------|------------------------------------------|
/// | `decimal` / `json` | `String`   | 中身は文字列だが、`String` の照合に無い  |
/// | `year` / `bit`     | `u64`      | UNSIGNED の旗が付いているときしか通らない |
/// | `geometry`         | `Vec<u8>`  | `Vec<u8>` の照合に無い                   |
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

    #[test]
    fn 符号なしの整数は収まれば整数_超えたら文字列で返す() {
        assert_eq!(unsigned_value(0), Value::Int(0));
        assert_eq!(unsigned_value(255), Value::Int(255));
        assert_eq!(unsigned_value(i64::MAX as u64), Value::Int(i64::MAX));
        // `i64` を超えたら丸めない。
        assert_eq!(
            unsigned_value(i64::MAX as u64 + 1),
            Value::Text("9223372036854775808".into())
        );
        assert_eq!(unsigned_value(u64::MAX), Value::Text(u64::MAX.to_string()));
    }

    #[test]
    fn 読めない接続文字列をエラー文に出さない() {
        // パスワードに `#` をそのまま書くと、ポートが読めなくなる典型の形。
        // 以前は `接続文字列が読めません（{url}）` とパスワードごと出していた。
        let url = "mysql://app:p4ss#word@db.example.com:3306/shop";
        let settings = ConnectionConfig::mysql("mysql", "").url(url);
        let message = build_options(&settings).expect_err("読めない").to_string();
        assert!(message.contains("`mysql`"), "{message}");
        assert!(message.contains("読めません"), "{message}");
        assert!(!message.contains("p4ss"), "{message}");
        assert!(!message.contains("word"), "{message}");
        assert!(!message.contains("db.example.com"), "{message}");
    }

    /// 符号なしの小さい整数・`year`・`bit`・空間の型が読めること。
    ///
    /// bengara の `Schema` はこれらの型を作りません。既にある表
    /// （Laravel の `unsignedInteger` など）につないだときに当たるので、型を手で書きます。
    /// 以前は 1 列でも含むと、行ごと「列を読めません」になっていました。
    #[tokio::test]
    async fn 符号なしの整数と_year_と_bit_と空間の型を読める() {
        let Some(db) = db().await else { return };
        let table = "bengara_unsigned";
        db.execute(&format!("drop table if exists `{table}`"), &[])
            .await
            .unwrap();
        db.execute(
            &format!(
                "create table `{table}` (\
                 `t` tinyint unsigned, `s` smallint unsigned, `m` mediumint unsigned, \
                 `i` int unsigned, `b` bigint unsigned, `big` bigint unsigned, \
                 `y` year, `flag` bit(1), `bits` bit(12), `wide` bit(64), \
                 `p` point)"
            ),
            &[],
        )
        .await
        .unwrap();
        db.execute(
            &format!(
                "insert into `{table}` values (\
                 255, 65535, 16777215, 4294967295, 7, 18446744073709551615, \
                 2026, b'1', b'101000000001', b'1{}', \
                 st_geomfromtext('POINT(1 2)'))",
                "0".repeat(63)
            ),
            &[],
        )
        .await
        .unwrap();

        let rows = db
            .fetch_all(&format!("select * from `{table}`"), &[])
            .await
            .expect("行ごと読める");
        let row = &rows[0];
        assert_eq!(row.value("t"), Some(&Value::Int(255)));
        assert_eq!(row.value("s"), Some(&Value::Int(65535)));
        assert_eq!(row.value("m"), Some(&Value::Int(16_777_215)));
        assert_eq!(
            row.value("i"),
            Some(&Value::Int(4_294_967_295)),
            "int unsigned の上限は i32 を超える"
        );
        assert_eq!(row.value("b"), Some(&Value::Int(7)));
        assert_eq!(
            row.value("big"),
            Some(&Value::Text("18446744073709551615".into())),
            "i64 を超えたら丸めずに文字列"
        );
        assert_eq!(row.value("y"), Some(&Value::Int(2026)));
        assert_eq!(row.value("flag"), Some(&Value::Int(1)));
        assert_eq!(row.value("bits"), Some(&Value::Int(0b1010_0000_0001)));
        assert_eq!(
            row.value("wide"),
            Some(&Value::Text((1u64 << 63).to_string())),
            "64 ビットの bit も丸めない"
        );
        // 空間の型は中身を解釈しない。先頭 4 バイトが SRID、後ろが WKB（点は 21 バイト）。
        match row.value("p") {
            Some(Value::Bytes(bytes)) => assert_eq!(bytes.len(), 25, "{bytes:?}"),
            other => panic!("バイト列で返る: {other:?}"),
        }

        db.execute(&format!("drop table if exists `{table}`"), &[])
            .await
            .unwrap();
    }

    /// 列の `unique()` で作った索引を、名前で落とせること。
    ///
    /// 以前は列の指定に `unique` と書いていたので、MySQL は索引の名前を列の名前
    /// （`email`）にしていました。`drop_index_named(表, "表_email_unique")` は
    /// 「索引がありません」で失敗していました。
    #[tokio::test]
    async fn 列の_unique_で作った索引を名前で落とせる() {
        let Some(db) = db().await else { return };
        let table = "bengara_unique_named";
        fresh(&db, table, |t| {
            t.id();
            t.string("email").unique();
        })
        .await;

        let sql = format!("insert into `{table}` (`email`) values (?)");
        let value = [Value::Text("a@example.com".into())];
        db.execute(&sql, &value).await.expect("1件目は入る");
        assert!(db.execute(&sql, &value).await.is_err(), "重なりは断る");

        let mut schema = Schema::new(Driver::MySql);
        schema.drop_unique(table, &["email"]);
        for statement in schema.into_statements() {
            db.execute(&statement, &[]).await.expect("名前で落とせる");
        }
        db.execute(&sql, &value)
            .await
            .expect("索引を落としたので重なっても入る");

        db.execute(&format!("drop table if exists `{table}`"), &[])
            .await
            .unwrap();
    }

    /// 表を消すのに失敗しても、外部キーの確かめを止めたままの接続がプールに戻らないこと。
    ///
    /// 接続を 1 本にして、失敗の後に同じ接続を借り直して確かめます。
    #[tokio::test]
    async fn 表を消すのに失敗しても外部キーの確かめは戻っている() {
        let Some(url) = url() else { return };
        let settings = ConnectionConfig::mysql("test", "")
            .url(url)
            .max_connections(1);
        let db = connect(&settings).await.expect("MySQL につながる");

        let (before, after) = super::super::grammar::defer_foreign_keys(Driver::MySql);
        // `if exists` の無い文で、無い表を消そうとして失敗させる。
        let statements = vec!["drop table `bengara_no_such_table_for_drop`".to_string()];
        let mut tx = db.begin().await.unwrap();
        let result =
            super::super::migrator::drop_with_checks_off(tx.as_mut(), before, &statements, after)
                .await;
        assert!(result.is_err(), "消す文は失敗する");
        drop(tx);

        let rows = db
            .fetch_all("select @@session.foreign_key_checks as v", &[])
            .await
            .unwrap();
        assert_eq!(
            rows[0].get::<i64>("v").unwrap(),
            1,
            "確かめを止めたままの接続がプールに戻っています"
        );
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

    /// `Schema` で組み立てた外部キーが、本物の MySQL で張れること。
    ///
    /// **型を手で書かず、必ず `Schema` を通してください。** `t.id()` は
    /// `bigint unsigned`、`t.foreign_id()` は以前 `bigint`（符号付き）だったので、
    /// MySQL は符号の違う列への外部キーを ERROR 3780 で断っていました。
    /// SQLite と PostgreSQL では通るうえ、ここのテストが型を手で書いていたので、
    /// サンプルアプリの `create_comments_table` が MySQL で失敗することに
    /// 気づけませんでした。
    #[tokio::test]
    async fn スキーマで作った外部キーが張れる() {
        let Some(db) = db().await else { return };
        let parent = "bengara_fk_parent";
        let child = "bengara_fk_child";
        // 子から先に消す。親を先に消そうとすると外部キーに引っかかります。
        for table in [child, parent] {
            db.execute(&format!("drop table if exists `{table}`"), &[])
                .await
                .unwrap();
        }

        fresh(&db, parent, |t| {
            t.id();
            t.string("name");
        })
        .await;

        let mut schema = Schema::new(Driver::MySql);
        schema.create(child, |t| {
            t.id();
            t.foreign_id("parent_id");
            t.foreign("parent_id").on(parent).cascade_on_delete();
        });
        for sql in schema.into_statements() {
            db.execute(&sql, &[])
                .await
                .expect("外部キー込みで表が作れる");
        }

        // 張れただけでなく、効いていることも確かめます。
        db.execute(
            &format!("insert into `{parent}` (`name`) values (?)"),
            &["親".into()],
        )
        .await
        .unwrap();
        db.execute(
            &format!("insert into `{child}` (`parent_id`) values (1)"),
            &[],
        )
        .await
        .expect("親がいる子は入る");
        // 居ない親を指す子は断られる。
        assert!(
            db.execute(
                &format!("insert into `{child}` (`parent_id`) values (999)"),
                &[],
            )
            .await
            .is_err(),
            "外部キーが効いていません"
        );
        // 親を消すと子も消える（cascade_on_delete）。
        db.execute(&format!("delete from `{parent}` where `id` = 1"), &[])
            .await
            .unwrap();
        let rows = db
            .fetch_all(&format!("select count(*) as n from `{child}`"), &[])
            .await
            .unwrap();
        assert_eq!(
            rows[0].get::<i64>("n").unwrap(),
            0,
            "親と一緒に消えていません"
        );

        for table in [child, parent] {
            db.execute(&format!("drop table if exists `{table}`"), &[])
                .await
                .unwrap();
        }
    }

    /// `Schema::table()` が出す「後から外部キーを足す」文が、本物の MySQL で通ること。
    ///
    /// `create table` の中に書くのとは**別の経路**（`alter table … add constraint …`）で、
    /// 新しく SQL を出すようにした所です。組み立てのテストだけでは、
    /// 本物のサーバが受け付けるかどうかが分かりません。
    #[tokio::test]
    async fn 表を変えて外部キーを足せる() {
        let Some(db) = db().await else { return };
        let parent = "bengara_alter_parent";
        let child = "bengara_alter_child";
        for table in [child, parent] {
            db.execute(&format!("drop table if exists `{table}`"), &[])
                .await
                .unwrap();
        }

        fresh(&db, parent, |t| {
            t.id();
        })
        .await;
        fresh(&db, child, |t| {
            t.id();
        })
        .await;

        // 列と外部キーを後から足す。
        let mut schema = Schema::new(Driver::MySql);
        schema.table(child, |t| {
            t.foreign_id("parent_id").default(0);
            t.foreign("parent_id").on(parent).cascade_on_delete();
        });
        let statements = schema.into_statements();
        assert!(
            statements.iter().any(|sql| sql.contains("add constraint")),
            "外部キーの文が出ていません: {statements:?}"
        );
        for sql in statements {
            db.execute(&sql, &[]).await.expect("後から外部キーを足せる");
        }

        // 効いていることを確かめる。
        db.execute(&format!("insert into `{parent}` () values ()"), &[])
            .await
            .unwrap();
        assert!(
            db.execute(
                &format!("insert into `{child}` (`parent_id`) values (999)"),
                &[],
            )
            .await
            .is_err(),
            "後から足した外部キーが効いていません"
        );

        for table in [child, parent] {
            db.execute(&format!("drop table if exists `{table}`"), &[])
                .await
                .unwrap();
        }
    }

    /// 束ねた件数を数える形を、本物の MySQL が受け付けること。
    ///
    /// `query.rs` の `sub_count_sql` が出す形をそのまま書いています。
    /// **形を変えたらこのテストも直してください。**
    ///
    /// 以前は内側が `select *` だったので、MySQL は既定の `ONLY_FULL_GROUP_BY` で
    /// ERROR 1055、PostgreSQL は「`group by` に無い列がある」で断っていました。
    /// SQLite だけが通るため、アプリのテストでは気づけませんでした。
    #[tokio::test]
    async fn 束ねたときの件数を数える形を受け付ける() {
        let Some(db) = db().await else { return };
        let table = "bengara_group_count";
        fresh(&db, table, |t| {
            t.id();
            t.string("status");
        })
        .await;
        for status in ["draft", "draft", "published"] {
            db.execute(
                &format!("insert into `{table}` (`status`) values (?)"),
                &[status.into()],
            )
            .await
            .unwrap();
        }

        let sql = format!(
            "select count(*) as bengara_aggregate from \
             (select 1 as bengara_group from `{table}` group by `status`) as bengara_sub"
        );
        let rows = db
            .fetch_all(&sql, &[])
            .await
            .expect("束ねた件数を数えられる");
        assert_eq!(
            rows[0].get::<i64>("bengara_aggregate").unwrap(),
            2,
            "数えるのは行の数ではなくグループの数です"
        );
    }
}
