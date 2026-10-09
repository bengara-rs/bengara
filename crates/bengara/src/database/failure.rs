//! SQL の実行に失敗したときのエラー。**3 つのドライバで共通です。**
//!
//! ドライバごとに書くと、片方だけ直す形が繰り返されます（決定記録 #119 と同じ理由）。
//! 一意制約違反の判定はここ 1 か所だけにあります。

use crate::error::Error;

/// SQL の実行に失敗したときのエラー。
///
/// **sqlx のエラーを原因として残します。** 一意制約違反などを種類で見分けるためです。
/// `Error::msg` に文だけ入れると、残るのは文字列だけになります。
#[derive(Debug)]
pub(crate) struct SqlFailure {
    sql: String,
    source: sqlx::Error,
}

impl SqlFailure {
    /// 一意制約（unique / primary key）に当たったか。
    fn is_unique_violation(&self) -> bool {
        self.source
            .as_database_error()
            .is_some_and(|e| e.is_unique_violation())
    }
}

impl std::fmt::Display for SqlFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SQL の実行に失敗しました: {}\n  SQL: {}",
            self.source, self.sql
        )
    }
}

impl std::error::Error for SqlFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// SQL の失敗を、どの文で起きたか分かる形にする。
///
/// **渡した値は出しません。** 個人情報が混じることがあるためです。
pub(crate) fn failed(sql: &str, error: sqlx::Error) -> Error {
    Error::other(SqlFailure {
        sql: sql.to_string(),
        source: error,
    })
}

/// 一意制約違反かどうかを、原因の連鎖から探す。
///
/// `Error::Other` に包んだ `SqlFailure` を見つけて、そこから判断します。
pub(crate) fn is_unique_violation(error: &Error) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(found) = current {
        if let Some(failure) = found.downcast_ref::<SqlFailure>() {
            return failure.is_unique_violation();
        }
        current = found.source();
    }
    false
}

/// 値の種類が分からない列を読もうとしたときのエラー。
///
/// 3 つのドライバで同じ文にそろえます。 **黙って別の値にしません。**
/// 読めない型は、列の名前と SQL の型名を添えて断ります。
pub(crate) fn unsupported_column(name: &str, type_name: &str) -> Error {
    Error::msg(format!(
        "列 `{name}` の型 `{type_name}` は読めません。\
         select で文字列に変換して取り出してください（例: `cast(列 as text) as 列`）"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 包んでいないエラーは一意制約違反ではない() {
        assert!(!is_unique_violation(&Error::msg("ただのメッセージ")));
        assert!(!is_unique_violation(&Error::http(409, "Conflict")));
    }

    #[test]
    fn 読めない型は列名と型名を添えて断る() {
        let error = unsupported_column("total", "NUMERIC").to_string();
        assert!(error.contains("total"), "{error}");
        assert!(error.contains("NUMERIC"), "{error}");
        assert!(error.contains("cast"), "{error}");
    }
}
