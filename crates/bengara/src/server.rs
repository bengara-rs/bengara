//! HTTP サーバー。
//!
//! 下回りは hyper（axum 経由）です。`bengara` の API の裏に隠してあるので、
//! 後から差し替えられます。

use std::future::{Future, IntoFuture};
use std::net::SocketAddr;

use axum::extract::State;

use crate::application::Application;
use crate::error::{Error, Result};
use crate::http::{Request, Response};

/// 受け取る本文の上限（2 MiB）。超えると 413 を返します。
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// サーバーを起動して、止まるまで待つ。
pub(crate) fn serve(app: Application, host: &str, port: u16) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(Error::Io)?;

    runtime.block_on(async move {
        let addr = format!("{host}:{port}");
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| Error::msg(format!("{addr} で待ち受けられません: {e}")))?;
        let local = listener.local_addr().map_err(Error::Io)?;
        println!(
            "bengara: http://{} で待ち受けます（Ctrl+C で終了）",
            display_addr(local)
        );

        let router = axum::Router::new()
            .fallback(axum::routing::any(dispatch))
            .with_state(app);

        // 合図を受け取った時刻を、待ち受けを閉じる側と打ち切る側の両方に伝える。
        let (signalled, mut wait_signal) = tokio::sync::watch::channel(false);
        let serving = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                shutdown_signal().await;
                // 送れなくても（受け手がもういなくても）止める処理は続ける。
                let _ = signalled.send(true);
            })
            .into_future();

        let timeout = shutdown_timeout();
        tokio::pin!(serving);

        // 合図が来るまでは、ただ処理を続ける。
        //
        // `wait_for` は、送り手が消えたときもエラーで返ります。送り手が消えるのは
        // 合図の処理が終わったときだけなので、どちらでも「合図は来た」と扱って構いません。
        let served = tokio::select! {
            result = &mut serving => result,
            _ = wait_signal.wait_for(|signalled| *signalled) => {
                drain(&mut serving, timeout).await
            }
        };
        served.map_err(Error::Io)?;

        println!("bengara: 終了しました");
        Ok(())
    })
}

/// 合図を受け取った後、処理中のリクエストが終わるのを待つ。
///
/// 上限時間を過ぎたら、警告を出して打ち切ります。
/// axum に打ち切りの仕組みは無いので、ここで待つのをやめることで打ち切ります。
async fn drain<F>(
    serving: &mut std::pin::Pin<&mut F>,
    timeout: Option<std::time::Duration>,
) -> F::Output
where
    F: Future<Output = std::io::Result<()>>,
{
    let Some(limit) = timeout else {
        tracing::info!("処理中のリクエストが終わるまで待ちます（上限なし）");
        return serving.as_mut().await;
    };

    tracing::info!(
        "処理中のリクエストが終わるまで待ちます（上限 {} 秒）",
        limit.as_secs()
    );
    match tokio::time::timeout(limit, serving.as_mut()).await {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!("{} 秒たったので、残りの接続を打ち切ります", limit.as_secs());
            Ok(())
        }
    }
}

/// `0.0.0.0` のままだと分かりにくいので、ブラウザで開けるアドレスにして見せる。
fn display_addr(addr: SocketAddr) -> String {
    if addr.ip().is_unspecified() {
        format!("127.0.0.1:{}", addr.port())
    } else {
        addr.to_string()
    }
}

/// axum から来たリクエストを `bengara` の形に直し、結果を axum の形に戻す。
async fn dispatch(
    State(app): State<Application>,
    request: axum::extract::Request,
) -> axum::response::Response {
    let (parts, body) = request.into_parts();

    let bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => bytes.to_vec(),
        Err(_) => {
            let error = Error::http(413, "本文が大きすぎます");
            return into_axum(crate::http::response::error_response(
                &error,
                crate::http::response::RenderOptions::new(false, false),
            ));
        }
    };

    let headers = parts
        .headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_ascii_lowercase(), v.to_string()))
        })
        .collect();

    let req = Request::new(parts.method.as_str(), parts.uri.path())
        .with_query(parts.uri.query().unwrap_or_default())
        .with_headers(headers)
        .with_body(bytes);

    let started = std::time::Instant::now();
    let method = req.method().to_string();
    let path = req.full_path();
    let response = app.handle(req).await;
    tracing::info!(
        "{method} {path} -> {} ({} ms)",
        response.status(),
        started.elapsed().as_millis()
    );

    into_axum(response)
}

/// `bengara` のレスポンスを axum の形に変える。
fn into_axum(response: Response) -> axum::response::Response {
    let mut builder = axum::http::Response::builder().status(response.status());
    for (name, value) in response.headers() {
        builder = builder.header(name, value);
    }
    match builder.body(axum::body::Body::from(response.body().to_vec())) {
        Ok(response) => response,
        Err(e) => {
            // ヘッダーの値が HTTP として不正だったときの保険。
            tracing::error!("レスポンスを組み立てられませんでした: {e}");
            axum::http::Response::builder()
                .status(500)
                .body(axum::body::Body::from("Internal Server Error"))
                .expect("最小限のレスポンスは必ず作れる")
        }
    }
}

/// 停止の合図を待つ。
///
/// 受け付けるものは OS で違いますが、 **受け取った後の動きは同じ**です
/// （待ち受けを閉じる → 処理中を終える → 上限時間で打ち切る）。
///
/// | OS      | 受け付けるもの                                                       |
/// |---------|----------------------------------------------------------------------|
/// | Unix    | SIGTERM、SIGINT（Ctrl+C）                                            |
/// | Windows | Ctrl+C、Ctrl+Break、コンソールを閉じる操作、ログオフ、シャットダウン |
///
/// Windows のサービス停止要求は受け取れません（サービスとして登録する仕組みが要ります）。
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!("Ctrl+C を待てませんでした: {e}");
            // 待てないなら、ここで返して他の合図に任せる。
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    {
        let terminate = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut signal) => {
                    signal.recv().await;
                }
                Err(e) => {
                    tracing::error!("SIGTERM を待てませんでした: {e}");
                    std::future::pending::<()>().await;
                }
            }
        };
        tokio::select! {
            _ = ctrl_c => tracing::info!("SIGINT を受け取りました"),
            _ = terminate => tracing::info!("SIGTERM を受け取りました"),
        }
    }

    #[cfg(windows)]
    {
        use tokio::signal::windows;

        /// 1つの合図を待つ。用意できなければ、永久に待って他に任せる。
        macro_rules! wait {
            ($make:expr, $label:literal) => {
                async {
                    match $make {
                        Ok(mut signal) => {
                            signal.recv().await;
                            $label
                        }
                        Err(e) => {
                            tracing::error!("{} を待てませんでした: {e}", $label);
                            std::future::pending::<&str>().await
                        }
                    }
                }
            };
        }

        let reason = tokio::select! {
            _ = ctrl_c => "Ctrl+C",
            r = wait!(windows::ctrl_break(), "Ctrl+Break") => r,
            r = wait!(windows::ctrl_close(), "コンソールを閉じる操作") => r,
            r = wait!(windows::ctrl_logoff(), "ログオフ") => r,
            r = wait!(windows::ctrl_shutdown(), "シャットダウン") => r,
        };
        tracing::info!("{reason} を受け取りました");
    }

    #[cfg(not(any(unix, windows)))]
    ctrl_c.await;
}

/// 停止の上限時間の既定（秒）。
const DEFAULT_SHUTDOWN_TIMEOUT: u64 = 30;

/// 停止の上限時間を言い方に直す（`about` で出す用）。
pub(crate) fn shutdown_timeout_label() -> String {
    label_for(shutdown_timeout())
}

fn label_for(timeout: Option<std::time::Duration>) -> String {
    match timeout {
        Some(limit) => format!("{} 秒", limit.as_secs()),
        None => "なし（終わるまで待つ）".to_string(),
    }
}

/// 停止の上限時間（秒）。`0` は無制限。
///
/// `APP_SHUTDOWN_TIMEOUT` で変えられます。数値でなければ既定を使い、警告を出します。
fn shutdown_timeout() -> Option<std::time::Duration> {
    parse_timeout(std::env::var("APP_SHUTDOWN_TIMEOUT").ok().as_deref())
}

/// 設定の文字を上限時間に直す。`None` は「設定が無い」の意味です。
fn parse_timeout(raw: Option<&str>) -> Option<std::time::Duration> {
    let secs = match raw {
        Some(raw) => match raw.trim().parse::<u64>() {
            Ok(secs) => secs,
            Err(_) => {
                tracing::warn!(
                    "APP_SHUTDOWN_TIMEOUT の値 `{raw}` が数値ではありません。\
                     {DEFAULT_SHUTDOWN_TIMEOUT} 秒にします"
                );
                DEFAULT_SHUTDOWN_TIMEOUT
            }
        },
        None => DEFAULT_SHUTDOWN_TIMEOUT,
    };

    if secs == 0 {
        None
    } else {
        Some(std::time::Duration::from_secs(secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn 待ち受けるアドレスの見せ方() {
        let any: SocketAddr = "0.0.0.0:8000".parse().unwrap();
        assert_eq!(display_addr(any), "127.0.0.1:8000");

        let local: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        assert_eq!(display_addr(local), "127.0.0.1:8080");
    }

    #[test]
    fn 上限時間の既定は30秒() {
        // この行は環境変数を触らない。既定の値そのものを確かめる。
        assert_eq!(parse_timeout(None), Some(Duration::from_secs(30)));
    }

    #[test]
    fn 上限時間を変えられる() {
        assert_eq!(parse_timeout(Some("5")), Some(Duration::from_secs(5)));
        assert_eq!(parse_timeout(Some("  90 ")), Some(Duration::from_secs(90)));
    }

    #[test]
    fn 上限時間の0は無制限() {
        assert_eq!(parse_timeout(Some("0")), None);
    }

    #[test]
    fn 数値でなければ既定に落ちる() {
        assert_eq!(parse_timeout(Some("すぐ")), Some(Duration::from_secs(30)));
        assert_eq!(parse_timeout(Some("")), Some(Duration::from_secs(30)));
        assert_eq!(parse_timeout(Some("-1")), Some(Duration::from_secs(30)));
    }

    #[test]
    fn 上限時間の言い方() {
        assert_eq!(label_for(Some(Duration::from_secs(30))), "30 秒");
        assert_eq!(label_for(None), "なし（終わるまで待つ）");
    }

    /// 上限時間を過ぎたら打ち切ることを、終わらない future で確かめる。
    #[tokio::test]
    async fn 終わらない処理は打ち切る() {
        let never = async { std::future::pending::<std::io::Result<()>>().await };
        tokio::pin!(never);

        let started = std::time::Instant::now();
        let result = drain(&mut never, Some(Duration::from_millis(50))).await;

        assert!(result.is_ok(), "打ち切りは成功として扱う");
        assert!(started.elapsed() >= Duration::from_millis(50));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "待ち続けてはいけない"
        );
    }

    /// 上限より早く終われば、そのまま返る。
    #[tokio::test]
    async fn 先に終われば待たない() {
        let done = async { Ok::<(), std::io::Error>(()) };
        tokio::pin!(done);

        let started = std::time::Instant::now();
        drain(&mut done, Some(Duration::from_secs(30)))
            .await
            .expect("成功する");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// 上限なしでも、終われば返る。
    #[tokio::test]
    async fn 上限なしでも終われば返る() {
        let done = async { Ok::<(), std::io::Error>(()) };
        tokio::pin!(done);
        drain(&mut done, None).await.expect("成功する");
    }
}
