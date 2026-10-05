//! ハンドラ（コントローラのメソッド）を型で受け取る仕組み。
//!
//! 受け付ける形は次の2つです。
//!
//! ```ignore
//! async fn index() -> Result<Response>
//! async fn show(req: Request) -> Result<Response>
//! ```

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;

use crate::error::Result;
use crate::http::{Request, Response};

/// ハンドラとミドルウェアの戻り値。
///
/// `Middleware` トレイトを自分で実装するときに使います。
/// 普通は `async fn` を書くだけなので、この名前を出すことはありません。
pub type BoxFuture = Pin<Box<dyn Future<Output = Result<Response>> + Send>>;

/// ルートに登録できる関数。
///
/// `Args` は引数の形を表す目印です。利用者が書くことはありません。
pub trait Handler<Args>: Send + Sync + 'static {
    /// リクエストを処理する。
    fn handle(&self, req: Request) -> BoxFuture;
}

impl<F, Fut> Handler<()> for F
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Response>> + Send + 'static,
{
    fn handle(&self, _req: Request) -> BoxFuture {
        Box::pin(self())
    }
}

impl<F, Fut> Handler<(Request,)> for F
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Response>> + Send + 'static,
{
    fn handle(&self, req: Request) -> BoxFuture {
        Box::pin(self(req))
    }
}

/// 引数の形を消して1つの型にまとめる。
pub(crate) trait ErasedHandler: Send + Sync + 'static {
    fn call(&self, req: Request) -> BoxFuture;
}

pub(crate) struct Erased<H, Args> {
    handler: H,
    _args: PhantomData<fn(Args)>,
}

impl<H, Args> Erased<H, Args>
where
    H: Handler<Args>,
    Args: 'static,
{
    pub(crate) fn new(handler: H) -> Self {
        Self {
            handler,
            _args: PhantomData,
        }
    }
}

impl<H, Args> ErasedHandler for Erased<H, Args>
where
    H: Handler<Args>,
    Args: 'static,
{
    fn call(&self, req: Request) -> BoxFuture {
        self.handler.handle(req)
    }
}
