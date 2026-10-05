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
/// `*`、`count(*)` のような式はそのまま返します。`as` を含む指定も触りません。
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
fn is_expression(name: &str) -> bool {
    name == "*"
        || name.contains('(')
        || name.contains(' ')
        || name.contains('"')
        || name.contains('`')
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
    fn プレースホルダはpostgresだけ番号付き() {
        assert_eq!(placeholder(Driver::Sqlite, 1), "?");
        assert_eq!(placeholder(Driver::MySql, 3), "?");
        assert_eq!(placeholder(Driver::Postgres, 3), "$3");
    }

    #[test]
    fn 名前から読める() {
        assert_eq!(Driver::parse("sqlite"), Some(Driver::Sqlite));
        assert_eq!(Driver::parse("PgSQL"), Some(Driver::Postgres));
        assert_eq!(Driver::parse("oracle"), None);
    }
}
