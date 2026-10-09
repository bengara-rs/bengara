//! HTTP サーバー。
//!
//! 下回りは hyper（axum 経由）です。`bengara` の API の裏に隠してあるので、
//! 後から差し替えられます。

use std::future::{Future, IntoFuture};
use std::net::SocketAddr;

use axum::extract::{ConnectInfo, State};

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
            .with_state(app)
            // 接続元のアドレスを `dispatch` で受け取れるようにする。
            // これが無いと `X-Forwarded-For` を信じるしかなくなり、回数制限を偽装で回せる。
            .into_make_service_with_connect_info::<SocketAddr>();

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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: axum::extract::Request,
) -> axum::response::Response {
    let (parts, body) = request.into_parts();

    // アプリへ渡す前に返すときも、相手が JSON を欲しがっているなら JSON で返す。
    // 判定に使うヘッダーはこの時点で手元にあるので、先に求めておく。
    let wants_json = crate::application::wants_json_with(|name| {
        parts
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
    });

    // 先に長さの申告を見る。上限を超えていると分かっているときだけ 413 にする。
    if too_large(declared_length(&parts.headers)) {
        return early_error(Error::http(413, "本文が大きすぎます"), wants_json);
    }

    let bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => bytes.to_vec(),
        Err(_) => {
            // ここに来るのは、途中で接続が切れた・chunked が壊れていた・
            // 長さの申告が無いまま上限を超えた、のどれかです。
            // 「大きすぎる」と分かっているのは申告を見た上の分岐だけなので、ここは 400。
            return early_error(Error::http(400, "本文を読み取れませんでした"), wants_json);
        }
    };

    let headers = parts
        .headers
        .iter()
        .map(|(name, value)| {
            // 読めないバイトは置換文字に落として**残します**。まるごと捨てると、
            // 1本の `Cookie` ヘッダーに同梱されたセッション Cookie まで消えて、
            // 毎回ログアウトする・CSRF が 419 になる、という症状になります。
            (
                name.as_str().to_ascii_lowercase(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();

    let req = Request::new(parts.method.as_str(), parts.uri.path())
        .with_query(parts.uri.query().unwrap_or_default())
        .with_headers(headers)
        .with_body(bytes)
        .with_remote_addr(peer);

    let started = std::time::Instant::now();
    let method = req.method().to_string();
    let target = log_target(&req);
    let response = app.handle(req).await;
    tracing::info!(
        "{method} {target} -> {} ({} ms)",
        response.status(),
        started.elapsed().as_millis()
    );

    into_axum(response)
}

/// ログに出す行き先。クエリは残すが、秘密になりうる値は伏せる。
///
/// 署名付き URL の `signature` やパスワードの再設定の `token` は、**それ自体が
/// 認証情報**です。そのままログに書くと、ログを読める人がリンクを使い回せます。
/// 伏せる名前の判定は、セッションに覚えない入力と同じ一覧（`request::is_sensitive`）です。
fn log_target(req: &Request) -> String {
    let query = req.query_string();
    if query.is_empty() {
        return req.path().to_string();
    }
    let mut out = String::with_capacity(req.path().len() + query.len() + 1);
    out.push_str(req.path());
    out.push('?');
    for (index, pair) in query.split('&').enumerate() {
        if index > 0 {
            out.push('&');
        }
        match pair.split_once('=') {
            Some((name, value)) => {
                out.push_str(name);
                out.push('=');
                if is_sensitive_name(name) {
                    out.push_str("***");
                } else {
                    out.push_str(value);
                }
            }
            // `=` が無い組はそのまま出す（値が無いので伏せるものがない）。
            None => out.push_str(pair),
        }
    }
    out
}

/// 伏せる名前か。**符号化した名前も元に戻してから見ます。**
///
/// `?%74oken=abc` は `?token=abc` と同じものです。生のまま見ると素通りするので、
/// `%` が入っているときだけ元に戻して確かめます（入っていなければ確保も増えません）。
/// ログに出すのは生の名前のままです。
fn is_sensitive_name(name: &str) -> bool {
    if crate::http::request::is_sensitive(name) {
        return true;
    }
    name.contains('%') && crate::http::request::is_sensitive(&crate::http::percent::decode(name))
}

/// `Content-Length` の申告を読む。無い・読めないときは `None`。
fn declared_length(headers: &axum::http::HeaderMap) -> Option<usize> {
    headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<usize>().ok())
}

/// 申告された長さが上限を超えているか。
///
/// 申告が無ければ、読んでみるまで分かりません。読んで失敗したときは
/// 「大きすぎる」と決めつけず 400 にします（接続が切れただけのこともあります）。
fn too_large(declared: Option<usize>) -> bool {
    declared.is_some_and(|length| length > MAX_BODY_BYTES)
}

/// アプリに渡す前に返すエラー。
///
/// 設定（`APP_DEBUG`）はまだ読めないので詳細は出しませんが、
/// **JSON で返すかどうかはヘッダーから分かる**ので、そこは尊重します。
fn early_error(error: Error, wants_json: bool) -> axum::response::Response {
    into_axum(crate::http::response::error_response(
        &error,
        crate::http::response::RenderOptions::new(false, wants_json),
    ))
}

/// `bengara` のレスポンスを axum の形に変える。
///
/// ヘッダーは**1本ずつ**積みます。以前はまとめて積んでいたので、1本でも
/// 組み立てられないと応答のヘッダーが全部消え、素の 500 になっていました
/// （直前にセッションが付けた `Set-Cookie` まで失われました）。
/// いまは組み立てに失敗したヘッダーだけを飛ばします。
fn into_axum(response: Response) -> axum::response::Response {
    let mut headers = axum::http::HeaderMap::with_capacity(response.headers().len());
    for (name, value) in response.headers() {
        let Ok(header_name) = axum::http::HeaderName::from_bytes(name.as_bytes()) else {
            tracing::warn!("ヘッダーの名前 {name} が使えないので、この1本を飛ばします");
            continue;
        };
        let Ok(header_value) = axum::http::HeaderValue::from_str(value) else {
            tracing::warn!("ヘッダー {name} の値が使えないので、この1本を飛ばします");
            continue;
        };
        // `set-cookie` のように同じ名前を何本も送るので `append`。
        headers.append(header_name, header_value);
    }

    let mut builder = axum::http::Response::builder().status(response.status());
    if let Some(slot) = builder.headers_mut() {
        *slot = headers;
    }
    // 本文は所有ごと渡す。`Cow::Borrowed`（埋め込んだ `public/`）はコピーされない。
    let body = match response.into_body() {
        std::borrow::Cow::Borrowed(bytes) => axum::body::Body::from(bytes),
        std::borrow::Cow::Owned(bytes) => axum::body::Body::from(bytes),
    };
    match builder.body(body) {
        Ok(response) => response,
        Err(e) => {
            // 最後の保険。ヘッダーは1本ずつ検査済みなので、ここには来ないはずです。
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
    fn 組み立てられないヘッダーだけを飛ばす() {
        // 以前は1本でも駄目だと応答のヘッダーを全部捨てていました。
        // 値は `with_header` が先に検査するので、ここでは名前が不正な1本で確かめます。
        let response = Response::text("ok")
            .with_added_header("set-cookie", "session=abc")
            .with_added_header("x dame", "1") // 名前に空白。HeaderName にできない
            .with_added_header("x-ok", "1");
        let axum_response = into_axum(response);

        assert_eq!(axum_response.status(), 200);
        let headers = axum_response.headers();
        assert_eq!(
            headers.get("set-cookie").map(|v| v.as_bytes()),
            Some(&b"session=abc"[..]),
            "Set-Cookie は残る"
        );
        assert_eq!(headers.get("x-ok").map(|v| v.as_bytes()), Some(&b"1"[..]));
        assert_eq!(
            headers.len(),
            3,
            "残るのは良い3本だけ（content-type・set-cookie・x-ok）。駄目な1本だけ飛ばす"
        );
        assert!(headers.get("content-type").is_some());
    }

    #[test]
    fn 同じ名前のヘッダーは何本でも渡る() {
        let response = Response::text("ok")
            .with_added_header("set-cookie", "a=1")
            .with_added_header("set-cookie", "b=2");
        let axum_response = into_axum(response);
        assert_eq!(
            axum_response.headers().get_all("set-cookie").iter().count(),
            2
        );
    }

    #[test]
    fn 本文の長さの申告を読む() {
        let mut headers = axum::http::HeaderMap::new();
        assert_eq!(declared_length(&headers), None);
        headers.insert(
            axum::http::header::CONTENT_LENGTH,
            "123".parse().expect("ヘッダーの値"),
        );
        assert_eq!(declared_length(&headers), Some(123));
        headers.insert(
            axum::http::header::CONTENT_LENGTH,
            "abc".parse().expect("ヘッダーの値"),
        );
        assert_eq!(declared_length(&headers), None, "読めなければ分からない");
    }

    #[test]
    fn 上限を超える申告だけ413にする() {
        assert!(too_large(Some(MAX_BODY_BYTES + 1)));
        assert!(!too_large(Some(MAX_BODY_BYTES)));
        assert!(!too_large(Some(0)));
        assert!(
            !too_large(None),
            "申告が無ければ、読んで失敗しても 413 にはしない"
        );
    }

    #[test]
    fn ログのクエリは秘密を伏せる() {
        let req = Request::new("GET", "/unsubscribe")
            .with_query("user=12&signature=9f3c&token=abc&page=2");
        assert_eq!(
            log_target(&req),
            "/unsubscribe?user=12&signature=***&token=***&page=2"
        );
    }

    #[test]
    fn ログは符号化した名前も伏せる() {
        // `%74oken` は `token` と同じもの。生のまま見ると素通りしていた。
        let req = Request::new("GET", "/reset").with_query("%74oken=abc&%73ignature=9f3c&page=2");
        assert_eq!(
            log_target(&req),
            "/reset?%74oken=***&%73ignature=***&page=2",
            "名前は生のまま出し、値だけ伏せる"
        );
    }

    #[test]
    fn クエリが無ければパスだけ出す() {
        assert_eq!(log_target(&Request::new("GET", "/posts")), "/posts");
        // `=` が無い組はそのまま出す。
        let req = Request::new("GET", "/posts").with_query("draft");
        assert_eq!(log_target(&req), "/posts?draft");
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
