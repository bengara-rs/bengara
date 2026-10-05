//! クエリビルダ。
//!
//! ```ignore
//! let posts = DB::table("posts")
//!     .where_("status", "published")
//!     .latest()
//!     .limit(10)
//!     .get()
//!     .await?;
//! ```
//!
//! 値は必ずプレースホルダで渡します。SQL の文字列に埋め込みません。

use super::grammar::{self, Driver};
use super::value::{Affected, FromValue, IntoValue, Row, Value};
use super::Source;
use crate::error::{Error, Result};

/// `and` でつなぐか `or` でつなぐか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Glue {
    And,
    Or,
}

impl Glue {
    fn as_sql(&self) -> &'static str {
        match self {
            Glue::And => " and ",
            Glue::Or => " or ",
        }
    }
}

/// 1つの条件。
#[derive(Debug, Clone)]
enum Where {
    Basic {
        boolean: Glue,
        column: String,
        operator: String,
        value: Value,
    },
    Null {
        boolean: Glue,
        column: String,
        not: bool,
    },
    In {
        boolean: Glue,
        column: String,
        values: Vec<Value>,
        not: bool,
    },
    Between {
        boolean: Glue,
        column: String,
        low: Value,
        high: Value,
        not: bool,
    },
    Column {
        boolean: Glue,
        left: String,
        operator: String,
        right: String,
    },
    Raw {
        boolean: Glue,
        sql: String,
        bindings: Vec<Value>,
    },
    Nested {
        boolean: Glue,
        wheres: Vec<Where>,
    },
}

impl Where {
    fn boolean(&self) -> Glue {
        match self {
            Where::Basic { boolean, .. }
            | Where::Null { boolean, .. }
            | Where::In { boolean, .. }
            | Where::Between { boolean, .. }
            | Where::Column { boolean, .. }
            | Where::Raw { boolean, .. }
            | Where::Nested { boolean, .. } => *boolean,
        }
    }
}

/// 表の結合。
#[derive(Debug, Clone)]
struct TableJoin {
    kind: &'static str,
    table: String,
    first: String,
    operator: String,
    second: String,
}

/// 組み立て中のクエリ。
///
/// メソッドは `self` を受けて `Self` を返します。終端（`get` など）は `async` です。
#[derive(Clone)]
pub struct QueryBuilder {
    source: Source,
    driver: Driver,
    /// 設定の読み出しで失敗していたら、終端メソッドでこのエラーを返す。
    deferred_error: Option<String>,
    table: String,
    columns: Vec<String>,
    distinct: bool,
    joins: Vec<TableJoin>,
    wheres: Vec<Where>,
    groups: Vec<String>,
    havings: Vec<Where>,
    orders: Vec<(String, bool)>,
    limit: Option<u64>,
    offset: Option<u64>,
}

impl std::fmt::Debug for QueryBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (sql, bindings) = self.to_sql();
        f.debug_struct("QueryBuilder")
            .field("sql", &sql)
            .field("bindings", &bindings.len())
            .finish()
    }
}

impl QueryBuilder {
    /// 新しく組み立てを始める。`DB::table(...)` から呼ばれます。
    pub(crate) fn new(source: Source, table: impl Into<String>) -> Self {
        let (driver, deferred_error) = match source.driver() {
            Ok(driver) => (driver, None),
            // ここでエラーにすると `DB::table()` が Result を返すことになる。
            // 組み立ては続けさせて、実行するときに知らせる。
            Err(e) => (Driver::Sqlite, Some(e.to_string())),
        };
        Self {
            source,
            driver,
            deferred_error,
            table: table.into(),
            columns: Vec::new(),
            distinct: false,
            joins: Vec::new(),
            wheres: Vec::new(),
            groups: Vec::new(),
            havings: Vec::new(),
            orders: Vec::new(),
            limit: None,
            offset: None,
        }
    }

    /// 投げ先を差し替える。モデルのクエリから使います。
    pub(crate) fn with_source(mut self, source: Source) -> Self {
        let (driver, deferred_error) = match source.driver() {
            Ok(driver) => (driver, None),
            Err(e) => (Driver::Sqlite, Some(e.to_string())),
        };
        self.source = source;
        self.driver = driver;
        self.deferred_error = deferred_error;
        self
    }

    /// つながる先のデータベースの種類。
    pub fn driver(&self) -> Driver {
        self.driver
    }

    // ---- 取る列 ----

    /// 取る列を決める。
    pub fn select(mut self, columns: &[&str]) -> Self {
        self.columns = columns.iter().map(|c| (*c).to_string()).collect();
        self
    }

    /// 取る列を足す。
    pub fn add_select(mut self, column: &str) -> Self {
        self.columns.push(column.to_string());
        self
    }

    /// 重なりを捨てる。
    pub fn distinct(mut self) -> Self {
        self.distinct = true;
        self
    }

    // ---- 条件 ----

    /// 列が値と等しい。
    pub fn where_(self, column: &str, value: impl IntoValue) -> Self {
        self.push_basic(Glue::And, column, "=", value.into_value())
    }

    /// 列と値を演算子で比べる。
    pub fn where_op(self, column: &str, operator: &str, value: impl IntoValue) -> Self {
        self.push_basic(Glue::And, column, operator, value.into_value())
    }

    /// `or` でつないで、列が値と等しい。
    pub fn or_where(self, column: &str, value: impl IntoValue) -> Self {
        self.push_basic(Glue::Or, column, "=", value.into_value())
    }

    /// `or` でつないで、列と値を演算子で比べる。
    pub fn or_where_op(self, column: &str, operator: &str, value: impl IntoValue) -> Self {
        self.push_basic(Glue::Or, column, operator, value.into_value())
    }

    /// 列が一覧のどれかと等しい。**空の一覧を渡すと必ず0件になります。**
    pub fn where_in<T: IntoValue + Clone>(self, column: &str, values: &[T]) -> Self {
        self.push_in(Glue::And, column, values, false)
    }

    /// 列が一覧のどれとも等しくない。
    pub fn where_not_in<T: IntoValue + Clone>(self, column: &str, values: &[T]) -> Self {
        self.push_in(Glue::And, column, values, true)
    }

    /// `or` でつないで、列が一覧のどれかと等しい。
    pub fn or_where_in<T: IntoValue + Clone>(self, column: &str, values: &[T]) -> Self {
        self.push_in(Glue::Or, column, values, false)
    }

    /// 列が `NULL`。
    pub fn where_null(mut self, column: &str) -> Self {
        self.wheres.push(Where::Null {
            boolean: Glue::And,
            column: column.to_string(),
            not: false,
        });
        self
    }

    /// 列が `NULL` ではない。
    pub fn where_not_null(mut self, column: &str) -> Self {
        self.wheres.push(Where::Null {
            boolean: Glue::And,
            column: column.to_string(),
            not: true,
        });
        self
    }

    /// 列が2つの値の間にある（両端を含む）。
    pub fn where_between(
        mut self,
        column: &str,
        low: impl IntoValue,
        high: impl IntoValue,
    ) -> Self {
        self.wheres.push(Where::Between {
            boolean: Glue::And,
            column: column.to_string(),
            low: low.into_value(),
            high: high.into_value(),
            not: false,
        });
        self
    }

    /// 列が2つの値の間に無い。
    pub fn where_not_between(
        mut self,
        column: &str,
        low: impl IntoValue,
        high: impl IntoValue,
    ) -> Self {
        self.wheres.push(Where::Between {
            boolean: Glue::And,
            column: column.to_string(),
            low: low.into_value(),
            high: high.into_value(),
            not: true,
        });
        self
    }

    /// 列が形に当てはまる（`like`）。`%` は自分で書きます。
    pub fn where_like(self, column: &str, pattern: impl IntoValue) -> Self {
        self.push_basic(Glue::And, column, "like", pattern.into_value())
    }

    /// 列と列を比べる。
    pub fn where_column(mut self, left: &str, operator: &str, right: &str) -> Self {
        self.wheres.push(Where::Column {
            boolean: Glue::And,
            left: left.to_string(),
            operator: operator.to_string(),
            right: right.to_string(),
        });
        self
    }

    /// SQL を直接書く。値はプレースホルダ（`?`）で渡します。
    pub fn where_raw(mut self, sql: &str, bindings: &[Value]) -> Self {
        self.wheres.push(Where::Raw {
            boolean: Glue::And,
            sql: sql.to_string(),
            bindings: bindings.to_vec(),
        });
        self
    }

    /// `or` でつないで SQL を直接書く。
    pub fn or_where_raw(mut self, sql: &str, bindings: &[Value]) -> Self {
        self.wheres.push(Where::Raw {
            boolean: Glue::Or,
            sql: sql.to_string(),
            bindings: bindings.to_vec(),
        });
        self
    }

    /// 括弧でくくった条件の塊を足す。
    ///
    /// ```ignore
    /// .where_("status", "published")
    /// .where_group(|q| q.where_("views", 0).or_where_op("views", ">", 100))
    /// ```
    pub fn where_group(self, build: impl FnOnce(QueryBuilder) -> QueryBuilder) -> Self {
        self.push_group(Glue::And, build)
    }

    /// `or` でつないで、括弧でくくった条件の塊を足す。
    pub fn or_where_group(self, build: impl FnOnce(QueryBuilder) -> QueryBuilder) -> Self {
        self.push_group(Glue::Or, build)
    }

    fn push_group(
        mut self,
        boolean: Glue,
        build: impl FnOnce(QueryBuilder) -> QueryBuilder,
    ) -> Self {
        let nested = build(QueryBuilder::new(self.source.clone(), self.table.clone()));
        if !nested.wheres.is_empty() {
            self.wheres.push(Where::Nested {
                boolean,
                wheres: nested.wheres,
            });
        }
        self
    }

    fn push_basic(mut self, boolean: Glue, column: &str, operator: &str, value: Value) -> Self {
        self.wheres.push(Where::Basic {
            boolean,
            column: column.to_string(),
            operator: operator.to_string(),
            value,
        });
        self
    }

    fn push_in<T: IntoValue + Clone>(
        mut self,
        boolean: Glue,
        column: &str,
        values: &[T],
        not: bool,
    ) -> Self {
        self.wheres.push(Where::In {
            boolean,
            column: column.to_string(),
            values: values.iter().cloned().map(IntoValue::into_value).collect(),
            not,
        });
        self
    }

    // ---- 結合 ----

    /// 内部結合。
    pub fn join(self, table: &str, first: &str, operator: &str, second: &str) -> Self {
        self.push_join("inner join", table, first, operator, second)
    }

    /// 左外部結合。
    pub fn left_join(self, table: &str, first: &str, operator: &str, second: &str) -> Self {
        self.push_join("left join", table, first, operator, second)
    }

    fn push_join(
        mut self,
        kind: &'static str,
        table: &str,
        first: &str,
        operator: &str,
        second: &str,
    ) -> Self {
        self.joins.push(TableJoin {
            kind,
            table: table.to_string(),
            first: first.to_string(),
            operator: operator.to_string(),
            second: second.to_string(),
        });
        self
    }

    // ---- 束ね・並び・件数 ----

    /// 列で束ねる。
    pub fn group_by(mut self, columns: &[&str]) -> Self {
        self.groups.extend(columns.iter().map(|c| (*c).to_string()));
        self
    }

    /// 束ねた結果を絞る。
    pub fn having_op(mut self, column: &str, operator: &str, value: impl IntoValue) -> Self {
        self.havings.push(Where::Basic {
            boolean: Glue::And,
            column: column.to_string(),
            operator: operator.to_string(),
            value: value.into_value(),
        });
        self
    }

    /// 束ねた結果を SQL で絞る。
    pub fn having_raw(mut self, sql: &str, bindings: &[Value]) -> Self {
        self.havings.push(Where::Raw {
            boolean: Glue::And,
            sql: sql.to_string(),
            bindings: bindings.to_vec(),
        });
        self
    }

    /// 昇順に並べる。
    pub fn order_by(mut self, column: &str) -> Self {
        self.orders.push((column.to_string(), false));
        self
    }

    /// 降順に並べる。
    pub fn order_by_desc(mut self, column: &str) -> Self {
        self.orders.push((column.to_string(), true));
        self
    }

    /// 新しい順（`created_at` の降順）。
    pub fn latest(self) -> Self {
        self.order_by_desc("created_at")
    }

    /// 古い順（`created_at` の昇順）。
    pub fn oldest(self) -> Self {
        self.order_by("created_at")
    }

    /// 指定した列の降順。
    pub fn latest_by(self, column: &str) -> Self {
        self.order_by_desc(column)
    }

    /// 指定した列の昇順。
    pub fn oldest_by(self, column: &str) -> Self {
        self.order_by(column)
    }

    /// 取る件数の上限。
    pub fn limit(mut self, count: u64) -> Self {
        self.limit = Some(count);
        self
    }

    /// 読み飛ばす件数。
    pub fn offset(mut self, count: u64) -> Self {
        self.offset = Some(count);
        self
    }

    /// `limit` と同じ。
    pub fn take(self, count: u64) -> Self {
        self.limit(count)
    }

    /// `offset` と同じ。
    pub fn skip(self, count: u64) -> Self {
        self.offset(count)
    }

    /// ページ番号から `limit` と `offset` を決める。ページは1から数えます。
    pub fn for_page(self, page: u64, per_page: u64) -> Self {
        let page = page.max(1);
        let per_page = per_page.max(1);
        self.limit(per_page).offset((page - 1) * per_page)
    }

    // ---- SQL の組み立て ----

    /// 組み立てた `select` の SQL と、渡す値の一覧を返す。
    ///
    /// 接続しないので、テストやデバッグで使えます。
    pub fn to_sql(&self) -> (String, Vec<Value>) {
        let mut c = Compiler::new(self.driver);
        c.sql.push_str("select ");
        if self.distinct {
            c.sql.push_str("distinct ");
        }
        if self.columns.is_empty() {
            c.sql.push('*');
        } else {
            let columns: Vec<String> = self
                .columns
                .iter()
                .map(|col| grammar::quote(self.driver, col))
                .collect();
            c.sql.push_str(&columns.join(", "));
        }
        c.sql.push_str(" from ");
        c.sql.push_str(&grammar::quote(self.driver, &self.table));
        self.compile_joins(&mut c);
        self.compile_wheres(&mut c);
        self.compile_groups(&mut c);
        self.compile_orders(&mut c);
        self.compile_limit(&mut c);
        (c.sql, c.bindings)
    }

    fn compile_joins(&self, c: &mut Compiler) {
        for join in &self.joins {
            c.sql.push(' ');
            c.sql.push_str(join.kind);
            c.sql.push(' ');
            c.sql.push_str(&grammar::quote(self.driver, &join.table));
            c.sql.push_str(" on ");
            c.sql.push_str(&grammar::quote(self.driver, &join.first));
            c.sql.push(' ');
            c.sql.push_str(&join.operator);
            c.sql.push(' ');
            c.sql.push_str(&grammar::quote(self.driver, &join.second));
        }
    }

    fn compile_wheres(&self, c: &mut Compiler) {
        if self.wheres.is_empty() {
            return;
        }
        c.sql.push_str(" where ");
        self.render_conditions(c, &self.wheres);
    }

    fn compile_groups(&self, c: &mut Compiler) {
        if !self.groups.is_empty() {
            let groups: Vec<String> = self
                .groups
                .iter()
                .map(|g| grammar::quote(self.driver, g))
                .collect();
            c.sql.push_str(" group by ");
            c.sql.push_str(&groups.join(", "));
        }
        if !self.havings.is_empty() {
            c.sql.push_str(" having ");
            self.render_conditions(c, &self.havings);
        }
    }

    fn compile_orders(&self, c: &mut Compiler) {
        if self.orders.is_empty() {
            return;
        }
        let orders: Vec<String> = self
            .orders
            .iter()
            .map(|(column, desc)| {
                format!(
                    "{} {}",
                    grammar::quote(self.driver, column),
                    if *desc { "desc" } else { "asc" }
                )
            })
            .collect();
        c.sql.push_str(" order by ");
        c.sql.push_str(&orders.join(", "));
    }

    fn compile_limit(&self, c: &mut Compiler) {
        // 件数は数値なので、そのまま置いても安全。
        if let Some(limit) = self.limit {
            c.sql.push_str(&format!(" limit {limit}"));
        }
        if let Some(offset) = self.offset {
            // SQLite と MySQL は limit が無いと offset を受け付けない。
            if self.limit.is_none() {
                c.sql.push_str(" limit -1");
            }
            c.sql.push_str(&format!(" offset {offset}"));
        }
    }

    fn render_conditions(&self, c: &mut Compiler, conditions: &[Where]) {
        for (index, condition) in conditions.iter().enumerate() {
            if index > 0 {
                c.sql.push_str(condition.boolean().as_sql());
            }
            self.render_condition(c, condition);
        }
    }

    fn render_condition(&self, c: &mut Compiler, condition: &Where) {
        let driver = self.driver;
        match condition {
            Where::Basic {
                column,
                operator,
                value,
                ..
            } => {
                c.sql.push_str(&grammar::quote(driver, column));
                c.sql.push(' ');
                c.sql.push_str(operator);
                c.sql.push(' ');
                c.push_value(value.clone());
            }
            Where::Null { column, not, .. } => {
                c.sql.push_str(&grammar::quote(driver, column));
                c.sql
                    .push_str(if *not { " is not null" } else { " is null" });
            }
            Where::In {
                column,
                values,
                not,
                ..
            } => {
                if values.is_empty() {
                    // 空の一覧は「どれにも当てはまらない」。not なら必ず真。
                    c.sql.push_str(if *not { "1 = 1" } else { "1 = 0" });
                    return;
                }
                c.sql.push_str(&grammar::quote(driver, column));
                c.sql.push_str(if *not { " not in (" } else { " in (" });
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        c.sql.push_str(", ");
                    }
                    c.push_value(value.clone());
                }
                c.sql.push(')');
            }
            Where::Between {
                column,
                low,
                high,
                not,
                ..
            } => {
                c.sql.push_str(&grammar::quote(driver, column));
                c.sql
                    .push_str(if *not { " not between " } else { " between " });
                c.push_value(low.clone());
                c.sql.push_str(" and ");
                c.push_value(high.clone());
            }
            Where::Column {
                left,
                operator,
                right,
                ..
            } => {
                c.sql.push_str(&grammar::quote(driver, left));
                c.sql.push(' ');
                c.sql.push_str(operator);
                c.sql.push(' ');
                c.sql.push_str(&grammar::quote(driver, right));
            }
            Where::Raw { sql, bindings, .. } => {
                c.push_raw(sql, bindings);
            }
            Where::Nested { wheres, .. } => {
                c.sql.push('(');
                self.render_conditions(c, wheres);
                c.sql.push(')');
            }
        }
    }

    fn check(&self) -> Result<()> {
        match &self.deferred_error {
            Some(message) => Err(Error::msg(message.clone())),
            None => Ok(()),
        }
    }

    // ---- 実行 ----

    /// 行を取る。
    pub async fn get(&self) -> Result<Vec<Row>> {
        self.check()?;
        let (sql, bindings) = self.to_sql();
        self.source.fetch_all(&sql, &bindings).await
    }

    /// 最初の1行を取る。
    pub async fn first(&self) -> Result<Option<Row>> {
        let rows = self.clone().limit(1).get().await?;
        Ok(rows.into_iter().next())
    }

    /// 主キー（`id`）で1行取る。
    pub async fn find(&self, id: impl IntoValue) -> Result<Option<Row>> {
        self.clone().where_("id", id.into_value()).first().await
    }

    /// 最初の行の、ある列の値を取る。
    pub async fn value<T: FromValue>(&self, column: &str) -> Result<Option<T>> {
        let row = self.clone().select(&[column]).first().await?;
        match row {
            Some(row) => row.get::<T>(column).map(Some),
            None => Ok(None),
        }
    }

    /// ある列だけを一覧で取る。
    pub async fn pluck<T: FromValue>(&self, column: &str) -> Result<Vec<T>> {
        let rows = self.clone().select(&[column]).get().await?;
        rows.iter().map(|row| row.get::<T>(column)).collect()
    }

    /// 件数を数える。
    pub async fn count(&self) -> Result<i64> {
        self.aggregate::<i64>("count", "*")
            .await
            .map(|v| v.unwrap_or(0))
    }

    /// 1件でもあるか。
    pub async fn exists(&self) -> Result<bool> {
        let rows = self
            .clone()
            .select(&["1 as bengara_exists"])
            .limit(1)
            .get()
            .await?;
        Ok(!rows.is_empty())
    }

    /// 1件も無いか。
    pub async fn doesnt_exist(&self) -> Result<bool> {
        Ok(!self.exists().await?)
    }

    /// 合計。
    pub async fn sum<T: FromValue>(&self, column: &str) -> Result<Option<T>> {
        self.aggregate("sum", column).await
    }

    /// 平均。
    pub async fn avg<T: FromValue>(&self, column: &str) -> Result<Option<T>> {
        self.aggregate("avg", column).await
    }

    /// 最小。
    pub async fn min<T: FromValue>(&self, column: &str) -> Result<Option<T>> {
        self.aggregate("min", column).await
    }

    /// 最大。
    pub async fn max<T: FromValue>(&self, column: &str) -> Result<Option<T>> {
        self.aggregate("max", column).await
    }

    async fn aggregate<T: FromValue>(&self, function: &str, column: &str) -> Result<Option<T>> {
        let column = if column == "*" {
            "*".to_string()
        } else {
            grammar::quote(self.driver, column)
        };
        let mut query = self.clone();
        query.orders.clear();
        query.limit = None;
        query.offset = None;
        let expression = format!("{function}({column}) as bengara_aggregate");
        let rows = query.select(&[expression.as_str()]).get().await?;
        match rows.first().and_then(|row| row.value("bengara_aggregate")) {
            Some(Value::Null) | None => Ok(None),
            Some(value) => T::from_value(value).map(Some),
        }
    }

    /// 1行足す。
    pub async fn insert(&self, values: &[(&str, Value)]) -> Result<Affected> {
        let rows = vec![values.to_vec()];
        self.insert_many(&rows).await
    }

    /// 何行かまとめて足す。列の並びは最初の行に合わせます。
    pub async fn insert_many(&self, rows: &[Vec<(&str, Value)>]) -> Result<Affected> {
        self.check()?;
        let Some(first) = rows.first() else {
            return Ok(Affected::default());
        };
        if first.is_empty() {
            return Err(Error::msg("insert に渡す列がありません"));
        }
        let (sql, bindings) = self.insert_sql(rows, first, "")?;
        self.source.execute(&sql, &bindings).await
    }

    /// 1行足して、自動採番された ID を返す。
    pub async fn insert_get_id(&self, values: &[(&str, Value)]) -> Result<i64> {
        self.insert_get_id_as(values, "id").await
    }

    /// 1行足して、指定した主キーの値を返す。
    pub async fn insert_get_id_as(
        &self,
        values: &[(&str, Value)],
        primary_key: &str,
    ) -> Result<i64> {
        self.check()?;
        if values.is_empty() {
            return Err(Error::msg("insert に渡す列がありません"));
        }
        let rows = vec![values.to_vec()];
        let returning = grammar::returning_id(self.driver, primary_key);
        let (sql, bindings) = self.insert_sql(&rows, values, &returning)?;

        if self.driver == Driver::Postgres {
            let rows = self.source.fetch_all(&sql, &bindings).await?;
            return rows
                .first()
                .ok_or_else(|| Error::msg("insert が ID を返しませんでした"))?
                .get::<i64>(primary_key);
        }

        let affected = self.source.execute(&sql, &bindings).await?;
        affected
            .last_insert_id
            .ok_or_else(|| Error::msg("insert が ID を返しませんでした"))
    }

    fn insert_sql(
        &self,
        rows: &[Vec<(&str, Value)>],
        shape: &[(&str, Value)],
        suffix: &str,
    ) -> Result<(String, Vec<Value>)> {
        let mut c = Compiler::new(self.driver);
        let columns: Vec<String> = shape
            .iter()
            .map(|(name, _)| grammar::quote(self.driver, name))
            .collect();
        c.sql.push_str("insert into ");
        c.sql.push_str(&grammar::quote(self.driver, &self.table));
        c.sql.push_str(" (");
        c.sql.push_str(&columns.join(", "));
        c.sql.push_str(") values ");

        for (index, row) in rows.iter().enumerate() {
            if row.len() != shape.len() {
                return Err(Error::msg(
                    "insert_many に渡した行で、列の数がそろっていません",
                ));
            }
            if index > 0 {
                c.sql.push_str(", ");
            }
            c.sql.push('(');
            for (position, (name, value)) in row.iter().enumerate() {
                if position > 0 {
                    c.sql.push_str(", ");
                }
                if *name != shape[position].0 {
                    return Err(Error::msg(format!(
                        "insert_many に渡した行で、列の並びが違います（`{}` と `{}`）",
                        shape[position].0, name
                    )));
                }
                c.push_value(value.clone());
            }
            c.sql.push(')');
        }
        c.sql.push_str(suffix);
        Ok((c.sql, c.bindings))
    }

    /// 条件に当てはまる行を更新する。返るのは変わった件数です。
    pub async fn update(&self, values: &[(&str, Value)]) -> Result<u64> {
        self.check()?;
        if values.is_empty() {
            return Err(Error::msg("update に渡す列がありません"));
        }
        let mut c = Compiler::new(self.driver);
        c.sql.push_str("update ");
        c.sql.push_str(&grammar::quote(self.driver, &self.table));
        c.sql.push_str(" set ");
        for (index, (name, value)) in values.iter().enumerate() {
            if index > 0 {
                c.sql.push_str(", ");
            }
            c.sql.push_str(&grammar::quote(self.driver, name));
            c.sql.push_str(" = ");
            c.push_value(value.clone());
        }
        self.compile_wheres(&mut c);
        let affected = self.source.execute(&c.sql, &c.bindings).await?;
        Ok(affected.rows)
    }

    /// 条件に当てはまる行を消す。返るのは消した件数です。
    pub async fn delete(&self) -> Result<u64> {
        self.check()?;
        let mut c = Compiler::new(self.driver);
        c.sql.push_str("delete from ");
        c.sql.push_str(&grammar::quote(self.driver, &self.table));
        self.compile_wheres(&mut c);
        let affected = self.source.execute(&c.sql, &c.bindings).await?;
        Ok(affected.rows)
    }

    /// 表を空にする。
    pub async fn truncate(&self) -> Result<()> {
        self.check()?;
        // SQLite に truncate は無い。条件なしの delete と同じ。
        let sql = match self.driver {
            Driver::Sqlite => format!("delete from {}", grammar::quote(self.driver, &self.table)),
            _ => format!(
                "truncate table {}",
                grammar::quote(self.driver, &self.table)
            ),
        };
        self.source.execute(&sql, &[]).await?;
        Ok(())
    }

    /// ページ分けして取る。ページは1から数えます。
    pub async fn paginate(&self, per_page: u64, page: u64) -> Result<Paginator<Row>> {
        let per_page = per_page.max(1);
        let page = page.max(1);
        let total = self.count().await?;
        let data = self.clone().for_page(page, per_page).get().await?;
        Ok(Paginator {
            data,
            total,
            per_page,
            current_page: page,
        })
    }
}

/// SQL と値を組み立てる途中の入れ物。
struct Compiler {
    driver: Driver,
    sql: String,
    bindings: Vec<Value>,
}

impl Compiler {
    fn new(driver: Driver) -> Self {
        Self {
            driver,
            sql: String::with_capacity(64),
            bindings: Vec::new(),
        }
    }

    /// 値を1つ足して、置き場所を SQL に書く。
    fn push_value(&mut self, value: Value) {
        self.bindings.push(value);
        let placeholder = grammar::placeholder(self.driver, self.bindings.len());
        self.sql.push_str(&placeholder);
    }

    /// 利用者が書いた SQL を足す。`?` を使っている場合は番号を振り直す。
    fn push_raw(&mut self, sql: &str, bindings: &[Value]) {
        if self.driver != Driver::Postgres {
            self.sql.push_str(sql);
            self.bindings.extend_from_slice(bindings);
            return;
        }
        // PostgreSQL は番号付きなので、`?` を順に差し替える。
        let mut out = String::with_capacity(sql.len());
        let mut values = bindings.iter();
        for c in sql.chars() {
            if c == '?' {
                if let Some(value) = values.next() {
                    self.bindings.push(value.clone());
                    out.push_str(&grammar::placeholder(self.driver, self.bindings.len()));
                    continue;
                }
            }
            out.push(c);
        }
        // 余った値はそのまま後ろに付ける（番号だけ消費される）。
        for value in values {
            self.bindings.push(value.clone());
        }
        self.sql.push_str(&out);
    }
}

/// ページ分けの結果。
///
/// `serde::Serialize` を実装しているので、そのまま JSON で返せます。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Paginator<T> {
    /// このページの中身。
    pub data: Vec<T>,
    /// 全部で何件か。
    pub total: i64,
    /// 1ページあたりの件数。
    pub per_page: u64,
    /// いま何ページ目か（1から）。
    pub current_page: u64,
}

impl<T> Paginator<T> {
    /// 最後のページ番号。
    pub fn last_page(&self) -> u64 {
        let total = self.total.max(0) as u64;
        let pages = total.div_ceil(self.per_page.max(1));
        pages.max(1)
    }

    /// 次のページがあるか。
    pub fn has_more_pages(&self) -> bool {
        self.current_page < self.last_page()
    }

    /// このページの最初の件が、全体で何件目か。0 件なら `None`。
    pub fn from(&self) -> Option<u64> {
        if self.data.is_empty() {
            return None;
        }
        Some((self.current_page - 1) * self.per_page + 1)
    }

    /// このページの最後の件が、全体で何件目か。0 件なら `None`。
    pub fn to(&self) -> Option<u64> {
        let from = self.from()?;
        Some(from + self.data.len() as u64 - 1)
    }

    /// このページが空か。
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// このページの件数。
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// 中身を別の型に移し替える。モデルのクエリが使います。
    pub(crate) fn map_data<U>(self, data: Vec<U>) -> Paginator<U> {
        Paginator {
            data,
            total: self.total,
            per_page: self.per_page,
            current_page: self.current_page,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builder() -> QueryBuilder {
        QueryBuilder::new(Source::Default, "posts")
    }

    #[test]
    fn 条件なしの一覧() {
        let (sql, bindings) = builder().to_sql();
        assert_eq!(sql, "select * from \"posts\"");
        assert!(bindings.is_empty());
    }

    #[test]
    fn 列と条件と並びと件数() {
        let (sql, bindings) = builder()
            .select(&["id", "title"])
            .where_("status", "published")
            .where_op("views", ">", 100)
            .latest()
            .limit(10)
            .to_sql();
        assert_eq!(
            sql,
            "select \"id\", \"title\" from \"posts\" where \"status\" = ? and \"views\" > ? \
             order by \"created_at\" desc limit 10"
        );
        assert_eq!(
            bindings,
            vec![Value::Text("published".into()), Value::Int(100)]
        );
    }

    #[test]
    fn or_でつなげる() {
        let (sql, _) = builder().where_("a", 1).or_where("b", 2).to_sql();
        assert_eq!(sql, "select * from \"posts\" where \"a\" = ? or \"b\" = ?");
    }

    #[test]
    fn 括弧でくくれる() {
        let (sql, bindings) = builder()
            .where_("status", "published")
            .where_group(|q| q.where_("views", 0).or_where_op("views", ">", 100))
            .to_sql();
        assert_eq!(
            sql,
            "select * from \"posts\" where \"status\" = ? and (\"views\" = ? or \"views\" > ?)"
        );
        assert_eq!(bindings.len(), 3);
    }

    #[test]
    fn in_と_null_と_between() {
        let (sql, bindings) = builder()
            .where_in("id", &[1, 2, 3])
            .where_not_null("title")
            .where_between("views", 0, 10)
            .to_sql();
        assert_eq!(
            sql,
            "select * from \"posts\" where \"id\" in (?, ?, ?) and \"title\" is not null \
             and \"views\" between ? and ?"
        );
        assert_eq!(bindings.len(), 5);
    }

    #[test]
    fn 空のinは必ず0件になる() {
        let empty: [i64; 0] = [];
        let (sql, bindings) = builder().where_in("id", &empty).to_sql();
        assert_eq!(sql, "select * from \"posts\" where 1 = 0");
        assert!(bindings.is_empty());
        let (sql, _) = builder().where_not_in("id", &empty).to_sql();
        assert_eq!(sql, "select * from \"posts\" where 1 = 1");
    }

    #[test]
    fn 結合と束ねと絞り込み() {
        let (sql, bindings) = builder()
            .select(&["posts.id", "count(*) as total"])
            .join("comments", "comments.post_id", "=", "posts.id")
            .group_by(&["posts.id"])
            .having_op("total", ">", 2)
            .to_sql();
        assert_eq!(
            sql,
            "select \"posts\".\"id\", count(*) as total from \"posts\" \
             inner join \"comments\" on \"comments\".\"post_id\" = \"posts\".\"id\" \
             group by \"posts\".\"id\" having \"total\" > ?"
        );
        assert_eq!(bindings, vec![Value::Int(2)]);
    }

    #[test]
    fn ページ番号から件数を決める() {
        let (sql, _) = builder().for_page(3, 15).to_sql();
        assert_eq!(sql, "select * from \"posts\" limit 15 offset 30");
        let (sql, _) = builder().offset(5).to_sql();
        assert_eq!(sql, "select * from \"posts\" limit -1 offset 5");
    }

    #[test]
    fn 生のsqlも書ける() {
        let (sql, bindings) = builder()
            .where_raw("length(title) > ?", &[Value::Int(3)])
            .to_sql();
        assert_eq!(sql, "select * from \"posts\" where length(title) > ?");
        assert_eq!(bindings, vec![Value::Int(3)]);
    }

    #[test]
    fn ページ分けの計算() {
        let page = Paginator {
            data: vec![1, 2, 3],
            total: 7,
            per_page: 3,
            current_page: 2,
        };
        assert_eq!(page.last_page(), 3);
        assert!(page.has_more_pages());
        assert_eq!(page.from(), Some(4));
        assert_eq!(page.to(), Some(6));
        assert_eq!(page.len(), 3);

        let empty: Paginator<i32> = Paginator {
            data: vec![],
            total: 0,
            per_page: 15,
            current_page: 1,
        };
        assert_eq!(empty.last_page(), 1);
        assert!(!empty.has_more_pages());
        assert_eq!(empty.from(), None);
        assert_eq!(empty.to(), None);
        assert!(empty.is_empty());
    }
}
