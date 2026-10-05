//! 表を作る・変える手順の組み立て。
//!
//! `up` / `down` は**同期の関数**です。ここで組み立てた SQL を、ランナーが実行します。
//!
//! ```ignore
//! pub fn up(schema: &mut Schema) {
//!     schema.create("posts", |t| {
//!         t.id();
//!         t.string("title");
//!         t.timestamps();
//!     });
//! }
//! ```

use super::grammar::{self, Driver};
use super::value::Value;

/// 列の種類。ドライバごとの型名はここから決めます。
#[derive(Debug, Clone, PartialEq)]
enum ColumnType {
    /// 自動採番の主キー。
    Id,
    Integer,
    BigInteger,
    /// 長さ付きの文字列。
    Str(u32),
    Text,
    Boolean,
    Float,
    Double,
    Decimal(u8, u8),
    Date,
    DateTime,
    Json,
    Binary,
    Uuid,
    /// SQL をそのまま書く。
    Raw(String),
}

/// 既定値の書き方。
#[derive(Debug, Clone)]
enum DefaultValue {
    /// 値として書く。
    Value(Value),
    /// SQL をそのまま書く（`current_timestamp` など）。
    Raw(String),
}

/// 1つの列。
#[derive(Debug, Clone)]
struct Column {
    name: String,
    kind: ColumnType,
    nullable: bool,
    default: Option<DefaultValue>,
    unique: bool,
    index: bool,
    primary: bool,
}

/// 索引。
#[derive(Debug, Clone)]
struct Index {
    columns: Vec<String>,
    unique: bool,
}

/// 外部キー。
#[derive(Debug, Clone)]
struct ForeignKey {
    column: String,
    references: String,
    on: String,
    on_delete: Option<String>,
    on_update: Option<String>,
}

/// 表を作るのか変えるのか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Create { if_not_exists: bool },
    Alter,
}

/// 表1つぶんの組み立て。`create` / `table` のクロージャが受け取る `t` です。
pub struct Blueprint {
    driver: Driver,
    table: String,
    mode: Mode,
    columns: Vec<Column>,
    indexes: Vec<Index>,
    primary: Vec<String>,
    foreigns: Vec<ForeignKey>,
}

impl Blueprint {
    fn new(driver: Driver, table: impl Into<String>, mode: Mode) -> Self {
        Self {
            driver,
            table: table.into(),
            mode,
            columns: Vec::new(),
            indexes: Vec::new(),
            primary: Vec::new(),
            foreigns: Vec::new(),
        }
    }

    fn push(&mut self, name: &str, kind: ColumnType) -> ColumnDefinition<'_> {
        self.columns.push(Column {
            name: name.to_string(),
            kind,
            nullable: false,
            default: None,
            unique: false,
            index: false,
            primary: false,
        });
        let index = self.columns.len() - 1;
        ColumnDefinition {
            blueprint: self,
            index,
        }
    }

    /// 自動採番の主キー `id`。
    pub fn id(&mut self) -> ColumnDefinition<'_> {
        self.push("id", ColumnType::Id)
    }

    /// 名前を決めた自動採番の主キー。
    pub fn increments(&mut self, name: &str) -> ColumnDefinition<'_> {
        self.push(name, ColumnType::Id)
    }

    /// 整数。
    pub fn integer(&mut self, name: &str) -> ColumnDefinition<'_> {
        self.push(name, ColumnType::Integer)
    }

    /// 大きい整数。
    pub fn big_integer(&mut self, name: &str) -> ColumnDefinition<'_> {
        self.push(name, ColumnType::BigInteger)
    }

    /// ほかの表を指す整数の列（`post_id` など）。
    pub fn foreign_id(&mut self, name: &str) -> ColumnDefinition<'_> {
        self.push(name, ColumnType::BigInteger)
    }

    /// 文字列（255 文字）。
    pub fn string(&mut self, name: &str) -> ColumnDefinition<'_> {
        self.push(name, ColumnType::Str(255))
    }

    /// 長さを決めた文字列。
    pub fn string_with(&mut self, name: &str, length: u32) -> ColumnDefinition<'_> {
        self.push(name, ColumnType::Str(length.max(1)))
    }

    /// 長い文字列。
    pub fn text(&mut self, name: &str) -> ColumnDefinition<'_> {
        self.push(name, ColumnType::Text)
    }

    /// 真偽。
    pub fn boolean(&mut self, name: &str) -> ColumnDefinition<'_> {
        self.push(name, ColumnType::Boolean)
    }

    /// 小数。
    pub fn float(&mut self, name: &str) -> ColumnDefinition<'_> {
        self.push(name, ColumnType::Float)
    }

    /// 倍精度の小数。
    pub fn double(&mut self, name: &str) -> ColumnDefinition<'_> {
        self.push(name, ColumnType::Double)
    }

    /// 桁を決めた数値。
    pub fn decimal(&mut self, name: &str, total: u8, places: u8) -> ColumnDefinition<'_> {
        self.push(name, ColumnType::Decimal(total, places))
    }

    /// 日付。
    pub fn date(&mut self, name: &str) -> ColumnDefinition<'_> {
        self.push(name, ColumnType::Date)
    }

    /// 日付と時刻。
    pub fn date_time(&mut self, name: &str) -> ColumnDefinition<'_> {
        self.push(name, ColumnType::DateTime)
    }

    /// 日付と時刻（Laravel の `timestamp`）。
    pub fn timestamp(&mut self, name: &str) -> ColumnDefinition<'_> {
        self.push(name, ColumnType::DateTime)
    }

    /// JSON。SQLite では文字列として入ります。
    pub fn json(&mut self, name: &str) -> ColumnDefinition<'_> {
        self.push(name, ColumnType::Json)
    }

    /// バイト列。
    pub fn binary(&mut self, name: &str) -> ColumnDefinition<'_> {
        self.push(name, ColumnType::Binary)
    }

    /// UUID。
    pub fn uuid(&mut self, name: &str) -> ColumnDefinition<'_> {
        self.push(name, ColumnType::Uuid)
    }

    /// SQL の型名をそのまま書く。
    pub fn raw_column(&mut self, name: &str, type_sql: &str) -> ColumnDefinition<'_> {
        self.push(name, ColumnType::Raw(type_sql.to_string()))
    }

    /// `created_at` と `updated_at` を足す（どちらも null 可）。
    pub fn timestamps(&mut self) {
        self.date_time("created_at").nullable();
        self.date_time("updated_at").nullable();
    }

    /// `deleted_at` を足す（null 可）。
    pub fn soft_deletes(&mut self) {
        self.date_time("deleted_at").nullable();
    }

    /// 索引を付ける。
    pub fn index(&mut self, columns: &[&str]) {
        self.indexes.push(Index {
            columns: columns.iter().map(|c| (*c).to_string()).collect(),
            unique: false,
        });
    }

    /// 重なりを禁じる。
    pub fn unique(&mut self, columns: &[&str]) {
        self.indexes.push(Index {
            columns: columns.iter().map(|c| (*c).to_string()).collect(),
            unique: true,
        });
    }

    /// 複合主キー。
    pub fn primary(&mut self, columns: &[&str]) {
        self.primary = columns.iter().map(|c| (*c).to_string()).collect();
    }

    /// 外部キーを付ける。
    ///
    /// ```ignore
    /// t.foreign("post_id").references("id").on("posts").on_delete("cascade");
    /// ```
    pub fn foreign(&mut self, column: &str) -> ForeignKeyDefinition<'_> {
        self.foreigns.push(ForeignKey {
            column: column.to_string(),
            references: "id".to_string(),
            on: String::new(),
            on_delete: None,
            on_update: None,
        });
        let index = self.foreigns.len() - 1;
        ForeignKeyDefinition {
            blueprint: self,
            index,
        }
    }

    /// 組み立てた SQL を取り出す。
    fn build(self) -> Vec<String> {
        match self.mode {
            Mode::Create { if_not_exists } => self.build_create(if_not_exists),
            Mode::Alter => self.build_alter(),
        }
    }

    fn build_create(self, if_not_exists: bool) -> Vec<String> {
        let driver = self.driver;
        let mut parts: Vec<String> = self
            .columns
            .iter()
            .map(|column| self.column_sql(column))
            .collect();

        if !self.primary.is_empty() {
            let columns: Vec<String> = self
                .primary
                .iter()
                .map(|c| grammar::quote(driver, c))
                .collect();
            parts.push(format!("primary key ({})", columns.join(", ")));
        }
        for foreign in &self.foreigns {
            parts.push(self.foreign_sql(foreign));
        }

        let head = if if_not_exists {
            "create table if not exists "
        } else {
            "create table "
        };
        let mut statements = vec![format!(
            "{head}{} ({})",
            grammar::quote(driver, &self.table),
            parts.join(", ")
        )];
        statements.extend(self.index_statements());
        statements
    }

    fn build_alter(self) -> Vec<String> {
        let driver = self.driver;
        let mut statements: Vec<String> = self
            .columns
            .iter()
            .map(|column| {
                format!(
                    "alter table {} add column {}",
                    grammar::quote(driver, &self.table),
                    self.column_sql(column)
                )
            })
            .collect();
        statements.extend(self.index_statements());
        statements
    }

    /// 列に付いた `index()` / `unique()` と、表に付けた索引をまとめる。
    fn index_statements(&self) -> Vec<String> {
        let mut out = Vec::new();
        for column in &self.columns {
            if column.index {
                out.push(self.index_sql(std::slice::from_ref(&column.name), false));
            }
            // 列に付けた unique は、作るときは列の指定に入れている。
            // 表を変えるときだけ、後から索引として足す。
            if column.unique && self.mode == Mode::Alter {
                out.push(self.index_sql(std::slice::from_ref(&column.name), true));
            }
        }
        for index in &self.indexes {
            out.push(self.index_sql(&index.columns, index.unique));
        }
        out
    }

    fn index_sql(&self, columns: &[String], unique: bool) -> String {
        let driver = self.driver;
        let name = format!(
            "{}_{}_{}",
            self.table,
            columns.join("_"),
            if unique { "unique" } else { "index" }
        );
        let quoted: Vec<String> = columns.iter().map(|c| grammar::quote(driver, c)).collect();
        // 表が `if not exists` なら索引にも付ける。
        // 付けないと、2回流したときに索引の作成だけが落ちる。
        let guard = match self.mode {
            Mode::Create {
                if_not_exists: true,
            } => "if not exists ",
            _ => "",
        };
        format!(
            "create {}index {guard}{} on {} ({})",
            if unique { "unique " } else { "" },
            grammar::quote(driver, &name),
            grammar::quote(driver, &self.table),
            quoted.join(", ")
        )
    }

    fn column_sql(&self, column: &Column) -> String {
        let driver = self.driver;
        let mut sql = grammar::quote(driver, &column.name);
        sql.push(' ');

        if column.kind == ColumnType::Id {
            // 自動採番の主キーは型名1つで全部書くので、ほかの指定を置く場所がない。
            // 黙って捨てると気づけないので警告を出す。
            // ここは同期の組み立てで `Result` を返せず、エラーにするとパニックしか
            // 残らないため、警告にしてあります（`index()` は別の文になるので効きます）。
            if column.nullable || column.default.is_some() || column.unique {
                tracing::warn!(
                    "{}.{} は自動採番の主キーなので、nullable / default / unique は無視します",
                    self.table,
                    column.name
                );
            }
            if self.mode == Mode::Alter {
                tracing::warn!(
                    "{}.{} に id() を使っています。alter table add column では \
                     自動採番の主キーを足せないので、この SQL は流れません",
                    self.table,
                    column.name
                );
            }
            sql.push_str(auto_increment_sql(driver));
            return sql;
        }

        sql.push_str(&type_sql(driver, &column.kind));
        if column.primary {
            sql.push_str(" primary key");
        }
        if column.nullable {
            sql.push_str(" null");
        } else {
            sql.push_str(" not null");
        }
        if let Some(default) = &column.default {
            sql.push_str(" default ");
            sql.push_str(&default_sql(driver, default));
        }
        // 作るときの unique は、列の指定として書く。
        if column.unique && self.mode != Mode::Alter {
            sql.push_str(" unique");
        }
        sql
    }

    fn foreign_sql(&self, foreign: &ForeignKey) -> String {
        let driver = self.driver;
        let mut sql = format!(
            "foreign key ({}) references {} ({})",
            grammar::quote(driver, &foreign.column),
            grammar::quote(driver, &foreign.on),
            grammar::quote(driver, &foreign.references)
        );
        if let Some(action) = &foreign.on_delete {
            sql.push_str(" on delete ");
            sql.push_str(action);
        }
        if let Some(action) = &foreign.on_update {
            sql.push_str(" on update ");
            sql.push_str(action);
        }
        sql
    }
}

/// 列に付ける指定。`t.string("title").nullable()` の形で続けて書きます。
pub struct ColumnDefinition<'a> {
    blueprint: &'a mut Blueprint,
    index: usize,
}

impl ColumnDefinition<'_> {
    fn column(&mut self) -> &mut Column {
        &mut self.blueprint.columns[self.index]
    }

    /// null を許す。
    pub fn nullable(mut self) -> Self {
        self.column().nullable = true;
        self
    }

    /// 既定値を決める。
    pub fn default(mut self, value: impl super::value::IntoValue) -> Self {
        self.column().default = Some(DefaultValue::Value(value.into_value()));
        self
    }

    /// 既定値を SQL で決める（`current_timestamp` など）。
    pub fn default_raw(mut self, sql: &str) -> Self {
        self.column().default = Some(DefaultValue::Raw(sql.to_string()));
        self
    }

    /// 重なりを禁じる。
    pub fn unique(mut self) -> Self {
        self.column().unique = true;
        self
    }

    /// 索引を付ける。
    pub fn index(mut self) -> Self {
        self.column().index = true;
        self
    }

    /// 主キーにする。
    pub fn primary(mut self) -> Self {
        self.column().primary = true;
        self
    }

    /// 説明を付ける。SQLite では無視されます。
    pub fn comment(self, _text: &str) -> Self {
        self
    }
}

/// 外部キーに付ける指定。
pub struct ForeignKeyDefinition<'a> {
    blueprint: &'a mut Blueprint,
    index: usize,
}

impl ForeignKeyDefinition<'_> {
    fn foreign(&mut self) -> &mut ForeignKey {
        &mut self.blueprint.foreigns[self.index]
    }

    /// 指す先の列（既定は `id`）。
    pub fn references(mut self, column: &str) -> Self {
        self.foreign().references = column.to_string();
        self
    }

    /// 指す先の表。
    pub fn on(mut self, table: &str) -> Self {
        self.foreign().on = table.to_string();
        self
    }

    /// 元の行が消えたときの動き（`cascade` / `set null` / `restrict`）。
    pub fn on_delete(mut self, action: &str) -> Self {
        self.foreign().on_delete = Some(action.to_string());
        self
    }

    /// 元の行が変わったときの動き。
    pub fn on_update(mut self, action: &str) -> Self {
        self.foreign().on_update = Some(action.to_string());
        self
    }

    /// `on_delete("cascade")` と同じ。
    pub fn cascade_on_delete(self) -> Self {
        self.on_delete("cascade")
    }
}

/// マイグレーションの `up` / `down` が受け取る組み立て先。
pub struct Schema {
    driver: Driver,
    statements: Vec<String>,
}

impl Schema {
    /// ドライバを決めて作る。ランナーが呼びます。
    pub(crate) fn new(driver: Driver) -> Self {
        Self {
            driver,
            statements: Vec::new(),
        }
    }

    /// つながる先のデータベースの種類。
    pub fn driver(&self) -> Driver {
        self.driver
    }

    /// 表を作る。
    pub fn create(&mut self, table: &str, build: impl FnOnce(&mut Blueprint)) {
        self.run(
            table,
            Mode::Create {
                if_not_exists: false,
            },
            build,
        );
    }

    /// 無ければ表を作る。
    pub fn create_if_not_exists(&mut self, table: &str, build: impl FnOnce(&mut Blueprint)) {
        self.run(
            table,
            Mode::Create {
                if_not_exists: true,
            },
            build,
        );
    }

    /// 表を変える。足せるのは列と索引だけです。
    pub fn table(&mut self, table: &str, build: impl FnOnce(&mut Blueprint)) {
        self.run(table, Mode::Alter, build);
    }

    fn run(&mut self, table: &str, mode: Mode, build: impl FnOnce(&mut Blueprint)) {
        let mut blueprint = Blueprint::new(self.driver, table, mode);
        build(&mut blueprint);
        self.statements.extend(blueprint.build());
    }

    /// 表を消す。
    pub fn drop(&mut self, table: &str) {
        self.statements
            .push(format!("drop table {}", grammar::quote(self.driver, table)));
    }

    /// あれば表を消す。
    pub fn drop_if_exists(&mut self, table: &str) {
        self.statements.push(format!(
            "drop table if exists {}",
            grammar::quote(self.driver, table)
        ));
    }

    /// 表の名前を変える。
    pub fn rename(&mut self, from: &str, to: &str) {
        self.statements.push(format!(
            "alter table {} rename to {}",
            grammar::quote(self.driver, from),
            grammar::quote(self.driver, to)
        ));
    }

    /// 列を消す。
    pub fn drop_column(&mut self, table: &str, column: &str) {
        self.statements.push(format!(
            "alter table {} drop column {}",
            grammar::quote(self.driver, table),
            grammar::quote(self.driver, column)
        ));
    }

    /// SQL をそのまま足す。
    pub fn raw(&mut self, sql: impl Into<String>) {
        self.statements.push(sql.into());
    }

    /// 組み立てた SQL の一覧を見る。
    pub fn to_sql(&self) -> &[String] {
        &self.statements
    }

    /// 組み立てた SQL の一覧を取り出す。ランナーが呼びます。
    pub(crate) fn into_statements(self) -> Vec<String> {
        self.statements
    }
}

/// 自動採番の主キーの書き方。
fn auto_increment_sql(driver: Driver) -> &'static str {
    match driver {
        Driver::Sqlite => "integer primary key autoincrement not null",
        Driver::MySql => "bigint unsigned not null auto_increment primary key",
        Driver::Postgres => "bigserial primary key",
    }
}

/// 列の型名。
fn type_sql(driver: Driver, kind: &ColumnType) -> String {
    match (driver, kind) {
        (_, ColumnType::Raw(sql)) => sql.clone(),
        (_, ColumnType::Id) => auto_increment_sql(driver).to_string(),
        (_, ColumnType::Integer) => "integer".into(),
        (Driver::Sqlite, ColumnType::BigInteger) => "integer".into(),
        (_, ColumnType::BigInteger) => "bigint".into(),
        (_, ColumnType::Str(length)) => format!("varchar({length})"),
        (_, ColumnType::Text) => "text".into(),
        (Driver::Sqlite, ColumnType::Boolean) => "boolean".into(),
        (Driver::MySql, ColumnType::Boolean) => "tinyint(1)".into(),
        (Driver::Postgres, ColumnType::Boolean) => "boolean".into(),
        (Driver::Sqlite, ColumnType::Float) => "real".into(),
        (Driver::Sqlite, ColumnType::Double) => "real".into(),
        (_, ColumnType::Float) => "float".into(),
        (_, ColumnType::Double) => "double precision".into(),
        (_, ColumnType::Decimal(total, places)) => format!("numeric({total}, {places})"),
        (_, ColumnType::Date) => "date".into(),
        (Driver::Postgres, ColumnType::DateTime) => "timestamp".into(),
        (_, ColumnType::DateTime) => "datetime".into(),
        (Driver::Sqlite, ColumnType::Json) => "text".into(),
        (_, ColumnType::Json) => "json".into(),
        (Driver::Sqlite, ColumnType::Binary) => "blob".into(),
        (Driver::MySql, ColumnType::Binary) => "blob".into(),
        (Driver::Postgres, ColumnType::Binary) => "bytea".into(),
        (_, ColumnType::Uuid) => "varchar(36)".into(),
    }
}

/// 既定値の書き方。値は SQL に直接書くので、文字列は引用符を重ねて逃がす。
fn default_sql(driver: Driver, default: &DefaultValue) -> String {
    match default {
        DefaultValue::Raw(sql) => sql.clone(),
        DefaultValue::Value(Value::Null) => "null".into(),
        DefaultValue::Value(Value::Int(v)) => v.to_string(),
        DefaultValue::Value(Value::Float(v)) => v.to_string(),
        DefaultValue::Value(Value::Bool(v)) => match driver {
            Driver::Postgres => v.to_string(),
            _ => i32::from(*v).to_string(),
        },
        DefaultValue::Value(Value::Text(v)) => {
            let escaped = v.replace('\'', "''");
            // MySQL は既定で `\` が逃がし文字。重ねないと引用が閉じない。
            let escaped = match driver {
                Driver::MySql => escaped.replace('\\', "\\\\"),
                _ => escaped,
            };
            format!("'{escaped}'")
        }
        DefaultValue::Value(Value::Bytes(_)) => "null".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sqlite() -> Schema {
        Schema::new(Driver::Sqlite)
    }

    #[test]
    fn 表を作る() {
        let mut schema = sqlite();
        schema.create("posts", |t| {
            t.id();
            t.string("title");
            t.text("body").nullable();
            t.string("status").default("draft");
            t.integer("views").default(0);
            t.boolean("pinned").default(false);
            t.timestamps();
        });
        let sql = schema.to_sql();
        assert_eq!(sql.len(), 1);
        assert_eq!(
            sql[0],
            "create table \"posts\" (\
             \"id\" integer primary key autoincrement not null, \
             \"title\" varchar(255) not null, \
             \"body\" text null, \
             \"status\" varchar(255) not null default 'draft', \
             \"views\" integer not null default 0, \
             \"pinned\" boolean not null default 0, \
             \"created_at\" datetime null, \
             \"updated_at\" datetime null)"
        );
    }

    #[test]
    fn 索引は別の文になる() {
        let mut schema = sqlite();
        schema.create("users", |t| {
            t.id();
            t.string("email").unique();
            t.string("name").index();
            t.index(&["name", "email"]);
        });
        let sql = schema.to_sql();
        assert_eq!(sql.len(), 3);
        assert!(sql[0].contains("\"email\" varchar(255) not null unique"));
        assert_eq!(
            sql[1],
            "create index \"users_name_index\" on \"users\" (\"name\")"
        );
        assert_eq!(
            sql[2],
            "create index \"users_name_email_index\" on \"users\" (\"name\", \"email\")"
        );
    }

    #[test]
    fn 外部キーを書ける() {
        let mut schema = sqlite();
        schema.create("comments", |t| {
            t.id();
            t.foreign_id("post_id");
            t.foreign("post_id").on("posts").cascade_on_delete();
        });
        assert!(schema.to_sql()[0]
            .contains("foreign key (\"post_id\") references \"posts\" (\"id\") on delete cascade"));
    }

    #[test]
    fn 表を変える() {
        let mut schema = sqlite();
        schema.table("posts", |t| {
            t.string("slug").nullable();
            t.integer("likes").default(0);
            t.index(&["slug"]);
        });
        let sql = schema.to_sql();
        assert_eq!(
            sql[0],
            "alter table \"posts\" add column \"slug\" varchar(255) null"
        );
        assert_eq!(
            sql[1],
            "alter table \"posts\" add column \"likes\" integer not null default 0"
        );
        assert_eq!(
            sql[2],
            "create index \"posts_slug_index\" on \"posts\" (\"slug\")"
        );
    }

    #[test]
    fn 消す_名前を変える_生で書く() {
        let mut schema = sqlite();
        schema.drop_if_exists("posts");
        schema.drop("users");
        schema.rename("a", "b");
        schema.drop_column("posts", "slug");
        schema.raw("pragma foreign_keys = on");
        let expected = [
            "drop table if exists \"posts\"",
            "drop table \"users\"",
            "alter table \"a\" rename to \"b\"",
            "alter table \"posts\" drop column \"slug\"",
            "pragma foreign_keys = on",
        ];
        assert_eq!(schema.to_sql().len(), expected.len());
        for (actual, want) in schema.to_sql().iter().zip(expected) {
            assert_eq!(actual, want);
        }
    }

    #[test]
    fn 文字列の既定値は引用符を逃がす() {
        let mut schema = sqlite();
        schema.create("t", |t| {
            t.string("a").default("it's");
            t.date_time("b").default_raw("current_timestamp");
        });
        assert!(schema.to_sql()[0].contains("default 'it''s'"));
        assert!(schema.to_sql()[0].contains("default current_timestamp"));
    }

    #[test]
    fn 無ければ作るときは索引にも_if_not_exists_が付く() {
        let mut schema = sqlite();
        schema.create_if_not_exists("users", |t| {
            t.id();
            t.string("name").index();
            t.unique(&["name"]);
        });
        let sql = schema.to_sql();
        assert!(sql[0].starts_with("create table if not exists \"users\""));
        assert_eq!(
            sql[1],
            "create index if not exists \"users_name_index\" on \"users\" (\"name\")"
        );
        assert_eq!(
            sql[2],
            "create unique index if not exists \"users_name_unique\" on \"users\" (\"name\")"
        );

        // ふつうの create には付けない（これまでどおり）。
        let mut schema = sqlite();
        schema.create("users", |t| {
            t.id();
            t.string("name").index();
        });
        assert_eq!(
            schema.to_sql()[1],
            "create index \"users_name_index\" on \"users\" (\"name\")"
        );
    }

    #[test]
    fn mysql_の既定値は逆斜線も逃がす() {
        let mut schema = Schema::new(Driver::MySql);
        schema.create("t", |t| {
            t.string("a").default("a\\");
        });
        assert!(
            schema.to_sql()[0].contains("default 'a\\\\'"),
            "{}",
            schema.to_sql()[0]
        );

        // SQLite と PostgreSQL では `\` は普通の文字なので触らない。
        let mut schema = sqlite();
        schema.create("t", |t| {
            t.string("a").default("a\\");
        });
        assert!(schema.to_sql()[0].contains("default 'a\\'"));
    }

    #[test]
    fn mysql_と_postgres_でも型名が出る() {
        for driver in [Driver::MySql, Driver::Postgres] {
            let mut schema = Schema::new(driver);
            schema.create("posts", |t| {
                t.id();
                t.string("title");
                t.boolean("pinned").default(true);
            });
            assert_eq!(schema.to_sql().len(), 1);
            assert_eq!(schema.driver(), driver);
        }
    }
}
