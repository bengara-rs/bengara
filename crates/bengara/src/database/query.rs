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
    /// いまの投げ先のドライバが決まらなかった理由（接続名の間違いなど）。
    ///
    /// **投げ先に付いた間違いなので、`deferred_error` とは別に持ちます。**
    /// 投げ先を替えたら（`with_source`）入れ直します。同じ欄に積むと、
    /// 有効なトランザクションへ替えた後も、元の接続の間違いで落ちていました。
    driver_error: Option<String>,
    /// 組み立てのときに見つけた間違い。終端メソッドでこのエラーを返す。
    ///
    /// 組み立ての途中でパニックさせないため、最初の1つだけ覚えておきます。
    deferred_error: Option<String>,
    /// `*_raw` に `$` を書いた最初の場所（`where_raw` など）。
    ///
    /// 断るのは PostgreSQL のときだけですが、**ドライバは後から替わります**
    /// （`using(&tx)`）。書いた時点では決めず、終端で、いまのドライバと照らします。
    raw_dollar: Option<String>,
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

/// 終端メソッドが差し替える所だけを持つ形。
///
/// `select` の SQL は、取る列・重なり・並び・件数だけが場面で変わります。
/// 以前は `first()` や `count()` がビルダー全体を `clone()` してから
/// 差し替えていました。1行取るだけで条件と値まで複製するので、ここだけ
/// 借りて組み立てます。**組み立てる SQL は clone していた頃と同じです。**
struct Shape<'a> {
    columns: &'a [String],
    distinct: bool,
    orders: &'a [Order],
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
            // 接続の間違いと `$` の直書きも、終端で返すものと同じ文で出す。
            .field("deferred_error", &self.pending_error())
            .finish()
    }
}

impl QueryBuilder {
    /// 新しく組み立てを始める。`DB::table(...)` から呼ばれます。
    pub(crate) fn new(source: Source, table: impl Into<String>) -> Self {
        let (driver, driver_error) = resolve_driver(&source);
        let mut query = Self {
            source,
            driver,
            driver_error,
            deferred_error: None,
            raw_dollar: None,
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
    ///
    /// **接続の間違い（`driver_error`）は入れ直します。** 元の投げ先のものなので、
    /// 替えた先には当てはまりません。既定の接続が設定に無くても、
    /// `Post::query().using(&tx)` は有効なトランザクションで動きます。
    pub(crate) fn with_source(mut self, source: Source) -> Self {
        let (driver, driver_error) = resolve_driver(&source);
        self.driver = driver;
        self.driver_error = driver_error;
        self.source = source;
        self
    }

    /// 終端メソッドで返す間違い。無ければ `None`。
    ///
    /// 順は、接続の間違い → 組み立ての間違い → `$` の直書き。
    /// 接続名の間違いは、列名や演算子の間違いより先に知らせます。
    fn pending_error(&self) -> Option<String> {
        if let Some(message) = &self.driver_error {
            return Some(message.clone());
        }
        if let Some(message) = &self.deferred_error {
            return Some(message.clone());
        }
        self.dollar_error()
    }

    /// PostgreSQL へ投げるのに、`*_raw` に `$` を書いていたときの文。
    fn dollar_error(&self) -> Option<String> {
        if self.driver != Driver::Postgres {
            return None;
        }
        let place = self.raw_dollar.as_ref()?;
        Some(format!(
            "{place} に書いた `$` は使えません。ビルダーの `*_raw` では `?` を使ってください。\
             `$1` は DB::select / DB::statement 専用です"
        ))
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

    /// 列と列を比べる演算子として受け付けるか確かめる。
    fn check_operator(&mut self, place: &str, operator: &str) {
        if !grammar::is_allowed_operator(operator) {
            self.defer(format!(
                "{place} に渡した演算子 `{operator}` は使えません（使えるもの: {}）",
                grammar::OPERATORS.join(", ")
            ));
        }
    }

    /// 値1つと比べる演算子として受け付けるか確かめる。
    ///
    /// `in` / `between` 系は値1つでは SQL が成り立ちません。専用のメソッドへ
    /// 案内します。
    fn check_value_operator(&mut self, place: &str, operator: &str) {
        if grammar::is_allowed_value_operator(operator) {
            return;
        }
        if grammar::is_set_operator(operator) {
            self.defer(format!(
                "{place} に渡した演算子 `{operator}` は、値1つとは比べられません。\
                 一覧と比べるときは where_in / where_not_in、\
                 範囲で絞るときは where_between / where_not_between を使ってください"
            ));
            return;
        }
        self.defer(format!(
            "{place} に渡した演算子 `{operator}` は使えません（使えるもの: {}）",
            grammar::VALUE_OPERATORS.join(", ")
        ));
    }

    /// 生の SQL に書いた `?` の数と、渡した値の数が合っているか確かめる。
    fn check_raw(&mut self, place: &str, sql: &str, bindings: &[Value]) {
        self.check_raw_dollar(place, sql);
        let holes = grammar::count_placeholders(sql);
        if holes != bindings.len() {
            self.defer(format!(
                "{place} に書いた `?` の数（{holes}）と、渡した値の数（{}）が合っていません。\
                 `?` は値の置き場所としてのみ使えます",
                bindings.len()
            ));
        }
    }

    /// PostgreSQL で `$1` を直接書いていないか確かめる。
    ///
    /// ビルダーは値を積んだ順に `$1` `$2` … を振ります。利用者が `$1` と書くと、
    /// ビルダーが振った番号と重なります。`where_("status", "published")` の次に
    /// `where_raw("views > $1", &[])` と書くと、値は1つしか渡っていないので
    /// `views > 'published'` を比べることになります。
    ///
    /// `DB::select` / `DB::statement` は値をその SQL だけに渡すので、
    /// そちらでは `$1` の直書きを通します。
    ///
    /// **ここでは書いた場所を覚えるだけです。** 断るかどうかは終端の `check()` が、
    /// そのときのドライバで決めます（`dollar_error`）。ここで決めると、
    /// SQLite の接続で組み立ててから `using(&tx)` で PostgreSQL の
    /// トランザクションへ替えたときに、検査を抜けていました。
    fn check_raw_dollar(&mut self, place: &str, sql: &str) {
        if self.raw_dollar.is_none() && sql.contains('$') {
            self.raw_dollar = Some(place.to_string());
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
        self.check_value_operator("where_op", operator);
        self.push_basic("where_op", Glue::And, column, operator, value.into_value())
    }

    /// `or` でつないで、列が値と等しい。
    pub fn or_where(self, column: &str, value: impl IntoValue) -> Self {
        self.push_basic("or_where", Glue::Or, column, "=", value.into_value())
    }

    /// `or` でつないで、列と値を演算子で比べる。演算子は許可一覧で照合します。
    pub fn or_where_op(mut self, column: &str, operator: &str, value: impl IntoValue) -> Self {
        self.check_value_operator("or_where_op", operator);
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
        // `$` の直書きも引き継ぐ。断るかどうかは、外側の終端が決める。
        if self.raw_dollar.is_none() {
            self.raw_dollar = nested.raw_dollar;
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
        let condition = self.basic_condition(place, boolean, column, operator, value);
        self.wheres.push(condition);
        self
    }

    /// 列と値1つを比べる条件を作る。`where` 系と `having_op` が通ります。
    ///
    /// **`NULL` と比べるときは `is null` / `is not null` に読み替えます。**
    ///
    /// | 演算子             | 値が `NULL` のとき |
    /// |--------------------|--------------------|
    /// | `=` / `is`         | `is null`          |
    /// | `!=` `<>` `is not` | `is not null`      |
    /// | それ以外           | そのまま値を渡す   |
    ///
    /// `"c" = NULL` は真にも偽にもならないので、エラーも無く必ず 0 件になります。
    /// `where_("deleted_at", None::<String>)` と書いた人が欲しいのは
    /// `is null` です（Laravel の `where('deleted_at', null)` と同じ）。
    ///
    /// **`is` / `is not` に `NULL` 以外を渡したときは断ります。**
    /// `"c" is ?` が通るのは SQLite だけで、PostgreSQL と MySQL では
    /// 構文エラーになります。SQLite で通って本番で落ちる形を残しません。
    fn basic_condition(
        &mut self,
        place: &str,
        boolean: Glue,
        column: &str,
        operator: &str,
        value: Value,
    ) -> Where {
        self.check_column(place, column);
        if value.is_null() {
            if let Some(not) = grammar::null_comparison(operator) {
                return Where::Null {
                    boolean,
                    column: column.to_string(),
                    not,
                };
            }
        } else if grammar::is_null_operator(operator).is_some() {
            self.defer(format!(
                "{place} の演算子 `{operator}` には NULL だけを渡せます。\
                 `is ?` の形は PostgreSQL と MySQL で構文エラーになるためです。\
                 NULL かどうかは where_null / where_not_null、\
                 値と比べるときは `=` / `!=` を使ってください"
            ));
        }
        Where::Basic {
            boolean,
            column: column.to_string(),
            operator: operator.to_string(),
            value,
        }
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
        if values.len() > MAX_IN_VALUES {
            self.defer(format!(
                "{place} に渡した値が多すぎます（{} 件。上限は {MAX_IN_VALUES} 件）。\
                 一覧を分割して、何回かに分けて問い合わせてください",
                values.len()
            ));
        }
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
    /// **渡せるのは実在の列だけです。** `select` に書いた別名
    /// （`count(*) as total` の `total`）は渡せません。PostgreSQL は `having` で
    /// select の別名を解決しないので、`column "total" does not exist` になります。
    /// SQLite と MySQL では通るので、気づかないまま移すと落ちます。
    ///
    /// 集計の結果で絞るときは `having_raw` を使ってください。
    ///
    /// ```ignore
    /// // 別名ではなく式をそのまま書く。
    /// .group_by(&["author_id"]).having_raw("count(*) > ?", &[2.into()])
    /// ```
    ///
    /// 列名は英数字と `_` `.` だけ、演算子は許可一覧で照合します。
    pub fn having_op(mut self, column: &str, operator: &str, value: impl IntoValue) -> Self {
        self.check_value_operator("having_op", operator);
        // `NULL` の読み替えと `is` の検査は `where_op` と同じ（`basic_condition`）。
        let condition =
            self.basic_condition("having_op", Glue::And, column, operator, value.into_value());
        self.havings.push(condition);
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
    ///
    /// PostgreSQL で `$1` と書いたときだけ、終端メソッドでエラーになります。
    /// ビルダーが振る番号と重なるためです。
    pub fn order_by_raw(mut self, sql: &str) -> Self {
        self.check_raw_dollar("order_by_raw", sql);
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
    ///
    /// 掛け算はあふれさせません。`?page=18446744073709551615` のような値が
    /// `req.page()` からそのまま届くので、飽和させて上限に張り付かせます
    /// （デバッグビルドでパニックし、リリースでは別のページが返っていました）。
    pub fn for_page(self, page: u64, per_page: u64) -> Self {
        let per_page = per_page.max(1);
        self.limit(per_page).offset(page_offset(page, per_page))
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
        self.select_sql(&self.shape())
    }

    /// 組み立てたままの形。
    fn shape(&self) -> Shape<'_> {
        Shape {
            columns: &self.columns,
            distinct: self.distinct,
            orders: &self.orders,
            limit: self.limit,
            offset: self.offset,
        }
    }

    /// 並びと件数の指定を外した形。集計はこの形で組み立てます。
    fn aggregate_shape(&self) -> Shape<'_> {
        Shape {
            orders: &[],
            limit: None,
            offset: None,
            ..self.shape()
        }
    }

    /// `select` の SQL を組み立てる。差し替える所は `Shape` から取ります。
    fn select_sql(&self, shape: &Shape<'_>) -> (String, Vec<Value>) {
        let mut c = Compiler::new(self.driver);
        c.sql.push_str("select ");
        if shape.distinct {
            c.sql.push_str("distinct ");
        }
        if shape.columns.is_empty() {
            c.sql.push('*');
        } else {
            let columns: Vec<String> = shape
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
        self.compile_orders(&mut c, shape.orders);
        self.compile_limit(&mut c, shape.limit, shape.offset);
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

    fn compile_orders(&self, c: &mut Compiler, orders: &[Order]) {
        if orders.is_empty() {
            return;
        }
        let orders: Vec<String> = orders
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

    fn compile_limit(&self, c: &mut Compiler, limit: Option<u64>, offset: Option<u64>) {
        // 件数は数値なので、そのまま置いても安全。
        // SQLite と PostgreSQL は符号付き 64 ビットまでしか受け付けないので、
        // それより大きい値は上限に丸める（`u64::MAX` は "datatype mismatch" になる）。
        if let Some(limit) = limit {
            c.sql.push_str(&format!(" limit {}", clamp_count(limit)));
        }
        if let Some(offset) = offset {
            if limit.is_none() {
                // SQLite と MySQL は limit が無いと offset を受け付けない。
                // 「上限なし」の書き方は方言で違うので grammar に集めてある。
                c.sql.push_str(grammar::no_limit(self.driver));
            }
            c.sql.push_str(&format!(" offset {}", clamp_count(offset)));
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
                // `NULL` と `=` / `is` などの組は、組み立てのとき（`basic_condition`）に
                // `Where::Null` へ読み替えてある。ここには来ない。
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
    ///
    /// モデルの側（`model.rs`）も呼びます。問い合わせずに「無い」を返すときに、
    /// 組み立てと接続の間違いだけは知らせるためです。
    pub(crate) fn check(&self) -> Result<()> {
        // 接続の間違いは、いまの投げ先に聞き直す（`driver_error` は表示用の控え）。
        self.source.driver()?;
        if let Some(message) = &self.deferred_error {
            return Err(Error::msg(message.clone()));
        }
        // `$` の直書きは、ここで初めて、いまのドライバと照らす。
        if let Some(message) = self.dollar_error() {
            return Err(Error::msg(message));
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

    /// `insert` 系で使えない指定が付いていないか。
    ///
    /// 足す行に条件や結合は効きません。黙って無視すると、絞ったつもりの
    /// `insert` が通ってしまいます。
    ///
    /// **文は `check_write` と分けます。** あちらの「先に主キーを取り出して
    /// where_in で絞る」は、行を足す場面には当てはまらない案内です。
    fn check_insert(&self, what: &str) -> Result<()> {
        if !self.joins.is_empty() {
            return Err(Error::msg(format!(
                "join を付けた問い合わせでは {what} できません。\
                 足す行に結合は効きません。足す値だけを渡してください"
            )));
        }
        if !self.groups.is_empty() || !self.havings.is_empty() || self.distinct {
            return Err(Error::msg(format!(
                "group_by / having / distinct を付けた問い合わせでは {what} できません。\
                 足す行には効きません。足す値だけを渡してください"
            )));
        }
        if self.limit.is_some() || self.offset.is_some() {
            return Err(Error::msg(format!(
                "limit / offset を付けた問い合わせでは {what} できません。\
                 足す行の数は、渡した行の数で決まります。limit / offset を外してください"
            )));
        }
        if !self.wheres.is_empty() {
            return Err(Error::msg(format!(
                "{what} では条件が効きません。足す値だけを渡してください"
            )));
        }
        Ok(())
    }

    // ---- 実行 ----

    /// 行を返す文を投げる。**終端メソッドは必ずここか `run` を通します。**
    ///
    /// 束縛する値の総数を、投げる直前に確かめるためです。`where_in` 単体の
    /// 件数は組み立てのときに見ていますが、何回か重ねた合計はここでしか
    /// 分かりません。
    async fn fetch(&self, sql: &str, bindings: &[Value]) -> Result<Vec<Row>> {
        ensure_binding_count(self.driver, bindings.len())?;
        self.source.fetch_all(sql, bindings).await
    }

    /// 行を返さない文を投げる。確かめる内容は `fetch` と同じです。
    async fn run(&self, sql: &str, bindings: &[Value]) -> Result<Affected> {
        ensure_binding_count(self.driver, bindings.len())?;
        self.source.execute(sql, bindings).await
    }

    /// 行を取る。
    pub async fn get(&self) -> Result<Vec<Row>> {
        self.check()?;
        let (sql, bindings) = self.to_sql();
        self.fetch(&sql, &bindings).await
    }

    /// 最初の1行を取る。
    pub async fn first(&self) -> Result<Option<Row>> {
        self.check()?;
        let (sql, bindings) = self.select_sql(&Shape {
            limit: Some(1),
            ..self.shape()
        });
        let rows = self.fetch(&sql, &bindings).await?;
        Ok(rows.into_iter().next())
    }

    /// **`id` という名前の列**で1行取る。
    ///
    /// 主キーの名前が `id` でない表には使えません。その表では
    /// `where_(列, 値)` に `first()` を続けてください。モデルなら
    /// `Model::find` / `ModelQuery::find` が主キーの名前を知っています。
    ///
    /// 整数に読める文字列は整数として渡します。PostgreSQL は `bigint = text`
    /// を断るためです。
    ///
    /// 条件を1つ足すので、ここだけはビルダーを複製します。
    /// 以前は `first()` も複製していたので、2回複製していました。
    ///
    /// **`NULL`（`None`）を渡したときは、問い合わせずに `None` を返します。**
    /// `where_` は `NULL` を `is null` に読み替えるので、そのまま渡すと
    /// 「`id` が NULL の行」を探してしまいます。
    pub async fn find(&self, id: impl IntoValue) -> Result<Option<Row>> {
        let id = super::normalize_key(id.into_value());
        if id.is_null() {
            // 組み立ての間違いは、問い合わせないときも知らせる。
            self.check()?;
            return Ok(None);
        }
        self.clone().where_("id", id).first().await
    }

    /// 最初の行の、ある列の値を取る。
    ///
    /// `users.name` のように表名を付けた指定も渡せます。
    /// 1列だけ取るので、名前ではなく**位置**から読むためです。
    ///
    /// 列名は英数字と `_` `.` だけが使えます。式が欲しいときは `select(...)` ＋
    /// `get()` を使ってください。
    ///
    /// **NULL がありうる列は `Option<T>` で読んでください**
    /// （`value::<Option<i64>>("views")`）。`T` のままだと、NULL の行で
    /// 読み出しのエラーになります。`row.get::<T>()` と同じ決まりです。
    pub async fn value<T: FromValue>(&self, column: &str) -> Result<Option<T>> {
        ensure_value_column("value", column)?;
        self.check()?;
        let columns = vec![column.to_string()];
        let (sql, bindings) = self.select_sql(&Shape {
            columns: &columns,
            limit: Some(1),
            ..self.shape()
        });
        let rows = self.fetch(&sql, &bindings).await?;
        let row = rows.into_iter().next();
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
    ///
    /// 列名は英数字と `_` `.` だけが使えます。式が欲しいときは `select(...)` ＋
    /// `get()` を使ってください。
    ///
    /// **NULL がありうる列は `Option<T>` で読んでください**
    /// （`pluck::<Option<String>>("body")`）。`T` のままだと、NULL の行が
    /// 1つでもあると読み出しのエラーになります。
    pub async fn pluck<T: FromValue>(&self, column: &str) -> Result<Vec<T>> {
        ensure_value_column("pluck", column)?;
        self.check()?;
        let columns = vec![column.to_string()];
        let (sql, bindings) = self.select_sql(&Shape {
            columns: &columns,
            ..self.shape()
        });
        let rows = self.fetch(&sql, &bindings).await?;
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
    ///
    /// **`distinct()` を付けたときは `select` で列を1つ指定してください。**
    /// 数える列が決まらないと断ります（理由は `count_sql` にあります）。
    pub async fn count(&self) -> Result<i64> {
        self.check()?;
        let (sql, bindings) = self.count_sql()?;
        let rows = self.fetch(&sql, &bindings).await?;
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
    fn count_sql(&self) -> Result<(String, Vec<Value>)> {
        if !self.groups.is_empty() {
            // 束ねた結果の「行の数」を数えたいので、副問い合わせの外から数える。
            return Ok(self.sub_count_sql());
        }
        if self.distinct {
            // 数える列が1つに決まるなら `count(distinct 列)`。
            let Some(column) = self.distinct_count_column() else {
                // 決まらない（指定なし・複数・式）ときは断る。
                // 派生表で数えると、MySQL は列名が重なってエラー 1060 になります
                // （`join` を併用したとき）。SQL を出さずに書き方を伝えます。
                return Err(Error::msg(
                    "distinct() を付けた問い合わせの件数は、数える列が1つに決まらないと\
                     数えられません。select で列を1つ指定してください\
                     （例: select(&[\"author_id\"])）",
                ));
            };
            return Ok(self.plain_count_sql(&format!("count(distinct {column})")));
        }
        Ok(self.plain_count_sql("count(*)"))
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

    /// 集計の式を1つだけ取る形。
    fn plain_count_sql(&self, expression: &str) -> (String, Vec<Value>) {
        let columns = vec![format!("{expression} as bengara_aggregate")];
        self.select_sql(&Shape {
            columns: &columns,
            // 重なりは `count(distinct …)` の側で見るので、ここでは外す。
            distinct: false,
            ..self.aggregate_shape()
        })
    }

    /// 副問い合わせの外からグループの数を数える形。
    ///
    /// **内側の列は `1` に固定します。** 以前は組み立てたままの列を入れていたので、
    /// 指定が無いと `select *` になりました。PostgreSQL は
    /// `column "t.id" must appear in the GROUP BY clause`、MySQL は既定の
    /// `ONLY_FULL_GROUP_BY` でエラー 1055 になり、SQLite だけが通る形でした。
    fn sub_count_sql(&self) -> (String, Vec<Value>) {
        let columns = vec!["1 as bengara_group".to_string()];
        let (inner, bindings) = self.select_sql(&Shape {
            columns: &columns,
            // 内側の中身は見ないので、重なりを除く意味がない。
            // 除くとグループが1行に潰れて、数が合わなくなります。
            distinct: false,
            ..self.aggregate_shape()
        });
        (
            format!("select count(*) as bengara_aggregate from ({inner}) as bengara_sub"),
            bindings,
        )
    }

    /// 1件でもあるか。
    ///
    /// 並びと件数の指定は外します。`latest()` を付けた問い合わせでも、
    /// 並べ替えの手間をかけないためです。
    pub async fn exists(&self) -> Result<bool> {
        self.check()?;
        let (sql, bindings) = self.exists_sql();
        let rows = self.fetch(&sql, &bindings).await?;
        Ok(!rows.is_empty())
    }

    /// `exists` の SQL を組み立てる。
    fn exists_sql(&self) -> (String, Vec<Value>) {
        let columns = vec!["1 as bengara_exists".to_string()];
        self.select_sql(&Shape {
            columns: &columns,
            limit: Some(1),
            ..self.aggregate_shape()
        })
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
        let rows = self.fetch(&sql, &bindings).await?;
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
            // `count(*)` と同じ書き方。ここだけ別扱いにする。
            "*".to_string()
        } else {
            // `quote` は空白や `(` を含む名前を式として素通しする。
            // 検査しないと列名がそのまま SQL に入る。
            ensure_value_column(&format!("{function}()"), column)?;
            grammar::quote(self.driver, column)
        };
        let expression = if self.distinct {
            // 重なりは集計の中で除く。`select distinct` のままでは効かない。
            format!("{function}(distinct {quoted}) as bengara_aggregate")
        } else {
            format!("{function}({quoted}) as bengara_aggregate")
        };
        let columns = vec![expression];
        Ok(self.select_sql(&Shape {
            columns: &columns,
            // 重なりは集計の式の中で見るので、ここでは外す。
            distinct: false,
            ..self.aggregate_shape()
        }))
    }

    /// 1行足す。
    pub async fn insert(&self, values: &[(&str, Value)]) -> Result<Affected> {
        // 行は借りたまま渡す。以前は `values.to_vec()` で1回、束縛の一覧へ
        // 積むときにもう1回、値を複製していました。
        self.insert_rows(&[values]).await
    }

    /// 何行かまとめて足す。
    ///
    /// **列の名前と並びは最初の行とそろえてください。** ずれているときはエラーにします。
    /// 行ごとに並びを読み替えると、取り違えに気づけないためです。
    ///
    /// **`where_` / `join` / `limit` などは使えません。** 組み立てた SQL に入らない
    /// ので、付いているときはエラーにします。
    ///
    /// **1回に渡せる値の数（行 × 列）には上限があります。** SQLite は 32,766、
    /// MySQL と PostgreSQL は 65,535 です。超えるときはエラーにするので、
    /// 行を分割して何回かに分けて呼んでください。
    pub async fn insert_many(&self, rows: &[Vec<(&str, Value)>]) -> Result<Affected> {
        self.insert_rows(rows).await
    }

    /// `insert` と `insert_many` の中身。行は借りたまま受けます。
    async fn insert_rows<'a, R>(&self, rows: &[R]) -> Result<Affected>
    where
        R: AsRef<[(&'a str, Value)]>,
    {
        self.check()?;
        self.check_insert("insert")?;
        let Some(first) = rows.first() else {
            return Ok(Affected::default());
        };
        if first.as_ref().is_empty() {
            return Err(Error::msg("insert に渡す列がありません"));
        }
        let (sql, bindings) = self.insert_sql(rows, "")?;
        self.run(&sql, &bindings).await
    }

    /// 1行足して、自動採番された ID を返す。
    pub async fn insert_get_id(&self, values: &[(&str, Value)]) -> Result<i64> {
        self.insert_get_id_as(values, "id").await
    }

    /// 1行足して、採番された主キーの値を返す。
    ///
    /// **`primary_key` を見るのは PostgreSQL だけです。** ドライバごとに返るものが
    /// 違います。
    ///
    /// | ドライバ   | 返るもの                             | `primary_key` の使い道 |
    /// |------------|--------------------------------------|------------------------|
    /// | PostgreSQL | `returning` が返した指定の列の値     | 列名として SQL に入る  |
    /// | SQLite     | 直前に入れた行の rowid               | 検査だけ               |
    /// | MySQL      | `last_insert_id()`                   | 検査だけ               |
    ///
    /// SQLite の `integer primary key` は rowid の別名なので、名前が `id` でなくても
    /// 正しい値が返ります。MySQL の `last_insert_id()` も、`auto_increment` の列の
    /// 名前に関係なく、その列に採番された値を返します。
    ///
    /// **外れるのは、自動採番でない列を `primary_key` に渡したときです。**
    /// その場合、SQLite は指定とは別の値（rowid）を返し、MySQL はエラーにします。
    /// 名前だけでは見分けられないので（`id` 以外でも普通に正しい）、
    /// 警告は出しません。毎回の `insert` で鳴って雑音になるためです。
    ///
    /// **採番されない表に使うと、MySQL ではエラー（「insert が ID を返しませんでした」）
    /// になります。行は入っています。** `last_insert_id()` は `auto_increment` が
    /// 働かなかったときに `0` を返し、bengara はその `0` を「番号なし」と見なすためです
    /// （`0` を主キーとして返しません）。`auto_increment` を持たない表（主キーが
    /// 文字列の表や、値を自分で入れる表）がこれに当たります。そういう表には
    /// [`insert`](Self::insert) を使ってください。
    /// SQLite では rowid が返るので、エラーにはならず、主キーとは別の値になります。
    pub async fn insert_get_id_as(
        &self,
        values: &[(&str, Value)],
        primary_key: &str,
    ) -> Result<i64> {
        self.check()?;
        self.check_insert("insert")?;
        if values.is_empty() {
            return Err(Error::msg("insert に渡す列がありません"));
        }
        // 主キーは `returning` に置くだけなので、`t.id` のような表名付きも通す。
        ensure_column("insert_get_id_as の主キー", primary_key)?;
        let returning = grammar::returning_id(self.driver, primary_key);
        let (sql, bindings) = self.insert_sql(&[values], &returning)?;

        if self.driver == Driver::Postgres {
            let rows = self.fetch(&sql, &bindings).await?;
            // 1列だけ返るので位置で読む。`t.id` のように表名を付けて渡されると、
            // 返る列名は `id` になって名前では引けない。
            return match rows.first().and_then(|row| row.at(0)) {
                Some(value) => i64::from_value(value),
                None => Err(Error::msg("insert が ID を返しませんでした")),
            };
        }

        let affected = self.run(&sql, &bindings).await?;
        affected
            .last_insert_id
            .ok_or_else(|| Error::msg("insert が ID を返しませんでした"))
    }

    /// `insert` の SQL を組み立てる。列の名前と並びは最初の行から取ります。
    ///
    /// 行は `Vec` でもスライスでも受けます（`AsRef`）。1行の `insert` が
    /// 行を複製せずに渡せるようにするためです。
    fn insert_sql<'a, R>(&self, rows: &[R], suffix: &str) -> Result<(String, Vec<Value>)>
    where
        R: AsRef<[(&'a str, Value)]>,
    {
        let shape: &[(&str, Value)] = match rows.first() {
            Some(first) => first.as_ref(),
            None => &[],
        };
        let mut c = Compiler::new(self.driver);
        let mut columns: Vec<String> = Vec::with_capacity(shape.len());
        for (name, _) in shape {
            ensure_write_column("insert", name)?;
            columns.push(grammar::quote(self.driver, name));
        }
        c.sql.push_str("insert into ");
        c.sql.push_str(&grammar::quote(self.driver, &self.table));
        c.sql.push_str(" (");
        c.sql.push_str(&columns.join(", "));
        c.sql.push_str(") values ");

        for (index, row) in rows.iter().enumerate() {
            let row = row.as_ref();
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
            ensure_write_column("update", name)?;
            c.sql.push_str(&grammar::quote(self.driver, name));
            c.sql.push_str(" = ");
            c.push_value(value.clone());
        }
        self.compile_wheres(&mut c);
        let affected = self.run(&c.sql, &c.bindings).await?;
        Ok(affected.rows)
    }

    /// 列の値を足す。返るのは変わった件数です。
    ///
    /// `views = views + 1` を1文で流します。読んでから書くのと違い、同時に
    /// 2本来ても片方の `+1` が消えません。
    ///
    /// ```ignore
    /// DB::table("posts").where_("id", id).increment("views", 1).await?;
    /// ```
    ///
    /// 列名は英数字と `_` だけが使えます（表名は付けられません）。
    /// `update` と同じく、**`updated_at` は自動で入れません。**
    /// 使えない指定（`join` / `limit` など）は `update` と同じです。
    pub async fn increment(&self, column: &str, amount: i64) -> Result<u64> {
        self.step("increment", column, '+', amount).await
    }

    /// 列の値を引く。中身は `increment` と同じで、符号だけが違います。
    ///
    /// ```ignore
    /// DB::table("posts").where_("id", id).decrement("stock", 1).await?;
    /// ```
    pub async fn decrement(&self, column: &str, amount: i64) -> Result<u64> {
        self.step("decrement", column, '-', amount).await
    }

    /// `increment` と `decrement` の中身。
    async fn step(&self, place: &str, column: &str, sign: char, amount: i64) -> Result<u64> {
        self.check()?;
        self.check_write(place)?;
        let (sql, bindings) = self.step_sql(place, column, sign, amount)?;
        let affected = self.run(&sql, &bindings).await?;
        Ok(affected.rows)
    }

    /// `increment` と `decrement` の SQL を組み立てる。
    fn step_sql(
        &self,
        place: &str,
        column: &str,
        sign: char,
        amount: i64,
    ) -> Result<(String, Vec<Value>)> {
        // 書き込む列なので、表名付き（`posts.views`）は断る。
        ensure_write_column(place, column)?;
        let quoted = grammar::quote(self.driver, column);
        let mut c = Compiler::new(self.driver);
        c.sql.push_str("update ");
        c.sql.push_str(&grammar::quote(self.driver, &self.table));
        c.sql.push_str(" set ");
        c.sql.push_str(&quoted);
        c.sql.push_str(" = ");
        c.sql.push_str(&quoted);
        c.sql.push(' ');
        c.sql.push(sign);
        c.sql.push(' ');
        // 増やす量もプレースホルダで渡す。SQL に埋め込まない。
        c.push_value(Value::Int(amount));
        self.compile_wheres(&mut c);
        Ok((c.sql, c.bindings))
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
        let affected = self.run(&c.sql, &c.bindings).await?;
        Ok(affected.rows)
    }

    /// 表を空にする。**条件は効きません。** 自動採番も 1 に戻します。
    ///
    /// `where` を付けたときはエラーにします。黙って全件消すのを防ぐためです。
    /// `join` / `limit` などが使えないのも `delete` と同じです。
    ///
    /// | ドライバ   | 流す文                                         |
    /// |------------|------------------------------------------------|
    /// | SQLite     | `delete from` と、`sqlite_sequence` の行の削除 |
    /// | MySQL      | `truncate table`                               |
    /// | PostgreSQL | `truncate table … restart identity`            |
    ///
    /// **ほかの表から外部キーで指されている表は、MySQL と PostgreSQL では
    /// 断られます**（指している側が空でも同じ）。SQLite は `delete` なので、
    /// 指している行が無ければ通ります。
    pub async fn truncate(&self) -> Result<()> {
        self.check()?;
        self.check_write("truncate")?;
        if !self.wheres.is_empty() {
            return Err(Error::msg(
                "truncate では条件が効きません。delete() を使ってください",
            ));
        }
        match self.driver {
            // SQLite に truncate は無い。条件なしの delete と同じ。
            // ただし delete だけでは `sqlite_sequence` が残り、採番が戻らない。
            // MySQL / PostgreSQL の truncate は戻すので、文を分けてそろえる。
            Driver::Sqlite => {
                // `sqlite_sequence` は autoincrement の表が1つも無いと存在しない。
                // **有無を先に読んで確かめます。** 以前は消す文をそのまま流して
                // 失敗を捨てていました。トランザクションの中では、1本失敗すると
                // 以後が使えなくなるので、続く `delete` まで落ちていました。
                let has_sequence = !self
                    .fetch(
                        "select 1 from sqlite_master \
                         where type = 'table' and name = 'sqlite_sequence'",
                        &[],
                    )
                    .await?
                    .is_empty();
                // 先に行を消す。外部キーで断られたときに、採番だけ戻るのを避ける。
                let sql = format!("delete from {}", grammar::quote(self.driver, &self.table));
                self.run(&sql, &[]).await?;
                if has_sequence {
                    let reset = "delete from sqlite_sequence where name = ?";
                    self.run(reset, &[Value::Text(self.table.clone())]).await?;
                }
            }
            // PostgreSQL は `restart identity` を付けて採番を戻す（`grammar::truncate`）。
            _ => {
                let sql = grammar::truncate(self.driver, &self.table);
                self.run(&sql, &[]).await?;
            }
        }
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
        if total == 0 {
            // 1件も無いなら、どのページも空。本体の問い合わせは流さない。
            return Ok(Paginator {
                data: Vec::new(),
                total,
                per_page,
                current_page: page,
            });
        }
        // `count()` が `check()` を通している。ここでは組み立てるだけ。
        let (sql, bindings) = self.select_sql(&Shape {
            limit: Some(per_page),
            offset: Some(page_offset(page, per_page)),
            ..self.shape()
        });
        let data = self.fetch(&sql, &bindings).await?;
        Ok(Paginator {
            data,
            total,
            per_page,
            current_page: page,
        })
    }
}

/// ページ番号から読み飛ばす件数を決める。あふれたら上限に張り付く。
fn page_offset(page: u64, per_page: u64) -> u64 {
    page.max(1)
        .saturating_sub(1)
        .saturating_mul(per_page.max(1))
}

/// `limit` / `offset` に書く数を、データベースが受け付ける範囲（符号付き 64 ビット）に丸める。
fn clamp_count(count: u64) -> u64 {
    count.min(i64::MAX as u64)
}

/// `where_in` に一度に渡せる値の数の上限。
///
/// 下回りの上限は SQLite が 32,766、PostgreSQL が 65,535 です。超えると
/// ドライバの分かりにくいエラーになるので、手前で分割を案内します。
const MAX_IN_VALUES: usize = 30_000;

/// 束縛する値の総数が、ドライバの上限に収まっているか確かめる。
///
/// 超えたまま投げると、ドライバごとに違う分かりにくいエラーになります
/// （SQLite は `too many SQL variables`、PostgreSQL は sqlx の
/// `too many arguments for query`）。手前で止めて、分割を案内します。
fn ensure_binding_count(driver: Driver, count: usize) -> Result<()> {
    let max = grammar::max_bindings(driver);
    if count <= max {
        return Ok(());
    }
    Err(Error::msg(format!(
        "1つの文に渡す値が多すぎます（{count} 個。{driver} の上限は {max} 個）。\
         insert_many は行を、where_in は一覧を分割して、何回かに分けて流してください"
    )))
}

/// 投げ先から、SQL の組み立てに使うドライバを決める。
///
/// 設定が読めないときは SQLite を仮に使い、**理由を一緒に返します。**
/// 組み立ては `Result` を返せないので、受け取った側が `deferred_error` に積みます。
///
/// 終端メソッドの `check` も `source.driver()` を見るので、エラーになること自体は
/// 理由を捨てても変わりません。積むのは `to_sql()` と `Debug` のためです。
/// 設定に無い接続名（`DB::connection("typo")`）を渡したとき、理由を捨てていると
/// `Debug` に SQLite の SQL だけが出て、何が悪いのか分かりませんでした。
fn resolve_driver(source: &Source) -> (Driver, Option<String>) {
    match source.driver() {
        Ok(driver) => (driver, None),
        Err(e) => (Driver::Sqlite, Some(e.to_string())),
    }
}

/// 書き込む列の名前として受け付けるか確かめる。
///
/// `insert` / `update` / `increment` / `decrement` の列名はここを通します。
/// 組み立てが `Result` を返すので、`deferred_error` に積む必要がありません。
///
/// **表名付き（`posts.title`）は断ります。** `set "posts"."title" = ?` や
/// `insert into "posts" ("posts"."title")` は、SQLite と PostgreSQL で
/// SQL エラーになります（通るのは MySQL だけ）。書き込む表は1つに決まって
/// いるので、表名を付ける必要がありません。
fn ensure_write_column(place: &str, column: &str) -> Result<()> {
    if grammar::is_bare_identifier(column) {
        return Ok(());
    }
    if grammar::is_plain_identifier(column) {
        return Err(Error::msg(format!(
            "{place} に渡した列名 `{column}` は使えません。\
             書き込む列に表名は付けられません。列の名前だけを渡してください"
        )));
    }
    Err(Error::msg(format!(
        "{place} に渡した列名 `{column}` は使えません。英数字と `_` だけが使えます"
    )))
}

/// 列の名前として受け付けるか確かめる。その場でエラーを返せる所で使います。
///
/// 通るのは `insert_get_id_as` の主キーだけです。`returning` に置く名前なので、
/// 表名付き（`t.id`）も通します。書き込む列は [`ensure_write_column`] を見てください。
fn ensure_column(place: &str, column: &str) -> Result<()> {
    if grammar::is_plain_identifier(column) {
        return Ok(());
    }
    Err(Error::msg(format!(
        "{place} に渡した列名 `{column}` は使えません。英数字と `_` `.` だけが使えます"
    )))
}

/// 値を1つ取り出す所の列名を確かめる。
///
/// `sum` / `avg` / `min` / `max` / `pluck` / `value` はここを通します。
/// `grammar::quote` は空白や `(` を含む名前を式として素通しするので、
/// 検査しないと列名がそのまま SQL に入ります。
/// `increment` / `decrement` は書き込む列なので、`ensure_write_column` で見ます。
///
/// 式を書きたいときの逃げ道は `select(...)` ＋ `get()` です。
fn ensure_value_column(place: &str, column: &str) -> Result<()> {
    if grammar::is_plain_identifier(column) {
        return Ok(());
    }
    Err(Error::msg(format!(
        "{place} に渡した列名 `{column}` は使えません。英数字と `_` `.` だけが使えます。\
         式を書きたいときは select(...) に書いて get() してください"
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
    ///
    /// 書く文字は `grammar::placeholder` と同じです。SQLite と MySQL は `?` の
    /// 1文字なので、`String` を作らずその場で足します。`where_in` に3万件渡すと
    /// 3万回の確保になっていました。
    fn push_value(&mut self, value: Value) {
        self.bindings.push(value);
        match self.driver {
            Driver::Postgres => {
                use std::fmt::Write;
                // `String` への書き込みは失敗しないので、結果は見ない。
                let _ = write!(self.sql, "${}", self.bindings.len());
            }
            _ => self.sql.push('?'),
        }
    }

    /// 利用者が書いた SQL を足す。PostgreSQL では `?` を `$n` に振り直す。
    ///
    /// `?` の数と値の数が合っているかは、組み立てのとき
    /// （`QueryBuilder::check_raw`）に確かめます。振り直しは
    /// `DB::select` などの生の SQL と同じ関数（`grammar::number_placeholders`）です。
    fn push_raw(&mut self, sql: &str, bindings: &[Value]) {
        let numbered = grammar::number_placeholders(self.driver, sql, self.bindings.len());
        self.bindings.extend_from_slice(bindings);
        self.sql.push_str(&numbered);
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
        Some(page_offset(self.current_page, self.per_page).saturating_add(1))
    }

    /// このページの最後の件が、全体で何件目か。0 件なら `None`。
    pub fn to(&self) -> Option<u64> {
        let from = self.from()?;
        // `from()` が `Some` なら 1 件以上あるので、`len - 1` は 0 以上。
        Some(from.saturating_add(self.data.len() as u64 - 1))
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
        // 集計の結果で絞るときは having_raw。select の別名（total）は
        // PostgreSQL の having で解決されないので、having_op には渡さない。
        let (sql, bindings) = builder()
            .select(&["posts.id", "count(*) as total"])
            .join("comments", "comments.post_id", "=", "posts.id")
            .group_by(&["posts.id"])
            .having_raw("count(*) > ?", &[Value::Int(2)])
            .to_sql();
        assert_eq!(
            sql,
            "select \"posts\".\"id\", count(*) as total from \"posts\" \
             inner join \"comments\" on \"comments\".\"post_id\" = \"posts\".\"id\" \
             group by \"posts\".\"id\" having count(*) > ?"
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
    fn 大きなページ番号でもあふれない() {
        // `req.page()` の値がそのまま届く。以前はデバッグでパニック、リリースで回り込んでいた。
        let (sql, _) = builder().for_page(u64::MAX, 15).to_sql();
        assert_eq!(
            sql,
            format!("select * from \"posts\" limit 15 offset {}", i64::MAX)
        );
        assert_eq!(page_offset(0, 10), 0);
        assert_eq!(page_offset(1, 10), 0);
        assert_eq!(page_offset(3, 10), 20);
        assert_eq!(page_offset(u64::MAX, u64::MAX), u64::MAX);

        // limit / offset に直接渡した値も、データベースが受ける範囲に丸める。
        let (sql, _) = builder().limit(u64::MAX).offset(u64::MAX).to_sql();
        assert_eq!(
            sql,
            format!(
                "select * from \"posts\" limit {} offset {}",
                i64::MAX,
                i64::MAX
            )
        );

        // ページ分けの計算も同じ。
        let page: Paginator<i32> = Paginator {
            data: vec![1],
            total: 1,
            per_page: u64::MAX,
            current_page: u64::MAX,
        };
        assert_eq!(page.from(), Some(u64::MAX));
        assert_eq!(page.to(), Some(u64::MAX));
    }

    #[test]
    fn nullと_is_で比べるときはプレースホルダを置かない() {
        // PostgreSQL は `"c" is $1` を受け付けない。
        let (sql, bindings) = builder()
            .where_op("deleted_at", "is", Value::Null)
            .or_where_op("body", "IS NOT", Value::Null)
            .to_sql();
        assert_eq!(
            sql,
            "select * from \"posts\" where \"deleted_at\" is null or \"body\" is not null"
        );
        assert!(bindings.is_empty());
    }

    #[test]
    fn is_にnull以外を渡すと組み立てのエラーになる() {
        // 以前は `"flag" is ?` を出していた。通るのは SQLite だけで、
        // PostgreSQL と MySQL では構文エラーになる。
        for query in [
            builder().where_op("flag", "is", true),
            builder().or_where_op("flag", "IS NOT", 1),
            builder().group_by(&["flag"]).having_op("flag", " is ", "x"),
            builder().where_group(|q| q.where_op("flag", "is not", 0)),
        ] {
            let message = query.check().expect_err("断られる").to_string();
            assert!(message.contains("NULL だけ"), "{message}");
            assert!(message.contains("where_null"), "{message}");
        }
    }

    #[test]
    fn nullと等しいかを比べると_is_null_になる() {
        // `"c" = NULL` は真にも偽にもならず、エラーも無く必ず 0 件になる。
        let none: Option<String> = None;
        let (sql, bindings) = builder()
            .where_("deleted_at", none.clone())
            .or_where("body", Value::Null)
            .to_sql();
        assert_eq!(
            sql,
            "select * from \"posts\" where \"deleted_at\" is null or \"body\" is null"
        );
        assert!(bindings.is_empty(), "NULL は束縛しない");

        // `!=` と `<>` は `is not null`。
        let (sql, bindings) = builder()
            .where_op("deleted_at", "!=", none.clone())
            .or_where_op("body", "<>", Value::Null)
            .to_sql();
        assert_eq!(
            sql,
            "select * from \"posts\" where \"deleted_at\" is not null or \"body\" is not null"
        );
        assert!(bindings.is_empty());

        // 束ねた後の条件も同じ。
        let (sql, bindings) = builder()
            .group_by(&["author_id"])
            .having_op("author_id", "=", none.clone())
            .having_op("status", "!=", none.clone())
            .to_sql();
        assert_eq!(
            sql,
            "select * from \"posts\" group by \"author_id\" \
             having \"author_id\" is null and \"status\" is not null"
        );
        assert!(bindings.is_empty());

        // 括弧の中でも同じ。PostgreSQL でも番号がずれない。
        let query = QueryBuilder {
            driver: Driver::Postgres,
            ..builder()
        };
        let (sql, bindings) = query
            .where_("a", 1)
            .where_group(|q| q.where_("b", none.clone()).or_where("c", 2))
            .to_sql();
        assert_eq!(
            sql,
            "select * from \"posts\" where \"a\" = $1 and (\"b\" is null or \"c\" = $2)"
        );
        assert_eq!(bindings, vec![Value::Int(1), Value::Int(2)]);

        // 値があるときは、これまでどおり束縛する。
        let (sql, bindings) = builder().where_("body", Some("x")).to_sql();
        assert_eq!(sql, "select * from \"posts\" where \"body\" = ?");
        assert_eq!(bindings, vec![Value::Text("x".into())]);

        // 大小や like は読み替えない（これまでどおり値として渡す）。
        let (sql, bindings) = builder().where_op("views", ">", none).to_sql();
        assert_eq!(sql, "select * from \"posts\" where \"views\" > ?");
        assert_eq!(bindings, vec![Value::Null]);
    }

    #[tokio::test]
    async fn findにnullを渡すと問い合わせずに無いを返す() {
        // `where_` が NULL を `is null` に読み替えるので、そのまま渡すと
        // 「id が NULL の行」を探してしまう。
        // 「問い合わせていない」ことは `model.rs` のテストが、流れた SQL で確かめる。
        let none: Option<i64> = None;
        let row = builder().find(none).await;
        assert!(matches!(row, Ok(None)), "{row:?}");

        // 組み立ての間違いは、問い合わせないときも知らせる。
        let message = builder()
            .order_by("id; drop table posts")
            .find(none)
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("使えません"), "{message}");
    }

    #[test]
    fn ドル記号の検査は終端のときのドライバで決まる() {
        // SQLite で組み立ててから、PostgreSQL のトランザクションへ替えた形。
        // 以前は書いた時点のドライバで決めていたので、検査を抜けていた。
        let built = builder().where_raw("x > $1", &[]);
        built.check().expect("SQLite のままなら通る");
        let moved = QueryBuilder {
            driver: Driver::Postgres,
            ..built
        };
        let message = moved.check().expect_err("断られる").to_string();
        assert!(message.contains("where_raw"), "{message}");
        assert!(message.contains("`?` を使ってください"), "{message}");
        assert!(format!("{moved:?}").contains("`?` を使ってください"));

        // 逆向き。PostgreSQL で書いても、投げ先が SQLite になれば通る。
        let built = QueryBuilder {
            driver: Driver::Postgres,
            ..builder()
        }
        .order_by_raw("$1");
        let moved = QueryBuilder {
            driver: Driver::Sqlite,
            ..built
        };
        moved.check().expect("SQLite なら通る");

        // 括弧の中に書いた `$` も、外側の終端で見つかる。
        let nested = QueryBuilder {
            driver: Driver::Postgres,
            ..builder().where_group(|q| q.where_raw("x > $1", &[]))
        };
        let message = nested.check().expect_err("断られる").to_string();
        assert!(message.contains("where_raw"), "{message}");
    }

    #[test]
    fn 投げ先を替えたら接続の間違いは入れ直す() {
        // 既定の接続が設定に無い状態で組み立てた形。
        let broken = QueryBuilder {
            driver_error: Some("`typo` という接続は設定にありません".to_string()),
            ..builder()
        };
        let text = format!("{broken:?}");
        assert!(text.contains("設定にありません"), "{text}");

        // 有効な投げ先へ替えたら、元の接続の間違いは消える。
        // 以前は `deferred_error` に積んだままで、終端でエラーになっていた。
        let moved = broken.clone().with_source(Source::Default);
        assert!(moved.driver_error.is_none());
        moved.check().expect("替えた先が有効なら通る");
        assert!(!format!("{moved:?}").contains("設定にありません"));

        // 組み立てのときに見つけた間違いは、替えても残す。
        let message = broken
            .order_by("id; drop table posts")
            .with_source(Source::Default)
            .check()
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("使えません"), "{message}");
    }

    #[test]
    fn 束縛する値の総数が上限を超えたら分割を案内する() {
        for (driver, max) in [
            (Driver::Sqlite, 32_766usize),
            (Driver::MySql, 65_535),
            (Driver::Postgres, 65_535),
        ] {
            ensure_binding_count(driver, max).expect("上限ちょうどは通る");
            let message = ensure_binding_count(driver, max + 1)
                .expect_err("断られる")
                .to_string();
            assert!(message.contains("分割"), "{message}");
            assert!(message.contains(&max.to_string()), "{message}");
            assert!(message.contains(driver.as_str()), "{message}");
        }
    }

    #[tokio::test]
    async fn where_inを重ねて上限を超えたら投げる前に止まる() {
        // 1回ぶんは `where_in` の上限（30,000 件）に収まるが、合計は超える。
        let ids: Vec<i64> = (0..20_000).collect();
        let query = builder()
            .where_in("id", &ids)
            .where_not_in("author_id", &ids);
        query.check().expect("組み立ては通る");
        let message = query.get().await.expect_err("断られる").to_string();
        assert!(message.contains("多すぎます"), "{message}");
        assert!(message.contains("40000"), "{message}");

        // 書き込みも同じ所を通る。
        let message = query.delete().await.expect_err("断られる").to_string();
        assert!(message.contains("多すぎます"), "{message}");
    }

    #[tokio::test]
    async fn insert_manyの行と列の積が上限を超えたら止まる() {
        // 3 列 × 11,000 行 = 33,000 個。SQLite の上限（32,766）を超える。
        let rows: Vec<Vec<(&str, Value)>> = (0..11_000)
            .map(|i| {
                vec![
                    ("title", Value::Int(i)),
                    ("body", Value::Int(i)),
                    ("views", Value::Int(i)),
                ]
            })
            .collect();
        let message = builder()
            .insert_many(&rows)
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("多すぎます"), "{message}");
        assert!(message.contains("insert_many は行を"), "{message}");
    }

    #[test]
    fn insertは行を借りたまま同じsqlを組み立てる() {
        // 1行の `insert` はスライスを、`insert_many` は `Vec` を渡す。出る SQL は同じ。
        let values = [("title", Value::Text("x".into())), ("views", Value::Int(1))];
        let (sql, bindings) = builder()
            .insert_sql(&[&values[..]], "")
            .expect("組み立てられる");
        assert_eq!(
            sql,
            "insert into \"posts\" (\"title\", \"views\") values (?, ?)"
        );
        assert_eq!(bindings, vec![Value::Text("x".into()), Value::Int(1)]);

        let rows = vec![values.to_vec(), values.to_vec()];
        let (sql, bindings) = builder().insert_sql(&rows, "").expect("組み立てられる");
        assert_eq!(
            sql,
            "insert into \"posts\" (\"title\", \"views\") values (?, ?), (?, ?)"
        );
        assert_eq!(bindings.len(), 4);

        // 列の並びが違う行は、これまでどおり断る。
        let rows = vec![
            vec![("title", Value::Int(1)), ("views", Value::Int(1))],
            vec![("views", Value::Int(1)), ("title", Value::Int(1))],
        ];
        let message = builder()
            .insert_sql(&rows, "")
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("列の並びが違います"), "{message}");
    }

    #[tokio::test]
    async fn 書き込みの列名に表名は付けられない() {
        // `set "posts"."title" = ?` は SQLite と PostgreSQL で SQL エラーになる。
        let values = [("posts.title", Value::Text("x".into()))];
        for message in [
            builder().insert(&values).await.expect_err("断られる"),
            builder()
                .insert_many(&[values.to_vec()])
                .await
                .expect_err("断られる"),
            builder()
                .insert_get_id(&values)
                .await
                .expect_err("断られる"),
        ] {
            let message = message.to_string();
            assert!(message.contains("表名は付けられません"), "{message}");
            assert!(message.contains("posts.title"), "{message}");
        }
        let message = builder()
            .where_("id", 1)
            .update(&values)
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("表名は付けられません"), "{message}");

        for sign in ['+', '-'] {
            let message = builder()
                .step_sql("increment", "posts.views", sign, 1)
                .expect_err("断られる")
                .to_string();
            assert!(message.contains("表名は付けられません"), "{message}");
        }

        // 点だけの名前や、区切りの間が空の名前も断る。
        for broken in [".", "a..b", "title."] {
            let values = [(broken, Value::Int(1))];
            let message = builder()
                .insert(&values)
                .await
                .expect_err("断られる")
                .to_string();
            assert!(message.contains("使えません"), "{message}");
        }

        // 読む側は、表名付きをこれまでどおり通す。
        builder()
            .where_("posts.title", "x")
            .order_by("posts.id")
            .check()
            .expect("条件と並びは表名付きで書ける");
        // `insert_get_id_as` の主キーは `returning` に置くだけなので、表名付きも通す。
        let query = QueryBuilder {
            driver: Driver::Postgres,
            ..builder()
        };
        let returning = grammar::returning_id(query.driver, "posts.id");
        let (sql, _) = query
            .insert_sql(&[&[("title", Value::Int(1))][..]], &returning)
            .expect("組み立てられる");
        assert_eq!(
            sql,
            "insert into \"posts\" (\"title\") values ($1) returning \"posts\".\"id\""
        );
    }

    #[test]
    fn 区切りの間が空の列名は条件でも断る() {
        for broken in [".", "a..b", "users.", ".id"] {
            for query in [
                builder().where_(broken, 1),
                builder().order_by(broken),
                builder().join(broken, "a", "=", "b"),
                QueryBuilder::new(Source::Default, broken),
            ] {
                let message = query.check().expect_err("断られる").to_string();
                assert!(message.contains("使えません"), "{broken}: {message}");
            }
        }
    }

    #[tokio::test]
    async fn limitを付けたinsertは専用の文で断る() {
        // 以前は update / delete と同じ「主キーを取り出して where_in で絞る」を
        // 案内していた。行を足す場面には当てはまらない。
        let values = [("title", Value::Text("x".into()))];
        for query in [builder().limit(1), builder().offset(1)] {
            let message = query
                .insert(&values)
                .await
                .expect_err("断られる")
                .to_string();
            assert!(message.contains("limit / offset"), "{message}");
            assert!(message.contains("外してください"), "{message}");
            assert!(!message.contains("where_in"), "{message}");
        }
        for query in [
            builder().group_by(&["author_id"]),
            builder().distinct(),
            builder().join("c", "c.post_id", "=", "posts.id"),
        ] {
            let message = query
                .insert_get_id(&values)
                .await
                .expect_err("断られる")
                .to_string();
            assert!(message.contains("足す値だけを渡してください"), "{message}");
            assert!(!message.contains("where_in"), "{message}");
        }
        // update / delete の案内は変えない。
        let message = builder()
            .limit(1)
            .update(&values)
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("where_in"), "{message}");
    }

    #[test]
    fn 多すぎるwhere_inは分割を案内する() {
        let many: Vec<i64> = (0..=MAX_IN_VALUES as i64).collect();
        let message = builder()
            .where_in("id", &many)
            .check()
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("分割"), "{message}");
        let fits: Vec<i64> = (0..MAX_IN_VALUES as i64).collect();
        builder()
            .where_in("id", &fits)
            .check()
            .expect("上限ちょうどは通る");
    }

    #[test]
    fn postgresの生のsqlは番号を続きから振る() {
        let query = QueryBuilder {
            driver: Driver::Postgres,
            ..builder()
        };
        let (sql, bindings) = query
            .where_("status", "published")
            .where_raw("length(title) > ?", &[Value::Int(3)])
            .group_by(&["author_id"])
            .having_raw(
                "count(*) > ? and sum(views) > ?",
                &[Value::Int(1), Value::Int(2)],
            )
            .to_sql();
        assert_eq!(
            sql,
            "select * from \"posts\" where \"status\" = $1 and length(title) > $2 \
             group by \"author_id\" having count(*) > $3 and sum(views) > $4"
        );
        assert_eq!(bindings.len(), 4);
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
            builder().having_op("views", danger, 1),
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
            .having_op("views", ">=", 2)
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
        // 内側の列は 1 に固定する。`select *` のままだと PostgreSQL は
        // 「group by に無い列」、MySQL は ONLY_FULL_GROUP_BY で断る。
        let (sql, bindings) = builder()
            .where_("status", "published")
            .group_by(&["post_id"])
            .count_sql()
            .expect("組み立てられる");
        assert_eq!(
            sql,
            "select count(*) as bengara_aggregate from (\
             select 1 as bengara_group from \"posts\" where \"status\" = ? group by \"post_id\"\
             ) as bengara_sub"
        );
        assert_eq!(bindings, vec![Value::Text("published".into())]);

        // 結合を併用しても、内側に列が出ないので名前が重ならない。
        let (sql, _) = builder()
            .join("comments", "comments.post_id", "=", "posts.id")
            .group_by(&["posts.id"])
            .count_sql()
            .expect("組み立てられる");
        assert_eq!(
            sql,
            "select count(*) as bengara_aggregate from (\
             select 1 as bengara_group from \"posts\" \
             inner join \"comments\" on \"comments\".\"post_id\" = \"posts\".\"id\" \
             group by \"posts\".\"id\"\
             ) as bengara_sub"
        );
    }

    #[test]
    fn 重なりを除いた件数を数える() {
        // 列が1つに決まるときは count(distinct 列)。
        let (sql, _) = builder()
            .distinct()
            .select(&["author_id"])
            .count_sql()
            .expect("組み立てられる");
        assert_eq!(
            sql,
            "select count(distinct \"author_id\") as bengara_aggregate from \"posts\""
        );
    }

    #[test]
    fn 数える列が決まらないdistinctは断る() {
        // 以前は派生表で数えていた。MySQL は join した列名が重なってエラー 1060。
        for query in [
            builder().distinct(),
            builder().distinct().select(&["id", "author_id"]),
            builder()
                .distinct()
                .join("comments", "comments.post_id", "=", "posts.id"),
        ] {
            let message = query.count_sql().expect_err("断られる").to_string();
            assert!(message.contains("select で列を1つ"), "{message}");
        }
    }

    #[test]
    fn 条件なしの件数はそのまま数える() {
        let (sql, _) = builder()
            .latest()
            .limit(10)
            .count_sql()
            .expect("組み立てられる");
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
    fn postgresのビルダーでドル記号の直書きは断る() {
        // `$1` を直接書くと、ビルダーが振る番号と重なって別の値を見にいく。
        let postgres = || QueryBuilder {
            driver: Driver::Postgres,
            ..builder()
        };
        for query in [
            postgres().where_raw("x > $1", &[]),
            postgres().or_where_raw("x > $1", &[]),
            postgres().having_raw("count(*) > $1", &[]),
            postgres().order_by_raw("$1"),
        ] {
            let message = query.check().expect_err("断られる").to_string();
            assert!(message.contains("`?` を使ってください"), "{message}");
            assert!(message.contains("DB::select"), "{message}");
        }

        // ほかのドライバは `$` を特別扱いしない。
        for driver in [Driver::Sqlite, Driver::MySql] {
            let query = QueryBuilder {
                driver,
                ..builder()
            };
            query
                .where_raw("x > $1", &[])
                .check()
                .expect("postgres 以外は通る");
        }

        // `?` で渡す形はこれまでどおり通る。
        postgres()
            .where_raw("x > ?", &[Value::Int(1)])
            .check()
            .expect("`?` なら通る");
    }

    #[test]
    fn 値の置き場所はドライバごとに変わる() {
        // `push_value` を `?` の直書きに変えても、出る SQL は同じこと。
        for (driver, expected) in [
            (
                Driver::Sqlite,
                "select * from \"posts\" where \"id\" in (?, ?, ?)",
            ),
            (
                Driver::MySql,
                "select * from `posts` where `id` in (?, ?, ?)",
            ),
            (
                Driver::Postgres,
                "select * from \"posts\" where \"id\" in ($1, $2, $3)",
            ),
        ] {
            let query = QueryBuilder {
                driver,
                ..builder()
            };
            let (sql, bindings) = query.where_in("id", &[1i64, 2, 3]).to_sql();
            assert_eq!(sql, expected, "{driver}");
            assert_eq!(bindings.len(), 3, "{driver}");
        }
    }

    #[test]
    fn offset_だけのときの_limit_は方言ごとに変わる() {
        // `limit -1` が通るのは SQLite だけ。MySQL は構文エラーになる。
        for (driver, expected) in [
            (Driver::Sqlite, "select * from \"posts\" limit -1 offset 5"),
            (
                Driver::MySql,
                "select * from `posts` limit 18446744073709551615 offset 5",
            ),
            (
                Driver::Postgres,
                "select * from \"posts\" limit all offset 5",
            ),
        ] {
            let query = QueryBuilder {
                driver,
                ..builder()
            };
            let (sql, _) = query.offset(5).to_sql();
            assert_eq!(sql, expected, "{driver}");
        }
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
            .having_op("views", ">", 1)
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
            .having_op("views", ">", 1)
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

    #[test]
    fn 集計の列名も検査する() {
        // `quote` は空白や `(` を含む名前を式として素通しするので、
        // 検査しないと列名がそのまま SQL に入る。
        for danger in ["views from users --", "views) , (select 1", "views desc"] {
            for function in ["sum", "avg", "min", "max"] {
                let message = builder()
                    .aggregate_sql(function, danger)
                    .expect_err("断られる")
                    .to_string();
                assert!(message.contains("使えません"), "{message}");
                assert!(message.contains(danger), "{message}");
            }
        }
    }

    #[test]
    fn 表名付きの列と米印は集計で通る() {
        let (sql, _) = builder().aggregate_sql("sum", "posts.views").expect("通る");
        assert_eq!(
            sql,
            "select sum(\"posts\".\"views\") as bengara_aggregate from \"posts\""
        );

        let (sql, _) = builder().aggregate_sql("count", "*").expect("通る");
        assert_eq!(sql, "select count(*) as bengara_aggregate from \"posts\"");
    }

    #[tokio::test]
    async fn pluckとvalueの列名も検査する() {
        let danger = "name from users --";
        let message = builder()
            .pluck::<String>(danger)
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("使えません"), "{message}");
        assert!(message.contains("select"), "{message}");

        let message = builder()
            .value::<String>(danger)
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("使えません"), "{message}");
    }

    #[test]
    fn 値1つと比べられない演算子は案内付きで断る() {
        for operator in ["between", "not between", "in", "not in"] {
            for query in [
                builder().where_op("views", operator, 1),
                builder().or_where_op("views", operator, 1),
                builder()
                    .group_by(&["author_id"])
                    .having_op("views", operator, 1),
            ] {
                let message = query.check().expect_err("断られる").to_string();
                assert!(message.contains("値1つとは比べられません"), "{message}");
                assert!(message.contains("where_in"), "{message}");
                assert!(message.contains("where_between"), "{message}");
            }
        }
        // MySQL だけの書き方も値と比べる側では断る。
        let message = builder()
            .where_op("views", "<=>", 1)
            .check()
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("使えません"), "{message}");
        // 列と列を比べる側は今までどおり通る。
        builder()
            .where_column("a", "between", "b")
            .check()
            .expect("列と列の比較はこれまでどおり");
    }

    #[tokio::test]
    async fn 条件や結合を付けたinsertは断る() {
        let values = [("title", Value::Text("x".into()))];
        let message = builder()
            .where_("status", "draft")
            .insert(&values)
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("条件が効きません"), "{message}");

        let message = builder()
            .join("c", "c.post_id", "=", "posts.id")
            .insert(&values)
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("join"), "{message}");

        let message = builder()
            .where_("id", 1)
            .insert_get_id(&values)
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("条件が効きません"), "{message}");
    }

    #[test]
    fn 増減のsqlは列を自分に足す形() {
        let (sql, bindings) = builder()
            .where_("id", 1)
            .step_sql("increment", "views", '+', 1)
            .expect("組み立てられる");
        assert_eq!(
            sql,
            "update \"posts\" set \"views\" = \"views\" + ? where \"id\" = ?"
        );
        assert_eq!(bindings, vec![Value::Int(1), Value::Int(1)]);

        let (sql, _) = builder()
            .step_sql("decrement", "stock", '-', 3)
            .expect("組み立てられる");
        assert_eq!(sql, "update \"posts\" set \"stock\" = \"stock\" - ?");

        // 列名は検査する。
        let message = builder()
            .step_sql("increment", "views = 0, title", '+', 1)
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("使えません"), "{message}");
    }

    #[tokio::test]
    async fn 結合を付けた増減は断る() {
        let message = builder()
            .join("c", "c.post_id", "=", "posts.id")
            .increment("views", 1)
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("join"), "{message}");

        let message = builder()
            .limit(1)
            .decrement("views", 1)
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("limit"), "{message}");
    }

    #[test]
    fn existsは並びと件数を外す() {
        // 並べ替えても1件あるかは変わらない。手間だけが増える。
        let query = builder().where_("status", "published").latest().limit(10);
        let (sql, _) = query.exists_sql();
        assert_eq!(
            sql,
            "select 1 as bengara_exists from \"posts\" where \"status\" = ? limit 1"
        );
        assert!(!sql.contains("order by"), "{sql}");
    }

    #[test]
    fn 終端メソッドの_sql_は複製していた頃と同じ() {
        // ビルダーを clone せずに組み立てるようにした。SQL は変えないこと。
        let query = builder().where_("status", "published").latest();

        let (sql, bindings) = query.select_sql(&Shape {
            limit: Some(1),
            ..query.shape()
        });
        assert_eq!(
            sql,
            "select * from \"posts\" where \"status\" = ? \
             order by \"created_at\" desc limit 1"
        );
        assert_eq!(bindings, vec![Value::Text("published".into())]);

        // value / pluck は列を1つに差し替える。
        let columns = vec!["title".to_string()];
        let (sql, _) = query.select_sql(&Shape {
            columns: &columns,
            ..query.shape()
        });
        assert_eq!(
            sql,
            "select \"title\" from \"posts\" where \"status\" = ? order by \"created_at\" desc"
        );

        // paginate は件数だけ差し替える。for_page と同じ形になること。
        let (sql, _) = query.select_sql(&Shape {
            limit: Some(15),
            offset: Some(30),
            ..query.shape()
        });
        let (expected, _) = query.clone().for_page(3, 15).to_sql();
        assert_eq!(sql, expected);
    }

    // ---- 実際の SQLite を使うテスト ----

    /// メモリ上の SQLite に `posts` を用意して、トランザクションから使う。
    ///
    /// `QueryBuilder` は `Source` を通すので、接続を直接は渡せません。
    /// トランザクションから作ると、置き場所の設定に触らずに試せます。
    #[cfg(feature = "sqlite")]
    async fn live() -> super::super::Transaction {
        use super::super::{ConnectionConfig, Schema, Transaction};

        let backend = super::super::sqlite::connect(&ConnectionConfig::sqlite("test", ":memory:"))
            .await
            .expect("メモリ上の SQLite につながる");
        let mut schema = Schema::new(Driver::Sqlite);
        schema.create("posts", |t| {
            t.id();
            t.string("title").unique();
            t.integer("views").default(0);
        });
        for sql in schema.into_statements() {
            backend.execute(&sql, &[]).await.expect("表が作れる");
        }
        Transaction::start(backend).await.expect("始められる")
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn truncateの後は採番が1から始まる() {
        let tx = live().await;
        let posts = tx.table("posts");
        let id = posts
            .insert_get_id(&[("title", Value::Text("1本目".into()))])
            .await
            .expect("入る");
        assert_eq!(id, 1);
        let id = posts
            .insert_get_id(&[("title", Value::Text("2本目".into()))])
            .await
            .expect("入る");
        assert_eq!(id, 2);

        posts.truncate().await.expect("空にできる");

        // `sqlite_sequence` を消さないと 3 になる。
        let id = posts
            .insert_get_id(&[("title", Value::Text("やり直し".into()))])
            .await
            .expect("入る");
        assert_eq!(id, 1, "truncate の後は 1 から");
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 採番の表が無いsqliteでもトランザクションの中でtruncateできる() {
        use super::super::{ConnectionConfig, Transaction};

        // autoincrement の表が1つも無いので、`sqlite_sequence` が存在しない。
        let backend = super::super::sqlite::connect(&ConnectionConfig::sqlite("test", ":memory:"))
            .await
            .expect("メモリ上の SQLite につながる");
        backend
            .execute(
                "create table plain (id integer primary key, title text)",
                &[],
            )
            .await
            .expect("表が作れる");
        let found = backend
            .fetch_all(
                "select 1 from sqlite_master where name = 'sqlite_sequence'",
                &[],
            )
            .await
            .expect("読める");
        assert!(found.is_empty(), "前提: sqlite_sequence が無い");

        let tx = Transaction::start(backend).await.expect("始められる");
        let plain = tx.table("plain");
        plain
            .insert(&[("title", Value::Text("1本目".into()))])
            .await
            .expect("入る");

        // 以前は `delete from sqlite_sequence` が失敗して、トランザクションが
        // 使えなくなっていた（続く `delete from "plain"` が落ちる）。
        plain.truncate().await.expect("空にできる");
        assert_eq!(plain.count().await.expect("数えられる"), 0);

        // トランザクションは生きている。続けて書けて、確定もできる。
        plain
            .insert(&[("title", Value::Text("やり直し".into()))])
            .await
            .expect("truncate の後も書ける");
        assert_eq!(plain.count().await.expect("数えられる"), 1);
        tx.commit().await.expect("確定できる");
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn nullの条件は実際のsqliteでも当たる() {
        let tx = live().await;
        tx.statement("alter table posts add column body text", &[])
            .await
            .expect("列を足せる");
        let posts = tx.table("posts");
        posts
            .insert(&[("title", Value::Text("本文なし".into()))])
            .await
            .expect("入る");
        posts
            .insert(&[
                ("title", Value::Text("本文あり".into())),
                ("body", Value::Text("x".into())),
            ])
            .await
            .expect("入る");

        // 以前は `"body" = NULL` になって、必ず 0 件だった。
        let none: Option<String> = None;
        let titles = tx
            .table("posts")
            .where_("body", none.clone())
            .pluck::<String>("title")
            .await
            .expect("読める");
        assert_eq!(titles, vec!["本文なし".to_string()]);
        let titles = tx
            .table("posts")
            .where_op("body", "!=", none)
            .pluck::<String>("title")
            .await
            .expect("読める");
        assert_eq!(titles, vec!["本文あり".to_string()]);

        // NULL がありうる列は `Option<T>` で読む。
        let bodies = tx
            .table("posts")
            .order_by("id")
            .pluck::<Option<String>>("body")
            .await
            .expect("読める");
        assert_eq!(bodies, vec![None, Some("x".to_string())]);
        assert!(tx.table("posts").pluck::<String>("body").await.is_err());
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 空の表のページ分けは空で返る() {
        let tx = live().await;
        let page = tx
            .table("posts")
            .latest_by("id")
            .paginate(15, 3)
            .await
            .expect("取れる");
        assert_eq!(page.total, 0);
        assert!(page.data.is_empty());
        assert_eq!(page.per_page, 15);
        assert_eq!(page.current_page, 3, "頼んだページ番号はそのまま返す");
        assert_eq!(page.last_page(), 1);

        // 1件でもあれば、これまでどおり本体も取る。
        tx.table("posts")
            .insert(&[("title", Value::Text("やきそば".into()))])
            .await
            .expect("入る");
        let page = tx.table("posts").paginate(15, 1).await.expect("取れる");
        assert_eq!(page.total, 1);
        assert_eq!(page.len(), 1);
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn incrementは原子的に増やす() {
        let tx = live().await;
        let posts = tx.table("posts");
        posts
            .insert(&[("title", Value::Text("やきそば".into()))])
            .await
            .expect("入る");
        posts
            .insert(&[("title", Value::Text("たこやき".into()))])
            .await
            .expect("入る");

        let one = tx.table("posts").where_("id", 1);
        assert_eq!(one.increment("views", 1).await.expect("増える"), 1);
        assert_eq!(one.increment("views", 1).await.expect("増える"), 1);
        assert_eq!(
            one.value::<i64>("views").await.expect("読める"),
            Some(2),
            "2回掛けたら 2 増える"
        );
        // 条件を付けていない行は変わらない。
        assert_eq!(
            tx.table("posts")
                .where_("id", 2)
                .value::<i64>("views")
                .await
                .expect("読める"),
            Some(0)
        );

        // 条件なしは全件に当たる。
        assert_eq!(
            tx.table("posts")
                .increment("views", 10)
                .await
                .expect("増える"),
            2
        );
        assert_eq!(
            tx.table("posts")
                .where_("id", 2)
                .value::<i64>("views")
                .await
                .expect("読める"),
            Some(10)
        );

        // decrement は引く。
        assert_eq!(one.decrement("views", 5).await.expect("減る"), 1);
        assert_eq!(one.value::<i64>("views").await.expect("読める"), Some(7));
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 一意制約違反を種類で見分けられる() {
        use super::super::is_unique_violation;

        let tx = live().await;
        let posts = tx.table("posts");
        let values = [("title", Value::Text("やきそば".into()))];
        posts.insert(&values).await.expect("1本目は入る");

        let error = posts.insert(&values).await.expect_err("2本目は断られる");
        assert!(is_unique_violation(&error), "{error}");

        // 列が無いだけのエラーは偽。
        let error = posts
            .insert(&[("nope", Value::Int(1))])
            .await
            .expect_err("無い列は断られる");
        assert!(!is_unique_violation(&error), "{error}");

        // 組み立てのエラー（接続より手前）も偽。
        let error = Error::msg("ただのメッセージ");
        assert!(!is_unique_violation(&error));
    }
}
