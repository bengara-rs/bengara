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
//!
//! # 値の前後の空白は落とします
//!
//! 検査する前に、値の前後の空白を落とします（Laravel の `TrimStrings` と同じ）。
//! `age=" 5 "` は `5` として検査し、通った値も `5` で返ります。
//! 落とさないと、同じフォームが Laravel では通り、こちらでは 422 になります。
//! 間の空白はそのままです（`"a b"` は `"a b"`）。
//!
//! **落とさない項目が3つあります。** `current_password`・`password`・
//! `password_confirmation` です（Laravel の `TrimStrings` の `$except` と同じ）。
//! パスワードの前後の空白は本人が意図して入れていることがあり、落とすと
//! 資格情報を黙って書き換えてしまいます。`password` の `min:8` / `max:72` は、
//! 前後の空白も数えた長さで測ります。これも Laravel と同じです。

use std::collections::BTreeMap;

use crate::error::{Error, Result};

/// 検査を通った入力。
///
/// 中身は検査した項目だけです。規則を書かなかった項目は入りません。
/// 「要求した物しか入っていない」状態にして、入力をそのまま保存する事故を防ぎます。
///
/// **値は前後の空白を落とした形で入ります**（Laravel の `TrimStrings` と同じ）。
/// ただし `current_password`・`password`・`password_confirmation` は
/// 送られてきたままです。
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
            // 1 件だけなら、そのまま出す。
            Some(first) if self.total() == 1 => write!(f, "{first}"),
            Some(first) => write!(f, "{first}（ほか {} 件）", self.total() - 1),
            None => write!(f, "入力に誤りがあります。"),
        }
    }
}

impl ValidationErrors {
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
}

/// 使える規則の名前。知らない規則を断るときの案内に出します。
const RULE_NAMES: &str = "required, nullable, integer (int), numeric, boolean (bool), email, \
                          url, alpha, alpha_num, alpha_dash, min, max, size, between, in, \
                          confirmed, same, different, starts_with, ends_with";

impl Rule {
    /// `max:255` のような1語を規則にする。
    fn parse(raw: &str) -> Result<Self> {
        let (name, arg) = match raw.split_once(':') {
            Some((n, a)) => (n.trim(), Some(a.trim())),
            None => (raw.trim(), None),
        };

        // **有限の数だけを通します。** Rust の `parse::<f64>()` は `nan` と `inf` を
        // 読めるので、素のままだと `max:nan` が通り、比較が全部 false になって
        // **何も検査しなくなります**（`max:255` が黙って無制限になる形）。
        // 規則の書き間違いなので、引数忘れと同じ扱いで 500 にします。
        let number = |what: &str| -> Result<f64> {
            arg.and_then(|a| a.parse::<f64>().ok())
                .filter(|n| n.is_finite())
                .ok_or_else(|| {
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
            // `size` だけは文字数と突き合わせるので `usize` にします。
            // `as usize` は飽和変換なので、`size:-3` が `0`、`size:1e30` が
            // `usize::MAX` に化けます。書き間違いを黙って通さないよう、
            // 0 以上の整数であることを確かめます（引数忘れと同じ扱いで 500）。
            "size" => {
                let n = number("size")?;
                if !(n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n <= usize::MAX as f64) {
                    return Err(Error::msg(
                        "規則 `size` には 0 以上の整数が要ります（例: size:10）",
                    ));
                }
                Rule::Size(n as usize)
            }
            "between" => {
                let raw = text("between")?;
                let (lo, hi) = raw.split_once(',').ok_or_else(|| {
                    Error::msg("規則 `between` は between:1,10 の形で書いてください")
                })?;
                // 2 つの引数も `min` / `max` と同じで、有限の数だけを通します。
                let lo = lo
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite())
                    .ok_or_else(|| Error::msg("規則 `between` の下限が数値ではありません"))?;
                let hi = hi
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite())
                    .ok_or_else(|| Error::msg("規則 `between` の上限が数値ではありません"))?;
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
            // 知らない規則は**書き間違い**です。利用者に見せる 422 にはしません。
            // `min`（引数忘れ）と同じ扱いで、作った人へ 500 で知らせます。
            // 422 にすると、内部の規則名がそのまま画面に出ます。
            other => {
                return Err(Error::msg(format!(
                    "規則 `{other}` は bengara にありません。使えるのは {RULE_NAMES} です"
                )))
            }
        })
    }
}

/// 入力をひとまとめにしたもの（クエリ・フォーム・JSON のどれか）。
pub(crate) struct Input {
    values: BTreeMap<String, String>,
}

/// 前後の空白を**落とさない**項目の名前。
///
/// Laravel の `TrimStrings` の `$except` と同じ 3 つです。
/// パスワードの前後の空白は本人が意図して入れていることがあり、
/// 落とすと資格情報を黙って書き換えてしまいます。
///
/// **`http/request.rs` の `SENSITIVE` とは別の一覧です。** あちらは
/// `token` や `key` も含みますが、トークンは空白を落としても困りません。
/// 混ぜると意味が変わるので、分けたままにしてください。
const NO_TRIM: &[&str] = &["current_password", "password", "password_confirmation"];

/// その名前の値の空白を落とすか。
fn should_trim(field: &str) -> bool {
    !NO_TRIM.contains(&field)
}

impl Input {
    /// 入力を受け取る。**値の前後の空白はここで落とします。**
    ///
    /// Laravel が `TrimStrings` を標準で通すのと同じ形です。落とさないと、
    /// `age=" 5 "` が `integer` で落ち、`email="a@b.com "` が空白の確認で落ちます。
    /// 同じフォームが Laravel では通り、こちらでは 422 になっていました。
    /// 検査を通った値（`Validated`）も、空白を落とした形で返ります。
    ///
    /// **パスワードの類いは落としません**（[`NO_TRIM`] の 3 つ）。
    /// そのため `password` の `min:8` / `max:72` は、前後の空白も 1 文字として
    /// 数えた長さで測ります。Laravel も同じです。
    pub(crate) fn new(pairs: Vec<(String, String)>) -> Self {
        let mut values = BTreeMap::new();
        for (k, v) in pairs {
            // 同じ名前が何度も来たら最初を残す。後勝ちだと上書きで意図を変えられる。
            let trim = should_trim(&k);
            values
                .entry(k)
                .or_insert_with(|| if trim { v.trim().to_string() } else { v });
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
        // 値は `Input::new` で前後の空白を落としてある。
        // 以前はここだけ `trim()` していたので、`age=" 5 "` が `integer` で落ちた。
        let value = raw.unwrap_or("");
        let is_blank = value.is_empty();
        let nullable = parsed.contains(&Rule::Nullable);
        let required = parsed.contains(&Rule::Required);

        if is_blank && required {
            errors.add(field, format!("{field} は必ず入力してください。"));
            continue;
        }
        // 検査を飛ばすのは、**送られてこなかった**ときと `nullable` が付いているときだけ。
        // 空文字は飛ばしません（Laravel と同じ）。飛ばすと `in` や `email` が素通りします。
        if is_blank && (raw.is_none() || nullable) {
            // 送られてきた項目と `nullable` の項目だけ、空文字として入れておく。
            if raw.is_some() || nullable {
                out.insert(field.to_string(), String::new());
            }
            continue;
        }

        let before = errors.total();
        for rule in &parsed {
            check(rule, field, value, input, &parsed, &mut errors);
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

/// 1つの規則を当てる。
///
/// `rules` はその項目に付いた規則の一覧です。`min` などが「値で比べるか、文字数で比べるか」を
/// 決めるために要ります。
fn check(
    rule: &Rule,
    field: &str,
    value: &str,
    input: &Input,
    rules: &[Rule],
    errors: &mut ValidationErrors,
) {
    match rule {
        Rule::Required | Rule::Nullable => {}

        Rule::Integer => {
            if value.parse::<i64>().is_err() {
                errors.add(field, format!("{field} は整数で入力してください。"));
            }
        }
        Rule::Numeric => {
            // **有限の数として読めるか**で見る。`is_err()` だけだと
            // `inf` / `NaN` / `1e400` が通り、`max:1000` も素通りします。
            if !value.parse::<f64>().is_ok_and(f64::is_finite) {
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

        // `numeric` か `integer` が付いているときだけ値そのもの、
        // そうでなければ文字数で比べる（Laravel と同じ考え方）。
        Rule::Min(min) => match measure(value, rules) {
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
        Rule::Max(max) => match measure(value, rules) {
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
        Rule::Between(lo, hi) => match measure(value, rules) {
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
        // `size` も `min` / `max` と同じ決まりです。
        // `numeric` か `integer` が付いていれば値そのもの、付いていなければ文字数。
        // 数値として読めない値のときは何も言いません（`integer` が言います）。
        Rule::Size(size) => match measure(value, rules) {
            Measure::Number(n) if n != *size as f64 => {
                errors.add(field, format!("{field} は {size} にしてください。"));
            }
            Measure::Length(len) if len != *size => {
                errors.add(field, format!("{field} は {size} 文字で入力してください。"));
            }
            _ => {}
        },

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
    }
}

/// `min` / `max` / `between` が何と比べるか。
enum Measure {
    /// 値そのもの。
    Number(f64),
    /// 文字数。
    Length(usize),
    /// どちらでも測れない。**何も言いません。**
    ///
    /// `integer` / `numeric` が付いているのに数値として読めない場合です。
    /// 文字数へ逃がすと、`("n", "integer|min:5")` に `abc` を送ったときに
    /// 「整数で入力してください」と「3 文字以上で入力してください」の 2 件が出ます。
    /// 後者は意図と食い違うので、数値の規則だけに言わせます。
    Invalid,
}

/// 値で測るか、文字数で測るかを決める。
///
/// `numeric` か `integer` が付いているときだけ値で測ります（Laravel と同じ）。
/// 付いていない項目を値で測ると、`password=9` が `min:8` を通り、
/// `password=12345678` が `max:72` で落ちます。
///
/// 数値の規則が付いているのに読めない値のときは [`Measure::Invalid`] です。
/// **文字数へ逃がしません。**
fn measure(value: &str, rules: &[Rule]) -> Measure {
    let by_value = rules
        .iter()
        .any(|rule| matches!(rule, Rule::Numeric | Rule::Integer));
    if !by_value {
        return Measure::Length(value.chars().count());
    }
    match value.parse::<f64>().ok().filter(|n| n.is_finite()) {
        Some(n) => Measure::Number(n),
        None => Measure::Invalid,
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
    fn minとmaxはnumericかintegerが付いていれば値で見る() {
        // 値で比べる。
        assert!(!errors_of(&input(&[("n", "3")]), &[("n", "integer|min:5")]).is_empty());
        assert!(errors_of(&input(&[("n", "7")]), &[("n", "integer|min:5")]).is_empty());
        assert!(errors_of(&input(&[("n", "7.5")]), &[("n", "numeric|min:5")]).is_empty());
        // 付いていなければ文字数で比べる。
        assert!(errors_of(&input(&[("s", "abcdef")]), &[("s", "min:5")]).is_empty());
        assert!(!errors_of(&input(&[("s", "abc")]), &[("s", "min:5")]).is_empty());
        // 日本語は見た目の文字数で数える。
        assert!(errors_of(&input(&[("s", "あいう")]), &[("s", "max:3")]).is_empty());
        assert!(!errors_of(&input(&[("s", "あいうえ")]), &[("s", "max:3")]).is_empty());
    }

    #[test]
    fn 数値に見える文字列は文字数で測る() {
        // パスワードは文字数で測る。値で測ると 9 が min:8 を通ってしまう。
        assert!(
            !errors_of(
                &input(&[("password", "9")]),
                &[("password", "required|min:8|max:72")]
            )
            .is_empty(),
            "1 文字なので落ちる"
        );
        assert!(
            errors_of(
                &input(&[("password", "12345678")]),
                &[("password", "required|min:8|max:72")]
            )
            .is_empty(),
            "8 文字なので通る"
        );
        // 題名も同じ。桁数の多い数字でも 50 文字には届かない。
        assert!(errors_of(&input(&[("title", "9999999")]), &[("title", "max:50")]).is_empty());
        // integer が付いていれば値で測る。
        assert!(!errors_of(&input(&[("n", "5")]), &[("n", "integer|min:8")]).is_empty());
    }

    #[test]
    fn betweenとsize() {
        assert!(errors_of(&input(&[("n", "5")]), &[("n", "integer|between:1,10")]).is_empty());
        assert!(!errors_of(&input(&[("n", "11")]), &[("n", "integer|between:1,10")]).is_empty());
        // 規則が無ければ文字数で見る（`11` は 2 文字なので通る）。
        assert!(errors_of(&input(&[("s", "11")]), &[("s", "between:1,10")]).is_empty());
        assert!(errors_of(&input(&[("s", "abc")]), &[("s", "size:3")]).is_empty());
        assert!(!errors_of(&input(&[("s", "ab")]), &[("s", "size:3")]).is_empty());
    }

    #[test]
    fn sizeもnumericかintegerが付いていれば値で見る() {
        // 値で比べる。`5` は `size:5` を通る（文字数では 1 文字なので落ちていた）。
        assert!(errors_of(&input(&[("n", "5")]), &[("n", "integer|size:5")]).is_empty());
        assert!(!errors_of(&input(&[("n", "6")]), &[("n", "integer|size:5")]).is_empty());
        assert!(errors_of(&input(&[("n", "5.0")]), &[("n", "numeric|size:5")]).is_empty());

        // 数値として読めない値には何も言わない（`integer` の 1 件だけ）。
        let e = errors_of(&input(&[("n", "abc")]), &[("n", "integer|size:5")]);
        assert_eq!(e.total(), 1, "理由は 1 件だけ: {:?}", e.all());

        // 規則が無ければ文字数で見る（従来どおり）。
        assert!(errors_of(&input(&[("s", "abcde")]), &[("s", "size:5")]).is_empty());
        assert!(!errors_of(&input(&[("s", "abcd")]), &[("s", "size:5")]).is_empty());
    }

    #[test]
    fn sizeの書き間違いはエラーになる() {
        // `as usize` の飽和変換で黙って `0` や `usize::MAX` にしない。
        let i = input(&[("n", "1")]);
        for bad in ["size:-3", "size:1e30", "size:1.5", "size:abc", "size"] {
            let error = validate(&i, &[("n", bad)]).unwrap_err();
            assert!(
                !matches!(error, Error::Validation(_)),
                "`{bad}` は 422 にしない: {error}"
            );
        }
        // 正しい書き方は通る。
        assert!(validate(&i, &[("n", "size:1")]).is_ok());
        assert!(validate(&input(&[("n", "")]), &[("n", "size:0")]).is_ok());
    }

    #[test]
    fn 空文字にも規則を当てる() {
        // 空文字は「送られてきた値」なので検査する。
        assert!(!errors_of(&input(&[("role", "")]), &[("role", "in:admin,user")]).is_empty());
        assert!(!errors_of(&input(&[("email", "")]), &[("email", "email")]).is_empty());
        // nullable が付いていれば飛ばす。
        assert!(errors_of(&input(&[("age", "")]), &[("age", "nullable|integer")]).is_empty());
        // 送られてこなければ、nullable が無くても飛ばす（従来どおり）。
        assert!(errors_of(&input(&[]), &[("role", "in:admin,user")]).is_empty());
        let ok = validate(&input(&[]), &[("role", "in:admin,user")]).unwrap();
        assert!(!ok.has("role"), "送られてこなかった項目は入らない");
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
        let i = input(&[("e", "これはメールではない"), ("e_x", "")]);
        let e = errors_of(&i, &[("e", "email|starts_with:zz")]);
        assert_eq!(
            e.get("e").len(),
            2,
            "メールの形でない、かつ zz で始まらない"
        );
        assert_eq!(e.len(), 1, "項目は1つ");
        assert_eq!(e.total(), 2);
    }

    #[test]
    fn 数値として読めなければ長さの規則は何も言わない() {
        // 以前は「整数で入力してください」と「5 文字以上で入力してください」の
        // 2 件が出ていた。後者は文字数の話で、`integer|min:5` の意図と食い違う。
        let e = errors_of(&input(&[("n", "abc")]), &[("n", "integer|min:5")]);
        assert_eq!(e.get("n").len(), 1, "{:?}", e.get("n"));
        assert!(e.get("n")[0].contains("整数で入力してください"));

        // `max` と `between` も同じ。
        let e = errors_of(
            &input(&[("n", "abc")]),
            &[("n", "numeric|max:5|between:1,3")],
        );
        assert_eq!(e.get("n").len(), 1, "{:?}", e.get("n"));
        assert!(e.get("n")[0].contains("数値で入力してください"));
    }

    #[test]
    fn numericは無限とnanを通さない() {
        // `parse::<f64>()` は `inf` / `NaN` / `1e400` を読めてしまう。
        // 通すと、そのあと文字数比較に落ちて `max:1000` まで素通りしていた。
        for bad in ["inf", "-inf", "infinity", "NaN", "nan", "1e400", "-1e400"] {
            let e = errors_of(&input(&[("n", bad)]), &[("n", "numeric|min:0|max:1000")]);
            assert!(!e.is_empty(), "`{bad}` は弾くはず");
            assert!(
                e.get("n")[0].contains("数値で入力してください"),
                "`{bad}`: {:?}",
                e.get("n")
            );
        }
        // 普通の数は通る。
        assert!(errors_of(&input(&[("n", "1e3")]), &[("n", "numeric|max:1000")]).is_empty());
    }

    #[test]
    fn 複数の項目が名前の順に並ぶ() {
        let e = errors_of(&input(&[]), &[("zebra", "required"), ("apple", "required")]);
        let names: Vec<&String> = e.all().keys().collect();
        assert_eq!(names, vec!["apple", "zebra"], "名前の順にそろえる");
    }

    #[test]
    fn 知らない規則は開発者向けのエラーになる() {
        // 書き間違いなので、利用者向けの 422 にはしない（`min` の引数忘れと同じ扱い）。
        let i = input(&[("s", "x")]);
        let error = validate(&i, &[("s", "requiredd")]).unwrap_err();
        assert!(
            !matches!(error, Error::Validation(_)),
            "422 にはしない: {error}"
        );
        let text = error.to_string();
        assert!(
            text.contains("`requiredd` は bengara にありません"),
            "{text}"
        );
        assert!(text.contains("starts_with"), "使える規則を並べる: {text}");
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
    fn 引数の非有限な値は開発者向けのエラーになる() {
        // `parse::<f64>()` は `nan` / `inf` を読めるので、素のままだと
        // `max:nan` が通り、比較が全部 false になって**何も検査しなくなる**。
        let i = input(&[("n", "999999")]);
        for bad in [
            "max:nan",
            "min:nan",
            "max:inf",
            "min:-inf",
            "max:infinity",
            "max:1e400",
            "between:nan,10",
            "between:1,inf",
            "size:nan",
        ] {
            let error = validate(&i, &[("n", bad)]).unwrap_err();
            assert!(
                !matches!(error, Error::Validation(_)),
                "`{bad}` は 422 にせず 500 にする: {error}"
            );
        }

        // まともな書き方は今までどおり通る。
        assert!(validate(&input(&[("n", "5")]), &[("n", "max:10")]).is_ok());
        assert!(validate(&input(&[("n", "5")]), &[("n", "between:1,10")]).is_ok());
    }

    #[test]
    fn 値の前後の空白は落とす() {
        // Laravel は `TrimStrings` を標準で通す。落とさないと、同じフォームが
        // 向こうで通ってこちらで 422 になっていた。
        let ok = validate(&input(&[("age", " 5 ")]), &[("age", "integer")]).unwrap();
        assert_eq!(ok.get("age"), "5", "通った値も空白を落とす");

        let ok = validate(
            &input(&[("email", " a@b.com ")]),
            &[("email", "required|email")],
        )
        .unwrap();
        assert_eq!(ok.get("email"), "a@b.com");

        // 間の空白はそのまま。
        let ok = validate(&input(&[("s", " a b ")]), &[("s", "required")]).unwrap();
        assert_eq!(ok.get("s"), "a b");

        // 文字数も落としたあとで数える。
        assert!(errors_of(&input(&[("s", " ab ")]), &[("s", "size:4")]).has("s"));
        assert!(errors_of(&input(&[("s", " ab ")]), &[("s", "size:2")]).is_empty());
    }

    #[test]
    fn パスワードの空白は落とさない() {
        // Laravel の `TrimStrings` も、この 3 つは `$except` に入れている。
        // 前後の空白は本人が意図して入れていることがあり、落とすと
        // 資格情報を黙って書き換えてしまう。
        for field in ["current_password", "password", "password_confirmation"] {
            let ok = validate(&input(&[(field, " ひみつ ")]), &[(field, "required")]).unwrap();
            assert_eq!(ok.get(field), " ひみつ ", "{field} はそのまま");
        }

        // 確認の一致も、送られてきたままで比べる。
        let i = input(&[
            ("password", " ひみつ "),
            ("password_confirmation", " ひみつ "),
        ]);
        assert!(errors_of(&i, &[("password", "confirmed")]).is_empty());

        // 長さは前後の空白も数える（Laravel と同じ）。
        let ok = validate(
            &input(&[("password", " 1234567 ")]),
            &[("password", "required|min:8|max:72")],
        )
        .unwrap();
        assert_eq!(ok.get("password"), " 1234567 ", "9 文字として通る");
        assert!(
            errors_of(
                &input(&[("password", "1234567")]),
                &[("password", "required|min:8|max:72")]
            )
            .has("password"),
            "空白が無ければ 7 文字で落ちる"
        );

        // ほかの項目は従来どおり落とす。
        let ok = validate(&input(&[("age", " 5 ")]), &[("age", "integer")]).unwrap();
        assert_eq!(ok.get("age"), "5");
        // 似た名前は落とす対象外にしない（一覧は完全一致）。
        let ok = validate(
            &input(&[("password1", " x ")]),
            &[("password1", "required")],
        )
        .unwrap();
        assert_eq!(ok.get("password1"), "x");
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

        // 同じ項目に 2 件でも「ほか 1 件」になる（総数で数える）。
        let mut e = ValidationErrors::default();
        e.add("a", "1 つめ。");
        e.add("a", "2 つめ。");
        assert_eq!(e.len(), 1, "項目は 1 つ");
        assert_eq!(e.to_string(), "1 つめ。（ほか 1 件）");
    }
}
