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
//! 規則を [`rules!`](crate::rules) で書くと、書き間違いをコンパイルのときに止めます。
//! 規則と読み先の型をまとめるときは [`FormRequest`] と `req.validate_as()` を使います。
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
    /// `nullable` を付けた項目は、送られてこなかったときと、空だったときに
    /// 空文字になります。**空白だけの値も空文字です。** そうしないと
    /// `nullable|min:8` を飛ばした項目で、規則が 1 つも当たっていない値が
    /// 「入力があった」ように見えてしまいます。
    pub fn get(&self, field: &str) -> &str {
        self.values.get(field).map(String::as_str).unwrap_or("")
    }

    /// 値を取り出す。**検査した項目に無ければ `None`。**
    ///
    /// 「送られてきたか」は分かりません。`nullable` を付けた項目は、
    /// 送られてこなくても空文字で入るので `Some("")` になります。
    /// `None` になるのは、規則を書かなかった項目と、`nullable` も `required` も
    /// 無いまま送られてこなかった項目です。
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

    /// 通った値を、まとめて型に読む。[`FormRequest`] の `validate_as` が使います。
    ///
    /// ```ignore
    /// #[derive(bengara::serde::Deserialize)]
    /// #[serde(crate = "bengara::serde")]
    /// struct Input { title: String, age: Option<i64> }
    ///
    /// let input: Input = req.validate(rules! { "title" => "required", "age" => "nullable|integer" })?
    ///     .to_typed()?;
    /// ```
    ///
    /// | 項目の型 | 読み方 | 読めないとき |
    /// |---|---|---|
    /// | `String` | そのまま | — |
    /// | 整数（`i8`〜`i64`・`u8`〜`u64`・`isize`・`usize`） | 前後の空白を落として読む | 500 |
    /// | `f32` / `f64` | 同上 | 500 |
    /// | `bool` | `1` `true` `on` `yes` は真、`0` `false` `off` `no` は偽。大文字小文字は区別する | 500 |
    /// | `Option<T>` | 空文字と、表に無い項目は `None`。それ以外は `T` として読む | — |
    /// | `Carbon` | `Carbon::parse` | 500 |
    /// | 中身の無い列挙（`enum Role { Admin, User }`） | 値を変種の名前として読む | 500 |
    /// | それ以外（`Vec`・入れ子の構造体など） | 読まない | 500 |
    ///
    /// 読めないときは 500 です（検査を通った値が読めないのは、規則と型の食い違いのため）。
    /// 規則に無い項目は表に入らないので、`Option` でない項目は「値がありません」で落ちます。
    pub fn to_typed<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        T::deserialize(typed::Table(&self.values)).map_err(|e| e.into_error())
    }

    /// 検査した項目に入っているか。
    ///
    /// [`Validated::try_get`] と同じで、`nullable` の項目は送られてこなくても真です。
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

/// 使える規則の名前の一覧。**ここだけに持ちます。**
///
/// 実行時の [`RULE_NAMES`] と、コンパイル時の [`check_rules`] の `panic!` の文が
/// 両方ともこれを使います。`concat!` に渡すため、定数ではなくマクロにしています。
/// 規則を足したら、ここと [`Rule::parse`] と `check_rule` の 3 か所を直してください。
macro_rules! rule_names {
    () => {
        "required, nullable, integer (int), numeric, boolean (bool), email, url, alpha, \
         alpha_num, alpha_dash, min, max, size, between, in, confirmed, same, different, \
         starts_with, ends_with, date"
    };
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
    Date,
}

/// 使える規則の名前。知らない規則を断るときの案内に出します。
///
/// 一覧は [`rule_names!`] の 1 か所だけに持ちます。コンパイル時の検査
/// （[`check_rules`]）も同じものを使います。
const RULE_NAMES: &str = rule_names!();

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
            // 許可値から**空の要素を落とします**。`in:admin,user,` と書くと、
            // 末尾のカンマが空文字を許可値にして `role=`（空）が通ってしまいます。
            // 1 つも残らなければ規則の書き間違いなので、`min` の引数忘れと同じ 500。
            "in" => {
                let allowed: Vec<String> = arg
                    .unwrap_or("")
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if allowed.is_empty() {
                    return Err(Error::msg(
                        "規則 `in` には 1 つ以上の値が要ります（例: in:admin,user）",
                    ));
                }
                Rule::In(allowed)
            }
            "confirmed" => Rule::Confirmed,
            "same" => Rule::Same(text("same")?),
            "different" => Rule::Different(text("different")?),
            "starts_with" => Rule::StartsWith(text("starts_with")?),
            "ends_with" => Rule::EndsWith(text("ends_with")?),
            "date" => Rule::Date,
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

/// 規則を書き、**コンパイルのときに確かめる。**
///
/// ```ignore
/// let input = req.validate(rules! {
///     "title" => "required|max:255",
///     "age"   => "nullable|integer",
/// })?;
/// ```
///
/// 返すのは `&'static [(&'static str, &'static str)]` です。`req.validate(&[..])` と
/// 同じ物を渡せます。空の `rules! {}` も書けます。
///
/// 規則の書き間違い（知らない名前・`min` の数の忘れなど）は、実行して 500 になる
/// 前に**コンパイルが止まります**（`evaluation of constant value failed`）。
/// 判定は実行時と同じで、実行時に通る書き方は止めません。
///
/// | 止まるもの | 例 |
/// |---|---|
/// | 知らない規則名 | `maxx:10` |
/// | `min` / `max` に数が無い | `min`・`max:nan`・`min:inf` |
/// | `size` が 0 以上の整数でない | `size:-3`・`size:1.5` |
/// | `between` の形が違う | `between:5`・`between:a,10` |
/// | `in` に値が 1 つも無い | `in:`・`in:,,` |
/// | `same` などに値が無い | `same:` |
///
/// 次のものは**実行時に任せます**（500 になります）。`between:10,1`（下限が上限より大きい）、
/// `max:1e400`（桁あふれ）、`size:1e30`（大きすぎる）。
#[macro_export]
macro_rules! rules {
    ($($field:literal => $spec:literal),* $(,)?) => {{
        const __BENGARA_RULES: &[(&str, &str)] = &[$(($field, $spec)),*];
        const _: () = $crate::validation::check_rules(__BENGARA_RULES);
        __BENGARA_RULES
    }};
}

/// 型付きの検査。Laravel の FormRequest に当たります。
///
/// `app/Http/Requests/` に、規則と読み先の型を 1 つにまとめて置きます。
///
/// ```ignore
/// // app/Http/Requests/StorePost.rs
/// use bengara::validation::FormRequest;
///
/// #[derive(bengara::serde::Deserialize)]
/// #[serde(crate = "bengara::serde")]
/// pub struct StorePost {
///     pub title: String,
///     pub age: Option<i64>,
/// }
///
/// impl FormRequest for StorePost {
///     const RULES: &'static [(&'static str, &'static str)] = bengara::rules! {
///         "title" => "required|max:255",
///         "age"   => "nullable|integer",
///     };
/// }
///
/// // ハンドラ
/// let input: StorePost = req.validate_as()?;
/// ```
///
/// 検査に落ちたら 422、通った値を型に読めなければ 500 です
/// （[`Validated::to_typed`]）。
///
/// `RULES` の書き間違いは `validate_as` を呼ぶ所で確かめます。この確かめは
/// `cargo build`（`serve`・`test` を含む）で効き、`cargo check` では効きません。
/// `rules!` で書けば `cargo check` でも止まります。
pub trait FormRequest: serde::de::DeserializeOwned {
    /// 項目と規則の組。`rules!` で書くと、コンパイルのときに確かめます。
    const RULES: &'static [(&'static str, &'static str)];
}

/// 規則の書き間違いを、コンパイルのときに止める。`rules!` と `validate_as` が使います。
///
/// **判定は実行時（`Rule::parse`）と同じです。** 実行時に通る書き方は止めず、
/// 実行時に 500 になるもののうち、文字列だけで決まるものを止めます。
/// 引数を取らない規則（`required` など）に付いた引数は、実行時と同じく無視します。
///
/// 止めるときは `panic!` です。`const` の中で呼ぶので、コンパイルのエラー（E0080）になります。
///
/// ```compile_fail,E0080
/// let _ = bengara::rules! { "title" => "maxx:10" };
/// ```
///
/// ```compile_fail,E0080
/// let _ = bengara::rules! { "n" => "min" };
/// ```
///
/// ```compile_fail,E0080
/// let _ = bengara::rules! { "n" => "max:nan" };
/// ```
///
/// ```compile_fail,E0080
/// let _ = bengara::rules! { "n" => "min:inf" };
/// ```
///
/// ```compile_fail,E0080
/// let _ = bengara::rules! { "s" => "size:-3" };
/// ```
///
/// ```compile_fail,E0080
/// let _ = bengara::rules! { "s" => "size:1.5" };
/// ```
///
/// ```compile_fail,E0080
/// let _ = bengara::rules! { "role" => "in:" };
/// ```
///
/// ```compile_fail,E0080
/// let _ = bengara::rules! { "role" => "in:,," };
/// ```
///
/// ```compile_fail,E0080
/// let _ = bengara::rules! { "n" => "between:5" };
/// ```
///
/// ```compile_fail,E0080
/// let _ = bengara::rules! { "a" => "same:" };
/// ```
///
/// `FormRequest::RULES` を `rules!` で書くと、`validate_as` を呼ばなくても止まります。
///
/// ```compile_fail,E0080
/// #[derive(bengara::serde::Deserialize)]
/// #[serde(crate = "bengara::serde")]
/// struct Input {
///     n: i64,
/// }
/// impl bengara::FormRequest for Input {
///     const RULES: &'static [(&'static str, &'static str)] = bengara::rules! { "n" => "min" };
/// }
/// ```
///
/// `rules!` を使わなくても、`validate_as` を使う所で止まります（`cargo build` のとき）。
///
/// ```compile_fail,E0080
/// #[derive(bengara::serde::Deserialize)]
/// #[serde(crate = "bengara::serde")]
/// struct Input {
///     n: i64,
/// }
/// impl bengara::FormRequest for Input {
///     const RULES: &'static [(&'static str, &'static str)] = &[("n", "maxx:10")];
/// }
/// fn store(req: &bengara::Request) -> bengara::Result<i64> {
///     Ok(req.validate_as::<Input>()?.n)
/// }
/// // ルートに登録したハンドラと同じく、使われる関数にする。
/// let handler: fn(&bengara::Request) -> bengara::Result<i64> = store;
/// let _ = handler;
/// ```
///
/// 正しい書き方は通ります。
///
/// ```
/// let rules = bengara::rules! {
///     "title" => "required|max:255",
///     "age"   => "nullable|integer|between:0,150",
///     "at"    => "date",
/// };
/// assert_eq!(rules.len(), 3);
/// let none: &[(&str, &str)] = bengara::rules! {};
/// assert!(none.is_empty());
/// ```
#[doc(hidden)]
pub const fn check_rules(rules: &[(&str, &str)]) {
    let mut i = 0;
    while i < rules.len() {
        let spec = rules[i].1.as_bytes();
        // 縦棒で分け、空白だけの区切りは飛ばす（実行時と同じ）。
        let mut start = 0;
        while start <= spec.len() {
            let end = find_byte(spec, start, spec.len(), b'|');
            let (s, e) = trim_range(spec, start, end);
            if s < e {
                check_rule(spec, start, end);
            }
            start = end + 1;
        }
        i += 1;
    }
}

/// 規則 1 つ（`spec[start..end]`）を確かめる。`Rule::parse` と同じ判定です。
const fn check_rule(spec: &[u8], start: usize, end: usize) {
    // 最初の `:` の前が名前、後ろが引数。どちらも前後の空白を落とす。
    // 引数が無いときは空として扱います。空の引数で通る規則は無いので、結果は同じです。
    let colon = find_byte(spec, start, end, b':');
    let (ns, ne) = trim_range(spec, start, colon);
    let (a, b) = if colon < end {
        trim_range(spec, colon + 1, end)
    } else {
        (end, end)
    };
    // const fn の中ではクロージャを呼べないので、小さなマクロで書く。
    macro_rules! is {
        ($name:literal) => {
            bytes_eq(spec, ns, ne, $name)
        };
    }

    // 引数を取らない規則。付いた引数は実行時も無視しているので、ここでも見ない。
    if is!("required")
        || is!("nullable")
        || is!("integer")
        || is!("int")
        || is!("numeric")
        || is!("boolean")
        || is!("bool")
        || is!("email")
        || is!("url")
        || is!("alpha")
        || is!("alpha_num")
        || is!("alpha_dash")
        || is!("confirmed")
        || is!("date")
    {
        return;
    }
    if is!("min") {
        if !is_number(spec, a, b) {
            panic!("検査の規則: `min` には数値が要ります（例: min:10）");
        }
        return;
    }
    if is!("max") {
        if !is_number(spec, a, b) {
            panic!("検査の規則: `max` には数値が要ります（例: max:10）");
        }
        return;
    }
    if is!("size") {
        if !is_number(spec, a, b) || is_negative(spec, a, b) || has_fraction(spec, a, b) {
            panic!("検査の規則: `size` には 0 以上の整数が要ります（例: size:10）");
        }
        return;
    }
    if is!("between") {
        let comma = find_byte(spec, a, b, b',');
        if comma == b {
            panic!("検査の規則: `between` は between:1,10 の形で書いてください");
        }
        let (ls, le) = trim_range(spec, a, comma);
        if !is_number(spec, ls, le) {
            panic!("検査の規則: `between` の下限が数値ではありません（例: between:1,10）");
        }
        let (hs, he) = trim_range(spec, comma + 1, b);
        if !is_number(spec, hs, he) {
            panic!("検査の規則: `between` の上限が数値ではありません（例: between:1,10）");
        }
        return;
    }
    if is!("in") {
        // `,` で分けて空白を落とし、空でない値が 1 つでもあれば通す。
        let mut s = a;
        while s <= b {
            let e = find_byte(spec, s, b, b',');
            let (vs, ve) = trim_range(spec, s, e);
            if vs < ve {
                return;
            }
            s = e + 1;
        }
        panic!("検査の規則: `in` には 1 つ以上の値が要ります（例: in:admin,user）");
    }
    if is!("same") {
        if a == b {
            panic!("検査の規則: `same` には項目名が要ります（例: same:password）");
        }
        return;
    }
    if is!("different") {
        if a == b {
            panic!("検査の規則: `different` には項目名が要ります（例: different:old_password）");
        }
        return;
    }
    if is!("starts_with") {
        if a == b {
            panic!("検査の規則: `starts_with` には値が要ります（例: starts_with:abc）");
        }
        return;
    }
    if is!("ends_with") {
        if a == b {
            panic!("検査の規則: `ends_with` には値が要ります（例: ends_with:abc）");
        }
        return;
    }
    panic!(concat!(
        "検査の規則: 知らない規則名です。使えるのは ",
        rule_names!(),
        " です"
    ));
}

/// `bytes[start..end]` の中で最初の `needle` の位置。無ければ `end`。
const fn find_byte(bytes: &[u8], start: usize, end: usize, needle: u8) -> usize {
    let mut i = start;
    while i < end {
        if bytes[i] == needle {
            return i;
        }
        i += 1;
    }
    end
}

/// `bytes[start..end]` が `text` と同じか。
const fn bytes_eq(bytes: &[u8], start: usize, end: usize, text: &str) -> bool {
    let text = text.as_bytes();
    if end - start != text.len() {
        return false;
    }
    let mut i = 0;
    while i < text.len() {
        if bytes[start + i] != text[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// `bytes[start..end]` の前後の空白を落とした範囲。
///
/// 実行時の `str::trim` と同じく、Unicode の空白（全角の空白など）も落とします。
/// ASCII だけを落とすと、実行時に通る `"required|\u{3000}max:10"` を止めてしまいます。
const fn trim_range(bytes: &[u8], mut start: usize, mut end: usize) -> (usize, usize) {
    while start < end {
        let (c, width) = decode_char(bytes, start);
        if !is_white_space(c) {
            break;
        }
        start += width;
    }
    while start < end {
        // 末尾の 1 文字の頭まで戻る（UTF-8 の続きのバイトは 10xxxxxx）。
        let mut head = end - 1;
        while bytes[head] & 0xC0 == 0x80 {
            head -= 1;
        }
        let (c, _) = decode_char(bytes, head);
        if !is_white_space(c) {
            break;
        }
        end = head;
    }
    (start, end)
}

/// `bytes[i]` から始まる 1 文字（UTF-8）を読み、文字の番号とバイト数を返す。
///
/// 元は `&str` なので、正しい UTF-8 であることは確かです。
const fn decode_char(bytes: &[u8], i: usize) -> (u32, usize) {
    let lead = bytes[i] as u32;
    if lead < 0x80 {
        (lead, 1)
    } else if lead < 0xE0 {
        (((lead & 0x1F) << 6) | tail(bytes, i + 1), 2)
    } else if lead < 0xF0 {
        (
            ((lead & 0x0F) << 12) | (tail(bytes, i + 1) << 6) | tail(bytes, i + 2),
            3,
        )
    } else {
        (
            ((lead & 0x07) << 18)
                | (tail(bytes, i + 1) << 12)
                | (tail(bytes, i + 2) << 6)
                | tail(bytes, i + 3),
            4,
        )
    }
}

/// UTF-8 の続きのバイトから、中身の 6 ビットを取り出す。
const fn tail(bytes: &[u8], i: usize) -> u32 {
    (bytes[i] as u32) & 0x3F
}

/// Unicode の空白か（`char::is_whitespace` と同じ一覧）。
const fn is_white_space(c: u32) -> bool {
    matches!(
        c,
        0x09..=0x0D
            | 0x20
            | 0x85
            | 0xA0
            | 0x1680
            | 0x2000..=0x200A
            | 0x2028
            | 0x2029
            | 0x202F
            | 0x205F
            | 0x3000
    )
}

/// 数の形か。`parse::<f64>()` が有限の数として読む形と同じです。
///
/// `[+-]?` に続いて「数字 1 個以上（`.` と数字 0 個以上が続いてよい）」か
/// 「`.` と数字 1 個以上」。その後に `e` か `E`、`[+-]?`、数字 1 個以上が続いてよい。
/// `inf`・`nan`・`1_000` は数の形ではありません。`1e400` のような桁あふれは
/// 形としては通し、実行時に任せます。
const fn is_number(bytes: &[u8], start: usize, end: usize) -> bool {
    let mut i = start;
    if i < end && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let int_start = i;
    while i < end && bytes[i].is_ascii_digit() {
        i += 1;
    }
    let mut digits = i - int_start;
    if i < end && bytes[i] == b'.' {
        i += 1;
        let frac_start = i;
        while i < end && bytes[i].is_ascii_digit() {
            i += 1;
        }
        digits += i - frac_start;
    }
    if digits == 0 {
        return false;
    }
    if i < end && (bytes[i] == b'e' || bytes[i] == b'E') {
        i += 1;
        if i < end && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let exp_start = i;
        while i < end && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == exp_start {
            return false;
        }
    }
    i == end
}

/// 負の数か。`-` で始まり、指数より前に 0 以外の数字があるもの。
///
/// `-0` と `-0.0` は実行時に 0 として通るので、負とみなしません。
const fn is_negative(bytes: &[u8], start: usize, end: usize) -> bool {
    if start >= end || bytes[start] != b'-' {
        return false;
    }
    let mut i = start + 1;
    while i < end && bytes[i] != b'e' && bytes[i] != b'E' {
        if bytes[i] >= b'1' && bytes[i] <= b'9' {
            return true;
        }
        i += 1;
    }
    false
}

/// 指数（`e` / `E`）が無く、`.` の後ろに 0 以外の数字があるか（`1.5` など）。
///
/// 指数がある値（`1.5e1` は 15）の整数判定は実行時に任せます。
const fn has_fraction(bytes: &[u8], start: usize, end: usize) -> bool {
    if find_byte(bytes, start, end, b'e') < end || find_byte(bytes, start, end, b'E') < end {
        return false;
    }
    let dot = find_byte(bytes, start, end, b'.');
    let mut i = dot;
    while i < end {
        if bytes[i] >= b'1' && bytes[i] <= b'9' {
            return true;
        }
        i += 1;
    }
    false
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
        // 「空」の判定だけは空白を落として見る。`password` の類いは値の空白を落とさないが、
        // 空白だけのパスワードを `required` が通してしまうのは違う（Laravel も落とす）。
        let is_blank = value.trim().is_empty();
        let nullable = parsed.contains(&Rule::Nullable);
        let required = parsed.contains(&Rule::Required);

        if is_blank && required {
            errors.add(field, format!("{field} は必ず入力してください。"));
            continue;
        }
        // 検査を飛ばすのは、**送られてこなかった**ときと `nullable` が付いているときだけ。
        // 空文字は飛ばしません（Laravel と同じ）。飛ばすと `in` や `email` が素通りします。
        if is_blank && (raw.is_none() || nullable) {
            // 空として飛ばした項目には**空文字**を入れる。来た値をそのまま入れては
            // いけない。`password` の類いは空白を落とさないので、`nullable|min:8|confirmed`
            // に `"   "` を送ると、規則が 1 つも当たらないまま
            // `get("password")` が空でない値を返す。アプリが「入力があった」と見なして、
            // 長さも確認欄の一致も通さずに保存してしまう。
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
            if !looks_like_url(value) {
                errors.add(
                    field,
                    format!("{field} は http:// か https:// で始まる URL を入力してください。"),
                );
            }
        }
        // `alpha` の 3 つは、空の列に `all` が `true` を返すので空文字を明示的に弾きます。
        // ここまで来た空文字は「送られてきた空」です。`in` や `email` と同じく落とします。
        Rule::Alpha => {
            if value.is_empty() || !value.chars().all(char::is_alphabetic) {
                errors.add(field, format!("{field} は文字だけで入力してください。"));
            }
        }
        Rule::AlphaNum => {
            if value.is_empty() || !value.chars().all(char::is_alphanumeric) {
                errors.add(
                    field,
                    format!("{field} は文字と数字だけで入力してください。"),
                );
            }
        }
        Rule::AlphaDash => {
            if value.is_empty()
                || !value
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
        // 受け付ける形は `Carbon::parse` と同じです（Laravel の `date` より狭い）。
        // 通った値は `get_as::<Carbon>` や `to_typed` でそのまま読めます。
        Rule::Date => {
            if crate::Carbon::parse(value).is_err() {
                errors.add(
                    field,
                    format!(
                        "{field} は日付（YYYY-MM-DD か YYYY-MM-DD HH:MM:SS）で入力してください。"
                    ),
                );
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

/// `http://` か `https://` で始まる URL らしい形か。
///
/// 完全な URL の解析はしません。次のものだけを断ります。
///
/// - スキームが `http` / `https` でない（大文字小文字は見ない）
/// - 空白・制御文字、URL にそのまま書けない記号（`< > " { } | \ ^` と `` ` ``）を含む
/// - スキームの直後にホストが無い（`https://`・`https:///evil`・`https://:80`・`https://user@`）
/// - ポートが数字でない（`https://a:b`）
fn looks_like_url(value: &str) -> bool {
    let rest = ["https://", "http://"].iter().find_map(|scheme| {
        value
            .get(..scheme.len())
            .filter(|head| head.eq_ignore_ascii_case(scheme))
            .map(|_| &value[scheme.len()..])
    });
    let Some(rest) = rest else {
        return false;
    };
    if rest.chars().any(|c| {
        c.is_whitespace()
            || c.is_control()
            || matches!(c, '<' | '>' | '"' | '{' | '}' | '|' | '\\' | '^' | '`')
    }) {
        return false;
    }
    // ホストとポート（`/` `?` `#` の手前まで）。利用者名（`user@`）は外す。
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    if host_port.starts_with('[') {
        // IPv6（`[::1]:8080`）。`]` で閉じていて、中が空でないこと。
        return match host_port.find(']') {
            Some(end) if end > 1 => {
                let after = &host_port[end + 1..];
                after.is_empty()
                    || after
                        .strip_prefix(':')
                        .is_some_and(|port| port.bytes().all(|b| b.is_ascii_digit()))
            }
            _ => false,
        };
    }
    let (host, port) = match host_port.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (host_port, None),
    };
    !host.is_empty() && port.is_none_or(|port| port.bytes().all(|b| b.is_ascii_digit()))
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

/// [`Validated::to_typed`] の読み手。検査を通った値（文字列の表）を serde で型に読みます。
///
/// 依存を足さないため、自前で書いています。読めるのは 1 段の構造体だけです。
/// 項目ごとの読み方は `to_typed` の表を見てください。
mod typed {
    use std::collections::BTreeMap;
    use std::fmt;

    use serde::de::{self, DeserializeSeed, IntoDeserializer, MapAccess, Visitor};

    use crate::error::Error;

    /// 読めなかった理由。**どの項目かを一緒に持ちます。**
    #[derive(Debug)]
    pub(super) struct ReadError {
        field: Option<String>,
        reason: String,
    }

    impl ReadError {
        /// 項目名がまだ無ければ付ける。値の読み手の外（serde の derive の中）で
        /// 作られたエラーにも、項目名を付けるために使います。
        fn in_field(mut self, field: &str) -> Self {
            if self.field.is_none() {
                self.field = Some(field.to_string());
            }
            self
        }

        /// 500 になるエラーにする。
        pub(super) fn into_error(self) -> Error {
            match self.field {
                Some(field) => Error::msg(format!(
                    "検査を通った値を型に読めません（項目 `{field}`）: {}",
                    self.reason
                )),
                None => Error::msg(format!("検査を通った値を型に読めません: {}", self.reason)),
            }
        }
    }

    impl fmt::Display for ReadError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.reason)
        }
    }

    impl std::error::Error for ReadError {}

    impl de::Error for ReadError {
        fn custom<T: fmt::Display>(msg: T) -> Self {
            Self {
                field: None,
                reason: msg.to_string(),
            }
        }

        // 規則に無い項目と、送られてこなかった項目は表に入りません。
        // `Option` でない項目がそうなると、serde がここを呼びます。
        fn missing_field(field: &'static str) -> Self {
            Self {
                field: Some(field.to_string()),
                reason: "値がありません。規則に `required` を付けるか、`Option` にしてください"
                    .to_string(),
            }
        }

        fn unknown_variant(variant: &str, expected: &'static [&'static str]) -> Self {
            let names: Vec<String> = expected.iter().map(|name| format!("`{name}`")).collect();
            Self::custom(format!(
                "`{variant}` は選べる値にありません（{}）",
                names.join("・")
            ))
        }
    }

    /// 表全体。構造体として読みます。
    pub(super) struct Table<'a>(pub(super) &'a BTreeMap<String, String>);

    impl<'de> de::Deserializer<'de> for Table<'_> {
        type Error = ReadError;

        fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
            visitor.visit_map(Entries {
                iter: self.0.iter(),
                current: None,
            })
        }

        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
            bytes byte_buf option unit unit_struct newtype_struct seq tuple
            tuple_struct map struct enum identifier ignored_any
        }
    }

    /// 表の組を 1 つずつ渡す。
    struct Entries<'a> {
        iter: std::collections::btree_map::Iter<'a, String, String>,
        current: Option<(&'a str, &'a str)>,
    }

    impl<'de> MapAccess<'de> for Entries<'_> {
        type Error = ReadError;

        fn next_key_seed<K: DeserializeSeed<'de>>(
            &mut self,
            seed: K,
        ) -> Result<Option<K::Value>, ReadError> {
            let Some((field, value)) = self.iter.next() else {
                return Ok(None);
            };
            self.current = Some((field, value));
            seed.deserialize(field.as_str().into_deserializer())
                .map(Some)
        }

        fn next_value_seed<V: DeserializeSeed<'de>>(
            &mut self,
            seed: V,
        ) -> Result<V::Value, ReadError> {
            let (field, value) = self.current.take().ok_or_else(|| {
                <ReadError as de::Error>::custom("項目名より先に値を読もうとしました")
            })?;
            seed.deserialize(Field { value })
                .map_err(|e| e.in_field(field))
        }

        fn size_hint(&self) -> Option<usize> {
            Some(self.iter.len())
        }
    }

    /// 項目 1 つの値。
    struct Field<'a> {
        value: &'a str,
    }

    impl Field<'_> {
        /// 前後の空白を落として、数として読む。
        fn number<T: std::str::FromStr>(&self, what: &str) -> Result<T, ReadError> {
            self.value.trim().parse().map_err(|_| {
                <ReadError as de::Error>::custom(format!(
                    "`{}` を {what} として読めません",
                    self.value
                ))
            })
        }

        fn unsupported(what: &str) -> ReadError {
            <ReadError as de::Error>::custom(format!(
                "{what}には読めません（読めるのは文字列・整数・小数・真偽値・`Option`・\
                 `Carbon`・中身の無い列挙です）"
            ))
        }
    }

    /// 数の型の読み方をまとめて書く。
    macro_rules! numbers {
        ($($method:ident => $visit:ident($ty:ty)),* $(,)?) => {$(
            fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
                visitor.$visit(self.number::<$ty>(stringify!($ty))?)
            }
        )*};
    }

    /// 読まない型をまとめて書く。
    macro_rules! unsupported {
        ($($method:ident => $what:literal),* $(,)?) => {$(
            fn $method<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value, ReadError> {
                Err(Self::unsupported($what))
            }
        )*};
    }

    impl<'de> de::Deserializer<'de> for Field<'_> {
        type Error = ReadError;

        fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
            visitor.visit_str(self.value)
        }

        fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
            visitor.visit_str(self.value)
        }

        fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
            visitor.visit_str(self.value)
        }

        fn deserialize_identifier<V: Visitor<'de>>(
            self,
            visitor: V,
        ) -> Result<V::Value, ReadError> {
            visitor.visit_str(self.value)
        }

        // `boolean` 規則と同じ 8 つ。大文字小文字も同じく区別します。
        fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
            match self.value {
                "1" | "true" | "on" | "yes" => visitor.visit_bool(true),
                "0" | "false" | "off" | "no" => visitor.visit_bool(false),
                other => Err(<ReadError as de::Error>::custom(format!(
                    "`{other}` を真偽値として読めません（1 true on yes 0 false off no のどれか）"
                ))),
            }
        }

        numbers! {
            deserialize_i8 => visit_i8(i8),
            deserialize_i16 => visit_i16(i16),
            deserialize_i32 => visit_i32(i32),
            deserialize_i64 => visit_i64(i64),
            deserialize_u8 => visit_u8(u8),
            deserialize_u16 => visit_u16(u16),
            deserialize_u32 => visit_u32(u32),
            deserialize_u64 => visit_u64(u64),
            deserialize_f32 => visit_f32(f32),
            deserialize_f64 => visit_f64(f64),
        }

        // 空文字は `None`。`nullable` で飛ばした項目は空文字で入るためです。
        // 表に無い項目は、serde の derive が `None` にします。
        fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
            if self.value.is_empty() {
                visitor.visit_none()
            } else {
                visitor.visit_some(self)
            }
        }

        // 中身の無い列挙だけ。値を変種の名前として読みます
        // （`#[serde(rename_all = "lowercase")]` などが効きます）。
        fn deserialize_enum<V: Visitor<'de>>(
            self,
            _name: &'static str,
            _variants: &'static [&'static str],
            visitor: V,
        ) -> Result<V::Value, ReadError> {
            visitor.visit_enum(self.value.into_deserializer())
        }

        // 構造体に無い項目は読み飛ばす。
        fn deserialize_ignored_any<V: Visitor<'de>>(
            self,
            visitor: V,
        ) -> Result<V::Value, ReadError> {
            visitor.visit_unit()
        }

        unsupported! {
            deserialize_char => "1 文字の型（char）",
            deserialize_bytes => "バイト列",
            deserialize_byte_buf => "バイト列",
            deserialize_unit => "`()`",
            deserialize_seq => "並び（Vec など）",
            deserialize_map => "表（HashMap など）",
        }

        fn deserialize_unit_struct<V: Visitor<'de>>(
            self,
            _name: &'static str,
            _visitor: V,
        ) -> Result<V::Value, ReadError> {
            Err(Self::unsupported("中身の無い構造体"))
        }

        fn deserialize_newtype_struct<V: Visitor<'de>>(
            self,
            _name: &'static str,
            _visitor: V,
        ) -> Result<V::Value, ReadError> {
            Err(Self::unsupported("包んだ型（struct Id(i64) など）"))
        }

        fn deserialize_tuple<V: Visitor<'de>>(
            self,
            _len: usize,
            _visitor: V,
        ) -> Result<V::Value, ReadError> {
            Err(Self::unsupported("組（タプル）"))
        }

        fn deserialize_tuple_struct<V: Visitor<'de>>(
            self,
            _name: &'static str,
            _len: usize,
            _visitor: V,
        ) -> Result<V::Value, ReadError> {
            Err(Self::unsupported("組の構造体"))
        }

        fn deserialize_struct<V: Visitor<'de>>(
            self,
            _name: &'static str,
            _fields: &'static [&'static str],
            _visitor: V,
        ) -> Result<V::Value, ReadError> {
            Err(Self::unsupported("入れ子の構造体"))
        }
    }
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
    fn 空白だけのパスワードもrequiredで落ちる() {
        // `password` は空白を落とさない項目だが、「空か」の判定は空白を除いて見る。
        // 以前は `"   "` が `required` を通り、`min:8` にも当たらない長さなら素通りだった。
        for value in ["   ", "\t"] {
            let e = errors_of(&input(&[("password", value)]), &[("password", "required")]);
            assert!(e.has("password"), "`{value:?}` は空として扱う");
        }
        // 空白を含む本物のパスワードは、空白も含めてそのまま通る。
        let ok = validate(
            &input(&[("password", " pass word ")]),
            &[("password", "required|min:8")],
        )
        .unwrap();
        assert_eq!(ok.get("password"), " pass word ");
    }

    #[test]
    fn nullableなら空でも通り他の規則は当たらない() {
        let ok = validate(&input(&[("age", "")]), &[("age", "nullable|integer")]).unwrap();
        assert_eq!(ok.get("age"), "");
        assert!(ok.has("age"));
    }

    #[test]
    fn try_getは検査した項目に無いときだけnoneを返す() {
        // `try_get` で分かるのは「検査した項目に入ったか」で、
        // 「送られてきたか」ではない。
        let ok = validate(
            &input(&[("title", "題"), ("extra", "x")]),
            &[
                ("title", "required"),
                ("note", "nullable"),
                ("memo", "max:10"),
            ],
        )
        .unwrap();

        // `nullable` の項目は、送られてこなくても空文字で入る。
        assert_eq!(ok.try_get("note"), Some(""));
        assert!(ok.has("note"));
        // `nullable` が無く、送られてこなかった項目は入らない。
        assert_eq!(ok.try_get("memo"), None);
        assert!(!ok.has("memo"));
        // 規則を書かなかった項目は、送られてきても入らない。
        assert_eq!(ok.try_get("extra"), None);
        assert_eq!(ok.try_get("title"), Some("題"));
    }

    #[test]
    fn 空として飛ばした項目は空文字で返る() {
        // `nullable` で飛ばした項目に来た値を入れると、規則が 1 つも当たらないまま
        // `get` が空でない値を返す。アプリが「入力があった」と見なして、
        // `min:8` も `confirmed` も通さずに保存してしまう。
        let ok = validate(
            &input(&[("password", "   ")]),
            &[("password", "nullable|min:8")],
        )
        .unwrap();
        assert_eq!(ok.get("password"), "", "空白だけなら空文字で返す");
        assert!(ok.has("password"));

        // 確認欄が無くても同じ。飛ばした項目は空文字。
        let ok = validate(
            &input(&[("password", "\t")]),
            &[("password", "nullable|min:8|confirmed")],
        )
        .unwrap();
        assert_eq!(ok.get("password"), "");

        // 空白を落とす項目も空文字（従来どおり）。
        let ok = validate(&input(&[("age", " ")]), &[("age", "nullable|integer")]).unwrap();
        assert_eq!(ok.get("age"), "");
    }

    #[test]
    fn alphaの3つは空文字を通さない() {
        // `all` は空の列に `true` を返す。空文字は飛ばされないので、ここで弾く。
        for rule in ["alpha", "alpha_num", "alpha_dash"] {
            let e = errors_of(&input(&[("s", "")]), &[("s", rule)]);
            assert!(e.has("s"), "`{rule}` は空文字を弾くはず");
        }
        // `nullable` が付いていれば今までどおり飛ばす。
        assert!(errors_of(&input(&[("s", "")]), &[("s", "nullable|alpha")]).is_empty());
        // 送られてこなければ飛ばす。
        assert!(errors_of(&input(&[]), &[("s", "alpha")]).is_empty());
    }

    #[test]
    fn inの許可値から空を落とす() {
        // 末尾のカンマで空文字が許可値になると、`role=` が通ってしまう。
        assert!(!errors_of(&input(&[("role", "")]), &[("role", "in:admin,user,")]).is_empty());
        // 値そのものは今までどおり通る。
        assert!(errors_of(&input(&[("role", "user")]), &[("role", "in:admin,user,")]).is_empty());

        // 許可値が 1 つも残らないのは規則の書き間違い。422 ではなく 500。
        let i = input(&[("role", "")]);
        for bad in ["in", "in:", "in:,,", "in: , "] {
            let error = validate(&i, &[("role", bad)]).unwrap_err();
            assert!(
                !matches!(error, Error::Validation(_)),
                "`{bad}` は 422 にせず 500 にする: {error}"
            );
        }
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
    fn urlはホストの無いものと書けない文字を断る() {
        for ok in [
            "http://example.com",
            "HTTPS://Example.com/a?b=c#d",
            "https://localhost:8000/x",
            "https://user:pass@example.com",
            "http://[::1]:8080/",
            "https://例え.jp/パス",
        ] {
            assert!(looks_like_url(ok), "{ok} は通すはず");
        }
        for ng in [
            "https://a b",
            "https:///evil",
            "https://<x>",
            "https://a\tb",
            "https://a\nb",
            "https://?q",
            "https://#x",
            "https://:80",
            "https://user@",
            "https://a:b",
            "https://[]",
            "https://[::1",
            "ftp://example.com",
            "https:/example.com",
        ] {
            assert!(!looks_like_url(ng), "{ng:?} は弾くはず");
        }
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
    fn 日時の型で取り出せる() {
        let ok = validate(
            &input(&[("at", "2026-11-20 09:30:00"), ("bad", "2026-02-30")]),
            &[("at", "required"), ("bad", "required")],
        )
        .unwrap();
        let at = ok.get_as::<crate::Carbon>("at").unwrap();
        assert_eq!(at.to_date_time_string(), "2026-11-20 09:30:00");
        assert!(ok.get_as::<crate::Carbon>("bad").is_err());
    }

    #[test]
    fn date規則() {
        for ok in [
            "2026-11-20",
            "2026-11-20 09:30:00",
            "2026-11-20T09:30:00Z",
            " 2026-11-20 ",
        ] {
            assert!(
                errors_of(&input(&[("at", ok)]), &[("at", "date")]).is_empty(),
                "{ok:?} は通る"
            );
        }
        for ng in [
            "2026-02-30",
            "2026/11/20",
            "2026-11-20T09:30:00+09:00",
            "あした",
            "",
        ] {
            let e = errors_of(&input(&[("at", ng)]), &[("at", "date")]);
            assert_eq!(
                e.get("at"),
                ["at は日付（YYYY-MM-DD か YYYY-MM-DD HH:MM:SS）で入力してください。"],
                "{ng:?} は落ちる"
            );
        }
        // `nullable` なら空は飛ばす。
        assert!(errors_of(&input(&[("at", "")]), &[("at", "nullable|date")]).is_empty());
    }

    /// 一覧（`rule_names!`）から規則名を取り出す。`integer (int)` は 2 つに分ける。
    fn listed_names() -> Vec<&'static str> {
        RULE_NAMES
            .split(',')
            .flat_map(|item| item.split([' ', '(', ')']))
            .filter(|name| !name.is_empty())
            .collect()
    }

    /// 規則名に、その規則が通る例の引数を付ける。
    fn with_example(name: &str) -> String {
        match name {
            "min" | "max" | "size" => format!("{name}:3"),
            "between" => format!("{name}:1,3"),
            "in" | "same" | "different" | "starts_with" | "ends_with" => format!("{name}:a"),
            _ => name.to_string(),
        }
    }

    /// 実行時の判定。規則が読めれば `true`（検査に落ちた 422 も読めたうち）。
    fn runtime_accepts(spec: &str) -> bool {
        !matches!(
            validate(&input(&[]), &[("f", spec)]),
            Err(error) if !matches!(error, Error::Validation(_))
        )
    }

    /// コンパイル時の判定（`check_rules`）を実行時に呼ぶ。止まれば `false`。
    fn compile_accepts(spec: &str) -> bool {
        std::panic::catch_unwind(|| check_rules(&[("f", spec)])).is_ok()
    }

    #[test]
    fn 一覧のすべての規則名が実行時もコンパイル時も通る() {
        let names = listed_names();
        assert_eq!(names.len(), 23, "{names:?}");
        assert!(names.contains(&"int") && names.contains(&"date"));
        for name in names {
            let spec = with_example(name);
            assert!(runtime_accepts(&spec), "実行時に `{spec}` が通らない");
            assert!(compile_accepts(&spec), "コンパイル時に `{spec}` が止まる");
        }
    }

    #[test]
    fn コンパイル時の判定は実行時と同じ() {
        // 文字列だけで決まるものは、両方で同じ結果になる。
        for spec in [
            // 通るもの
            "",
            "|",
            " | ",
            "required",
            " required | max:10 ",
            "required:引数は無視",
            "nullable|date",
            "max:+1.5e3",
            "min:.5",
            "min:5.",
            "min:-3",
            "max: 10 ",
            "size:0",
            "size:-0",
            "size:-0.0",
            "size:1.0",
            "size:1.5e1",
            "between:1,10",
            "between: 1 , 10 ",
            "in:a",
            "in:,a,",
            "same:other",
            "starts_with: x ",
            // 全角の空白も `str::trim` と同じく落とす。
            "required|\u{3000}max:10\u{3000}",
            "\u{a0}in:\u{2003}a",
            // 止めるもの
            "maxx:10",
            "Required",
            "min",
            "min:",
            "max:nan",
            "max:NaN",
            "min:inf",
            "min:-infinity",
            "max:1_000",
            "max:1e",
            "max:.",
            "max:0x10",
            "size:-3",
            "size:1.5",
            "size:abc",
            "size",
            "between:5",
            "between:,10",
            "between:a,10",
            "between:1,b",
            "between:1,2,3",
            "in",
            "in:",
            "in:,,",
            "in: , ",
            "same",
            "same:",
            "different: ",
            "starts_with:",
            "ends_with",
            "required|\u{3000}maxx:10",
        ] {
            assert_eq!(
                compile_accepts(spec),
                runtime_accepts(spec),
                "`{spec}` の判定が実行時と違う"
            );
        }
    }

    #[test]
    fn 実行時に任せるもの() {
        // 文字列の形だけでは決めないもの。コンパイル時は通し、実行時に 500。
        for spec in ["between:10,1", "max:1e400", "size:1e30"] {
            assert!(compile_accepts(spec), "`{spec}` は止めない");
            assert!(!runtime_accepts(spec), "`{spec}` は実行時に 500");
        }
    }

    #[test]
    fn 数の形はf64として読める形と同じ() {
        for text in [
            "0", "10", "-3", "+3", "1.5", ".5", "5.", "1e3", "1E-3", "+.5e+2", "nan", "inf",
            "-inf", "infinity", "NaN", "1_000", "1e", ".", "", "+", "-", "e3", "1.2.3", " 1",
            "0x1", "1f", "١",
        ] {
            let bytes = text.as_bytes();
            assert_eq!(
                is_number(bytes, 0, bytes.len()),
                text.parse::<f64>().is_ok_and(f64::is_finite),
                "`{text}`"
            );
        }
    }

    #[test]
    fn rulesで書いた規則をvalidateに渡せる() {
        let rules = crate::rules! {
            "title" => "required|max:5",
            "at"    => "nullable|date",
        };
        let ok = validate(&input(&[("title", "題"), ("at", "2026-11-20")]), rules).unwrap();
        assert_eq!(ok.get("title"), "題");
        assert!(errors_of(&input(&[("title", "長すぎる題名")]), rules).has("title"));
        let empty: &[(&str, &str)] = crate::rules! {};
        assert!(validate(&input(&[]), empty).unwrap().all().is_empty());
    }

    #[test]
    fn コンパイル時に止める文() {
        let message = |spec: &'static str| {
            let panic = std::panic::catch_unwind(|| check_rules(&[("f", spec)])).unwrap_err();
            panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap()
        };
        let text = message("maxx:10");
        assert!(
            text.starts_with("検査の規則: 知らない規則名です。"),
            "{text}"
        );
        assert!(text.contains("starts_with, ends_with, date"), "{text}");
        assert!(message("min").contains("`min` には数値が要ります"));
        assert!(message("size:1.5").contains("`size` には 0 以上の整数が要ります"));
        assert!(message("in:,").contains("`in` には 1 つ以上の値が要ります"));
    }

    mod typed_values {
        use super::*;
        use serde::Deserialize;

        fn validated(pairs: &[(&str, &str)]) -> Validated {
            Validated {
                values: pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            }
        }

        fn error_of<T: serde::de::DeserializeOwned + std::fmt::Debug>(
            pairs: &[(&str, &str)],
        ) -> String {
            let error = validated(pairs).to_typed::<T>().unwrap_err();
            assert_eq!(error.status(), 500, "{error}");
            error.to_string()
        }

        #[derive(Debug, PartialEq, Deserialize)]
        #[serde(rename_all = "lowercase")]
        enum Role {
            Admin,
            User,
        }

        #[derive(Debug, Deserialize)]
        struct All {
            title: String,
            i8_: i8,
            i64_: i64,
            u16_: u16,
            usize_: usize,
            isize_: isize,
            f32_: f32,
            f64_: f64,
            yes: bool,
            no: bool,
            note: Option<String>,
            age: Option<i64>,
            missing: Option<i64>,
            at: crate::Carbon,
            role: Role,
        }

        #[test]
        fn 表の各型に読める() {
            let all: All = validated(&[
                ("title", "題"),
                ("i8_", "-8"),
                ("i64_", " 64 "),
                ("u16_", "16"),
                ("usize_", "7"),
                ("isize_", "-7"),
                ("f32_", "1.5"),
                ("f64_", "2.25"),
                ("yes", "on"),
                ("no", "0"),
                ("note", ""),
                ("age", "30"),
                ("at", "2026-11-20 09:30:00"),
                ("role", "admin"),
                ("extra", "規則にない項目は読み飛ばす"),
            ])
            .to_typed()
            .unwrap();
            assert_eq!(all.title, "題");
            assert_eq!((all.i8_, all.i64_, all.u16_), (-8, 64, 16));
            assert_eq!((all.usize_, all.isize_), (7, -7));
            assert_eq!((all.f32_, all.f64_), (1.5, 2.25));
            assert!(all.yes && !all.no);
            assert_eq!(all.note, None, "空文字は None");
            assert_eq!(all.age, Some(30));
            assert_eq!(all.missing, None, "表に無い項目は None");
            assert_eq!(all.at.to_date_time_string(), "2026-11-20 09:30:00");
            assert_eq!(all.role, Role::Admin);
        }

        #[test]
        fn 真偽値の読み方はboolean規則と同じ() {
            #[derive(Debug, Deserialize)]
            struct B {
                #[allow(dead_code)]
                b: bool,
            }
            for (text, expected) in [
                ("1", true),
                ("true", true),
                ("on", true),
                ("yes", true),
                ("0", false),
                ("false", false),
                ("off", false),
                ("no", false),
            ] {
                #[derive(Deserialize)]
                struct V {
                    b: bool,
                }
                let v: V = validated(&[("b", text)]).to_typed().unwrap();
                assert_eq!(v.b, expected, "{text}");
            }
            // 大文字小文字は区別する。
            for text in ["TRUE", "Yes", "On", "2", ""] {
                let error = error_of::<B>(&[("b", text)]);
                assert!(error.contains("項目 `b`"), "{error}");
            }
        }

        #[test]
        fn 読めないときは項目名を添えて500() {
            #[derive(Debug, Deserialize)]
            #[allow(dead_code)]
            struct N {
                n: i32,
            }
            let error = error_of::<N>(&[("n", "abc")]);
            assert!(
                error.starts_with("検査を通った値を型に読めません（項目 `n`）: "),
                "{error}"
            );
            assert!(error.contains("`abc` を i32 として読めません"), "{error}");
            // 型の範囲を超える値も読めない。
            assert!(error_of::<N>(&[("n", "99999999999")]).contains("項目 `n`"));
            // `nullable` で空になった値は、`Option` でなければ読めない。
            assert!(error_of::<N>(&[("n", "")]).contains("項目 `n`"));

            #[derive(Debug, Deserialize)]
            #[allow(dead_code)]
            struct F {
                f: f64,
            }
            assert!(error_of::<F>(&[("f", "x")]).contains("項目 `f`"));

            #[derive(Debug, Deserialize)]
            #[allow(dead_code)]
            struct D {
                at: crate::Carbon,
            }
            let error = error_of::<D>(&[("at", "2026-02-30")]);
            assert!(error.contains("項目 `at`"), "{error}");
            assert!(error.contains("日時として読めません"), "{error}");

            #[derive(Debug, Deserialize)]
            #[allow(dead_code)]
            struct R {
                role: Role,
            }
            let error = error_of::<R>(&[("role", "Admin")]);
            assert!(error.contains("項目 `role`"), "{error}");
            assert!(error.contains("`Admin` は選べる値にありません"), "{error}");

            // Option の中身も同じ決まりで読む。
            #[derive(Debug, Deserialize)]
            #[allow(dead_code)]
            struct O {
                age: Option<u8>,
            }
            assert!(error_of::<O>(&[("age", "-1")]).contains("項目 `age`"));
        }

        #[test]
        fn 規則に無い必須の項目は500() {
            #[derive(Debug, Deserialize)]
            #[allow(dead_code)]
            struct T {
                title: String,
            }
            let error = error_of::<T>(&[]);
            assert!(error.contains("項目 `title`"), "{error}");
            assert!(error.contains("値がありません"), "{error}");
        }

        #[test]
        fn 読まない型は500() {
            #[derive(Debug, Deserialize)]
            #[allow(dead_code)]
            struct Tags {
                tags: Vec<String>,
            }
            let error = error_of::<Tags>(&[("tags", "a,b")]);
            assert!(error.contains("項目 `tags`"), "{error}");
            assert!(error.contains("並び（Vec など）には読めません"), "{error}");

            #[derive(Debug, Deserialize)]
            #[allow(dead_code)]
            struct Inner {
                a: String,
            }
            #[derive(Debug, Deserialize)]
            #[allow(dead_code)]
            struct Outer {
                inner: Inner,
            }
            assert!(error_of::<Outer>(&[("inner", "x")]).contains("入れ子の構造体"));

            #[derive(Debug, Deserialize)]
            #[allow(dead_code)]
            enum WithData {
                A(i64),
            }
            #[derive(Debug, Deserialize)]
            #[allow(dead_code)]
            struct E {
                e: WithData,
            }
            assert!(error_of::<E>(&[("e", "A")]).contains("項目 `e`"));

            // 構造体でない型は、表全体を読めない。
            let error = validated(&[]).to_typed::<i64>().unwrap_err().to_string();
            assert!(
                error.starts_with("検査を通った値を型に読めません: "),
                "{error}"
            );
        }
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
