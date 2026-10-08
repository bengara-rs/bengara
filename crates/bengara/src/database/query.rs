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

/// 並べ方。
#[derive(Debug, Clone)]
enum Order {
    /// 列の名前で並べる。
    Column { column: String, desc: bool },
    /// SQL をそのまま書く。
    Raw(String),
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
    /// 組み立てのときに見つけた間違い。終端メソッドでこのエラーを返す。
    ///
    /// 組み立ての途中でパニックさせないため、最初の1つだけ覚えておきます。
    deferred_error: Option<String>,
    table: String,
    columns: Vec<String>,
    distinct: bool,
    joins: Vec<TableJoin>,
    wheres: Vec<Where>,
    groups: Vec<String>,
    havings: Vec<Where>,
    orders: Vec<Order>,
    limit: Option<u64>,
    offset: Option<u64>,
}

impl std::fmt::Debug for QueryBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (sql, bindings) = self.to_sql();
        f.debug_struct("QueryBuilder")
            .field("sql", &sql)
            .field("bindings", &bindings.len())
            // 組み立ての間違いは `to_sql()` に出ないので、ここで見せる。
            .field("deferred_error", &self.deferred_error)
            .finish()
    }
}

impl QueryBuilder {
    /// 新しく組み立てを始める。`DB::table(...)` から呼ばれます。
    pub(crate) fn new(source: Source, table: impl Into<String>) -> Self {
        let driver = resolve_driver(&source);
        let mut query = Self {
            source,
            driver,
            deferred_error: None,
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
        };
        let table = query.table.clone();
        query.check_table("DB::table", &table);
        query
    }

    /// 投げ先を差し替える。モデルのクエリから使います。
    ///
    /// 組み立てのときに見つけた間違い（`deferred_error`）は残します。
    pub(crate) fn with_source(mut self, source: Source) -> Self {
        self.driver = resolve_driver(&source);
        self.source = source;
        self
    }

    /// 後で知らせる間違いを積む。最初の1つだけ残します。
    fn defer(&mut self, message: impl Into<String>) {
        if self.deferred_error.is_none() {
            self.deferred_error = Some(message.into());
        }
    }

    /// 列の名前として受け付けるか確かめる。
    ///
    /// 外れていたら組み立ては続けて、終端メソッドでエラーにします。
    fn check_column(&mut self, place: &str, column: &str) {
        if !grammar::is_plain_identifier(column) {
            self.defer(format!(
                "{place} に渡した列名 `{column}` は使えません。\
                 英数字と `_` `.` だけが使えます。\
                 式を書きたいときは order_by_raw / having_raw / where_raw を使ってください"
            ));
        }
    }

    /// 表の名前として受け付けるか確かめる。
    ///
    /// 列名と同じ決まりです。外れていたら終端メソッドでエラーにします。
    fn check_table(&mut self, place: &str, table: &str) {
        if !grammar::is_plain_identifier(table) {
            self.defer(format!(
                "{place} に渡した表の名前 `{table}` は使えません。\
                 英数字と `_` `.` だけが使えます"
            ));
        }
    }

    /// 比べる演算子として受け付けるか確かめる。
    fn check_operator(&mut self, place: &str, operator: &str) {
        if !grammar::is_allowed_operator(operator) {
            self.defer(format!(
                "{place} に渡した演算子 `{operator}` は使えません（使えるもの: {}）",
                grammar::OPERATORS.join(", ")
            ));
        }
    }

    /// 生の SQL に書いた `?` の数と、渡した値の数が合っているか確かめる。
    fn check_raw(&mut self, place: &str, sql: &str, bindings: &[Value]) {
        let holes = sql.matches('?').count();
        if holes != bindings.len() {
            self.defer(format!(
                "{place} に書いた `?` の数（{holes}）と、渡した値の数（{}）が合っていません。\
                 `?` は値の置き場所としてのみ使えます",
                bindings.len()
            ));
        }
    }

    /// つながる先のデータベースの種類。
    pub fn driver(&self) -> Driver {
        self.driver
    }

    // ---- 取る列 ----

    /// 取る列を決める。
    ///
    /// ここは**式も書けます**（`count(*) as total` など）。
    ///
    /// **列名を検査しません。** 引用せずそのまま置く意図した逃げ道です
    /// （`*_raw` 系も同じ）。外から来た文字列をそのまま渡さないでください。
    pub fn select(mut self, columns: &[&str]) -> Self {
        self.columns = columns.iter().map(|c| (*c).to_string()).collect();
        self
    }

    /// 取る列を足す。`select` と同じく**列名を検査しません。**
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

    /// 列が値と等しい。列名は英数字と `_` `.` だけが使えます。
    pub fn where_(self, column: &str, value: impl IntoValue) -> Self {
        self.push_basic("where_", Glue::And, column, "=", value.into_value())
    }

    /// 列と値を演算子で比べる。
    ///
    /// 演算子は許可一覧（`=` `!=` `<` `<=` `>` `>=` `like` `is` など）で照合します。
    /// 外れているときは終端メソッドでエラーになります。
    pub fn where_op(mut self, column: &str, operator: &str, value: impl IntoValue) -> Self {
        self.check_operator("where_op", operator);
        self.push_basic("where_op", Glue::And, column, operator, value.into_value())
    }

    /// `or` でつないで、列が値と等しい。
    pub fn or_where(self, column: &str, value: impl IntoValue) -> Self {
        self.push_basic("or_where", Glue::Or, column, "=", value.into_value())
    }

    /// `or` でつないで、列と値を演算子で比べる。演算子は許可一覧で照合します。
    pub fn or_where_op(mut self, column: &str, operator: &str, value: impl IntoValue) -> Self {
        self.check_operator("or_where_op", operator);
        self.push_basic(
            "or_where_op",
            Glue::Or,
            column,
            operator,
            value.into_value(),
        )
    }

    /// 列が一覧のどれかと等しい。**空の一覧を渡すと必ず0件になります。**
    pub fn where_in<T: IntoValue + Clone>(self, column: &str, values: &[T]) -> Self {
        self.push_in("where_in", Glue::And, column, values, false)
    }

    /// 列が一覧のどれとも等しくない。
    pub fn where_not_in<T: IntoValue + Clone>(self, column: &str, values: &[T]) -> Self {
        self.push_in("where_not_in", Glue::And, column, values, true)
    }

    /// `or` でつないで、列が一覧のどれかと等しい。
    pub fn or_where_in<T: IntoValue + Clone>(self, column: &str, values: &[T]) -> Self {
        self.push_in("or_where_in", Glue::Or, column, values, false)
    }

    /// 列が `NULL`。
    pub fn where_null(mut self, column: &str) -> Self {
        self.check_column("where_null", column);
        self.wheres.push(Where::Null {
            boolean: Glue::And,
            column: column.to_string(),
            not: false,
        });
        self
    }

    /// 列が `NULL` ではない。
    pub fn where_not_null(mut self, column: &str) -> Self {
        self.check_column("where_not_null", column);
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
        self.check_column("where_between", column);
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
        self.check_column("where_not_between", column);
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
        self.push_basic(
            "where_like",
            Glue::And,
            column,
            "like",
            pattern.into_value(),
        )
    }

    /// 列と列を比べる。演算子は許可一覧で照合します。
    pub fn where_column(mut self, left: &str, operator: &str, right: &str) -> Self {
        self.check_operator("where_column", operator);
        self.check_column("where_column", left);
        self.check_column("where_column", right);
        self.wheres.push(Where::Column {
            boolean: Glue::And,
            left: left.to_string(),
            operator: operator.to_string(),
            right: right.to_string(),
        });
        self
    }

    /// SQL を直接書く。値はプレースホルダ（`?`）で渡します。
    ///
    /// **`?` は値の置き場所としてのみ使えます。** 文字列の中に書いた `?` も
    /// 値の置き場所として数えるので、`'a?b'` のような書き方はできません。
    /// `?` の数と値の数が合わないときは、終端メソッドでエラーになります。
    pub fn where_raw(mut self, sql: &str, bindings: &[Value]) -> Self {
        self.check_raw("where_raw", sql, bindings);
        self.wheres.push(Where::Raw {
            boolean: Glue::And,
            sql: sql.to_string(),
            bindings: bindings.to_vec(),
        });
        self
    }

    /// `or` でつないで SQL を直接書く。`?` は値の置き場所としてのみ使えます。
    pub fn or_where_raw(mut self, sql: &str, bindings: &[Value]) -> Self {
        self.check_raw("or_where_raw", sql, bindings);
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
    ///
    /// **使われるのは条件だけです。** クロージャの中で `select` / `limit` /
    /// `order_by` を触っても捨てられます。
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
        // 中で見つけた間違いも引き継ぐ。捨てると気づけない。
        if let Some(message) = nested.deferred_error {
            self.defer(message);
        }
        if !nested.wheres.is_empty() {
            self.wheres.push(Where::Nested {
                boolean,
                wheres: nested.wheres,
            });
        }
        self
    }

    fn push_basic(
        mut self,
        place: &str,
        boolean: Glue,
        column: &str,
        operator: &str,
        value: Value,
    ) -> Self {
        self.check_column(place, column);
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
        place: &str,
        boolean: Glue,
        column: &str,
        values: &[T],
        not: bool,
    ) -> Self {
        self.check_column(place, column);
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
    ///
    /// 表の名前と両側の列名は英数字と `_` `.` だけ、演算子は許可一覧で照合します。
    pub fn join(self, table: &str, first: &str, operator: &str, second: &str) -> Self {
        self.push_join("join", "inner join", table, first, operator, second)
    }

    /// 左外部結合。確かめる内容は `join` と同じです。
    pub fn left_join(self, table: &str, first: &str, operator: &str, second: &str) -> Self {
        self.push_join("left_join", "left join", table, first, operator, second)
    }

    fn push_join(
        mut self,
        place: &str,
        kind: &'static str,
        table: &str,
        first: &str,
        operator: &str,
        second: &str,
    ) -> Self {
        self.check_table(place, table);
        self.check_column(place, first);
        self.check_column(place, second);
        self.check_operator(place, operator);
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
    ///
    /// 列名は英数字と `_` `.` だけが使えます。外れているときは終端メソッドで
    /// エラーになります。
    pub fn group_by(mut self, columns: &[&str]) -> Self {
        for column in columns {
            self.check_column("group_by", column);
        }
        self.groups.extend(columns.iter().map(|c| (*c).to_string()));
        self
    }

    /// 束ねた結果を絞る。
    ///
    /// 列名は英数字と `_` `.` だけ、演算子は許可一覧で照合します。
    /// 式で絞りたいときは `having_raw` を使ってください。
    pub fn having_op(mut self, column: &str, operator: &str, value: impl IntoValue) -> Self {
        self.check_column("having_op", column);
        self.check_operator("having_op", operator);
        self.havings.push(Where::Basic {
            boolean: Glue::And,
            column: column.to_string(),
            operator: operator.to_string(),
            value: value.into_value(),
        });
        self
    }

    /// 束ねた結果を SQL で絞る。`?` は値の置き場所としてのみ使えます。
    pub fn having_raw(mut self, sql: &str, bindings: &[Value]) -> Self {
        self.check_raw("having_raw", sql, bindings);
        self.havings.push(Where::Raw {
            boolean: Glue::And,
            sql: sql.to_string(),
            bindings: bindings.to_vec(),
        });
        self
    }

    /// 昇順に並べる。
    ///
    /// 列名は英数字と `_` `.` だけが使えます。外れているときは終端メソッドで
    /// エラーになります。式で並べたいときは `order_by_raw` を使ってください。
    pub fn order_by(mut self, column: &str) -> Self {
        self.check_column("order_by", column);
        self.orders.push(Order::Column {
            column: column.to_string(),
            desc: false,
        });
        self
    }

    /// 降順に並べる。列名は英数字と `_` `.` だけが使えます。
    pub fn order_by_desc(mut self, column: &str) -> Self {
        self.check_column("order_by_desc", column);
        self.orders.push(Order::Column {
            column: column.to_string(),
            desc: true,
        });
        self
    }

    /// 並べ方を SQL で直接書く。
    ///
    /// ```ignore
    /// .order_by_raw("case when pinned then 0 else 1 end asc")
    /// ```
    ///
    /// **外から来た文字列をそのまま渡さないでください。** 引用も検査もしません。
    pub fn order_by_raw(mut self, sql: &str) -> Self {
        self.orders.push(Order::Raw(sql.to_string()));
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
    ///
    /// 接続の設定が読めないとき（名前の間違いなど）は、SQLite の書き方を仮に使います。
    /// つまり**本当に投げる方言とは違う SQL が返ることがあります。**
    /// その場合は終端メソッド（`get` など）がエラーになります。
    /// 組み立てのときに見つけた間違い（使えない列名など）も、ここでは出ません。
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
            .map(|order| match order {
                Order::Column { column, desc } => format!(
                    "{} {}",
                    grammar::quote(self.driver, column),
                    if *desc { "desc" } else { "asc" }
                ),
                Order::Raw(sql) => sql.clone(),
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
            if self.limit.is_none() {
                // SQLite と MySQL は limit が無いと offset を受け付けない。
                // PostgreSQL では `limit -1` が構文エラーなので `limit all` を使う。
                c.sql.push_str(match self.driver {
                    Driver::Postgres => " limit all",
                    _ => " limit -1",
                });
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

    /// 終端メソッドの頭で呼ぶ確かめ。
    ///
    /// 設定が読めないこと（接続の名前が無いなど）も、組み立ての間違いも、
    /// ここで初めてエラーにします。`DB::table()` を `Result` にしないためです。
    fn check(&self) -> Result<()> {
        self.source.driver()?;
        if let Some(message) = &self.deferred_error {
            return Err(Error::msg(message.clone()));
        }
        if !self.havings.is_empty() && self.groups.is_empty() {
            // `having` は束ねた結果を絞るものなので、束ねていないと意味が決まらない。
            // group_by と集計を断っているのと同じ方針です。
            return Err(Error::msg(
                "having を使うには group_by が必要です。\
                 束ねずに絞りたいときは where を使ってください",
            ));
        }
        Ok(())
    }

    /// `update` / `delete` / `truncate` で使えない指定が付いていないか。
    ///
    /// 黙って全件に当てるより、エラーにしたほうが安全です。
    ///
    /// `order_by` だけは黙って無視します。SQL に入らなくても、当たる行が変わらない
    /// ためです。
    fn check_write(&self, what: &str) -> Result<()> {
        if !self.joins.is_empty() {
            return Err(Error::msg(format!(
                "join を付けた問い合わせでは {what} できません。where で絞ってください"
            )));
        }
        if !self.groups.is_empty() || !self.havings.is_empty() || self.distinct {
            return Err(Error::msg(format!(
                "group_by / having / distinct を付けた問い合わせでは {what} できません。\
                 組み立てた SQL に入らないので、黙って全件に当たるのを防ぐためです。\
                 先に主キーを取り出して、where_in で絞ってください"
            )));
        }
        if self.limit.is_some() || self.offset.is_some() {
            return Err(Error::msg(format!(
                "limit / offset を付けた問い合わせでは {what} できません。\
                 黙って全件に当たるのを防ぐためです。\
                 先に主キーを取り出して、where_in で絞ってください"
            )));
        }
        Ok(())
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
    ///
    /// `users.name` のように表名を付けた指定や、別名付きの式も渡せます。
    /// 1列だけ取るので、名前ではなく**位置**から読むためです。
    pub async fn value<T: FromValue>(&self, column: &str) -> Result<Option<T>> {
        let row = self.clone().select(&[column]).first().await?;
        match row.as_ref().and_then(|row| row.at(0)) {
            Some(value) => T::from_value(value)
                .map(Some)
                .map_err(|e| Error::msg(format!("`{column}` を読めません: {e}"))),
            None => Ok(None),
        }
    }

    /// ある列だけを一覧で取る。
    ///
    /// `pluck::<String>("users.name")` のように表名を付けても取れます。
    /// SQLite が返す列名は `name` になりますが、**位置**から読むためです。
    pub async fn pluck<T: FromValue>(&self, column: &str) -> Result<Vec<T>> {
        let rows = self.clone().select(&[column]).get().await?;
        rows.iter()
            .map(|row| match row.at(0) {
                Some(value) => T::from_value(value)
                    .map_err(|e| Error::msg(format!("`{column}` を読めません: {e}"))),
                None => Err(Error::msg(format!("`{column}` の値が返りませんでした"))),
            })
            .collect()
    }

    /// 件数を数える。
    ///
    /// `group_by` が付いているときは**グループの数**を返します。
    /// `distinct()` が付いているときは重なりを除いて数えます。
    pub async fn count(&self) -> Result<i64> {
        self.check()?;
        let (sql, bindings) = self.count_sql();
        let rows = self.source.fetch_all(&sql, &bindings).await?;
        match rows.first().and_then(|row| row.at(0)) {
            None | Some(Value::Null) => Ok(0),
            Some(value) => i64::from_value(value),
        }
    }

    /// `count` の SQL を組み立てる。
    ///
    /// `group_by` と `distinct` で形が変わります。そのまま `count(*)` を足すと、
    /// `group_by` ではグループごとの件数（＝最初のグループの件数）になり、
    /// `distinct` では重なりを除かない件数になります。
    fn count_sql(&self) -> (String, Vec<Value>) {
        if !self.groups.is_empty() {
            // 束ねた結果の「行の数」を数えたいので、副問い合わせの外から数える。
            return self.sub_count_sql();
        }
        if self.distinct {
            // 数える列が1つに決まるなら `count(distinct 列)`。
            // 決まらない（指定なし・複数・式）ときは副問い合わせで数える。
            return match self.distinct_count_column() {
                Some(column) => self.plain_count_sql(&format!("count(distinct {column})")),
                None => self.sub_count_sql(),
            };
        }
        self.plain_count_sql("count(*)")
    }

    /// `count(distinct …)` に使える列。1列だけ指定されているときだけ決まります。
    fn distinct_count_column(&self) -> Option<String> {
        let [column] = self.columns.as_slice() else {
            return None;
        };
        if !grammar::is_plain_identifier(column) {
            return None;
        }
        Some(grammar::quote(self.driver, column))
    }

    /// 並びと件数の指定を外した問い合わせ。集計はこの形で組み立てます。
    fn for_aggregate(&self) -> Self {
        let mut query = self.clone();
        query.orders.clear();
        query.limit = None;
        query.offset = None;
        query
    }

    /// 集計の式を1つだけ取る形。
    fn plain_count_sql(&self, expression: &str) -> (String, Vec<Value>) {
        let mut query = self.for_aggregate();
        // 重なりは `count(distinct …)` の側で見るので、ここでは外す。
        query.distinct = false;
        query.columns = vec![format!("{expression} as bengara_aggregate")];
        query.to_sql()
    }

    /// 副問い合わせの外から数える形。
    fn sub_count_sql(&self) -> (String, Vec<Value>) {
        let (inner, bindings) = self.for_aggregate().to_sql();
        (
            format!("select count(*) as bengara_aggregate from ({inner}) as bengara_sub"),
            bindings,
        )
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
        self.check()?;
        let (sql, bindings) = self.aggregate_sql(function, column)?;
        let rows = self.source.fetch_all(&sql, &bindings).await?;
        // 1列だけ取るので、名前ではなく位置から読む。
        match rows.first().and_then(|row| row.at(0)) {
            Some(Value::Null) | None => Ok(None),
            Some(value) => T::from_value(value).map(Some),
        }
    }

    /// 集計の SQL を組み立てる。
    ///
    /// `distinct()` が付いているときは `{関数}(distinct 列)` にします。
    /// `select distinct {関数}(列)` のままでは、重なりが除かれないためです。
    fn aggregate_sql(&self, function: &str, column: &str) -> Result<(String, Vec<Value>)> {
        if !self.groups.is_empty() {
            // 束ねた結果に対する sum / avg / min / max は、1つの値に決まらない。
            // 黙って最初のグループの値を返すのをやめて、書き方を伝える。
            return Err(Error::msg(format!(
                "group_by と {function}() は組み合わせられません。\
                 グループごとの値が欲しいときは、select に `{function}(列) as 名前` を書いて \
                 get() してください"
            )));
        }
        let plain = grammar::is_plain_identifier(column);
        if self.distinct && !plain {
            // `select distinct sum(式)` は重なりを除かない合計になる。
            // 列が1つに決まらないので、組み替えようがない。
            return Err(Error::msg(format!(
                "distinct() と {function}(`{column}`) は組み合わせられません。\
                 重なりを除いて集計できるのは、ただの列名を渡したときだけです"
            )));
        }
        let quoted = if column == "*" {
            "*".to_string()
        } else {
            grammar::quote(self.driver, column)
        };
        let expression = if self.distinct {
            // 重なりは集計の中で除く。`select distinct` のままでは効かない。
            format!("{function}(distinct {quoted}) as bengara_aggregate")
        } else {
            format!("{function}({quoted}) as bengara_aggregate")
        };
        let mut query = self.for_aggregate();
        // 重なりは集計の式の中で見るので、ここでは外す。
        query.distinct = false;
        query.columns = vec![expression];
        Ok(query.to_sql())
    }

    /// 1行足す。
    pub async fn insert(&self, values: &[(&str, Value)]) -> Result<Affected> {
        let rows = vec![values.to_vec()];
        self.insert_many(&rows).await
    }

    /// 何行かまとめて足す。
    ///
    /// **列の名前と並びは最初の行とそろえてください。** ずれているときはエラーにします。
    /// 行ごとに並びを読み替えると、取り違えに気づけないためです。
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
        ensure_column("insert_get_id_as の主キー", primary_key)?;
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
        let mut columns: Vec<String> = Vec::with_capacity(shape.len());
        for (name, _) in shape {
            ensure_column("insert", name)?;
            columns.push(grammar::quote(self.driver, name));
        }
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
    ///
    /// **`join` / `limit` / `offset` / `group_by` / `having` / `distinct` は
    /// 使えません。** 組み立てた SQL に入らないので、黙って全件に当たってしまいます。
    /// 付いているときはエラーにします。`order_by` は黙って無視します。
    pub async fn update(&self, values: &[(&str, Value)]) -> Result<u64> {
        self.check()?;
        self.check_write("update")?;
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
            ensure_column("update", name)?;
            c.sql.push_str(&grammar::quote(self.driver, name));
            c.sql.push_str(" = ");
            c.push_value(value.clone());
        }
        self.compile_wheres(&mut c);
        let affected = self.source.execute(&c.sql, &c.bindings).await?;
        Ok(affected.rows)
    }

    /// 条件に当てはまる行を消す。返るのは消した件数です。
    ///
    /// **`join` / `limit` / `offset` / `group_by` / `having` / `distinct` は
    /// 使えません。** 理由は `update` と同じです。
    pub async fn delete(&self) -> Result<u64> {
        self.check()?;
        self.check_write("delete")?;
        let mut c = Compiler::new(self.driver);
        c.sql.push_str("delete from ");
        c.sql.push_str(&grammar::quote(self.driver, &self.table));
        self.compile_wheres(&mut c);
        let affected = self.source.execute(&c.sql, &c.bindings).await?;
        Ok(affected.rows)
    }

    /// 表を空にする。**条件は効きません。**
    ///
    /// `where` を付けたときはエラーにします。黙って全件消すのを防ぐためです。
    /// `join` / `limit` などが使えないのも `delete` と同じです。
    pub async fn truncate(&self) -> Result<()> {
        self.check()?;
        self.check_write("truncate")?;
        if !self.wheres.is_empty() {
            return Err(Error::msg(
                "truncate では条件が効きません。delete() を使ってください",
            ));
        }
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
    ///
    /// `total` は `count()` と同じ数え方です（`group_by` ならグループの数、
    /// `distinct` なら重なりを除いた数）。
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

/// 投げ先から、SQL の組み立てに使うドライバを決める。
///
/// 設定が読めないときは SQLite を仮に使います。本当のエラーは終端メソッド
/// （`check`）で返します。
fn resolve_driver(source: &Source) -> Driver {
    source.driver().unwrap_or(Driver::Sqlite)
}

/// 列の名前として受け付けるか確かめる。その場でエラーを返せる所で使います。
///
/// `insert` と `update` の列名はここを通します。組み立てが `Result` を返すので、
/// `deferred_error` に積む必要がありません。
fn ensure_column(place: &str, column: &str) -> Result<()> {
    if grammar::is_plain_identifier(column) {
        return Ok(());
    }
    Err(Error::msg(format!(
        "{place} に渡した列名 `{column}` は使えません。英数字と `_` `.` だけが使えます"
    )))
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
    ///
    /// `?` の数と値の数が合っているかは、組み立てのとき
    /// （`QueryBuilder::check_raw`）に確かめます。
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
    fn 危ない列名は終端でエラーになる() {
        let danger = "id; drop table posts --";
        for query in [
            builder().order_by(danger),
            builder().order_by_desc(danger),
            builder().group_by(&[danger]),
            builder().having_op(danger, ">", 1),
        ] {
            let message = query.check().expect_err("断られる").to_string();
            assert!(message.contains("使えません"), "{message}");
            assert!(message.contains(danger), "{message}");
        }
    }

    #[test]
    fn 危ない列名はどの条件でも断る() {
        // 外から来た文字列が届きうる場所を、ひととおり通す。
        let danger = "id; drop table posts --";
        let empty: [i64; 0] = [];
        for query in [
            builder().where_(danger, 1),
            builder().where_op(danger, ">", 1),
            builder().or_where(danger, 1),
            builder().or_where_op(danger, ">", 1),
            builder().where_in(danger, &[1]),
            builder().where_not_in(danger, &[1]),
            builder().or_where_in(danger, &empty),
            builder().where_null(danger),
            builder().where_not_null(danger),
            builder().where_between(danger, 1, 2),
            builder().where_not_between(danger, 1, 2),
            builder().where_like(danger, "x"),
            builder().where_column(danger, "=", "b"),
            builder().where_column("a", "=", danger),
            builder().join("c", danger, "=", "posts.id"),
            builder().left_join("c", "c.post_id", "=", danger),
            builder().where_group(|q| q.where_(danger, 1)),
            QueryBuilder::new(Source::Default, danger),
        ] {
            let message = query.check().expect_err("断られる").to_string();
            assert!(message.contains("使えません"), "{message}");
            assert!(message.contains(danger), "{message}");
        }
    }

    #[test]
    fn 引用符を混ぜた列名も断る() {
        // `"` を含む名前は式として素通しされないので、検査に掛かる。
        let danger = "a\" or \"1\"=\"1";
        let message = builder()
            .where_(danger, 1)
            .check()
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("使えません"), "{message}");
    }

    #[test]
    fn 結合の表の名前と演算子も確かめる() {
        builder()
            .join("comments", "comments.post_id", "=", "posts.id")
            .check()
            .expect("普通の結合は通る");
        let message = builder()
            .join("c; drop table posts", "a", "=", "b")
            .check()
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("表の名前"), "{message}");
        let message = builder()
            .join("c", "a", "= 1 or 1", "b")
            .check()
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("演算子"), "{message}");
    }

    #[test]
    fn 危ない演算子は終端でエラーになる() {
        let danger = "= 1 or 1";
        for query in [
            builder().where_op("id", danger, 1),
            builder().or_where_op("id", danger, 1),
            builder().having_op("total", danger, 1),
            builder().where_column("a", danger, "b"),
        ] {
            let message = query.check().expect_err("断られる").to_string();
            assert!(message.contains("演算子"), "{message}");
        }
    }

    #[test]
    fn 普通の列名と演算子は通る() {
        let query = builder()
            .order_by("created_at")
            .order_by_desc("users.id")
            .group_by(&["post_id"])
            .having_op("total", ">=", 2)
            .where_op("views", "<", 10)
            .where_op("title", "NOT LIKE", "%x%")
            .where_column("a", "=", "b");
        query.check().expect("通る");
    }

    #[test]
    fn 並び順は生でも書ける() {
        let query = builder().order_by_raw("case when pinned then 0 else 1 end asc");
        query.check().expect("通る");
        let (sql, _) = query.to_sql();
        assert_eq!(
            sql,
            "select * from \"posts\" order by case when pinned then 0 else 1 end asc"
        );
    }

    #[test]
    fn 束ねたときの件数はグループの数を数える() {
        let (sql, bindings) = builder()
            .where_("status", "published")
            .group_by(&["post_id"])
            .count_sql();
        assert_eq!(
            sql,
            "select count(*) as bengara_aggregate from (\
             select * from \"posts\" where \"status\" = ? group by \"post_id\"\
             ) as bengara_sub"
        );
        assert_eq!(bindings, vec![Value::Text("published".into())]);
    }

    #[test]
    fn 重なりを除いた件数を数える() {
        // 列が1つに決まるときは count(distinct 列)。
        let (sql, _) = builder().distinct().select(&["author_id"]).count_sql();
        assert_eq!(
            sql,
            "select count(distinct \"author_id\") as bengara_aggregate from \"posts\""
        );

        // 決まらないときは副問い合わせ。
        let (sql, _) = builder().distinct().count_sql();
        assert_eq!(
            sql,
            "select count(*) as bengara_aggregate from (\
             select distinct * from \"posts\"\
             ) as bengara_sub"
        );
    }

    #[test]
    fn 条件なしの件数はそのまま数える() {
        let (sql, _) = builder().latest().limit(10).count_sql();
        assert_eq!(sql, "select count(*) as bengara_aggregate from \"posts\"");
    }

    #[test]
    fn 生のsqlは値の数を確かめる() {
        let message = builder()
            .where_raw("length(title) > ?", &[])
            .check()
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("合っていません"), "{message}");
        builder()
            .where_raw("length(title) > ?", &[Value::Int(3)])
            .check()
            .expect("数が合えば通る");
    }

    #[test]
    fn postgres_では_offset_だけでも通る形にする() {
        let query = QueryBuilder {
            driver: Driver::Postgres,
            ..builder()
        };
        let (sql, _) = query.offset(5).to_sql();
        assert_eq!(sql, "select * from \"posts\" limit all offset 5");
    }

    #[tokio::test]
    async fn 結合や件数を付けた更新と削除は断る() {
        let joined = builder().join("c", "c.post_id", "=", "posts.id");
        let message = joined
            .update(&[("title", Value::Text("x".into()))])
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("join"), "{message}");

        let limited = builder().limit(1);
        let message = limited
            .update(&[("title", Value::Text("x".into()))])
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("limit"), "{message}");

        let message = builder()
            .limit(1)
            .delete()
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("limit"), "{message}");
    }

    #[tokio::test]
    async fn 束ねや重なりを付けた更新と削除も断る() {
        // group_by / having / distinct は SQL に入らない。全件に当たるのを防ぐ。
        let values = [("title", Value::Text("x".into()))];
        let message = builder()
            .group_by(&["author_id"])
            .update(&values)
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("group_by"), "{message}");

        let message = builder()
            .group_by(&["author_id"])
            .having_op("views", ">", 1)
            .delete()
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("group_by"), "{message}");

        let message = builder()
            .distinct()
            .delete()
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("distinct"), "{message}");
    }

    #[tokio::test]
    async fn truncateは条件を断る() {
        let message = builder()
            .where_("status", "draft")
            .truncate()
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("delete()"), "{message}");

        let message = builder()
            .limit(1)
            .truncate()
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("limit"), "{message}");
    }

    #[test]
    fn group_byなしのhavingは断る() {
        let message = builder()
            .having_op("total", ">", 1)
            .check()
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("group_by が必要"), "{message}");
        let message = builder()
            .having_raw("count(*) > ?", &[Value::Int(1)])
            .check()
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("group_by が必要"), "{message}");
        builder()
            .group_by(&["post_id"])
            .having_op("total", ">", 1)
            .check()
            .expect("束ねていれば通る");
    }

    #[test]
    fn 重なりを除いた集計は関数の中で除く() {
        let (sql, _) = builder()
            .distinct()
            .aggregate_sql("sum", "views")
            .expect("組み立てられる");
        assert_eq!(
            sql,
            "select sum(distinct \"views\") as bengara_aggregate from \"posts\""
        );

        // distinct が無ければこれまでどおり。
        let (sql, _) = builder()
            .aggregate_sql("avg", "views")
            .expect("組み立てられる");
        assert_eq!(
            sql,
            "select avg(\"views\") as bengara_aggregate from \"posts\""
        );

        // 列が1つに決まらないときは断る。黙って重なりを数えないため。
        let message = builder()
            .distinct()
            .aggregate_sql("sum", "*")
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("distinct"), "{message}");
    }

    #[test]
    fn デバッグ表示に組み立ての間違いも出る() {
        let text = format!("{:?}", builder().order_by("id; drop table posts"));
        assert!(text.contains("deferred_error"), "{text}");
        assert!(text.contains("使えません"), "{text}");
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
