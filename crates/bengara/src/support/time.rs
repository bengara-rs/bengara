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
///
/// # 時刻の後ろに付いたものは落とします
///
/// 秒より細かい桁（`.123`）と、タイムゾーンの指定（`Z`・`+09:00`・`-05:00`）は
/// 見ません。**時刻部分の先頭の `HH:MM:SS` だけを読みます。**
/// `-` を区切りの一覧に並べる形だと負の時差を読み落とし、`None`（＝期限切れ扱い）
/// になっていました。
///
/// タイムゾーンを**足し引きはしません。** 自分で書く値は UTC だけで、
/// 外から入った値の時差まで扱い出すと、どこで直すかが分からなくなるためです。
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
    // 日の上限は月（とうるう年）で変わる。31 で通すと、`2026-02-31` が
    // `2026-03-03` として読めてしまう。
    if date_parts.next().is_some() || !(1..=12).contains(&month) {
        return None;
    }
    if !(1..=days_in_month(year, month)).contains(&day) {
        return None;
    }

    // 先頭の `HH:MM:SS` だけを取る。数字と `:` 以外が出たところで切ります。
    let time = time.trim();
    let end = time
        .find(|c: char| !(c.is_ascii_digit() || c == ':'))
        .unwrap_or(time.len());
    let mut time_parts = time[..end].split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next().unwrap_or("0").parse().ok()?;
    let second: i64 = time_parts.next().unwrap_or("0").parse().ok()?;
    // うるう秒（60 秒）は受け付けません。保存する側が作らない値です。
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) || !(0..60).contains(&second) {
        return None;
    }

    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// その月の日数。
fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        // 月の範囲は呼ぶ側で確かめています。
        _ => 0,
    }
}

/// うるう年か（グレゴリオ暦）。
fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
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
            // 月末を超えた日。31 で通していたころは繰り上がって読めてしまった。
            "2026-02-29",
            "2026-02-31",
            "2026-04-31",
            "2026-06-31",
            "2026-09-31",
            "2026-11-31",
            "2026-01-00",
            // 時刻の範囲。
            "2026-01-01 24:00:00",
            "2026-01-01 00:60:00",
            "2026-01-01 00:00:60",
            "2026-01-01T",
        ] {
            assert_eq!(parse_timestamp(broken), None, "{broken}");
        }
    }

    #[test]
    fn うるう年の二月二十九日は読める() {
        assert_eq!(parse_timestamp("2024-02-29"), Some(1_709_164_800));
        assert!(parse_timestamp("2000-02-29").is_some(), "400 の倍数");
        assert_eq!(parse_timestamp("1900-02-29"), None, "100 の倍数は平年");
        assert!(parse_timestamp("2026-02-28").is_some());
        // 月末ちょうどは通る。
        for text in ["2026-01-31", "2026-04-30", "2026-12-31"] {
            assert!(parse_timestamp(text).is_some(), "{text}");
        }
    }

    #[test]
    fn タイムゾーンつきの時刻も読める() {
        // 時差は足し引きせず、先頭の HH:MM:SS だけを読む。
        let base = parse_timestamp("2026-10-09 10:00:00").unwrap();
        for text in [
            "2026-10-09 10:00:00Z",
            "2026-10-09T10:00:00Z",
            "2026-10-09 10:00:00+09:00",
            // `-` が区切りの一覧に無かったころ、ここだけ None になっていた。
            // `PasswordReset::expired` は None を期限切れにするので、
            // こうした値が入るとトークンが常に拒否された。
            "2026-10-09 10:00:00-05:00",
            "2026-10-09 10:00:00-0500",
            "2026-10-09T10:00:00.123456-05:00",
            "2026-10-09 10:00:00.5",
        ] {
            assert_eq!(parse_timestamp(text), Some(base), "{text}");
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
