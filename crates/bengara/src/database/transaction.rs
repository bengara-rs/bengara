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
    /// 問い合わせが途中で落とされた。もう使えません。
    Poisoned,
}

/// 取り出した中身を持つ入れ物。
///
/// **Drop で必ず状態を戻します。** 問い合わせの future が途中で落とされると
/// （`tokio::select!`・`timeout`・接続切断）、戻す処理まで進みません。
/// 手で戻していた頃は、そこで `Busy` のまま固まって `commit` も `rollback` も
/// できなくなっていました。
struct Borrowed<'a> {
    shared: &'a TxShared,
    raw: Option<Box<dyn RawTx>>,
    /// 問い合わせが終わりまで進んだか。Drop のときに見ます。
    done: bool,
}

impl Borrowed<'_> {
    /// 中身を借りる。
    fn raw(&mut self) -> &mut Box<dyn RawTx> {
        self.raw
            .as_mut()
            .expect("取り出した直後から Drop までは必ず入っている")
    }

    /// 中身を取り上げて終わりにする。Drop で `Finished` になります。
    fn into_finished(mut self) -> Box<dyn RawTx> {
        self.raw.take().expect("take() の直後なので必ず入っている")
    }
}

impl Drop for Borrowed<'_> {
    fn drop(&mut self) {
        let raw = self.raw.take();
        let mut guard = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        *guard = match raw {
            // 問い合わせが終わって中身が戻ってきた。次の問い合わせに使える。
            Some(raw) if self.done => State::Ready(raw),
            // 途中で落とされた。どこまで進んだか分からないので、もう使わない。
            // ここで `raw` を落とすので、下回りが巻き戻します。
            Some(_) => State::Poisoned,
            // `commit` か `rollback` が持って行った。
            None => State::Finished,
        };
    }
}

impl TxShared {
    pub(crate) fn driver(&self) -> Driver {
        self.driver
    }

    /// 中身を取り出す。戻すのは `Borrowed` の Drop に任せます。
    fn take(&self) -> Result<Borrowed<'_>> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match std::mem::replace(&mut *guard, State::Busy) {
            State::Ready(raw) => Ok(Borrowed {
                shared: self,
                raw: Some(raw),
                done: false,
            }),
            // すでに `Busy` だったので、`Busy` に置き換えたままでよい。
            State::Busy => Err(Error::msg(
                "このトランザクションは別の問い合わせを処理しています。1本ずつ await してください",
            )),
            State::Finished => {
                *guard = State::Finished;
                Err(Error::msg(
                    "このトランザクションは終わっています（commit か rollback が済んでいます）",
                ))
            }
            State::Poisoned => {
                *guard = State::Poisoned;
                Err(Error::msg(
                    "問い合わせが中断されたので、このトランザクションは使えません。\
                     中身は巻き戻されます。DB::begin() からやり直してください",
                ))
            }
        }
    }

    /// 終わりにする。
    fn finish(&self) -> Result<Box<dyn RawTx>> {
        Ok(self.take()?.into_finished())
    }

    pub(crate) async fn fetch_all(&self, sql: &str, bindings: &[Value]) -> Result<Vec<Row>> {
        let mut borrowed = self.take()?;
        let result = borrowed.raw().fetch_all(sql, bindings).await;
        // ここまで来れば中断されていない。
        borrowed.done = true;
        result
    }

    pub(crate) async fn execute(&self, sql: &str, bindings: &[Value]) -> Result<Affected> {
        let mut borrowed = self.take()?;
        let result = borrowed.raw().execute(sql, bindings).await;
        borrowed.done = true;
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

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    use super::super::backend::DbFuture;
    use super::*;

    /// テスト用の中身。SQL は受け取るだけで、すぐ返します。
    struct FakeTx;

    impl RawTx for FakeTx {
        fn fetch_all<'a>(
            &'a mut self,
            _sql: &'a str,
            _bindings: &'a [Value],
        ) -> DbFuture<'a, Vec<Row>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn execute<'a>(
            &'a mut self,
            _sql: &'a str,
            _bindings: &'a [Value],
        ) -> DbFuture<'a, Affected> {
            Box::pin(async { Ok(Affected::default()) })
        }

        fn commit(self: Box<Self>) -> DbFuture<'static, ()> {
            Box::pin(async { Ok(()) })
        }

        fn rollback(self: Box<Self>) -> DbFuture<'static, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    /// 返ってこない中身。問い合わせの中断を試すために使います。
    struct SlowTx;

    impl RawTx for SlowTx {
        fn fetch_all<'a>(
            &'a mut self,
            _sql: &'a str,
            _bindings: &'a [Value],
        ) -> DbFuture<'a, Vec<Row>> {
            Box::pin(async {
                std::future::pending::<()>().await;
                Ok(Vec::new())
            })
        }

        fn execute<'a>(
            &'a mut self,
            _sql: &'a str,
            _bindings: &'a [Value],
        ) -> DbFuture<'a, Affected> {
            Box::pin(async {
                std::future::pending::<()>().await;
                Ok(Affected::default())
            })
        }

        fn commit(self: Box<Self>) -> DbFuture<'static, ()> {
            Box::pin(async { Ok(()) })
        }

        fn rollback(self: Box<Self>) -> DbFuture<'static, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    fn shared(raw: Box<dyn RawTx>) -> TxShared {
        TxShared {
            driver: Driver::Sqlite,
            inner: Mutex::new(State::Ready(raw)),
        }
    }

    fn is_ready(shared: &TxShared) -> bool {
        let guard = shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        matches!(&*guard, State::Ready(_))
    }

    #[tokio::test]
    async fn 問い合わせを往復しても使い続けられる() {
        let shared = shared(Box::new(FakeTx));
        shared.fetch_all("select 1", &[]).await.expect("読める");
        assert!(is_ready(&shared), "問い合わせの後は使える状態に戻る");
        shared.execute("delete from t", &[]).await.expect("流せる");
        assert!(is_ready(&shared));
        shared.finish().expect("終わりにできる");
    }

    #[test]
    fn ガードはdropで状態を戻す() {
        let shared = shared(Box::new(FakeTx));
        {
            let mut borrowed = shared.take().expect("取り出せる");
            // 問い合わせが終わったところ。
            borrowed.done = true;
        }
        assert!(is_ready(&shared), "Drop で Ready に戻る");
    }

    #[test]
    fn 終わった後の操作は断られる() {
        let shared = shared(Box::new(FakeTx));
        drop(shared.finish().expect("終わりにできる"));
        // `Box<dyn RawTx>` は Debug を持たないので、`expect_err` ではなく場合分けで取り出す。
        let message = match shared.finish() {
            Ok(_) => panic!("2回目は断られるはず"),
            Err(error) => error.to_string(),
        };
        assert!(message.contains("終わっています"), "{message}");
    }

    #[test]
    fn 中断された問い合わせの後は理由が分かる() {
        let shared = shared(Box::new(SlowTx));
        // 返ってこない問い合わせを1回だけ進めて、途中で落とす。
        // `tokio::select!` や `timeout` で起きることと同じです。
        let mut pending = Box::pin(shared.fetch_all("select 1", &[]));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(matches!(pending.as_mut().poll(&mut cx), Poll::Pending));
        drop(pending);

        let message = match shared.finish() {
            Ok(_) => panic!("中断された後は使えないはず"),
            Err(error) => error.to_string(),
        };
        assert!(message.contains("中断"), "{message}");
    }
}
