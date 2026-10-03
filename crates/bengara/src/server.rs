//! HTTP サーバー。
//!
//! 下回りは hyper（axum 経由）です。`bengara` の API の裏に隠してあるので、
//! 後から差し替えられます。

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

        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(Error::Io)?;

        println!("bengara: 終了しました");
        Ok(())
    })
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
            return into_axum(crate::http::response::error_response(&error, false));
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

/// Ctrl+C と SIGTERM を待つ。
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!("Ctrl+C を待てませんでした: {e}");
        }
    };

    #[cfg(unix)]
    {
        let terminate = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut signal) => {
                    signal.recv().await;
                }
                Err(e) => tracing::error!("SIGTERM を待てませんでした: {e}"),
            }
        };
        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate => {}
        }
    }

    #[cfg(not(unix))]
    ctrl_c.await;
}
