//! いまの時刻を文字列にする。
//!
//! 日時専用の型は持ちません（決定記録 #040）。`created_at` などに入れる
//! `YYYY-MM-DD HH:MM:SS`（UTC）の文字列だけを作ります。
//! `chrono` を入れないために自分で計算します。

use std::time::{SystemTime, UNIX_EPOCH};

/// いまの時刻（UTC）を `YYYY-MM-DD HH:MM:SS` で返す。
pub(crate) fn now_string() -> String {
    format_timestamp(now_seconds())
}

/// いまの UNIX 時刻（秒）。
pub(crate) fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `YYYY-MM-DD HH:MM:SS` を UNIX 時刻（秒）に戻す。読めなければ `None`。
///
/// 日付だけ（`YYYY-MM-DD`）も受け付け、00:00:00 として扱います。
pub(crate) fn parse_timestamp(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, time) = match text.split_once(['T', ' ']) {
        Some((date, time)) => (date, time),
        None => (text, "00:00:00"),
    };

    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // 秒より細かい桁とタイムゾーンの指定は切り捨てる。
    let time = time.split(['.', '+', 'Z']).next()?;
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next().unwrap_or("0").parse().ok()?;
    let second: i64 = time_parts.next().unwrap_or("0").parse().ok()?;
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) || !(0..=60).contains(&second) {
        return None;
    }

    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// 年・月・日を 1970-01-01 からの日数に直す。`civil_from_days` の逆です。
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let year_of_era = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64;
    let day_of_year = (153 * mp + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
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
    fn 文字列から秒に戻せる() {
        for seconds in [
            0i64,
            86_399,
            86_400,
            1_709_210_096,
            1_791_158_400,
            951_868_800,
        ] {
            let text = format_timestamp(seconds);
            assert_eq!(parse_timestamp(&text), Some(seconds), "{text}");
        }
    }

    #[test]
    fn 日付だけや区切り文字つきも読める() {
        assert_eq!(parse_timestamp("1970-01-02"), Some(86_400));
        assert_eq!(parse_timestamp("1970-01-02T00:00:00"), Some(86_400));
        assert_eq!(parse_timestamp("1970-01-02 00:00:00.123"), Some(86_400));
        assert_eq!(parse_timestamp("  1970-01-02 00:00:00  "), Some(86_400));
    }

    #[test]
    fn 読めない文字列はnoneになる() {
        for broken in [
            "",
            "こわれた値",
            "2026-13-01",
            "2026-01-32",
            "2026-01",
            "x-01-01",
        ] {
            assert_eq!(parse_timestamp(broken), None, "{broken}");
        }
    }

    #[test]
    fn いまの秒と文字列が一致する() {
        let seconds = now_seconds();
        let text = now_string();
        // 秒をまたぐことがあるので 2 秒まで許す。
        let parsed = parse_timestamp(&text).expect("自分が作った形は読める");
        assert!((parsed - seconds).abs() <= 2, "{text} と {seconds}");
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
