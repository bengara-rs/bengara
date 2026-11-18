//! 受け取る本文。上限・読み取りの時間・少しずつ読む口（[`BodyStream`]）。
//!
//! 本文の受け取り方は**ルートごと**に決まります。照合した後、ミドルウェアより前に
//! [`Request::receive_body`](crate::http::Request) が次のどちらかにします。
//!
//! | ルート | 本文 |
//! |---|---|
//! | ふつう | 全部メモリに読んでから渡す（`req.body()`・`req.input()` で読む） |
//! | `.stream_body()` 付き | 読まずに渡す（`req.body_stream()?` で少しずつ読む） |
//!
//! 上限は `.max_body(n)` があればその値、無ければ `APP_MAX_BODY_BYTES`（既定 2 MiB）です。
//!
//! 下回り（hyper の本文）の型は外に見せません。テストはメモリの本文で同じ道を通ります。

use std::pin::Pin;
use std::time::Duration;

use axum::body::Bytes;

use crate::error::{Error, Result};

/// 受け取る本文の上限の既定（2 MiB）。`APP_MAX_BODY_BYTES` で変えられます。
pub(crate) const DEFAULT_MAX_BODY_BYTES: u64 = 2 * 1024 * 1024;

/// 全部読むときに、読み終えるまでに掛けてよい時間。過ぎると 408 を返します。
///
/// 2 MiB をこの時間で送れない相手は、回線が細いのではなく、わざと遅く送って
/// 接続を握り続けている可能性のほうが高いです。
/// 大きな本文は、時間の数え方が違う `.stream_body()` のルートで受けてください。
/// 接続そのものの待ち時間（ヘッダーが来るまで・idle）と同時接続数の上限は
/// 持ちません。前段のプロキシで掛けてください（`docs/deployment.md`）。
pub(crate) const BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// 少しずつ読むときに、次のかたまりを待ってよい時間。過ぎると 408 を返します。
///
/// 全部を読む時間ではなく、**かたまりとかたまりの間**の時間です。大きな本文は
/// 回線によって何分もかかるので、全体には上限を置きません。止まったままの
/// 相手だけを切ります。
pub(crate) const BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// ルートが決めた本文の受け取り方。`Application` が照合の直後に作ります。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BodyPolicy {
    /// 受け取る本文の上限（バイト）。
    pub(crate) limit: u64,
    /// 読まずにハンドラへ渡すか（`.stream_body()`）。
    pub(crate) stream: bool,
}

/// まだ読んでいない本文。`server.rs`（ソケット）とテスト（メモリ）が載せます。
pub(crate) struct PendingBody(Source);

impl PendingBody {
    /// ソケットから届く本文。
    pub(crate) fn incoming(body: axum::body::Body) -> Self {
        Self(Source::Incoming(body))
    }

    /// 手元にある本文（テスト用のクライアント）。
    pub(crate) fn memory(bytes: Vec<u8>) -> Self {
        Self(Source::Memory(std::collections::VecDeque::from([
            Bytes::from(bytes),
        ])))
    }

    /// 手元にある本文を、いくつかのかたまりに分けて届ける（テスト用）。
    #[cfg(test)]
    pub(crate) fn memory_chunks(chunks: Vec<Vec<u8>>) -> Self {
        Self(Source::Memory(
            chunks.into_iter().map(Bytes::from).collect(),
        ))
    }
}

/// 本文の出どころ。
enum Source {
    Incoming(axum::body::Body),
    /// 積んだかたまりを順に返し、尽きたら終わり（`None`）を返します。
    Memory(std::collections::VecDeque<Bytes>),
}

impl Source {
    /// 次のかたまり。終わりなら `None`。読めなければ `Some(Err(()))`。
    ///
    /// 中身の無いかたまりと、データでないもの（trailer）は飛ばします。
    /// 届いた形（`Bytes`）のまま返し、ここでは写しません。
    async fn next(&mut self) -> Option<std::result::Result<Bytes, ()>> {
        match self {
            Self::Memory(chunks) => loop {
                let chunk = chunks.pop_front()?;
                if !chunk.is_empty() {
                    return Some(Ok(chunk));
                }
            },
            Self::Incoming(body) => loop {
                use axum::body::HttpBody;
                let frame = std::future::poll_fn(|cx| Pin::new(&mut *body).poll_frame(cx)).await?;
                match frame {
                    Ok(frame) => match frame.into_data() {
                        Ok(data) if data.is_empty() => continue,
                        Ok(data) => return Some(Ok(data)),
                        Err(_trailers) => continue,
                    },
                    Err(_) => return Some(Err(())),
                }
            },
        }
    }
}

/// 上限を超えた（413）。
pub(crate) fn too_large() -> Error {
    Error::http(413, "本文が大きすぎます")
}

/// 途中で切れた・chunked が壊れていた（400）。
fn unreadable() -> Error {
    Error::http(400, "本文を読み取れませんでした")
}

/// 時間がかかりすぎた（408）。
fn too_slow() -> Error {
    Error::http(408, "本文の受け取りに時間がかかりすぎました")
}

/// 本文を全部読む。上限を超えたら 413、読めなければ 400、時間切れは 408。
///
/// 長さの申告（`declared`）は、確保の目安にだけ使います。申告が上限を超えて
/// いるときは、呼ぶ側が読む前に 413 にしています。
pub(crate) async fn read_all(
    body: PendingBody,
    limit: u64,
    declared: Option<u64>,
) -> Result<Vec<u8>> {
    let mut source = body.0;
    let reading = async move {
        // かたまりが 1 つで終わる本文（小さなフォームや JSON のほとんど）は、
        // つなぐ先を作らずにそのかたまりを返す。2 つ目が来たときに初めてつなぐ。
        let mut first: Option<Bytes> = None;
        let mut buffer: Vec<u8> = Vec::new();
        let mut total: u64 = 0;
        while let Some(chunk) = source.next().await {
            let chunk = chunk.map_err(|()| unreadable())?;
            total = total.saturating_add(chunk.len() as u64);
            if total > limit {
                return Err(too_large());
            }
            // `next` は空のかたまりを返さないので、`buffer` が空ならまだ 1 つ目。
            if first.is_none() && buffer.is_empty() {
                first = Some(chunk);
                continue;
            }
            if let Some(head) = first.take() {
                // 申告を信じすぎない。確保するのは上限と 64 KiB の小さいほうまで。
                let hint = declared.unwrap_or(0).min(limit).min(64 * 1024);
                let hint = usize::try_from(hint).unwrap_or(0);
                buffer.reserve(hint.max(head.len() + chunk.len()));
                buffer.extend_from_slice(&head);
            }
            buffer.extend_from_slice(&chunk);
        }
        Ok(match first {
            // 自分だけが持つ `Vec` 由来の `Bytes` なら、写さずに `Vec` へ戻ります。
            Some(only) => Vec::from(only),
            None => buffer,
        })
    };
    match tokio::time::timeout(BODY_READ_TIMEOUT, reading).await {
        Ok(result) => result,
        Err(_elapsed) => Err(too_slow()),
    }
}

/// 少しずつ読む本文。`req.body_stream()?` で取り出します。
///
/// ```ignore
/// // routes/web.rs
/// Route::post("/uploads", UploadController::store)
///     .stream_body()
///     .max_body(512 * 1024 * 1024);
///
/// // ハンドラ
/// let mut body = req.body_stream()?;
/// while let Some(chunk) = body.chunk().await? {
///     Storage::disk("local").append_bytes("uploads/a.bin", chunk).await?;
/// }
/// ```
///
/// | 起きたこと | `chunk()` の結果 |
/// |---|---|
/// | 次のかたまりが届いた | `Ok(Some(バイト列))` |
/// | 本文の終わり | `Ok(None)` |
/// | 読んだ合計が上限を超えた | `Err`（413） |
/// | 次のかたまりが 30 秒届かない | `Err`（408） |
/// | 途中で接続が切れた | `Err`（400） |
///
/// 一度 `Err` を返したら、あとは同じ `Err` を返し続けます。
/// エラーはそのまま `?` で返せば、その状態コードの応答になります。
pub struct BodyStream {
    source: Source,
    limit: u64,
    received: u64,
    finished: bool,
    /// 一度失敗したときの状態コードと文。続けて呼ばれても同じものを返します。
    failed: Option<(u16, String)>,
}

impl BodyStream {
    pub(crate) fn new(body: PendingBody, limit: u64) -> Self {
        Self {
            source: body.0,
            limit,
            received: 0,
            finished: false,
            failed: None,
        }
    }

    /// 次のかたまりを読む。終わりなら `Ok(None)`。
    ///
    /// かたまりの大きさは相手と回線しだいです（数 KiB〜数百 KiB）。
    pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>> {
        if let Some((status, message)) = &self.failed {
            return Err(Error::http(*status, message.clone()));
        }
        if self.finished {
            return Ok(None);
        }
        let next = match tokio::time::timeout(BODY_IDLE_TIMEOUT, self.source.next()).await {
            Ok(next) => next,
            Err(_elapsed) => return Err(self.fail(too_slow())),
        };
        match next {
            None => {
                self.finished = true;
                Ok(None)
            }
            Some(Err(())) => Err(self.fail(unreadable())),
            Some(Ok(chunk)) => {
                let total = self.received.saturating_add(chunk.len() as u64);
                if total > self.limit {
                    return Err(self.fail(too_large()));
                }
                self.received = total;
                Ok(Some(Vec::from(chunk)))
            }
        }
    }

    /// ここまでに読んだバイト数。
    pub fn received(&self) -> u64 {
        self.received
    }

    /// 受け取る本文の上限（バイト）。
    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// 失敗を覚えて、そのエラーを返す。
    fn fail(&mut self, error: Error) -> Error {
        if let Error::Http { status, message } = &error {
            self.failed = Some((*status, message.clone()));
        }
        error
    }
}

impl std::fmt::Debug for BodyStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BodyStream")
            .field("received", &self.received)
            .field("limit", &self.limit)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

/// `APP_MAX_BODY_BYTES` を読む。無い・`0`・読めないときは既定（2 MiB）。
///
/// `Application` が起動時に 1 回だけ読みます。
pub(crate) fn max_body_from_env() -> u64 {
    let bytes: u64 = crate::env("APP_MAX_BODY_BYTES", DEFAULT_MAX_BODY_BYTES);
    if bytes == 0 {
        tracing::warn!(
            "APP_MAX_BODY_BYTES に 0 は使えません。既定の {DEFAULT_MAX_BODY_BYTES} バイトにします"
        );
        return DEFAULT_MAX_BODY_BYTES;
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream_of(bytes: &[u8], limit: u64) -> BodyStream {
        BodyStream::new(PendingBody::memory(bytes.to_vec()), limit)
    }

    #[tokio::test]
    async fn 全部読む() {
        let body = read_all(PendingBody::memory(b"abc".to_vec()), 10, None)
            .await
            .unwrap();
        assert_eq!(body, b"abc");
    }

    #[tokio::test]
    async fn 上限ちょうどは読めて_超えたら413() {
        assert!(read_all(PendingBody::memory(vec![0; 10]), 10, None)
            .await
            .is_ok());
        let error = read_all(PendingBody::memory(vec![0; 11]), 10, None)
            .await
            .unwrap_err();
        assert_eq!(error.status(), 413);
    }

    #[tokio::test]
    async fn いくつかのかたまりをつないで読む() {
        let chunks = || vec![b"ab".to_vec(), Vec::new(), b"cd".to_vec(), b"e".to_vec()];
        let body = read_all(PendingBody::memory_chunks(chunks()), 5, Some(5))
            .await
            .unwrap();
        assert_eq!(body, b"abcde");
        // 上限は合計で数える。最後のかたまりで超えたら 413。
        let error = read_all(PendingBody::memory_chunks(chunks()), 4, None)
            .await
            .unwrap_err();
        assert_eq!(error.status(), 413);
        // 申告が実際より小さくても、全部読む。
        let body = read_all(PendingBody::memory_chunks(chunks()), 100, Some(1))
            .await
            .unwrap();
        assert_eq!(body, b"abcde");

        // 少しずつ読む口でも、空のかたまりは飛ばす。
        let mut stream = BodyStream::new(PendingBody::memory_chunks(chunks()), 5);
        assert_eq!(stream.chunk().await.unwrap().as_deref(), Some(&b"ab"[..]));
        assert_eq!(stream.chunk().await.unwrap().as_deref(), Some(&b"cd"[..]));
        assert_eq!(stream.chunk().await.unwrap().as_deref(), Some(&b"e"[..]));
        assert_eq!(stream.chunk().await.unwrap(), None);
        assert_eq!(stream.received(), 5);
    }

    #[tokio::test]
    async fn 空の本文は空() {
        let body = read_all(PendingBody::memory(Vec::new()), 10, None)
            .await
            .unwrap();
        assert!(body.is_empty());
        let mut stream = stream_of(b"", 10);
        assert_eq!(stream.chunk().await.unwrap(), None);
    }

    #[tokio::test]
    async fn 少しずつ読んで終わりはnone() {
        let mut stream = stream_of(b"hello", 10);
        assert_eq!(
            stream.chunk().await.unwrap().as_deref(),
            Some(&b"hello"[..])
        );
        assert_eq!(stream.received(), 5);
        assert_eq!(stream.limit(), 10);
        assert_eq!(stream.chunk().await.unwrap(), None);
        assert_eq!(stream.chunk().await.unwrap(), None, "終わりのあとも None");
    }

    #[tokio::test]
    async fn 少しずつ読んでも合計が上限を超えたら413を返し続ける() {
        let mut stream = stream_of(&[0; 11], 10);
        assert_eq!(stream.chunk().await.unwrap_err().status(), 413);
        assert_eq!(
            stream.chunk().await.unwrap_err().status(),
            413,
            "同じエラー"
        );
        assert_eq!(stream.received(), 0, "超えたかたまりは数えない");
    }

    #[test]
    fn 失敗は状態コードごとに覚える() {
        let mut stream = stream_of(b"", 10);
        assert_eq!(stream.fail(too_slow()).status(), 408);
        assert_eq!(stream.failed.as_ref().map(|f| f.0), Some(408));
        let mut stream = stream_of(b"", 10);
        stream.fail(unreadable());
        assert_eq!(stream.failed.as_ref().map(|f| f.0), Some(400));
    }

    #[test]
    fn 中身はデバッグ表示に出さない() {
        let shown = format!("{:?}", stream_of(b"secret", 10));
        assert!(!shown.contains("secret"), "{shown}");
        assert!(shown.contains("limit"), "{shown}");
    }
}
