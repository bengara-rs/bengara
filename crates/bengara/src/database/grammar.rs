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
    /// ドライバごとに Cargo の機能フラグが必要です。
    ///
    /// | ドライバ   | フラグ                  |
    /// |------------|-------------------------|
    /// | SQLite     | `sqlite`                |
    /// | MySQL      | `mysql`（`mariadb` も） |
    /// | PostgreSQL | `postgres`              |
    ///
    /// SQL の組み立てはフラグが無くてもできます。つなぐところだけが変わります。
    pub fn is_available(&self) -> bool {
        match self {
            Driver::Sqlite => cfg!(feature = "sqlite"),
            Driver::MySql => cfg!(feature = "mysql"),
            Driver::Postgres => cfg!(feature = "postgres"),
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

/// 列と列を比べるのに使える演算子の一覧。これ以外は受け付けません。
///
/// 大文字小文字は区別しません。語の間の空白はいくつでもかまいません。
///
/// **値1つと比べる側では使えないものが混ざっています。** そちらは
/// [`VALUE_OPERATORS`] を見てください。
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

/// 値1つと比べるのに使える演算子の一覧。
///
/// `in` / `between` 系を外しています。値1つでは SQL が成り立たないためです。
/// `<=>` も外しています。MySQL だけの書き方なので、方言で結果が変わります。
pub(crate) const VALUE_OPERATORS: &[&str] = &[
    "=",
    "!=",
    "<>",
    "<",
    "<=",
    ">",
    ">=",
    "like",
    "not like",
    "ilike",
    "not ilike",
    "is",
    "is not",
];

/// 値を2つ以上とる演算子の一覧。専用のメソッドへ案内するために使います。
pub(crate) const SET_OPERATORS: &[&str] = &["in", "not in", "between", "not between"];

/// 大文字小文字と空白をそろえる。
fn normalize_operator(operator: &str) -> String {
    operator
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// 列と列を比べる演算子として受け付けるか。
pub(crate) fn is_allowed_operator(operator: &str) -> bool {
    OPERATORS.contains(&normalize_operator(operator).as_str())
}

/// 値1つと比べる演算子として受け付けるか。
pub(crate) fn is_allowed_value_operator(operator: &str) -> bool {
    VALUE_OPERATORS.contains(&normalize_operator(operator).as_str())
}

/// 値を2つ以上とる演算子か。
pub(crate) fn is_set_operator(operator: &str) -> bool {
    SET_OPERATORS.contains(&normalize_operator(operator).as_str())
}

/// `offset` だけを付けたときに補う `limit`。
///
/// `offset` は `limit` が無いと受け付けないデータベースがあるので、
/// 「上限なし」を表す書き方を足します。方言で違います。
///
/// | ドライバ   | 書き方                           |
/// |------------|----------------------------------|
/// | PostgreSQL | `limit all`                      |
/// | MySQL      | `limit 18446744073709551615`     |
/// | SQLite     | `limit -1`                       |
///
/// `limit -1` が通るのは SQLite だけです。MySQL では構文エラーになります。
pub(crate) fn no_limit(driver: Driver) -> &'static str {
    match driver {
        Driver::Postgres => " limit all",
        Driver::MySql => " limit 18446744073709551615",
        Driver::Sqlite => " limit -1",
    }
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

/// 表を全部消す文。外部キーの向きを気にせず消せる形にします。
///
/// | ドライバ   | 形                                              |
/// |------------|-------------------------------------------------|
/// | PostgreSQL | 1文にまとめて `cascade` を付ける                |
/// | MySQL      | 1表ずつ。確かめの止め方は [`defer_foreign_keys`] |
/// | SQLite     | 1表ずつ。同上                                   |
///
/// PostgreSQL の `cascade` は、**この文で消す表を指している外部キーも一緒に**
/// 落とします。表を全部消す場面だけで使います。
pub(crate) fn drop_all(driver: Driver, tables: &[String]) -> Vec<String> {
    if tables.is_empty() {
        return Vec::new();
    }
    let quoted: Vec<String> = tables.iter().map(|t| quote(driver, t)).collect();
    match driver {
        Driver::Postgres => vec![format!(
            "drop table if exists {} cascade",
            quoted.join(", ")
        )],
        _ => quoted
            .iter()
            .map(|t| format!("drop table if exists {t}"))
            .collect(),
    }
}

/// 表を全部消す間だけ、外部キーの確かめを止める SQL。
///
/// 返すのは（始めに流す文、終わりに流す文）です。終わりが `None` のときは、
/// 流しっぱなしにしてかまいません。
///
/// | ドライバ   | 始め                                | 終わり                              |
/// |------------|-------------------------------------|-------------------------------------|
/// | SQLite     | `pragma defer_foreign_keys = on`    | 無し（確定のときに自動で戻る）      |
/// | MySQL      | `set session foreign_key_checks = 0` | `set session foreign_key_checks = 1` |
/// | PostgreSQL | 無し（`cascade` で足りる）          | 無し                                |
pub(crate) fn defer_foreign_keys(driver: Driver) -> (Option<&'static str>, Option<&'static str>) {
    match driver {
        Driver::Sqlite => (Some("pragma defer_foreign_keys = on"), None),
        Driver::MySql => (
            Some("set session foreign_key_checks = 0"),
            Some("set session foreign_key_checks = 1"),
        ),
        Driver::Postgres => (None, None),
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
    fn 値と比べる演算子は一覧が狭い() {
        // 値1つと比べられるもの。
        assert!(is_allowed_value_operator("="));
        assert!(is_allowed_value_operator("NOT LIKE"));
        assert!(is_allowed_value_operator("is not"));
        // 値1つでは SQL が成り立たないもの。
        assert!(!is_allowed_value_operator("in"));
        assert!(!is_allowed_value_operator("between"));
        assert!(!is_allowed_value_operator("not between"));
        // MySQL だけの書き方。
        assert!(!is_allowed_value_operator("<=>"));
        // 列と列を比べる側では今までどおり通る。
        assert!(is_allowed_operator("between"));
        assert!(is_allowed_operator("<=>"));
    }

    #[test]
    fn 値を2つ以上とる演算子が分かる() {
        assert!(is_set_operator("IN"));
        assert!(is_set_operator("not  between"));
        assert!(!is_set_operator("="));
        assert!(!is_set_operator("like"));
    }

    #[test]
    fn 上限なしのlimitは方言で違う() {
        // `limit -1` が通るのは SQLite だけ。
        assert_eq!(no_limit(Driver::Sqlite), " limit -1");
        assert_eq!(no_limit(Driver::MySql), " limit 18446744073709551615");
        assert_eq!(no_limit(Driver::Postgres), " limit all");
    }

    #[test]
    fn 名前から読める() {
        assert_eq!(Driver::parse("sqlite"), Some(Driver::Sqlite));
        assert_eq!(Driver::parse("PgSQL"), Some(Driver::Postgres));
        assert_eq!(Driver::parse("oracle"), None);
    }
    #[test]
    fn 表を全部消す文は方言で違う() {
        let tables = vec!["posts".to_string(), "users".to_string()];

        // PostgreSQL は1文にまとめて cascade を付ける。
        assert_eq!(
            drop_all(Driver::Postgres, &tables),
            vec![r#"drop table if exists "posts", "users" cascade"#.to_string()]
        );
        // MySQL と SQLite は1表ずつ。
        assert_eq!(
            drop_all(Driver::MySql, &tables),
            vec![
                "drop table if exists `posts`".to_string(),
                "drop table if exists `users`".to_string(),
            ]
        );
        assert_eq!(
            drop_all(Driver::Sqlite, &tables),
            vec![
                r#"drop table if exists "posts""#.to_string(),
                r#"drop table if exists "users""#.to_string(),
            ]
        );
        // 表が無いときは何も出さない（空の文を投げないため）。
        assert!(drop_all(Driver::Postgres, &[]).is_empty());
    }

    #[test]
    fn 外部キーの止め方は方言で違う() {
        // SQLite は確定のときに自動で戻るので、戻す文が無い。
        let (before, after) = defer_foreign_keys(Driver::Sqlite);
        assert_eq!(before, Some("pragma defer_foreign_keys = on"));
        assert_eq!(after, None);

        // MySQL は接続ごとの設定なので、必ず戻す。
        let (before, after) = defer_foreign_keys(Driver::MySql);
        assert_eq!(before, Some("set session foreign_key_checks = 0"));
        assert_eq!(after, Some("set session foreign_key_checks = 1"));

        // PostgreSQL は cascade で足りる。
        assert_eq!(defer_foreign_keys(Driver::Postgres), (None, None));
    }
}
