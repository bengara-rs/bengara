//! ドライバごとの SQL の書き分け。
//!
//! 引用符、プレースホルダ、列の型名の3つだけが違います。ここに集めておくと、
//! MySQL と PostgreSQL を足すときに触る場所が1つで済みます。

/// 使うデータベース。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Driver {
    /// SQLite。
    Sqlite,
    /// MySQL / MariaDB。
    MySql,
    /// PostgreSQL。
    Postgres,
}

impl Driver {
    /// 設定に書く名前。
    pub fn as_str(&self) -> &'static str {
        match self {
            Driver::Sqlite => "sqlite",
            Driver::MySql => "mysql",
            Driver::Postgres => "pgsql",
        }
    }

    /// 設定の文字列から読む。Laravel の名前（`pgsql`）も受け付けます。
    pub fn parse(name: &str) -> Option<Driver> {
        match name.trim().to_ascii_lowercase().as_str() {
            "sqlite" | "sqlite3" => Some(Driver::Sqlite),
            "mysql" | "mariadb" => Some(Driver::MySql),
            "pgsql" | "postgres" | "postgresql" => Some(Driver::Postgres),
            _ => None,
        }
    }

    /// いまの作りで実際につなげるか。
    ///
    /// SQLite は機能フラグ `sqlite` が必要です。
    /// MySQL と PostgreSQL は SQL の組み立てだけ用意してあり、**まだつなげません。**
    pub fn is_available(&self) -> bool {
        match self {
            Driver::Sqlite => cfg!(feature = "sqlite"),
            Driver::MySql | Driver::Postgres => false,
        }
    }
}

impl std::fmt::Display for Driver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 値を渡す場所の書き方。`index` は1から数えます。
pub(crate) fn placeholder(driver: Driver, index: usize) -> String {
    match driver {
        Driver::Postgres => format!("${index}"),
        _ => "?".to_string(),
    }
}

/// 名前を引用符でくくる。`users.id` は `"users"."id"` になります。
///
/// `*`、`count(*)`、`id as key` のような式はそのまま返します。
///
/// **そのまま返す経路は意図した逃げ道です。** `select(&["count(*) as total"])` が
/// これに頼っています。引用符を含む名前は素通しせず、`""` に逃がしてくくります。
/// そのうえで、外から来た文字列が入りうる場所（`where_` / `order_by` / `join` の
/// 列名など）では [`is_plain_identifier`] で先に検査します。
pub(crate) fn quote(driver: Driver, name: &str) -> String {
    let trimmed = name.trim();
    if is_expression(trimmed) {
        return trimmed.to_string();
    }
    trimmed
        .split('.')
        .map(|part| quote_part(driver, part))
        .collect::<Vec<_>>()
        .join(".")
}

/// 引用符でくくらずにそのまま置く式か。
///
/// 引用符（`"` と `` ` ``）は見ません。`quote_part` が `""` に逃がすので、
/// 素通しさせる必要がないためです。素通しすると、引用符を1つ混ぜるだけで
/// 検査を抜けられてしまいます。
fn is_expression(name: &str) -> bool {
    name == "*" || name.contains('(') || name.contains(' ')
}

/// ただの列名か。英数字と `_` `.` だけを許します。
///
/// 英数字は Unicode で見ます。日本語の列名も通ります。`"` `;` `(` `-` と空白は
/// Unicode でも英数字ではないので、検査の強さは変わりません。
///
/// `*` も式も通しません。式を書きたいときは `order_by_raw` / `having_raw` /
/// `where_raw` を使ってください。
pub(crate) fn is_plain_identifier(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return false;
    }
    trimmed
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
}

/// 比べるのに使える演算子の一覧。これ以外は受け付けません。
///
/// 大文字小文字は区別しません。語の間の空白はいくつでもかまいません。
pub(crate) const OPERATORS: &[&str] = &[
    "=",
    "!=",
    "<>",
    "<",
    "<=",
    ">",
    ">=",
    "<=>",
    "like",
    "not like",
    "ilike",
    "not ilike",
    "in",
    "not in",
    "is",
    "is not",
    "between",
    "not between",
];

/// 演算子として受け付けるか。
pub(crate) fn is_allowed_operator(operator: &str) -> bool {
    let normalized = operator
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    OPERATORS.contains(&normalized.as_str())
}

fn quote_part(driver: Driver, part: &str) -> String {
    if part == "*" {
        return part.to_string();
    }
    match driver {
        Driver::MySql => format!("`{}`", part.replace('`', "``")),
        _ => format!("\"{}\"", part.replace('"', "\"\"")),
    }
}

/// `insert` の後に自動採番の ID を受け取るための追記（PostgreSQL だけ必要）。
pub(crate) fn returning_id(driver: Driver, primary_key: &str) -> String {
    match driver {
        Driver::Postgres => format!(" returning {}", quote(driver, primary_key)),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 名前の引用はドライバごとに変わる() {
        assert_eq!(quote(Driver::Sqlite, "title"), "\"title\"");
        assert_eq!(quote(Driver::MySql, "title"), "`title`");
        assert_eq!(quote(Driver::Postgres, "users.id"), "\"users\".\"id\"");
        assert_eq!(quote(Driver::Sqlite, "users.*"), "\"users\".*");
    }

    #[test]
    fn 式はそのまま置く() {
        assert_eq!(quote(Driver::Sqlite, "*"), "*");
        assert_eq!(quote(Driver::Sqlite, "count(*)"), "count(*)");
        assert_eq!(quote(Driver::Sqlite, "id as key"), "id as key");
    }

    #[test]
    fn 引用符を含む名前は素通しせず逃がす() {
        // `"` を混ぜて検査を抜ける道を塞ぐ。
        assert_eq!(quote(Driver::Sqlite, "a\"b"), "\"a\"\"b\"");
        assert_eq!(quote(Driver::MySql, "a`b"), "`a``b`");
        // バックティックは SQLite では普通の文字なので、そのままくくる。
        assert_eq!(quote(Driver::Sqlite, "a`b"), "\"a`b\"");
    }

    #[test]
    fn プレースホルダはpostgresだけ番号付き() {
        assert_eq!(placeholder(Driver::Sqlite, 1), "?");
        assert_eq!(placeholder(Driver::MySql, 3), "?");
        assert_eq!(placeholder(Driver::Postgres, 3), "$3");
    }

    #[test]
    fn ただの列名だけを通す() {
        assert!(is_plain_identifier("title"));
        assert!(is_plain_identifier("users.id"));
        assert!(is_plain_identifier("views_2"));
        assert!(!is_plain_identifier(""));
        assert!(!is_plain_identifier("*"));
        assert!(!is_plain_identifier("count(*)"));
        assert!(!is_plain_identifier("id desc"));
        assert!(!is_plain_identifier("id; drop table posts"));
        assert!(!is_plain_identifier("\"id\""));
        assert!(!is_plain_identifier("a`b"));
        assert!(!is_plain_identifier("id-1"));
    }

    #[test]
    fn 日本語の列名も通る() {
        // 英数字を Unicode で見るので、`where_` と `order_by` で結果がそろう。
        assert!(is_plain_identifier("題名"));
        assert!(is_plain_identifier("posts.題名"));
        assert_eq!(quote(Driver::Sqlite, "題名"), "\"題名\"");
    }

    #[test]
    fn 演算子は許可一覧で照合する() {
        assert!(is_allowed_operator("="));
        assert!(is_allowed_operator(">="));
        assert!(is_allowed_operator("LIKE"));
        assert!(is_allowed_operator("not   like"));
        assert!(is_allowed_operator(" is not "));
        assert!(!is_allowed_operator(""));
        assert!(!is_allowed_operator("= 1 or 1"));
        assert!(!is_allowed_operator("glob"));
    }

    #[test]
    fn 名前から読める() {
        assert_eq!(Driver::parse("sqlite"), Some(Driver::Sqlite));
        assert_eq!(Driver::parse("PgSQL"), Some(Driver::Postgres));
        assert_eq!(Driver::parse("oracle"), None);
    }
}
