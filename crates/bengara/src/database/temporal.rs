//! 日時の列を文字列に直す。**MySQL と PostgreSQL で共通です。**
//!
//! bengara は日時を `YYYY-MM-DD HH:MM:SS` の文字列で扱います（決定記録 #040）。
//! SQLite は値をそのまま文字列で返しますが、MySQL と PostgreSQL は本物の
//! 日付型を返すので、ここで文字列に直します。
//!
//! 組み立ては自前です。`time` クレートの書式づけ（`format_description`）は
//! 使いません。欲しい形が1つだけで、年月日時分秒を取り出せば足りるためです。
//!
//! ドライバごとに書くと、片方だけ直す形が繰り返されます（決定記録 #119 と同じ理由）。

use sqlx::types::time::{Date, PrimitiveDateTime, Time};
// 時差付きの日時は PostgreSQL の `timestamptz` だけで出てきます。
#[cfg(feature = "postgres")]
use sqlx::types::time::{OffsetDateTime, UtcOffset};

/// `YYYY-MM-DD`。
pub(crate) fn date_string(value: Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        value.year(),
        u8::from(value.month()),
        value.day()
    )
}

/// `HH:MM:SS`。小数の秒は落とします。
pub(crate) fn time_string(value: Time) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        value.hour(),
        value.minute(),
        value.second()
    )
}

/// `YYYY-MM-DD HH:MM:SS`。[`crate::database::now`] と同じ形です。
pub(crate) fn date_time_string(value: PrimitiveDateTime) -> String {
    format!(
        "{} {}",
        date_string(value.date()),
        time_string(value.time())
    )
}

/// 時差付きの日時を **UTC に直して** `YYYY-MM-DD HH:MM:SS` にする。
///
/// bengara の日時は UTC です。時差をそのまま落とすと、1つの表の中で基準が
/// 混ざってしまいます。
///
/// MySQL には時差付きの型が無いので、PostgreSQL のときだけ入ります。
#[cfg(feature = "postgres")]
pub(crate) fn offset_date_time_string(value: OffsetDateTime) -> String {
    let utc = value.to_offset(UtcOffset::UTC);
    format!("{} {}", date_string(utc.date()), time_string(utc.time()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 年と「1月1日から数えた日数」から日付を作る。
    ///
    /// `Date::from_calendar_date` を使わないのは、必要な `Month` が
    /// `sqlx::types::time` から出ていないためです。`time` クレートを直接の依存に
    /// 足したくないので、数えた日数で指定します。
    fn date(year: i32, ordinal: u16) -> Date {
        Date::from_ordinal_date(year, ordinal).expect("日付として成り立つ")
    }

    /// 2026年10月9日（うるう年ではないので 273 + 9 日目）。
    const OCT_9: u16 = 282;

    #[test]
    fn 日付は0詰めで並べる() {
        assert_eq!(date_string(date(2026, 9)), "2026-01-09");
        // 4桁に足りない年も0で埋める。
        assert_eq!(date_string(date(999, 365)), "0999-12-31");
        assert_eq!(date_string(date(2026, OCT_9)), "2026-10-09");
    }

    #[test]
    fn 時刻は秒までにする() {
        let t = Time::from_hms_micro(1, 2, 3, 456_789).expect("時刻として成り立つ");
        assert_eq!(time_string(t), "01:02:03");
    }

    #[test]
    fn 日時は_now_と同じ形になる() {
        let value = PrimitiveDateTime::new(date(2026, OCT_9), Time::from_hms(12, 34, 56).unwrap());
        assert_eq!(date_time_string(value), "2026-10-09 12:34:56");
        // `now()` と桁数がそろっていること。
        assert_eq!(date_time_string(value).len(), crate::database::now().len());
    }

    #[test]
    #[cfg(feature = "postgres")]
    fn 時差付きは_utc_に直す() {
        let value = PrimitiveDateTime::new(date(2026, OCT_9), Time::from_hms(9, 0, 0).unwrap())
            .assume_offset(UtcOffset::from_hms(9, 0, 0).unwrap());
        assert_eq!(offset_date_time_string(value), "2026-10-09 00:00:00");
    }
}
