//! トランザクション。ガード方式です（決定記録 #021）。
//!
//! ```ignore
//! let tx = DB::begin().await?;
//! tx.table("posts").insert(&[("title", "x".into())]).await?;
//! tx.commit().await?;
//! ```
//!
//! `commit()` を呼ばずに落ちた場合は巻き戻します。

use std::sync::{Arc, Mutex};

use super::backend::{Backend, RawTx};
use super::grammar::Driver;
use super::query::QueryBuilder;
use super::value::{Affected, Row, Value};
use super::Source;
use crate::error::{Error, Result};

/// 始まっているトランザクション。
///
/// `tx.table(...)` で作ったクエリは、このトランザクションの中で動きます。
pub struct Transaction {
    shared: Arc<TxShared>,
}

/// クエリビルダと共有する中身。
///
/// 問い合わせのたびに中身を取り出し、終わったら戻します。
/// **ロックを `await` の向こうへ持ち越しません。**
pub(crate) struct TxShared {
    driver: Driver,
    inner: Mutex<State>,
}

/// トランザクションの状態。
enum State {
    /// 使える。
    Ready(Box<dyn RawTx>),
    /// いま別の問い合わせが走っている。
    Busy,
    /// `commit` か `rollback` が済んだ。
    Finished,
}

impl TxShared {
    pub(crate) fn driver(&self) -> Driver {
        self.driver
    }

    /// 中身を取り出す。
    fn take(&self) -> Result<Box<dyn RawTx>> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match std::mem::replace(&mut *guard, State::Busy) {
            State::Ready(raw) => Ok(raw),
            State::Busy => {
                *guard = State::Busy;
                Err(Error::msg(
                    "このトランザクションは別の問い合わせを処理しています。1本ずつ await してください",
                ))
            }
            State::Finished => {
                *guard = State::Finished;
                Err(Error::msg(
                    "このトランザクションは終わっています（commit か rollback が済んでいます）",
                ))
            }
        }
    }

    /// 取り出した中身を戻す。
    fn put_back(&self, raw: Box<dyn RawTx>) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        *guard = State::Ready(raw);
    }

    /// 終わりにする。
    fn finish(&self) -> Result<Box<dyn RawTx>> {
        let raw = self.take()?;
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        *guard = State::Finished;
        Ok(raw)
    }

    pub(crate) async fn fetch_all(&self, sql: &str, bindings: &[Value]) -> Result<Vec<Row>> {
        let mut raw = self.take()?;
        let result = raw.fetch_all(sql, bindings).await;
        self.put_back(raw);
        result
    }

    pub(crate) async fn execute(&self, sql: &str, bindings: &[Value]) -> Result<Affected> {
        let mut raw = self.take()?;
        let result = raw.execute(sql, bindings).await;
        self.put_back(raw);
        result
    }
}

impl Transaction {
    /// 接続からトランザクションを始める。
    pub(crate) async fn start(found: Arc<dyn Backend>) -> Result<Self> {
        let driver = found.driver();
        let raw = found.begin().await?;
        Ok(Self {
            shared: Arc::new(TxShared {
                driver,
                inner: Mutex::new(State::Ready(raw)),
            }),
        })
    }

    /// このトランザクションの中でクエリを組み立てる。
    pub fn table(&self, table: impl Into<String>) -> QueryBuilder {
        QueryBuilder::new(Source::Tx(Arc::clone(&self.shared)), table)
    }

    /// このトランザクションを指す `Source`。モデルのクエリに渡します。
    pub(crate) fn source(&self) -> Source {
        Source::Tx(Arc::clone(&self.shared))
    }

    /// SQL を直接投げて行を受け取る。
    pub async fn select(&self, sql: &str, bindings: &[Value]) -> Result<Vec<Row>> {
        self.shared.fetch_all(sql, bindings).await
    }

    /// SQL を直接投げる。
    pub async fn statement(&self, sql: &str, bindings: &[Value]) -> Result<Affected> {
        self.shared.execute(sql, bindings).await
    }

    /// つながっているデータベースの種類。
    pub fn driver(&self) -> Driver {
        self.shared.driver
    }

    /// 確定する。
    pub async fn commit(self) -> Result<()> {
        let raw = self.shared.finish()?;
        raw.commit().await
    }

    /// 巻き戻す。
    pub async fn rollback(self) -> Result<()> {
        let raw = self.shared.finish()?;
        raw.rollback().await
    }
}

impl std::fmt::Debug for Transaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transaction")
            .field("driver", &self.shared.driver)
            .finish()
    }
}
