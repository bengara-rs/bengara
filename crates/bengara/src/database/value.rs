//! DB とやりとりする値（`Value`）と、結果の1行（`Row`）。
//!
//! sqlx の型を利用者に見せないための層です。ここを通るので、下回りを差し替えても
//! アプリのコードは変わりません。

use std::sync::Arc;

use crate::error::{Error, Result};

/// DB に渡す値、DB から来た値。
///
/// 文字列と数値は、読み出すときに多少の読み替えをします（`Int` を `String` で読める、など）。
/// SQLite は型をゆるく扱うので、そのほうが扱いやすいためです。
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `NULL`。
    Null,
    /// 真偽。SQLite では 0 / 1 になります。
    Bool(bool),
    /// 整数。
    Int(i64),
    /// 小数。
    Float(f64),
    /// 文字列。
    Text(String),
    /// バイト列。
    Bytes(Vec<u8>),
}

impl Value {
    /// `NULL` か。
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// 値の種類の名前。エラーのメッセージに使います。
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Null => "NULL",
            Value::Bool(_) => "真偽",
            Value::Int(_) => "整数",
            Value::Float(_) => "小数",
            Value::Text(_) => "文字列",
            Value::Bytes(_) => "バイト列",
        }
    }

    /// JSON に直す。`Bytes` は数値の配列になります。
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Null => serde_json::Value::Null,
            Value::Bool(v) => serde_json::Value::Bool(*v),
            Value::Int(v) => serde_json::Value::from(*v),
            Value::Float(v) => serde_json::Number::from_f64(*v)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            Value::Text(v) => serde_json::Value::String(v.clone()),
            Value::Bytes(v) => serde_json::Value::from(v.clone()),
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Null => f.write_str(""),
            Value::Bool(v) => write!(f, "{v}"),
            Value::Int(v) => write!(f, "{v}"),
            Value::Float(v) => write!(f, "{v}"),
            Value::Text(v) => f.write_str(v),
            Value::Bytes(v) => write!(f, "{} バイト", v.len()),
        }
    }
}

impl serde::Serialize for Value {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.to_json().serialize(serializer)
    }
}

/// `Value` に変えられるもの。`where_` や `insert` の引数で使います。
///
/// `impl<T: Into<Value>> IntoValue for T` なので、`From<T> for Value` がある型は
/// そのまま渡せます。
pub trait IntoValue {
    /// `Value` に変える。
    fn into_value(self) -> Value;
}

impl<T: Into<Value>> IntoValue for T {
    fn into_value(self) -> Value {
        self.into()
    }
}

macro_rules! from_int {
    ($($t:ty),*) => {
        $(impl From<$t> for Value {
            fn from(v: $t) -> Self {
                Value::Int(v as i64)
            }
        })*
    };
}
from_int!(i8, i16, i32, i64, u8, u16, u32, isize, usize);

impl From<u64> for Value {
    fn from(v: u64) -> Self {
        // i64 に収まらない値は、桁を落とさないように文字列にする。
        match i64::try_from(v) {
            Ok(v) => Value::Int(v),
            Err(_) => Value::Text(v.to_string()),
        }
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}

impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Value::Float(v as f64)
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Float(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.to_string())
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Text(v)
    }
}

impl From<&String> for Value {
    fn from(v: &String) -> Self {
        Value::Text(v.clone())
    }
}

impl From<Vec<u8>> for Value {
    fn from(v: Vec<u8>) -> Self {
        Value::Bytes(v)
    }
}

impl From<&[u8]> for Value {
    fn from(v: &[u8]) -> Self {
        Value::Bytes(v.to_vec())
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        match v {
            Some(v) => v.into(),
            None => Value::Null,
        }
    }
}

/// `Value` から取り出せる型。`row.get::<i64>("id")?` の `i64` の側です。
pub trait FromValue: Sized {
    /// 取り出す。種類が違うときはエラー。
    fn from_value(value: &Value) -> Result<Self>;
}

fn mismatch<T>(value: &Value, want: &str) -> Result<T> {
    Err(Error::msg(format!(
        "{} の値を {want} として読めません",
        value.kind()
    )))
}

impl FromValue for i64 {
    fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Int(v) => Ok(*v),
            Value::Bool(v) => Ok(i64::from(*v)),
            Value::Float(v) => float_to_i64(*v),
            Value::Text(v) => v
                .trim()
                .parse()
                .map_err(|_| Error::msg(format!("`{v}` を整数として読めません"))),
            other => mismatch(other, "整数"),
        }
    }
}

/// 小数を整数に直す。範囲の外は断ります。
///
/// 以前は飽和キャストだったので、`NaN` が黙って `0` になり、大きすぎる値は
/// `i64::MAX` に張り付いていました。`f32` の範囲外チェックと同じ形にそろえます。
fn float_to_i64(v: f64) -> Result<i64> {
    if !v.is_finite() {
        return Err(Error::msg(format!("{v} は整数に直せません")));
    }
    // `i64::MAX as f64` は 2^63 に丸め上がるので、上の境目は `>=` で見る。
    if v < i64::MIN as f64 || v >= i64::MAX as f64 {
        return Err(Error::msg(format!("{v} は i64 に収まりません")));
    }
    Ok(v as i64)
}

/// `i64` から幅の狭い整数へ読み替える実装を、型ごとに作る。
///
/// 入らない値は、どの型に入れようとしたかを添えてエラーにします。
macro_rules! from_value_int {
    ($($t:ty),* $(,)?) => {
        $(impl FromValue for $t {
            fn from_value(value: &Value) -> Result<Self> {
                let v = i64::from_value(value)?;
                <$t>::try_from(v).map_err(|_| {
                    Error::msg(format!("{v} は {} に収まりません", stringify!($t)))
                })
            }
        })*
    };
}

// `#[derive(Model)]` が主キーに許す整数は、ここに実装があるものと同じにそろえます。
// そろっていないと、主キーの型を変えたときに読みにくいエラーが出ます。
from_value_int!(i8, i16, i32, isize, u8, u16, u32, usize);

/// `FromValue` を実装している整数の型の名前。
///
/// `bengara-macros` の `INTEGER_TYPES` と同じ並びにそろえます。
/// 片方にしか無い型を主キーにすると、コンパイルエラーになるためです。
#[cfg(test)]
const INTEGER_TYPES: &[&str] = &[
    "i8", "i16", "i32", "i64", "isize", "u8", "u16", "u32", "u64", "usize",
];

impl FromValue for u64 {
    fn from_value(value: &Value) -> Result<Self> {
        // `i64` に収まらない値は `Value::Text` で入る（`From<u64> for Value`）。
        // 往復できるように、文字列のときは先に u64 として読む。
        if let Value::Text(v) = value {
            if let Ok(parsed) = v.trim().parse::<u64>() {
                return Ok(parsed);
            }
        }
        let v = i64::from_value(value)?;
        u64::try_from(v).map_err(|_| Error::msg(format!("{v} は u64 に収まりません")))
    }
}

impl FromValue for f64 {
    fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Float(v) => Ok(*v),
            Value::Int(v) => Ok(*v as f64),
            Value::Text(v) => v
                .trim()
                .parse()
                .map_err(|_| Error::msg(format!("`{v}` を小数として読めません"))),
            other => mismatch(other, "小数"),
        }
    }
}

impl FromValue for f32 {
    fn from_value(value: &Value) -> Result<Self> {
        // `From<f32> for Value` が `f64` に広げるので、読み戻しも `f64` 経由にする。
        let v = f64::from_value(value)?;
        if v.is_finite() && (v < f32::MIN as f64 || v > f32::MAX as f64) {
            return Err(Error::msg(format!("{v} は f32 に収まりません")));
        }
        Ok(v as f32)
    }
}

impl FromValue for bool {
    fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Bool(v) => Ok(*v),
            Value::Int(v) => Ok(*v != 0),
            Value::Text(v) => match v.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => Ok(true),
                "0" | "false" | "no" | "off" | "" => Ok(false),
                _ => Err(Error::msg(format!("`{v}` を真偽として読めません"))),
            },
            other => mismatch(other, "真偽"),
        }
    }
}

impl FromValue for String {
    fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Text(v) => Ok(v.clone()),
            Value::Int(v) => Ok(v.to_string()),
            Value::Float(v) => Ok(v.to_string()),
            Value::Bool(v) => Ok(v.to_string()),
            Value::Bytes(v) => String::from_utf8(v.clone())
                .map_err(|_| Error::msg("バイト列を文字列として読めません")),
            other => mismatch(other, "文字列"),
        }
    }
}

impl FromValue for Vec<u8> {
    fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Bytes(v) => Ok(v.clone()),
            Value::Text(v) => Ok(v.as_bytes().to_vec()),
            other => mismatch(other, "バイト列"),
        }
    }
}

impl FromValue for Value {
    fn from_value(value: &Value) -> Result<Self> {
        Ok(value.clone())
    }
}

impl<T: FromValue> FromValue for Option<T> {
    fn from_value(value: &Value) -> Result<Self> {
        if value.is_null() {
            return Ok(None);
        }
        T::from_value(value).map(Some)
    }
}

/// 結果の1行。
///
/// ```ignore
/// let row = DB::table("posts").find(1).await?.unwrap();
/// let title: String = row.get("title")?;
/// ```
/// 列の名前は `Arc` で共有します。同じ問い合わせの行はすべて同じ名前なので、
/// 行ごとに `Vec<String>` を複製すると、1万行×20列で20万回の確保になります。
///
/// 列が多いときは、名前から位置を引く表も `Arc` で共有します。
/// 以前は `get("列")` ごとに列名を順に比べていたので、30列1万行の
/// `#[derive(Model)]` で450万回の文字列比較になっていました。
#[derive(Debug, Clone)]
pub struct Row {
    columns: Arc<[String]>,
    /// 列名から位置を引く表。列が少ないときは `None`（下の `column_index` を参照）。
    index: Option<Arc<ColumnIndex>>,
    values: Vec<Value>,
}

/// 列名から位置を引く表。
type ColumnIndex = std::collections::HashMap<String, usize>;

/// 表を作る列の数の下限。これ以下なら順に比べます。
///
/// 数列しかないときは、ハッシュを1回計算するより `==` を並べたほうが速いためです。
///
/// 表を作るのは `column_index`（下回りから行を受け取るときだけ）なので、
/// 機能フラグが無いビルドでは使いません。`#[cfg]` を外すと
/// `cargo clippy -p bengara --no-default-features` が「使われていない」で落ちます。
#[cfg(feature = "database")]
const INDEX_FROM: usize = 9;

/// 列名から位置を引く表を、1つの問い合わせにつき1回だけ作る。
///
/// `with_columns` は行ごとに呼ばれるので、直前に作った表を覚えておいて使い回します。
/// 覚えるときは `Arc` ごと持つので、番地が別の列名に使い回されることはありません
/// （`Arc::ptr_eq` で同じ割り当てだと確かめられます）。
#[cfg(feature = "database")]
fn column_index(columns: &Arc<[String]>) -> Option<Arc<ColumnIndex>> {
    use std::cell::RefCell;

    /// 直前に作った（列名, 表）の組。
    type Last = Option<(Arc<[String]>, Arc<ColumnIndex>)>;

    thread_local! {
        static LAST: RefCell<Last> = const { RefCell::new(None) };
    }

    if columns.len() < INDEX_FROM {
        return None;
    }
    LAST.with(|cell| {
        let mut slot = cell.borrow_mut();
        if let Some((names, index)) = slot.as_ref() {
            if Arc::ptr_eq(names, columns) {
                return Some(Arc::clone(index));
            }
        }
        let mut map = ColumnIndex::with_capacity(columns.len());
        for (position, name) in columns.iter().enumerate() {
            // 同じ名前が2つあるときは左を採る。順に比べていた頃と同じ。
            map.entry(name.clone()).or_insert(position);
        }
        let index = Arc::new(map);
        *slot = Some((Arc::clone(columns), Arc::clone(&index)));
        Some(index)
    })
}

/// 列の名前と値が同じなら同じ行とみなす。
///
/// 位置の表は列の名前から決まるだけなので、見ません。
impl PartialEq for Row {
    fn eq(&self, other: &Self) -> bool {
        self.columns == other.columns && self.values == other.values
    }
}

impl Default for Row {
    fn default() -> Self {
        Self {
            columns: Arc::from(Vec::new()),
            index: None,
            values: Vec::new(),
        }
    }
}

impl Row {
    /// 列の名前と値から作る。下回りのドライバが使います。
    ///
    /// 単発の1行なので、位置の表は作りません。
    pub fn new(columns: Vec<String>, values: Vec<Value>) -> Self {
        Self {
            columns: Arc::from(columns),
            index: None,
            values,
        }
    }

    /// 列の名前を共有して作る。同じ問い合わせの行をまとめて作るときに使います。
    ///
    /// 呼ぶのは下回りのドライバだけなので、ドライバを1つも入れないときは出番がありません。
    #[cfg(feature = "database")]
    pub(crate) fn with_columns(columns: Arc<[String]>, values: Vec<Value>) -> Self {
        let index = column_index(&columns);
        Self {
            columns,
            index,
            values,
        }
    }

    /// 列の値を型で取り出す。列が無い・型が合わないときはエラー。
    pub fn get<T: FromValue>(&self, column: &str) -> Result<T> {
        let value = self.value(column).ok_or_else(|| {
            Error::msg(format!(
                "列 `{column}` がありません（ある列: {}）",
                self.columns.join(", ")
            ))
        })?;
        T::from_value(value).map_err(|e| Error::msg(format!("列 `{column}` を読めません: {e}")))
    }

    /// 列の値を型で取り出す。読めなければ `None`。
    pub fn try_get<T: FromValue>(&self, column: &str) -> Option<T> {
        self.value(column).and_then(|v| T::from_value(v).ok())
    }

    /// 列の値をそのまま借りる。
    pub fn value(&self, column: &str) -> Option<&Value> {
        let position = match &self.index {
            Some(index) => *index.get(column)?,
            // 列が少ないときは表を作っていない。順に比べる。
            None => self.columns.iter().position(|c| c == column)?,
        };
        self.values.get(position)
    }

    /// 左から数えた位置で値を借りる。
    pub fn at(&self, index: usize) -> Option<&Value> {
        self.values.get(index)
    }

    /// 列の名前の一覧。
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// 列の数。
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// 列が1つも無いか。
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// JSON のオブジェクトに直す。
    pub fn to_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::with_capacity(self.columns.len());
        for (name, value) in self.columns.iter().zip(&self.values) {
            map.insert(name.clone(), value.to_json());
        }
        serde_json::Value::Object(map)
    }
}

impl serde::Serialize for Row {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.to_json().serialize(serializer)
    }
}

/// 更新系の結果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Affected {
    /// 変わった行の数。
    pub rows: u64,
    /// 自動採番された ID（`insert` のとき）。
    pub last_insert_id: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 数値と文字列は読み替えられる() {
        assert_eq!(i64::from_value(&Value::Text("12".into())).unwrap(), 12);
        assert_eq!(String::from_value(&Value::Int(12)).unwrap(), "12");
        assert!(bool::from_value(&Value::Int(1)).unwrap());
        assert!(!bool::from_value(&Value::Text("off".into())).unwrap());
    }

    #[test]
    fn 読めない値はエラーになる() {
        assert!(i64::from_value(&Value::Text("やきそば".into())).is_err());
        assert!(i64::from_value(&Value::Null).is_err());
        assert_eq!(Option::<i64>::from_value(&Value::Null).unwrap(), None);
    }

    #[test]
    fn 大きなu64は往復できる() {
        // i64 に収まらないので Text で入る。読み戻しても同じ値になること。
        let stored = Value::from(u64::MAX);
        assert_eq!(stored, Value::Text(u64::MAX.to_string()));
        assert_eq!(u64::from_value(&stored).unwrap(), u64::MAX);

        // 収まる値はこれまでどおり Int のまま。
        assert_eq!(Value::from(7u64), Value::Int(7));
        assert_eq!(u64::from_value(&Value::Int(7)).unwrap(), 7);

        // 負の数は u64 として読めない。
        assert!(u64::from_value(&Value::Int(-1)).is_err());
        assert!(u64::from_value(&Value::Text("やきそば".into())).is_err());
    }

    #[test]
    fn option_は_null_になる() {
        let none: Option<&str> = None;
        assert_eq!(Value::from(none), Value::Null);
        assert_eq!(Value::from(Some("x")), Value::Text("x".into()));
    }

    #[test]
    fn 行から列を読める() {
        let row = Row::new(
            vec!["id".into(), "title".into()],
            vec![Value::Int(1), Value::Text("やきそば".into())],
        );
        assert_eq!(row.get::<i64>("id").unwrap(), 1);
        assert_eq!(row.get::<String>("title").unwrap(), "やきそば");
        assert_eq!(row.len(), 2);
        assert!(row.get::<i64>("none").is_err());
        assert_eq!(row.try_get::<i64>("none"), None);
        assert_eq!(row.to_json()["title"], "やきそば");
    }

    #[test]
    fn 幅の狭い整数も読める() {
        // `#[derive(Model)]` が主キーに許す整数は、ここに実装があるものと同じ。
        assert_eq!(i8::from_value(&Value::Int(7)).unwrap(), 7i8);
        assert_eq!(i16::from_value(&Value::Int(7)).unwrap(), 7i16);
        assert_eq!(isize::from_value(&Value::Int(7)).unwrap(), 7isize);
        assert_eq!(u8::from_value(&Value::Int(7)).unwrap(), 7u8);
        assert_eq!(u16::from_value(&Value::Int(7)).unwrap(), 7u16);
        assert_eq!(u32::from_value(&Value::Int(7)).unwrap(), 7u32);
        assert_eq!(usize::from_value(&Value::Int(7)).unwrap(), 7usize);
    }

    #[test]
    fn 整数の型の一覧はマクロ側とそろっている() {
        // `bengara-macros` の `INTEGER_TYPES` と同じ並び。
        // 片方にしか無い型を主キーにすると、コンパイルエラーになる。
        assert_eq!(
            INTEGER_TYPES,
            &["i8", "i16", "i32", "i64", "isize", "u8", "u16", "u32", "u64", "usize"]
        );
        // 一覧に挙げた型には、ここに実装がある（無ければコンパイルできない）。
        assert_eq!(i8::from_value(&Value::Int(1)).unwrap(), 1i8);
        assert_eq!(i16::from_value(&Value::Int(1)).unwrap(), 1i16);
        assert_eq!(i32::from_value(&Value::Int(1)).unwrap(), 1i32);
        assert_eq!(i64::from_value(&Value::Int(1)).unwrap(), 1i64);
        assert_eq!(isize::from_value(&Value::Int(1)).unwrap(), 1isize);
        assert_eq!(u8::from_value(&Value::Int(1)).unwrap(), 1u8);
        assert_eq!(u16::from_value(&Value::Int(1)).unwrap(), 1u16);
        assert_eq!(u32::from_value(&Value::Int(1)).unwrap(), 1u32);
        assert_eq!(u64::from_value(&Value::Int(1)).unwrap(), 1u64);
        assert_eq!(usize::from_value(&Value::Int(1)).unwrap(), 1usize);
    }

    #[test]
    fn f32も読める() {
        // `From<f32> for Value` があるので、読み戻しもできるようにそろえる。
        assert_eq!(f32::from_value(&Value::from(1.5f32)).unwrap(), 1.5f32);
        assert_eq!(f32::from_value(&Value::Int(3)).unwrap(), 3.0f32);
        assert_eq!(f32::from_value(&Value::Text("2.5".into())).unwrap(), 2.5f32);

        // 入らない値は型の名前を添えて断る。
        let error = f32::from_value(&Value::Float(1e40))
            .unwrap_err()
            .to_string();
        assert!(error.contains("f32"), "{error}");
    }

    #[test]
    fn 入らない値は型の名前を添えて断る() {
        let error = i8::from_value(&Value::Int(1000)).unwrap_err().to_string();
        assert!(error.contains("i8"), "{error}");
        assert!(u8::from_value(&Value::Int(-1)).is_err());
    }

    #[test]
    fn 整数に直せない小数は断る() {
        // 以前は飽和キャストだったので、NaN が黙って 0 になっていた。
        let error = i64::from_value(&Value::Float(f64::NAN))
            .unwrap_err()
            .to_string();
        assert!(error.contains("整数に直せません"), "{error}");
        assert!(i64::from_value(&Value::Float(f64::INFINITY)).is_err());
        assert!(i64::from_value(&Value::Float(f64::NEG_INFINITY)).is_err());

        // 範囲の外は、入らない型の名前を添えて断る。
        let error = i64::from_value(&Value::Float(1e30))
            .unwrap_err()
            .to_string();
        assert!(error.contains("i64"), "{error}");
        assert!(i64::from_value(&Value::Float(-1e30)).is_err());

        // 幅の狭い整数も、同じエラーが通って出る。
        assert!(i32::from_value(&Value::Float(f64::NAN)).is_err());

        // 普通の小数はこれまでどおり読める。
        assert_eq!(i64::from_value(&Value::Float(2.9)).unwrap(), 2);
        assert_eq!(i64::from_value(&Value::Float(-2.9)).unwrap(), -2);
    }

    #[cfg(feature = "database")]
    #[test]
    fn 列が多い行も位置の表で読める() {
        // 列が多いときは名前から位置を引く表を作る。読める値は変わらないこと。
        let names: Vec<String> = (0..30).map(|i| format!("c{i}")).collect();
        let columns: Arc<[String]> = Arc::from(names);
        let values: Vec<Value> = (0..30).map(Value::Int).collect();

        // 同じ問い合わせの行は、列名の `Arc` を共有する。
        let first = Row::with_columns(Arc::clone(&columns), values.clone());
        let second = Row::with_columns(Arc::clone(&columns), values);
        for i in 0..30 {
            assert_eq!(first.get::<i64>(&format!("c{i}")).unwrap(), i);
            assert_eq!(second.get::<i64>(&format!("c{i}")).unwrap(), i);
        }
        assert!(first.get::<i64>("none").is_err());
        assert_eq!(first, second);

        // 列名が別の問い合わせに変わっても、前の表を使い回さない。
        let other: Arc<[String]> = Arc::from((0..30).map(|i| format!("d{i}")).collect::<Vec<_>>());
        let row = Row::with_columns(other, (0..30).map(|i| Value::Int(i + 100)).collect());
        assert_eq!(row.get::<i64>("d5").unwrap(), 105);
        assert_eq!(row.try_get::<i64>("c5"), None);
    }

    #[cfg(feature = "database")]
    #[test]
    fn 同じ名前の列は左を採る() {
        // 順に比べていた頃と同じ。表を作る側でも左を採ること。
        let mut names: Vec<String> = (0..30).map(|i| format!("c{i}")).collect();
        names[20] = "c0".to_string();
        let row = Row::with_columns(Arc::from(names), (0..30).map(Value::Int).collect());
        assert_eq!(row.get::<i64>("c0").unwrap(), 0);
    }
}
