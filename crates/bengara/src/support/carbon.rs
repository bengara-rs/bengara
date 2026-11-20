//! 日時の型 `Carbon`。Laravel の `Carbon` に当たります。
//!
//! 中身は UTC の UNIX 秒だけです。タイムゾーンは持ちません。
//! `chrono` を入れないために、計算は [`super::time`] の手順を使います。

use std::fmt;
use std::str::FromStr;

use super::time;
use crate::error::{Error, Result};

/// 扱える最小の時刻（`0001-01-01 00:00:00`）。
const MIN_SECONDS: i64 = -62_135_596_800;
/// 扱える最大の時刻（`9999-12-31 23:59:59`）。
const MAX_SECONDS: i64 = 253_402_300_799;

/// 日時（UTC）。
///
/// 扱う範囲は `0001-01-01 00:00:00` 〜 `9999-12-31 23:59:59` です。
/// 範囲の外になる計算は端に寄せます。パニックも巻き戻りもしません。
///
/// ```
/// use bengara::Carbon;
///
/// let at = Carbon::parse("2026-01-31 12:00:00")?;
/// assert_eq!(at.add_months(1).to_date_time_string(), "2026-02-28 12:00:00");
/// # Ok::<(), bengara::Error>(())
/// ```
///
/// | 持たないもの | 代わりに |
/// |---|---|
/// | タイムゾーン | UTC で持ち、表示するときに自分で直す |
/// | `format()`（PHP の書式） | `to_date_string()` などの決まった形を使う |
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Carbon {
    seconds: i64, // UTC の UNIX 秒
}

impl Carbon {
    /// いまの時刻。
    pub fn now() -> Self {
        Self::from_timestamp(time::now_seconds())
    }

    /// UNIX 秒から作る。範囲の外は端に寄せます。
    pub fn from_timestamp(seconds: i64) -> Self {
        Self {
            seconds: seconds.clamp(MIN_SECONDS, MAX_SECONDS),
        }
    }

    /// 文字列を読む。
    ///
    /// | 受け付ける形 | 例 |
    /// |---|---|
    /// | 日付だけ（`00:00:00` として読む） | `2026-11-20` |
    /// | 日付と時刻（区切りは空白か `T`） | `2026-11-20 09:30:00`・`2026-11-20T09:30:00` |
    /// | 末尾の `Z` | `2026-11-20T09:30:00Z` |
    /// | 小数の秒（捨てる） | `2026-11-20 09:30:00.123`・`2026-11-20T09:30:00.5Z` |
    ///
    /// `+09:00` などの時差付きは受け付けません。足し引きを黙って捨てないためです。
    ///
    /// **桁はちょうどにそろえます。** 日付は 4 桁・2 桁・2 桁、時刻は `HH:MM` か
    /// `HH:MM:SS`（どれも 2 桁）です。`2026-1-5`・`+2026-01-01`・`2026-11-20 9:30`・
    /// `2026-11-20 09:30:00:00` は読めません。
    pub fn parse(text: &str) -> Result<Self> {
        let error = || {
            Error::msg(format!(
                "`{text}` は日時として読めません（YYYY-MM-DD か YYYY-MM-DD HH:MM:SS。UTC です）"
            ))
        };
        let trimmed = text.trim();
        if let Some((_, after)) = trimmed.split_once(['T', ' ']) {
            let end = after
                .find(|c: char| !(c.is_ascii_digit() || c == ':'))
                .unwrap_or(after.len());
            if !is_allowed_suffix(&after[end..]) {
                return Err(error());
            }
        }
        time::parse_timestamp(trimmed)
            .map(Self::from_timestamp)
            .ok_or_else(error)
    }

    /// UNIX 秒。
    pub fn timestamp(&self) -> i64 {
        self.seconds
    }

    /// 年。
    pub fn year(&self) -> i32 {
        self.civil().0 as i32
    }

    /// 月（1〜12）。
    pub fn month(&self) -> u32 {
        self.civil().1
    }

    /// 日（1〜31）。
    pub fn day(&self) -> u32 {
        self.civil().2
    }

    /// 時（0〜23）。
    pub fn hour(&self) -> u32 {
        (self.second_of_day() / 3_600) as u32
    }

    /// 分（0〜59）。
    pub fn minute(&self) -> u32 {
        (self.second_of_day() % 3_600 / 60) as u32
    }

    /// 秒（0〜59）。
    pub fn second(&self) -> u32 {
        (self.second_of_day() % 60) as u32
    }

    /// 秒を足す。負なら引きます。
    pub fn add_seconds(self, seconds: i64) -> Self {
        Self::from_timestamp(self.seconds.saturating_add(seconds))
    }

    /// 分を足す。負なら引きます。
    pub fn add_minutes(self, minutes: i64) -> Self {
        self.add_seconds(minutes.saturating_mul(60))
    }

    /// 時間を足す。負なら引きます。
    pub fn add_hours(self, hours: i64) -> Self {
        self.add_seconds(hours.saturating_mul(3_600))
    }

    /// 日を足す。負なら引きます。
    pub fn add_days(self, days: i64) -> Self {
        self.add_seconds(days.saturating_mul(86_400))
    }

    /// 秒を引く。
    pub fn sub_seconds(self, seconds: i64) -> Self {
        self.add_seconds(seconds.saturating_neg())
    }

    /// 分を引く。
    pub fn sub_minutes(self, minutes: i64) -> Self {
        self.add_minutes(minutes.saturating_neg())
    }

    /// 時間を引く。
    pub fn sub_hours(self, hours: i64) -> Self {
        self.add_hours(hours.saturating_neg())
    }

    /// 日を引く。
    pub fn sub_days(self, days: i64) -> Self {
        self.add_days(days.saturating_neg())
    }

    /// 月を足す。負なら引きます。
    ///
    /// **日が月末を超えたら月末に止めます。** 1 月 31 日 + 1 か月は 2 月 28 日です
    /// （Laravel の `addMonthsNoOverflow` と同じ）。
    pub fn add_months(self, months: i64) -> Self {
        let (year, month, day) = self.civil();
        let total = (year * 12 + i64::from(month) - 1).saturating_add(months);
        let year = total.div_euclid(12);
        let month = total.rem_euclid(12) as u32 + 1;
        if year < 1 {
            return Self::from_timestamp(MIN_SECONDS);
        }
        if year > 9999 {
            return Self::from_timestamp(MAX_SECONDS);
        }
        let day = day.min(time::days_in_month(year, month));
        let days = time::days_from_civil(year, month, day);
        Self::from_timestamp(days * 86_400 + self.second_of_day())
    }

    /// 月を引く。日が月末を超えたら月末に止めます。
    pub fn sub_months(self, months: i64) -> Self {
        self.add_months(months.saturating_neg())
    }

    /// 年を足す。`add_months(years * 12)` と同じです。
    pub fn add_years(self, years: i64) -> Self {
        self.add_months(years.saturating_mul(12))
    }

    /// 年を引く。
    pub fn sub_years(self, years: i64) -> Self {
        self.add_years(years.saturating_neg())
    }

    /// その日の `00:00:00`。
    pub fn start_of_day(self) -> Self {
        Self::from_timestamp(self.seconds - self.second_of_day())
    }

    /// その日の `23:59:59`。
    pub fn end_of_day(self) -> Self {
        Self::from_timestamp(self.seconds - self.second_of_day() + 86_399)
    }

    /// `other - self` の秒。`other` が後なら正です。
    pub fn diff_in_seconds(&self, other: &Carbon) -> i64 {
        other.seconds - self.seconds
    }

    /// `other - self` の分。端数は 0 に向けて切り捨てます。
    pub fn diff_in_minutes(&self, other: &Carbon) -> i64 {
        self.diff_in_seconds(other) / 60
    }

    /// `other - self` の時間。端数は 0 に向けて切り捨てます。
    pub fn diff_in_hours(&self, other: &Carbon) -> i64 {
        self.diff_in_seconds(other) / 3_600
    }

    /// `other - self` の日数。端数は 0 に向けて切り捨てます。
    pub fn diff_in_days(&self, other: &Carbon) -> i64 {
        self.diff_in_seconds(other) / 86_400
    }

    /// いまより前か。等しいときは偽です。
    pub fn is_past(&self) -> bool {
        *self < Self::now()
    }

    /// いまより後か。等しいときは偽です。
    pub fn is_future(&self) -> bool {
        *self > Self::now()
    }

    /// `YYYY-MM-DD HH:MM:SS`。
    pub fn to_date_time_string(&self) -> String {
        time::format_timestamp(self.seconds)
    }

    /// `YYYY-MM-DD`。
    pub fn to_date_string(&self) -> String {
        let (year, month, day) = self.civil();
        format!("{year:04}-{month:02}-{day:02}")
    }

    /// `HH:MM:SS`。
    pub fn to_time_string(&self) -> String {
        format!(
            "{:02}:{:02}:{:02}",
            self.hour(),
            self.minute(),
            self.second()
        )
    }

    /// `YYYY-MM-DDTHH:MM:SSZ`。
    pub fn to_iso8601_string(&self) -> String {
        format!("{}T{}Z", self.to_date_string(), self.to_time_string())
    }

    fn civil(&self) -> (i64, u32, u32) {
        time::civil_from_days(self.seconds.div_euclid(86_400))
    }

    fn second_of_day(&self) -> i64 {
        self.seconds.rem_euclid(86_400)
    }
}

/// 時刻の数字の後ろに付いてよいもの：空・`Z`・`.数字`・`.数字Z`。
fn is_allowed_suffix(rest: &str) -> bool {
    let rest = rest.strip_suffix('Z').unwrap_or(rest);
    if rest.is_empty() {
        return true;
    }
    match rest.strip_prefix('.') {
        Some(digits) => !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

impl fmt::Display for Carbon {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_date_time_string())
    }
}

impl fmt::Debug for Carbon {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Carbon({})", self.to_date_time_string())
    }
}

impl FromStr for Carbon {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        Self::parse(text)
    }
}

/// `String` の列だったときと同じ JSON（`"2026-11-20 09:30:00"`）になります。
impl serde::Serialize for Carbon {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_date_time_string())
    }
}

impl<'de> serde::Deserialize<'de> for Carbon {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

impl From<Carbon> for String {
    fn from(at: Carbon) -> Self {
        at.to_date_time_string()
    }
}

/// `#[derive(Model)]` が `Option<String>` の `created_at` / `updated_at` に入れるときに使います。
impl From<Carbon> for Option<String> {
    fn from(at: Carbon) -> Self {
        Some(at.to_date_time_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Carbon {
        Carbon::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    #[test]
    fn 決まった形を読める() {
        let base = at("2026-11-20 09:30:00");
        assert_eq!(base.to_date_time_string(), "2026-11-20 09:30:00");
        for text in [
            "2026-11-20T09:30:00",
            "2026-11-20T09:30:00Z",
            "2026-11-20 09:30:00.123",
            "  2026-11-20 09:30:00  ",
        ] {
            assert_eq!(at(text), base, "{text}");
        }
        assert_eq!(at("2026-11-20T09:30:00.5Z"), base);
        assert_eq!(
            at("2026-11-20").to_date_time_string(),
            "2026-11-20 00:00:00"
        );
    }

    #[test]
    fn 読めない形はエラーになる() {
        for text in [
            "2026-11-20T09:30:00+09:00",
            "2026-11-20 09:30:00 UTC",
            "2026-11-20T09:30:00-05:00",
            "2026-11-20 09:30:00.",
            "2026-11-20 09:30:00.Z",
            "2026-11-20 09:30:00ZZ",
            "2026-02-30",
            "0000-01-01",
            "10000-01-01",
            "2026-11-20T",
            "",
            // 桁はちょうど・符号なし・区切りは 3 つまで。
            "2026-1-20",
            "+2026-11-20",
            "2026-11-20 9:30:00",
            "2026-11-20 09:30:00:00",
            "2026-11-20 09",
        ] {
            assert!(Carbon::parse(text).is_err(), "{text}");
        }
        // 秒は省ける。
        assert_eq!(
            Carbon::parse("2026-11-20 09:30").unwrap(),
            at("2026-11-20 09:30:00")
        );
        let error = Carbon::parse("2026-02-30").unwrap_err().to_string();
        assert!(
            error.contains("`2026-02-30` は日時として読めません（YYYY-MM-DD か YYYY-MM-DD HH:MM:SS。UTC です）"),
            "{error}"
        );
    }

    #[test]
    fn 月の足し算は月末で止める() {
        for (from, months, want) in [
            ("2026-01-31 12:00:00", 1, "2026-02-28 12:00:00"),
            ("2026-01-31 12:00:00", 13, "2027-02-28 12:00:00"),
            ("2026-01-31 12:00:00", -2, "2025-11-30 12:00:00"),
            ("2024-03-31 00:00:00", -1, "2024-02-29 00:00:00"),
        ] {
            assert_eq!(at(from).add_months(months).to_date_time_string(), want);
            assert_eq!(at(from).sub_months(-months).to_date_time_string(), want);
        }
        assert_eq!(
            at("2024-02-29 08:00:00").add_years(1).to_date_time_string(),
            "2025-02-28 08:00:00"
        );
        assert_eq!(
            at("2024-02-29 08:00:00").sub_years(4).to_date_time_string(),
            "2020-02-29 08:00:00"
        );
    }

    #[test]
    fn 日と時が繰り上がる() {
        let base = at("2026-12-31 23:59:59");
        assert_eq!(
            base.add_seconds(1).to_date_time_string(),
            "2027-01-01 00:00:00"
        );
        assert_eq!(
            base.add_minutes(1).to_date_time_string(),
            "2027-01-01 00:00:59"
        );
        assert_eq!(
            base.add_hours(1).to_date_time_string(),
            "2027-01-01 00:59:59"
        );
        assert_eq!(
            base.add_days(1).to_date_time_string(),
            "2027-01-01 23:59:59"
        );
        assert_eq!(base.add_days(-1), base.sub_days(1));
        assert_eq!(
            at("2027-01-01 00:00:00").sub_seconds(1),
            base,
            "引くと繰り下がる"
        );
        assert_eq!(base.sub_minutes(60), base.sub_hours(1));
        assert_eq!(
            at("2024-02-28 12:00:00").add_days(1).to_date_string(),
            "2024-02-29"
        );
    }

    #[test]
    fn 日の始まりと終わり() {
        let base = at("2026-11-20 09:30:15");
        assert_eq!(
            base.start_of_day().to_date_time_string(),
            "2026-11-20 00:00:00"
        );
        assert_eq!(
            base.end_of_day().to_date_time_string(),
            "2026-11-20 23:59:59"
        );
        // 1970 年より前でも、その日の中に収まる。
        let old = at("1969-07-20 20:17:40");
        assert_eq!(
            old.start_of_day().to_date_time_string(),
            "1969-07-20 00:00:00"
        );
        assert_eq!(
            old.end_of_day().to_date_time_string(),
            "1969-07-20 23:59:59"
        );
    }

    #[test]
    fn 差は符号付きで0に向けて切り捨てる() {
        let a = at("2026-11-20 00:00:00");
        let b = at("2026-11-21 01:59:59");
        assert_eq!(a.diff_in_seconds(&b), 93_599);
        assert_eq!(a.diff_in_minutes(&b), 1_559);
        assert_eq!(a.diff_in_hours(&b), 25);
        assert_eq!(a.diff_in_days(&b), 1);
        assert_eq!(b.diff_in_seconds(&a), -93_599);
        assert_eq!(b.diff_in_minutes(&a), -1_559);
        assert_eq!(b.diff_in_hours(&a), -25);
        assert_eq!(b.diff_in_days(&a), -1);
        assert_eq!(a.diff_in_minutes(&a.add_seconds(59)), 0);
        assert_eq!(a.diff_in_minutes(&a.sub_seconds(59)), 0);
    }

    #[test]
    fn 範囲の端で止まる() {
        let min = Carbon::from_timestamp(i64::MIN);
        let max = Carbon::from_timestamp(i64::MAX);
        assert_eq!(min.to_date_time_string(), "0001-01-01 00:00:00");
        assert_eq!(max.to_date_time_string(), "9999-12-31 23:59:59");
        assert_eq!(max.add_seconds(i64::MAX), max);
        assert_eq!(max.add_days(i64::MAX), max);
        assert_eq!(min.sub_days(i64::MAX), min);
        assert_eq!(min.add_seconds(i64::MIN), min);
        assert_eq!(
            min.sub_seconds(i64::MIN),
            max,
            "i64::MIN を引くと飽和して足す"
        );
        assert_eq!(max.add_months(1), max);
        assert_eq!(max.add_months(i64::MAX), max);
        assert_eq!(min.sub_months(1), min);
        assert_eq!(min.add_months(i64::MIN), min);
        assert_eq!(min.add_years(i64::MAX), max);
        assert_eq!(max.sub_years(i64::MAX), min);
        assert_eq!(max.end_of_day(), max);
        assert_eq!(min.start_of_day(), min);
        assert_eq!(min.diff_in_seconds(&max), 253_402_300_799 + 62_135_596_800);
        // 端に寄せても月末の扱いは同じ。
        assert_eq!(
            at("9999-11-30 10:00:00")
                .add_months(1)
                .to_date_time_string(),
            "9999-12-30 10:00:00"
        );
    }

    #[test]
    fn 部分を取り出せる() {
        let base = at("2026-11-20 09:30:15");
        assert_eq!(base.year(), 2026);
        assert_eq!(base.month(), 11);
        assert_eq!(base.day(), 20);
        assert_eq!(base.hour(), 9);
        assert_eq!(base.minute(), 30);
        assert_eq!(base.second(), 15);
        assert_eq!(base.to_date_string(), "2026-11-20");
        assert_eq!(base.to_time_string(), "09:30:15");
        assert_eq!(base.to_iso8601_string(), "2026-11-20T09:30:15Z");
        assert_eq!(Carbon::parse(&base.to_iso8601_string()).unwrap(), base);
        assert_eq!(base.to_string(), "2026-11-20 09:30:15");
        assert_eq!(format!("{base:?}"), "Carbon(2026-11-20 09:30:15)");
        assert_eq!(Carbon::from_timestamp(base.timestamp()), base);
    }

    #[test]
    fn いまと比べられる() {
        let now = Carbon::now();
        assert!(now.sub_days(1).is_past());
        assert!(!now.sub_days(1).is_future());
        assert!(now.add_days(1).is_future());
        assert!(!now.add_days(1).is_past());
        assert!(now.year() >= 2026);
    }

    #[test]
    fn 文字列への変換は今のnowと同じ形() {
        for seconds in [0, 1_709_210_096, -62_135_596_800, 253_402_300_799] {
            let carbon = Carbon::from_timestamp(seconds);
            assert_eq!(String::from(carbon), time::format_timestamp(seconds));
            assert_eq!(
                Option::<String>::from(carbon),
                Some(time::format_timestamp(seconds))
            );
        }
    }

    #[test]
    fn valueと往復できる() {
        use crate::database::{FromValue, Value};

        let base = at("2026-11-20 09:30:00");
        let value = Value::from(base);
        assert_eq!(value, Value::Text("2026-11-20 09:30:00".into()));
        assert_eq!(Carbon::from_value(&value).unwrap(), base);
        assert_eq!(
            Carbon::from_value(&Value::Int(base.timestamp())).unwrap(),
            base
        );
        assert_eq!(
            Carbon::from_value(&Value::Int(i64::MAX)).unwrap(),
            Carbon::from_timestamp(i64::MAX),
            "整数も端に寄せる"
        );
        assert!(Carbon::from_value(&Value::Text("こわれた値".into())).is_err());
        for (other, kind) in [
            (Value::Float(1.5), "小数"),
            (Value::Bool(true), "真偽"),
            (Value::Bytes(vec![1]), "バイト列"),
            (Value::Null, "NULL"),
        ] {
            let error = Carbon::from_value(&other).unwrap_err().to_string();
            assert!(
                error.contains(&format!("{kind} の値を日時として読めません")),
                "{error}"
            );
        }

        assert_eq!(Option::<Carbon>::from_value(&Value::Null).unwrap(), None);
        assert_eq!(Option::<Carbon>::from_value(&value).unwrap(), Some(base));
        assert_eq!(Value::from(None::<Carbon>), Value::Null);
        assert_eq!(Value::from(Some(base)), value);
    }

    #[test]
    fn fromstrとserdeで往復できる() {
        let base: Carbon = "2026-11-20 09:30:00".parse().unwrap();
        assert_eq!(base, at("2026-11-20 09:30:00"));
        assert!("2026-11-20T09:30:00+09:00".parse::<Carbon>().is_err());

        let json = serde_json::to_string(&base).unwrap();
        assert_eq!(json, r#""2026-11-20 09:30:00""#);
        assert_eq!(serde_json::from_str::<Carbon>(&json).unwrap(), base);
        let error = serde_json::from_str::<Carbon>(r#""2026-02-30""#).unwrap_err();
        assert!(
            error.to_string().contains("日時として読めません"),
            "{error}"
        );
        assert!(serde_json::from_str::<Carbon>("5").is_err());
        let optional: Option<Carbon> = serde_json::from_str("null").unwrap();
        assert_eq!(optional, None);
    }
}
