//! いまの時刻を文字列にする。
//!
//! 日時専用の型は持ちません（決定記録 #040）。`created_at` などに入れる
//! `YYYY-MM-DD HH:MM:SS`（UTC）の文字列だけを作ります。
//! `chrono` を入れないために自分で計算します。

use std::time::{SystemTime, UNIX_EPOCH};

/// いまの時刻（UTC）を `YYYY-MM-DD HH:MM:SS` で返す。
pub(crate) fn now_string() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_timestamp(seconds)
}

/// UNIX 時刻（秒）を `YYYY-MM-DD HH:MM:SS` にする。
pub(crate) fn format_timestamp(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = rest / 3_600;
    let minute = (rest % 3_600) / 60;
    let second = rest % 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

/// 1970-01-01 からの日数を、年・月・日に直す。
///
/// Howard Hinnant の `civil_from_days` と同じ手順です。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 基準の日を変換できる() {
        assert_eq!(format_timestamp(0), "1970-01-01 00:00:00");
        assert_eq!(format_timestamp(86_399), "1970-01-01 23:59:59");
        assert_eq!(format_timestamp(86_400), "1970-01-02 00:00:00");
    }

    #[test]
    fn うるう年と月末を跨げる() {
        // 2024-02-29 12:34:56 UTC
        assert_eq!(format_timestamp(1_709_210_096), "2024-02-29 12:34:56");
        // 2026-10-05 00:00:00 UTC
        assert_eq!(format_timestamp(1_791_158_400), "2026-10-05 00:00:00");
        // 2000-03-01 00:00:00 UTC
        assert_eq!(format_timestamp(951_868_800), "2000-03-01 00:00:00");
    }

    #[test]
    fn いまの時刻は決まった形になる() {
        let now = now_string();
        assert_eq!(now.len(), 19, "YYYY-MM-DD HH:MM:SS の 19 文字");
        assert_eq!(now.as_bytes()[4], b'-');
        assert_eq!(now.as_bytes()[10], b' ');
        assert!(now.starts_with("20"));
    }
}
