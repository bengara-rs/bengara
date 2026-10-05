//! 下回りのドライバが満たすトレイト。利用者には見せません。
//!
//! ここから先（`sqlite.rs`）だけが sqlx を知っています。上の層は SQL の文字列と
//! `Value` の一覧しか扱いません。

use std::future::Future;
use std::pin::Pin;

use super::grammar::Driver;
use super::value::{Affected, Row, Value};
use crate::error::Result;

/// 箱に入れた非同期の結果。トレイトの中で `async fn` を使えないため。
pub(crate) type DbFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// 接続プール1本ぶん。
pub(crate) trait Backend: Send + Sync + 'static {
    /// どのデータベースか。
    fn driver(&self) -> Driver;

    /// 行を取る。
    fn fetch_all<'a>(&'a self, sql: &'a str, bindings: &'a [Value]) -> DbFuture<'a, Vec<Row>>;

    /// 行を変える。
    fn execute<'a>(&'a self, sql: &'a str, bindings: &'a [Value]) -> DbFuture<'a, Affected>;

    /// トランザクションを始める。
    fn begin(&self) -> DbFuture<'_, Box<dyn RawTx>>;

    /// 表の名前の一覧（システムの表は除く）。
    fn table_names(&self) -> DbFuture<'_, Vec<String>>;
}

/// 始まっているトランザクション。
///
/// `&mut self` を取るので、同時に2つの問い合わせは走りません。
/// `commit` を呼ばずに落ちた場合は、下回りが巻き戻します。
pub(crate) trait RawTx: Send + Sync {
    /// 行を取る。
    fn fetch_all<'a>(&'a mut self, sql: &'a str, bindings: &'a [Value]) -> DbFuture<'a, Vec<Row>>;

    /// 行を変える。
    fn execute<'a>(&'a mut self, sql: &'a str, bindings: &'a [Value]) -> DbFuture<'a, Affected>;

    /// 確定する。
    fn commit(self: Box<Self>) -> DbFuture<'static, ()>;

    /// 巻き戻す。
    fn rollback(self: Box<Self>) -> DbFuture<'static, ()>;
}
