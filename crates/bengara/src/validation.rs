//! 入力の検査。Laravel の Validation に当たります。
//!
//! ```ignore
//! let input = req.validate(&[
//!     ("title", "required|max:255"),
//!     ("email", "required|email"),
//!     ("age",   "nullable|integer|min:0"),
//! ])?;
//! let title = input.get("title");
//! ```
//!
//! 失敗すると `Error::Validation` になり、422 と、どの項目がなぜ駄目かの JSON を返します。

use std::collections::BTreeMap;

use crate::error::{Error, Result};

/// 検査を通った入力。
///
/// 中身は検査した項目だけです。規則を書かなかった項目は入りません。
/// 「要求した物しか入っていない」状態にして、入力をそのまま保存する事故を防ぎます。
#[derive(Debug, Clone, Default)]
pub struct Validated {
    values: BTreeMap<String, String>,
}

impl Validated {
    /// 値を取り出す。無ければ空文字。
    ///
    /// `nullable` を付けた項目は、送られてこなければ空文字になります。
    pub fn get(&self, field: &str) -> &str {
        self.values.get(field).map(String::as_str).unwrap_or("")
    }

    /// 値を取り出す。送られてこなかったときと区別したいとき。
    pub fn try_get(&self, field: &str) -> Option<&str> {
        self.values.get(field).map(String::as_str)
    }

    /// 型を変えて取り出す。`integer` などを通した後に使います。
    pub fn get_as<T: std::str::FromStr>(&self, field: &str) -> Result<T> {
        self.get(field)
            .parse()
            .map_err(|_| Error::msg(format!("`{field}` を目的の型に変換できませんでした")))
    }

    /// 全部の組。
    pub fn all(&self) -> &BTreeMap<String, String> {
        &self.values
    }

    /// 項目があるか。
    pub fn has(&self, field: &str) -> bool {
        self.values.contains_key(field)
    }
}

/// どの項目がなぜ駄目だったか。
///
/// 1つの項目に複数の理由が付きます。項目の名前の順に並びます。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationErrors {
    errors: BTreeMap<String, Vec<String>>,
}

impl ValidationErrors {
    /// 1件足す。
    pub fn add(&mut self, field: &str, message: impl Into<String>) {
        self.errors
            .entry(field.to_string())
            .or_default()
            .push(message.into());
    }

    /// 1件もないか。
    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    /// 項目の数。
    pub fn len(&self) -> usize {
        self.errors.len()
    }

    /// ある項目の理由。
    pub fn get(&self, field: &str) -> &[String] {
        self.errors.get(field).map(Vec::as_slice).unwrap_or(&[])
    }

    /// ある項目に理由が付いているか。
    pub fn has(&self, field: &str) -> bool {
        self.errors.contains_key(field)
    }

    /// 全部の組。
    pub fn all(&self) -> &BTreeMap<String, Vec<String>> {
        &self.errors
    }

    /// 最初の1件（画面に1つだけ出したいとき）。
    pub fn first(&self) -> Option<&str> {
        self.errors
            .values()
            .next()
            .and_then(|list| list.first())
            .map(String::as_str)
    }

    /// Laravel と同じ形の JSON にする。
    ///
    /// ```json
    /// { "message": "入力に誤りがあります。", "errors": { "title": ["title は必ず入力してください。"] } }
    /// ```
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "message": "入力に誤りがあります。",
            "errors": self.errors,
        })
    }
}

impl std::fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.first() {
            Some(first) if self.errors.len() == 1 && self.get_first_count() == 1 => {
                write!(f, "{first}")
            }
            Some(first) => write!(f, "{first}（ほか {} 件）", self.total() - 1),
            None => write!(f, "入力に誤りがあります。"),
        }
    }
}

impl ValidationErrors {
    fn get_first_count(&self) -> usize {
        self.errors.values().next().map(Vec::len).unwrap_or(0)
    }

    /// 理由の総数（項目ごとではなく1件ずつ数える）。
    pub fn total(&self) -> usize {
        self.errors.values().map(Vec::len).sum()
    }
}

/// 1つの規則。
///
/// `f64` を持つので `Eq` は付けません（`PartialEq` だけで足ります）。
#[derive(Debug, Clone, PartialEq)]
enum Rule {
    Required,
    Nullable,
    Integer,
    Numeric,
    Boolean,
    Email,
    Url,
    Alpha,
    AlphaNum,
    AlphaDash,
    Min(f64),
    Max(f64),
    Between(f64, f64),
    Size(usize),
    In(Vec<String>),
    Confirmed,
    Same(String),
    Different(String),
    StartsWith(String),
    EndsWith(String),
    Regexless(String),
}

impl Rule {
    /// `max:255` のような1語を規則にする。
    fn parse(raw: &str) -> Result<Self> {
        let (name, arg) = match raw.split_once(':') {
            Some((n, a)) => (n.trim(), Some(a.trim())),
            None => (raw.trim(), None),
        };

        let number = |what: &str| -> Result<f64> {
            arg.and_then(|a| a.parse::<f64>().ok()).ok_or_else(|| {
                Error::msg(format!("規則 `{what}` には数値が要ります（例: {what}:10）"))
            })
        };
        let text = |what: &str| -> Result<String> {
            arg.map(str::to_string)
                .filter(|a| !a.is_empty())
                .ok_or_else(|| {
                    Error::msg(format!("規則 `{what}` には値が要ります（例: {what}:abc）"))
                })
        };

        Ok(match name {
            "required" => Rule::Required,
            "nullable" => Rule::Nullable,
            "integer" | "int" => Rule::Integer,
            "numeric" => Rule::Numeric,
            "boolean" | "bool" => Rule::Boolean,
            "email" => Rule::Email,
            "url" => Rule::Url,
            "alpha" => Rule::Alpha,
            "alpha_num" => Rule::AlphaNum,
            "alpha_dash" => Rule::AlphaDash,
            "min" => Rule::Min(number("min")?),
            "max" => Rule::Max(number("max")?),
            "size" => Rule::Size(number("size")? as usize),
            "between" => {
                let raw = text("between")?;
                let (lo, hi) = raw.split_once(',').ok_or_else(|| {
                    Error::msg("規則 `between` は between:1,10 の形で書いてください")
                })?;
                let lo = lo
                    .trim()
                    .parse::<f64>()
                    .map_err(|_| Error::msg("規則 `between` の下限が数値ではありません"))?;
                let hi = hi
                    .trim()
                    .parse::<f64>()
                    .map_err(|_| Error::msg("規則 `between` の上限が数値ではありません"))?;
                if lo > hi {
                    return Err(Error::msg("規則 `between` の下限が上限より大きいです"));
                }
                Rule::Between(lo, hi)
            }
            "in" => Rule::In(
                text("in")?
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .collect(),
            ),
            "confirmed" => Rule::Confirmed,
            "same" => Rule::Same(text("same")?),
            "different" => Rule::Different(text("different")?),
            "starts_with" => Rule::StartsWith(text("starts_with")?),
            "ends_with" => Rule::EndsWith(text("ends_with")?),
            other => Rule::Regexless(other.to_string()),
        })
    }
}

/// 入力をひとまとめにしたもの（クエリ・フォーム・JSON のどれか）。
pub(crate) struct Input {
    values: BTreeMap<String, String>,
}

impl Input {
    pub(crate) fn new(pairs: Vec<(String, String)>) -> Self {
        let mut values = BTreeMap::new();
        for (k, v) in pairs {
            // 同じ名前が何度も来たら最初を残す。後勝ちだと上書きで意図を変えられる。
            values.entry(k).or_insert(v);
        }
        Self { values }
    }

    fn get(&self, field: &str) -> Option<&str> {
        self.values.get(field).map(String::as_str)
    }
}

/// 規則にそって入力を調べる。
pub(crate) fn validate(input: &Input, rules: &[(&str, &str)]) -> Result<Validated> {
    let mut errors = ValidationErrors::default();
    let mut out = BTreeMap::new();

    for (field, spec) in rules {
        let parsed: Vec<Rule> = spec
            .split('|')
            .filter(|s| !s.trim().is_empty())
            .map(Rule::parse)
            .collect::<Result<_>>()?;

        let raw = input.get(field);
        let value = raw.unwrap_or("");
        let is_blank = value.trim().is_empty();
        let nullable = parsed.contains(&Rule::Nullable);
        let required = parsed.contains(&Rule::Required);

        if is_blank {
            if required {
                errors.add(field, format!("{field} は必ず入力してください。"));
                continue;
            }
            // 空でよい項目は、ここで打ち切る。空文字に長さや型の規則を当てない。
            if nullable || raw.is_none() {
                if raw.is_some() || nullable {
                    out.insert(field.to_string(), String::new());
                }
                continue;
            }
            out.insert(field.to_string(), String::new());
            continue;
        }

        let before = errors.total();
        for rule in &parsed {
            check(rule, field, value, input, &mut errors);
        }
        if errors.total() == before {
            out.insert(field.to_string(), value.to_string());
        }
    }

    if errors.is_empty() {
        Ok(Validated { values: out })
    } else {
        Err(Error::Validation(Box::new(errors)))
    }
}

fn check(rule: &Rule, field: &str, value: &str, input: &Input, errors: &mut ValidationErrors) {
    match rule {
        Rule::Required | Rule::Nullable => {}

        Rule::Integer => {
            if value.parse::<i64>().is_err() {
                errors.add(field, format!("{field} は整数で入力してください。"));
            }
        }
        Rule::Numeric => {
            if value.parse::<f64>().is_err() {
                errors.add(field, format!("{field} は数値で入力してください。"));
            }
        }
        Rule::Boolean => {
            if !matches!(
                value,
                "1" | "0" | "true" | "false" | "on" | "off" | "yes" | "no"
            ) {
                errors.add(
                    field,
                    format!("{field} は true か false で入力してください。"),
                );
            }
        }
        Rule::Email => {
            if !looks_like_email(value) {
                errors.add(
                    field,
                    format!("{field} はメールアドレスの形で入力してください。"),
                );
            }
        }
        Rule::Url => {
            if !(value.starts_with("http://") || value.starts_with("https://"))
                || value.len() < http_prefix_len(value) + 1
            {
                errors.add(
                    field,
                    format!("{field} は http:// か https:// で始まる URL を入力してください。"),
                );
            }
        }
        Rule::Alpha => {
            if !value.chars().all(char::is_alphabetic) {
                errors.add(field, format!("{field} は文字だけで入力してください。"));
            }
        }
        Rule::AlphaNum => {
            if !value.chars().all(char::is_alphanumeric) {
                errors.add(
                    field,
                    format!("{field} は文字と数字だけで入力してください。"),
                );
            }
        }
        Rule::AlphaDash => {
            if !value
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                errors.add(
                    field,
                    format!("{field} は文字・数字・ハイフン・下線だけで入力してください。"),
                );
            }
        }

        // 数値なら値そのもの、そうでなければ文字数で比べる（Laravel と同じ考え方）。
        Rule::Min(min) => match measure(value) {
            Measure::Number(n) if n < *min => {
                errors.add(
                    field,
                    format!("{field} は {} 以上にしてください。", trim_num(*min)),
                );
            }
            Measure::Length(len) if (len as f64) < *min => {
                errors.add(
                    field,
                    format!("{field} は {} 文字以上で入力してください。", trim_num(*min)),
                );
            }
            _ => {}
        },
        Rule::Max(max) => match measure(value) {
            Measure::Number(n) if n > *max => {
                errors.add(
                    field,
                    format!("{field} は {} 以下にしてください。", trim_num(*max)),
                );
            }
            Measure::Length(len) if (len as f64) > *max => {
                errors.add(
                    field,
                    format!("{field} は {} 文字以内で入力してください。", trim_num(*max)),
                );
            }
            _ => {}
        },
        Rule::Between(lo, hi) => match measure(value) {
            Measure::Number(n) if n < *lo || n > *hi => {
                errors.add(
                    field,
                    format!(
                        "{field} は {} から {} の間にしてください。",
                        trim_num(*lo),
                        trim_num(*hi)
                    ),
                );
            }
            Measure::Length(len) if (len as f64) < *lo || (len as f64) > *hi => {
                errors.add(
                    field,
                    format!(
                        "{field} は {} 文字から {} 文字で入力してください。",
                        trim_num(*lo),
                        trim_num(*hi)
                    ),
                );
            }
            _ => {}
        },
        Rule::Size(size) => {
            if value.chars().count() != *size {
                errors.add(field, format!("{field} は {size} 文字で入力してください。"));
            }
        }

        Rule::In(allowed) => {
            if !allowed.iter().any(|a| a == value) {
                errors.add(
                    field,
                    format!(
                        "{field} は {} のどれかにしてください。",
                        allowed.join(" / ")
                    ),
                );
            }
        }
        Rule::Confirmed => {
            let other = format!("{field}_confirmation");
            if input.get(&other).unwrap_or("") != value {
                errors.add(field, format!("{field} と {other} が一致しません。"));
            }
        }
        Rule::Same(other) => {
            if input.get(other).unwrap_or("") != value {
                errors.add(field, format!("{field} と {other} が一致しません。"));
            }
        }
        Rule::Different(other) => {
            if input.get(other).unwrap_or("") == value {
                errors.add(
                    field,
                    format!("{field} と {other} には違う値を入れてください。"),
                );
            }
        }
        Rule::StartsWith(prefix) => {
            if !value.starts_with(prefix.as_str()) {
                errors.add(field, format!("{field} は {prefix} で始めてください。"));
            }
        }
        Rule::EndsWith(suffix) => {
            if !value.ends_with(suffix.as_str()) {
                errors.add(field, format!("{field} は {suffix} で終えてください。"));
            }
        }

        // 知らない規則は、黙って通さずに作った人へ知らせる。
        Rule::Regexless(name) => {
            errors.add(field, format!("規則 `{name}` は bengara にありません。"));
        }
    }
}

enum Measure {
    Number(f64),
    Length(usize),
}

/// 数値として読めれば値、読めなければ文字数で測る。
fn measure(value: &str) -> Measure {
    match value.parse::<f64>() {
        Ok(n) if n.is_finite() => Measure::Number(n),
        _ => Measure::Length(value.chars().count()),
    }
}

/// `10` を `10`、`10.5` を `10.5` と出す（`10.0` にしない）。
fn trim_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn http_prefix_len(value: &str) -> usize {
    if value.starts_with("https://") {
        8
    } else {
        7
    }
}

/// メールアドレスらしい形か。
///
/// 完全な判定はしません（RFC どおりに書くと、実在しないものまで通ります）。
/// `@` が1つあり、前後が空でなく、後ろに `.` があることだけを見ます。
fn looks_like_email(value: &str) -> bool {
    if value.contains(char::is_whitespace) {
        return false;
    }
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    if local.is_empty() || domain.contains('@') {
        return false;
    }
    let Some((host, tld)) = domain.rsplit_once('.') else {
        return false;
    };
    !host.is_empty() && tld.len() >= 2 && !domain.starts_with('.') && !domain.ends_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(pairs: &[(&str, &str)]) -> Input {
        Input::new(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    fn errors_of(input: &Input, rules: &[(&str, &str)]) -> ValidationErrors {
        match validate(input, rules) {
            Ok(_) => ValidationErrors::default(),
            Err(Error::Validation(e)) => *e,
            Err(other) => panic!("別のエラーになった: {other}"),
        }
    }

    #[test]
    fn 通れば検査した項目だけが入る() {
        let i = input(&[("title", "こんにちは"), ("extra", "よけいな値")]);
        let ok = validate(&i, &[("title", "required|max:10")]).unwrap();
        assert_eq!(ok.get("title"), "こんにちは");
        assert!(!ok.has("extra"), "規則を書いていない項目は入らない");
        assert_eq!(ok.all().len(), 1);
    }

    #[test]
    fn requiredは空を弾く() {
        for value in ["", "   ", "\t"] {
            let e = errors_of(&input(&[("name", value)]), &[("name", "required")]);
            assert!(e.has("name"), "`{value}` は空として扱う");
        }
        // そもそも送られてこない場合も同じ。
        let e = errors_of(&input(&[]), &[("name", "required")]);
        assert_eq!(e.get("name").len(), 1);
    }

    #[test]
    fn nullableなら空でも通り他の規則は当たらない() {
        let ok = validate(&input(&[("age", "")]), &[("age", "nullable|integer")]).unwrap();
        assert_eq!(ok.get("age"), "");
        assert!(ok.has("age"));
    }

    #[test]
    fn 数値の規則() {
        assert!(errors_of(&input(&[("n", "12")]), &[("n", "integer")]).is_empty());
        assert!(errors_of(&input(&[("n", "-3")]), &[("n", "integer")]).is_empty());
        assert!(!errors_of(&input(&[("n", "1.5")]), &[("n", "integer")]).is_empty());
        assert!(!errors_of(&input(&[("n", "abc")]), &[("n", "integer")]).is_empty());
        assert!(errors_of(&input(&[("n", "1.5")]), &[("n", "numeric")]).is_empty());
    }

    #[test]
    fn minとmaxは数値なら値文字なら長さで見る() {
        // 数値として読めるので値で比べる。
        assert!(!errors_of(&input(&[("n", "3")]), &[("n", "min:5")]).is_empty());
        assert!(errors_of(&input(&[("n", "7")]), &[("n", "min:5")]).is_empty());
        // 数値として読めないので文字数で比べる。
        assert!(errors_of(&input(&[("s", "abcdef")]), &[("s", "min:5")]).is_empty());
        assert!(!errors_of(&input(&[("s", "abc")]), &[("s", "min:5")]).is_empty());
        // 日本語は見た目の文字数で数える。
        assert!(errors_of(&input(&[("s", "あいう")]), &[("s", "max:3")]).is_empty());
        assert!(!errors_of(&input(&[("s", "あいうえ")]), &[("s", "max:3")]).is_empty());
    }

    #[test]
    fn betweenとsize() {
        assert!(errors_of(&input(&[("n", "5")]), &[("n", "between:1,10")]).is_empty());
        assert!(!errors_of(&input(&[("n", "11")]), &[("n", "between:1,10")]).is_empty());
        assert!(errors_of(&input(&[("s", "abc")]), &[("s", "size:3")]).is_empty());
        assert!(!errors_of(&input(&[("s", "ab")]), &[("s", "size:3")]).is_empty());
    }

    #[test]
    fn メールアドレスの形() {
        for ok in ["a@example.com", "a.b+c@sub.example.co.jp"] {
            assert!(
                errors_of(&input(&[("e", ok)]), &[("e", "email")]).is_empty(),
                "{ok} は通るはず"
            );
        }
        for ng in ["a", "a@", "@b.com", "a@b", "a b@c.com", "a@@b.com", "a@b."] {
            assert!(
                !errors_of(&input(&[("e", ng)]), &[("e", "email")]).is_empty(),
                "{ng} は弾くはず"
            );
        }
    }

    #[test]
    fn urlの形() {
        assert!(errors_of(&input(&[("u", "https://example.com")]), &[("u", "url")]).is_empty());
        assert!(!errors_of(&input(&[("u", "example.com")]), &[("u", "url")]).is_empty());
        assert!(!errors_of(&input(&[("u", "https://")]), &[("u", "url")]).is_empty());
    }

    #[test]
    fn 文字種の規則() {
        assert!(errors_of(&input(&[("s", "abc")]), &[("s", "alpha")]).is_empty());
        assert!(!errors_of(&input(&[("s", "ab1")]), &[("s", "alpha")]).is_empty());
        assert!(errors_of(&input(&[("s", "ab1")]), &[("s", "alpha_num")]).is_empty());
        assert!(errors_of(&input(&[("s", "a-b_1")]), &[("s", "alpha_dash")]).is_empty());
        assert!(!errors_of(&input(&[("s", "a b")]), &[("s", "alpha_dash")]).is_empty());
    }

    #[test]
    fn inと前後一致() {
        assert!(errors_of(&input(&[("s", "b")]), &[("s", "in:a,b,c")]).is_empty());
        assert!(!errors_of(&input(&[("s", "d")]), &[("s", "in:a,b,c")]).is_empty());
        assert!(errors_of(&input(&[("s", "abc")]), &[("s", "starts_with:ab")]).is_empty());
        assert!(errors_of(&input(&[("s", "abc")]), &[("s", "ends_with:bc")]).is_empty());
        assert!(!errors_of(&input(&[("s", "abc")]), &[("s", "starts_with:zz")]).is_empty());
    }

    #[test]
    fn 項目どうしの比較() {
        let i = input(&[("password", "secret"), ("password_confirmation", "secret")]);
        assert!(errors_of(&i, &[("password", "confirmed")]).is_empty());

        let i = input(&[("password", "secret"), ("password_confirmation", "typo")]);
        assert!(!errors_of(&i, &[("password", "confirmed")]).is_empty());

        let i = input(&[("a", "x"), ("b", "x")]);
        assert!(errors_of(&i, &[("a", "same:b")]).is_empty());
        assert!(!errors_of(&i, &[("a", "different:b")]).is_empty());
    }

    #[test]
    fn 複数の理由がまとまる() {
        let e = errors_of(&input(&[("n", "abc")]), &[("n", "integer|min:5")]);
        assert_eq!(e.get("n").len(), 2, "整数でない、かつ短い");
        assert_eq!(e.len(), 1, "項目は1つ");
        assert_eq!(e.total(), 2);
    }

    #[test]
    fn 複数の項目が名前の順に並ぶ() {
        let e = errors_of(&input(&[]), &[("zebra", "required"), ("apple", "required")]);
        let names: Vec<&String> = e.all().keys().collect();
        assert_eq!(names, vec!["apple", "zebra"], "名前の順にそろえる");
    }

    #[test]
    fn 知らない規則は黙って通さない() {
        let e = errors_of(&input(&[("s", "x")]), &[("s", "unknown_rule")]);
        assert!(e.get("s")[0].contains("bengara にありません"));
    }

    #[test]
    fn 規則の書き方が違えばエラーになる() {
        let i = input(&[("n", "1")]);
        assert!(validate(&i, &[("n", "min")]).is_err(), "min には数値が要る");
        assert!(
            validate(&i, &[("n", "between:5")]).is_err(),
            "between は 2 つ"
        );
        assert!(
            validate(&i, &[("n", "between:10,1")]).is_err(),
            "下限 > 上限"
        );
    }

    #[test]
    fn jsonの形がlaravelとそろう() {
        let e = errors_of(&input(&[]), &[("title", "required")]);
        let json = e.to_json();
        assert_eq!(json["message"], "入力に誤りがあります。");
        assert_eq!(json["errors"]["title"][0], "title は必ず入力してください。");
    }

    #[test]
    fn 同じ名前が2回来たら最初を使う() {
        let i = Input::new(vec![
            ("a".to_string(), "first".to_string()),
            ("a".to_string(), "second".to_string()),
        ]);
        let ok = validate(&i, &[("a", "required")]).unwrap();
        assert_eq!(ok.get("a"), "first");
    }

    #[test]
    fn 型を変えて取り出せる() {
        let ok = validate(&input(&[("n", "42")]), &[("n", "integer")]).unwrap();
        assert_eq!(ok.get_as::<i64>("n").unwrap(), 42);
        assert!(ok.get_as::<i64>("missing").is_err());
    }

    #[test]
    fn 表示用の文字列() {
        let mut e = ValidationErrors::default();
        e.add("a", "A が駄目です。");
        assert_eq!(e.to_string(), "A が駄目です。");
        e.add("b", "B も駄目です。");
        assert_eq!(e.to_string(), "A が駄目です。（ほか 1 件）");
    }
}
