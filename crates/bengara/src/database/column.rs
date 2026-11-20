//! 型付きの列。
//!
//! `#[model(table = "posts", columns)]` と書くと、`#[derive(Model)]` が
//! 列ごとに `Post::TITLE` のような定数を作ります。定数の型は [`Column<M, T>`] です。
//!
//! ```
//! use bengara::{Carbon, Model};
//!
//! #[derive(Model)]
//! #[model(table = "posts", columns)]
//! struct Post {
//!     id: i64,
//!     status: String,
//!     views: i64,
//!     published_at: Option<Carbon>,
//! }
//!
//! let query = Post::query()
//!     .where_(Post::STATUS, "published")
//!     .where_op(Post::VIEWS, ">", 100)
//!     .where_op(Post::PUBLISHED_AT, "<", Carbon::now());
//! let (sql, _) = query.to_sql();
//! assert_eq!(
//!     sql,
//!     "select * from \"posts\" where \"status\" = ? and \"views\" > ? and \"published_at\" < ?"
//! );
//! ```
//!
//! 型付きの列には、列の型に合う値しか渡せません。合わないとコンパイルで止まります。
//! 文字列の列名（`"title"`）も今までどおり使えます。そちらは値の型を見ません。
//!
//! 止まる例です（`VIEWS` は `i64` の列）。
//!
//! ```compile_fail,E0277
//! # use bengara::Model;
//! # #[derive(Model)]
//! # #[model(table = "posts", columns)]
//! # struct Post { id: i64, views: i64 }
//! Post::query().where_(Post::VIEWS, "abc");
//! ```
//!
//! 別のモデルの列は渡せません。
//!
//! ```compile_fail,E0277
//! # use bengara::Model;
//! # #[derive(Model)]
//! # #[model(table = "posts", columns)]
//! # struct Post { id: i64, views: i64 }
//! # #[derive(Model)]
//! # #[model(table = "users", columns)]
//! # struct User { id: i64, email: String }
//! Post::query().where_(User::EMAIL, "a@example.com");
//! ```
//!
//! 欠けるおそれのある広げ方（`u64` を `i64` の列へ）はできません。
//!
//! ```compile_fail,E0277
//! # use bengara::Model;
//! # #[derive(Model)]
//! # #[model(table = "posts", columns)]
//! # struct Post { id: i64, views: i64 }
//! let n: u64 = 5;
//! Post::query().where_(Post::VIEWS, n);
//! ```
//!
//! 日時の列（`Option<Carbon>`）に整数は渡せません。
//!
//! ```compile_fail,E0277
//! # use bengara::{Carbon, Model};
//! # #[derive(Model)]
//! # #[model(table = "posts", columns)]
//! # struct Post { id: i64, published_at: Option<Carbon> }
//! Post::query().where_(Post::PUBLISHED_AT, 5);
//! ```

use std::borrow::Cow;
use std::marker::PhantomData;

use super::value::{IntoValue, Value};
use crate::support::Carbon;

/// モデル `M` の、値の型が `T` の列。
///
/// 普通は `#[model(columns)]` が作る定数（`Post::TITLE`）として使います。
/// 列名は [`name`](Self::name) で取れます。`select` のように一覧で列を渡す所は
/// 文字列で受けるので、`Post::TITLE.name()` を渡してください。
pub struct Column<M, T> {
    name: &'static str,
    // `fn() -> ..` にしておくと、M と T が Send / Sync でなくても列は Send / Sync になる。
    _marker: PhantomData<fn() -> (M, T)>,
}

impl<M, T> Column<M, T> {
    /// 列名から作る。名前の検査は、問い合わせのときに文字列の列名と同じく行います。
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _marker: PhantomData,
        }
    }

    /// 列名。
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

// derive で書くと `M: Clone` などの制約が付くので、手で書く。
impl<M, T> Clone for Column<M, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M, T> Copy for Column<M, T> {}

impl<M, T> std::fmt::Debug for Column<M, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Column").field(&self.name).finish()
    }
}

/// 文字列で書いた列が受け取る値の印。値は作れません。
///
/// 文字列の列名は値の型を見ないので、[`IntoValue`] なら何でも渡せます。
pub enum AnyValue {}

/// 列として渡せるもの。
///
/// 文字列（`&str`・`String`・`Cow<str>`）と [`Column`] が当たります。
/// `Accepts` は、その列に渡せる値の型です（文字列なら [`AnyValue`]）。
pub trait ColumnRef {
    /// 渡せる値の型。
    type Accepts;
    /// 列名。
    fn column_name(&self) -> &str;
}

impl ColumnRef for str {
    type Accepts = AnyValue;
    fn column_name(&self) -> &str {
        self
    }
}

impl ColumnRef for String {
    type Accepts = AnyValue;
    fn column_name(&self) -> &str {
        self
    }
}

impl ColumnRef for Cow<'_, str> {
    type Accepts = AnyValue;
    fn column_name(&self) -> &str {
        self
    }
}

impl<C: ColumnRef + ?Sized> ColumnRef for &C {
    type Accepts = C::Accepts;
    fn column_name(&self) -> &str {
        (**self).column_name()
    }
}

impl<M, T> ColumnRef for Column<M, T> {
    type Accepts = T;
    fn column_name(&self) -> &str {
        self.name
    }
}

/// モデル `M` の列として渡せるもの。`ModelQuery<M>` の条件で使います。
///
/// 文字列はどのモデルにも渡せます。[`Column`] は同じモデルの列だけです。
pub trait ModelColumn<M>: ColumnRef {}

impl<M> ModelColumn<M> for str {}
impl<M> ModelColumn<M> for String {}
impl<M> ModelColumn<M> for Cow<'_, str> {}
impl<M, C: ModelColumn<M> + ?Sized> ModelColumn<M> for &C {}
impl<M, T> ModelColumn<M> for Column<M, T> {}

/// 列の型 `A` に渡せる値。
///
/// | 列 | 渡せる値 |
/// |---|---|
/// | 文字列の列名 | [`IntoValue`] なら何でも |
/// | `Column<_, T>` | `T` と、`T` に欠けなく広げられる型 |
/// | `Column<_, Option<T>>` | 上に加えて、その `Option` |
///
/// 広げられる組は、小さい整数から大きい整数（符号付きの列へは符号なしも）、
/// `f32` から `f64`、`&str` / `&String` から `String`、`&[u8]` から `Vec<u8>` です。
/// `u64` を `i64` の列へ渡すような、欠けるおそれのある組はコンパイルで止まります。
///
/// 型付きの列に [`Value`] をそのまま渡す口はありません。文字列の列名を使ってください。
pub trait Bind<A> {
    /// `Value` に変える。
    fn bind(self) -> Value;
}

impl<V: IntoValue> Bind<AnyValue> for V {
    fn bind(self) -> Value {
        self.into_value()
    }
}

/// 同じ型どうし。`T`・`Option<T>` の列に `T` を、`Option<T>` の列に `Option<T>` を渡せる。
macro_rules! bind_same {
    ($($t:ty),* $(,)?) => {
        $(
            impl Bind<$t> for $t {
                fn bind(self) -> Value {
                    self.into_value()
                }
            }
            impl Bind<Option<$t>> for $t {
                fn bind(self) -> Value {
                    self.into_value()
                }
            }
            impl Bind<Option<$t>> for Option<$t> {
                fn bind(self) -> Value {
                    self.into_value()
                }
            }
        )*
    };
}

bind_same!(
    i8,
    i16,
    i32,
    i64,
    isize,
    u8,
    u16,
    u32,
    u64,
    usize,
    bool,
    f32,
    f64,
    String,
    Vec<u8>,
    Carbon,
);

/// 欠けなく広げられる組。`列の型 => 渡せる型, ..` の形で 1 行ずつ書く。
///
/// 借用の型（`&str` など）の行は、先頭に `'a,` を付けてライフタイムの引数を足す。
macro_rules! bind_widen {
    ($lt:lifetime, $to:ty => $($from:ty),+) => {
        $(bind_widen!(@impl [$lt] $to, $from);)+
    };
    ($to:ty => $($from:ty),+) => {
        $(bind_widen!(@impl [] $to, $from);)+
    };
    (@impl [$($lt:lifetime)?] $to:ty, $from:ty) => {
        impl<$($lt)?> Bind<$to> for $from {
            fn bind(self) -> Value {
                self.into_value()
            }
        }
        impl<$($lt)?> Bind<Option<$to>> for $from {
            fn bind(self) -> Value {
                self.into_value()
            }
        }
        impl<$($lt)?> Bind<Option<$to>> for Option<$from> {
            fn bind(self) -> Value {
                self.into_value()
            }
        }
    };
}

bind_widen!(i16 => i8, u8);
bind_widen!(i32 => i8, i16, u8, u16);
bind_widen!(i64 => i8, i16, i32, u8, u16, u32);
bind_widen!(u16 => u8);
bind_widen!(u32 => u8, u16);
bind_widen!(u64 => u8, u16, u32);
bind_widen!(f64 => f32);
bind_widen!('a, String => &'a str, &'a String);
bind_widen!('a, Vec<u8> => &'a [u8]);

#[cfg(test)]
mod tests {
    use super::*;

    /// 型の印だけに使う。Clone も Debug も持たない。
    struct NotClone;

    fn bind<A>(value: impl Bind<A>) -> Value {
        value.bind()
    }

    fn name<C: ColumnRef>(column: C) -> String {
        column.column_name().to_string()
    }

    #[test]
    fn 列は名前を持ち_型に制約なしで複製できる() {
        const TITLE: Column<NotClone, NotClone> = Column::new("title");
        let copy = TITLE;
        assert_eq!(copy.name(), "title");
        assert_eq!(name(TITLE), "title");
        assert_eq!(format!("{TITLE:?}"), "Column(\"title\")");
    }

    #[test]
    fn 文字列の列名はどの書き方でも列名を返す() {
        let owned = String::from("title");
        let plain: &str = "title";
        assert_eq!(name(plain), "title");
        let double: &&str = &plain;
        assert_eq!(name(double), "title");
        assert_eq!(name(&owned), "title");
        assert_eq!(name(owned.clone()), "title");
        assert_eq!(name(Cow::Borrowed("title")), "title");
    }

    #[test]
    fn 広げた値は元の値のまま入る() {
        assert_eq!(bind::<i64>(7u32), Value::Int(7));
        assert_eq!(bind::<Option<i64>>(Some(7i8)), Value::Int(7));
        assert_eq!(bind::<Option<i64>>(None::<i32>), Value::Null);
        assert_eq!(bind::<f64>(1.5f32), Value::Float(1.5));
        assert_eq!(bind::<String>("a"), Value::Text("a".into()));
        assert_eq!(bind::<String>(&String::from("a")), Value::Text("a".into()));
        assert_eq!(bind::<Vec<u8>>(&[1u8, 2][..]), Value::Bytes(vec![1, 2]));
        assert_eq!(
            bind::<Option<Carbon>>(Carbon::from_timestamp(0)),
            Value::from(Carbon::from_timestamp(0))
        );
        assert_eq!(bind::<AnyValue>(Value::Int(1)), Value::Int(1));
    }
}
