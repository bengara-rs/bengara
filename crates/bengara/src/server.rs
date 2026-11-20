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

/// 上限で打ち切った後、裏のスレッドが終わるのを待つ時間。
///
/// 打ち切った時点で走っている `spawn_blocking`（静的ファイル・セッション・
/// キャッシュ・ストレージ・SMTP）と、`await` を挟まないハンドラは、途中で
/// 止められません。待たずに捨てると書きかけのファイルが残りやすいので、
/// 数秒だけ待ちます。過ぎたら待つのをやめてプロセスを終えます。
const CUT_OFF_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// サーバーを起動して、止まるまで待つ。
pub(crate) fn serve(app: Application, host: &str, port: u16) -> Result<()> {
    // 埋め込んだテンプレートの構文を、待ち受ける前に確かめる。誤りがあれば起動しない。
    crate::view::check_embedded()?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(Error::Io)?;

    let cut_off = runtime.block_on(async move {
        let addr = format!("{host}:{port}");
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| Error::msg(format!("{addr} で待ち受けられません: {e}")))?;
        let local = listener.local_addr().map_err(Error::Io)?;
        println!(
            "bengara: http://{} で待ち受けます（Ctrl+C で終了）",
            display_addr(local)
        );

        // WebSocket の状態（停止の合図・同時接続の数）。切り替えた後の接続は
        // axum の停止の待ちに入らないので、bengara が自分で数えて待つ。
        let sockets = crate::websocket::ServerState::from_env();
        let router = router(app, sockets.clone());

        // 配信のために「このプロセスはサーバー」と覚え、database なら中継を立てる。
        crate::broadcast::mark_server();
        #[cfg(feature = "websocket")]
        crate::broadcast::start_relay();

        // 合図を受け取ったことを、待ち受けを閉じる側と打ち切る側の両方に伝える。
        let (signalled, mut wait_signal) = tokio::sync::watch::channel(false);

        // 合図の見張りは、待ち受けとは別のタスクに置く。
        //
        // 待ち受けを閉じる future の中で 1 回だけ待っていたころは、受け取った時点で
        // 見張りが終わり、**2 回目の合図が捨てられました。** 上限（既定 30 秒、
        // `APP_SHUTDOWN_TIMEOUT=0` なら無期限）まで待つしかなく、`queue:work`
        // （2 回目で強制終了）と動きが食い違っていました。
        tokio::spawn(watch_shutdown(move || {
            // 送れなくても（受け手がもういなくても）止める処理は続ける。
            let _ = signalled.send(true);
        }));

        let mut closing = wait_signal.clone();
        let serving = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                // 送り手が消えたとき（見張りのタスクが落ちたとき）も閉じる。
                let _ = closing.wait_for(|signalled| *signalled).await;
            })
            .into_future();

        let timeout = shutdown_timeout();
        tokio::pin!(serving);

        // 合図が来るまでは、ただ処理を続ける。
        //
        // `wait_for` は、送り手が消えたときもエラーで返ります。送り手が消えるのは
        // 合図の処理が終わったときだけなので、どちらでも「合図は来た」と扱って構いません。
        let served = tokio::select! {
            result = &mut serving => Some(result),
            _ = wait_signal.wait_for(|signalled| *signalled) => {
                // HTTP の処理中のものと、WebSocket の接続を同時に待つ。
                // WebSocket は全接続に 1001 を送り、上限と 5 秒の短いほうだけ待つ。
                let (served, ()) = tokio::join!(
                    drain(&mut serving, timeout),
                    crate::websocket::shutdown(&sockets, timeout)
                );
                served
            }
        };
        match served {
            Some(result) => {
                result.map_err(Error::Io)?;
                Ok::<bool, Error>(false)
            }
            // 上限で打ち切った。
            None => Ok(true),
        }
    })?;

    finish(runtime, cut_off, CUT_OFF_GRACE);
    println!("bengara: 終了しました");
    Ok(())
}

/// ランタイムを片付ける。
///
/// **打ち切ったときだけ、待つ時間に上限を付けます。** `Runtime` をそのまま
/// 捨てると、tokio は走っている `spawn_blocking` が終わるまで**無期限に**
/// 待ちます（`BlockingPool::drop` が `shutdown(None)` を呼ぶため）。
/// 上限で打ち切ったのに、固まった NFS や返事の来ない SMTP を待ってプロセスが
/// 終わらない、という形になっていました。
///
/// 打ち切っていないとき（全部のリクエストが終わったとき）は、これまでどおり
/// 終わるまで待ちます。残っているのは書き込みの後始末なので、切りません。
fn finish(runtime: tokio::runtime::Runtime, cut_off: bool, grace: std::time::Duration) {
    if cut_off {
        runtime.shutdown_timeout(grace);
    } else {
        drop(runtime);
    }
}

/// 合図を受け取った後、処理中のリクエストが終わるのを待つ。
///
/// 上限時間を過ぎたら、警告を出して打ち切ります。**打ち切ったときは `None`** を
/// 返します（呼ぶ側がランタイムの片付け方を変えるため。[`finish`]）。
/// axum に打ち切りの仕組みは無いので、ここで待つのをやめることで打ち切ります。
///
/// 待っている間に 2 回目の合図が来たら、[`watch_shutdown`] が強制終了します。
async fn drain<F>(
    serving: &mut std::pin::Pin<&mut F>,
    timeout: Option<std::time::Duration>,
) -> Option<F::Output>
where
    F: Future<Output = std::io::Result<()>>,
{
    let Some(limit) = timeout else {
        tracing::info!(
            "処理中のリクエストが終わるまで待ちます（上限なし。もう一度合図を送ると強制終了します）"
        );
        return Some(serving.as_mut().await);
    };

    tracing::info!(
        "処理中のリクエストが終わるまで待ちます（上限 {} 秒。もう一度合図を送ると強制終了します）",
        limit.as_secs()
    );
    match tokio::time::timeout(limit, serving.as_mut()).await {
        Ok(result) => Some(result),
        Err(_) => {
            tracing::warn!("{} 秒たったので、残りの接続を打ち切ります", limit.as_secs());
            None
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

/// `dispatch` が受け取る状態。アプリと、WebSocket の状態。
#[derive(Clone)]
struct Served {
    app: Application,
    sockets: std::sync::Arc<crate::websocket::ServerState>,
}

/// 待ち受けに渡す axum の組み立て。`serve` とテストが使います。
pub(crate) fn router(
    app: Application,
    sockets: std::sync::Arc<crate::websocket::ServerState>,
) -> axum::extract::connect_info::IntoMakeServiceWithConnectInfo<axum::Router, SocketAddr> {
    axum::Router::new()
        .fallback(axum::routing::any(dispatch))
        .with_state(Served { app, sockets })
        // 接続元のアドレスを `dispatch` で受け取れるようにする。
        // これが無いと `X-Forwarded-For` を信じるしかなくなり、回数制限を偽装で回せる。
        .into_make_service_with_connect_info::<SocketAddr>()
}

/// axum から来たリクエストを `bengara` の形に直し、結果を axum の形に戻す。
async fn dispatch(
    State(Served { app, sockets }): State<Served>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: axum::extract::Request,
) -> axum::response::Response {
    #[cfg_attr(not(feature = "websocket"), allow(unused_mut))]
    let (mut parts, body) = request.into_parts();

    // WebSocket への切り替えの口。hyper は切り替えの要求（`Connection: upgrade`）にだけ入れる。
    // 捨てずに `Request` へ載せ、`on_upgrade` で取り出す。
    // ふつうのリクエストでは何も載せないので、確保は増えない。
    #[cfg(feature = "websocket")]
    let handoff = parts
        .extensions
        .remove::<hyper::upgrade::OnUpgrade>()
        .map(|on_upgrade| crate::websocket::Handoff {
            on_upgrade: Some(on_upgrade),
            server: sockets,
        });
    #[cfg(not(feature = "websocket"))]
    let _ = sockets;

    // 101 を直すとき（`websocket::after_response`）も、相手が JSON を欲しがっているなら
    // JSON で返す。判定に使うヘッダーはこの時点で手元にあるので、先に求めておく。
    let wants_json = crate::application::wants_json_with(|name| {
        parts
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
    });

    // 本文はここでは読まない。上限と読み方（全部・少しずつ）はルートごとに決まるので、
    // 照合した後に `Request::receive_body` が受け取ります（`http/body.rs`）。
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
        .with_pending_body(crate::http::body::PendingBody::incoming(body))
        .with_remote_addr(peer);
    #[cfg(feature = "websocket")]
    let req = match handoff {
        Some(handoff) => req.with_upgrade(handoff),
        None => req,
    };

    // アクセスログに出す文字列は、ログが有効なときだけ作る。
    // `RUST_LOG=warn` で動かしているときに、リクエストごとの確保を 2 本減らせます。
    let logging = tracing::enabled!(tracing::Level::INFO);
    let started = std::time::Instant::now();
    let method = logging.then(|| req.method().to_string());
    // 行き先はルートを引いた**あと**で作る（`Application::handle_logged`）。
    // パス引数の名前が分からないと、`/reset/{token}` の値を伏せられないためです。
    let (response, target) = app.handle_logged(req, logging).await;
    // 101 なら WebSocket の接続を動かすタスクを立てる（切り替えが載っていなければ 500 に直す）。
    // ログより先に呼ぶのは、直した後のステータスを残すためです。
    let response = crate::websocket::after_response(response, wants_json);
    if let (Some(method), Some(target)) = (method, target) {
        tracing::info!(
            "{method} {target} -> {} ({} ms)",
            response.status(),
            started.elapsed().as_millis()
        );
    }

    into_axum(response)
}

/// ログに出す行き先。クエリは残すが、秘密になりうる値は伏せる。
///
/// 署名付き URL の `signature` やパスワードの再設定の `token` は、**それ自体が
/// 認証情報**です。そのままログに書くと、ログを読める人がリンクを使い回せます。
/// 伏せる名前の判定は、セッションに覚えない入力と同じ一覧（`request::is_sensitive`）です。
///
/// **パス引数も伏せます。** `/reset/{token}` のようなルートでは、パスの中に秘密が入ります。
/// 名前が伏せる一覧に当たるパス引数の値を `***` にします（`mask_path_params`）。
/// そのため、ルートを引いたあとのリクエスト（`Application::handle_logged` の中）で呼びます。
pub(crate) fn log_target(req: &Request) -> String {
    let path = match req.route_path() {
        // 伏せる名前のパス引数があるときだけ組み直す。ふつうは確保を増やさない。
        Some(pattern)
            if req
                .params()
                .iter()
                .any(|(name, _)| crate::http::request::is_sensitive(name)) =>
        {
            std::borrow::Cow::Owned(mask_path_params(req.path(), pattern))
        }
        _ => std::borrow::Cow::Borrowed(req.path()),
    };
    let query = req.query_string();
    if query.is_empty() {
        return path.into_owned();
    }
    let mut out = String::with_capacity(path.len() + query.len() + 1);
    out.push_str(&path);
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

/// 届いたパスのうち、伏せる名前のパス引数に当たる部分を `***` にする。
///
/// `pattern` は当たったルートの登録パス（`/reset/{token}` の形）です。
/// 照合は区間（`/` の間）ごとなので、登録パスと届いたパスを区間ごとに並べて見ます。
///
/// | 登録パスの区間 | 出すもの |
/// |---|---|
/// | 固定文字だけ | 届いた区間そのまま |
/// | `file-{name}`（伏せない名前） | 届いた区間そのまま |
/// | `file-{token}`（伏せる名前） | `file-***` |
/// | `{*token}`（末尾まで。伏せる名前） | `***`（後ろの区間もまとめて） |
///
/// 届いたパスは照合の前に `%xx` の大文字化と末尾の `/` 落としを受けますが、
/// どちらも区間の数と前置きの長さを変えません。なので生のパスのまま並べられます。
fn mask_path_params(path: &str, pattern: &str) -> String {
    let segments: Vec<&str> = path.split('/').collect();
    let mut out = String::with_capacity(path.len());
    let mut used = 0;
    for (index, piece) in pattern.split('/').enumerate() {
        let Some(segment) = segments.get(index) else {
            break;
        };
        if index > 0 {
            out.push('/');
        }
        used = index + 1;
        let Some((prefix, name, catch_all)) = path_param(piece) else {
            out.push_str(segment);
            continue;
        };
        if catch_all {
            // 末尾までの引数。残りの区間も値の一部です。
            used = segments.len();
            if crate::http::request::is_sensitive(name) {
                out.push_str(segment.get(..prefix).unwrap_or(""));
                out.push_str("***");
            } else {
                out.push_str(&segments[index..].join("/"));
            }
            break;
        }
        if crate::http::request::is_sensitive(name) {
            out.push_str(segment.get(..prefix).unwrap_or(""));
            out.push_str("***");
        } else {
            out.push_str(segment);
        }
    }
    // 登録パスより後ろ（末尾の `/` など）はそのまま出す。
    for segment in &segments[used.min(segments.len())..] {
        out.push('/');
        out.push_str(segment);
    }
    out
}

/// 登録パスの 1 区間から、パス引数を探す。
///
/// 返すのは「前の固定文字の長さ（届いたパスでのバイト数）」「名前」「末尾までの引数か」です。
/// `{{` と `}}` は固定文字の `{` `}`（matchit の書き方）なので、1 文字と数えます。
fn path_param(piece: &str) -> Option<(usize, &str, bool)> {
    let bytes = piece.as_bytes();
    let mut index = 0;
    let mut literal = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'{' | b'}' if bytes.get(index + 1) == Some(&bytes[index]) => {
                index += 2;
                literal += 1;
            }
            b'{' => {
                let end = index + piece[index..].find('}')?;
                let name = &piece[index + 1..end];
                return Some(match name.strip_prefix('*') {
                    Some(name) => (literal, name, true),
                    None => (literal, name, false),
                });
            }
            _ => {
                index += 1;
                literal += 1;
            }
        }
    }
    None
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

/// 強制終了のときの終了コード（Ctrl+C の慣わし）。
const FORCED_EXIT_CODE: i32 = 130;

/// 停止の合図を見張る。**1 回目で `graceful` を呼び、2 回目で強制終了します。**
///
/// `serve` と `queue:work` の両方が使います。別々に書いていたころは、
/// `queue:work` が Ctrl+C（SIGINT）しか見ておらず、`systemctl stop` や
/// `docker stop` の SIGTERM で処理中のジョブごと即死していました。
/// `serve` のほうは 2 回目の合図を捨てていました。
///
/// 2 回目は待たずに落とします（終了コードは 130）。処理が固まったときに
/// `kill -9` を持ち出さなくて済むようにするためです。
pub(crate) async fn watch_shutdown(graceful: impl FnOnce()) {
    two_stage(shutdown_signal, graceful, || {
        eprintln!("\n強制終了します。");
        std::process::exit(FORCED_EXIT_CODE);
    })
    .await;
}

/// 合図を 2 回待ち、1 回目で `graceful`、2 回目で `force` を呼ぶ。
///
/// 合図の待ち方と終わらせ方を外から渡すのは、テストで確かめるためです
/// （本物は OS の合図を待ち、プロセスを終わらせます）。
async fn two_stage<S, F>(mut signal: S, graceful: impl FnOnce(), force: impl FnOnce())
where
    S: FnMut() -> F,
    F: Future<Output = ()>,
{
    signal().await;
    graceful();
    signal().await;
    force();
}

/// 停止の合図を 1 回待つ。
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
///
/// 呼ぶたびに合図の受け口を作り直します。2 回目を待つときは、もう一度呼んでください
/// （[`watch_shutdown`] がそうしています）。
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
    fn ログはかっこ付きの名前と合い言葉も伏せる() {
        // `token[]` は 1 語として扱われ、以前は値がそのままログに残っていた。
        let req = Request::new("GET", "/pay")
            .with_query("token[]=abc&cvc=123&recovery_code=xyz&keyword=rust");
        assert_eq!(
            log_target(&req),
            "/pay?token[]=***&cvc=***&recovery_code=***&keyword=rust"
        );
    }

    /// ルートに当たったあとのリクエストを作る（`Application` が載せるものを手で載せる）。
    fn routed(path: &str, pattern: &str, params: &[(&str, &str)]) -> Request {
        let mut req = Request::new("GET", path);
        req.set_route_path(Some(pattern.into()));
        req.set_params(
            params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        );
        req
    }

    #[test]
    fn ログはパス引数の秘密も伏せる() {
        // 以前はクエリだけを伏せていたので、`/reset/{token}` の値が INFO で残っていた。
        let req = routed("/reset/abc123", "/reset/{token}", &[("token", "abc123")]);
        assert_eq!(log_target(&req), "/reset/***");

        // 伏せない名前はそのまま。クエリの伏せ方とも組み合わさる。
        let req = routed(
            "/users/5/reset/abc",
            "/users/{id}/reset/{token}",
            &[("id", "5"), ("token", "abc")],
        )
        .with_query("signature=9f3c&page=2");
        assert_eq!(log_target(&req), "/users/5/reset/***?signature=***&page=2");

        // 前置きの固定文字は残す。符号化された前置きも長さは変わらない。
        let req = routed("/t-abc", "/t-{token}", &[("token", "abc")]);
        assert_eq!(log_target(&req), "/t-***");
        let req = routed("/%e3%81%82-abc", "/%E3%81%82-{token}", &[("token", "abc")]);
        assert_eq!(log_target(&req), "/%e3%81%82-***");

        // 末尾までの引数は、後ろの区間もまとめて伏せる。
        let req = routed("/k/a/b/c", "/k/{*api_key}", &[("api_key", "a/b/c")]);
        assert_eq!(log_target(&req), "/k/***");
        let req = routed("/files/a/b", "/files/{*path}", &[("path", "a/b")]);
        assert_eq!(
            log_target(&req),
            "/files/a/b",
            "伏せる名前が無ければそのまま"
        );

        // 末尾の `/` は残す。
        let req = routed("/reset/abc/", "/reset/{token}", &[("token", "abc")]);
        assert_eq!(log_target(&req), "/reset/***/");

        // `{{` は固定文字の `{`。
        assert_eq!(path_param("a{{b-{token}"), Some((4, "token", false)));
        assert_eq!(path_param("{*rest}"), Some((0, "rest", true)));
        assert_eq!(path_param("plain"), None);
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

        // 打ち切ったことは `None` で伝える（ランタイムの片付け方を変えるため）。
        assert!(result.is_none(), "打ち切りは None で返る");
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
            .expect("打ち切っていない")
            .expect("成功する");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// 上限なしでも、終われば返る。
    #[tokio::test]
    async fn 上限なしでも終われば返る() {
        let done = async { Ok::<(), std::io::Error>(()) };
        tokio::pin!(done);
        drain(&mut done, None)
            .await
            .expect("打ち切っていない")
            .expect("成功する");
    }

    /// 合図の 1 回目と 2 回目で、呼ぶものが分かれる。
    #[tokio::test]
    async fn 合図の1回目は穏やかに止め2回目で強制終了する() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let signal = Arc::new(tokio::sync::Notify::new());
        let graceful = Arc::new(AtomicU32::new(0));
        let forced = Arc::new(AtomicU32::new(0));

        let watching = {
            let signal = signal.clone();
            let graceful = graceful.clone();
            let forced = forced.clone();
            tokio::spawn(two_stage(
                move || {
                    let signal = signal.clone();
                    async move { signal.notified().await }
                },
                move || {
                    graceful.fetch_add(1, Ordering::SeqCst);
                },
                move || {
                    forced.fetch_add(1, Ordering::SeqCst);
                },
            ))
        };

        // 合図が来るまでは、どちらも呼ばない。
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(graceful.load(Ordering::SeqCst), 0);
        assert_eq!(forced.load(Ordering::SeqCst), 0);

        // 1 回目。穏やかに止めるほうだけ。以前の `serve` はここで見張りをやめ、
        // 2 回目の合図を捨てていた。
        signal.notify_one();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(graceful.load(Ordering::SeqCst), 1);
        assert_eq!(forced.load(Ordering::SeqCst), 0, "1 回では強制終了しない");

        // 2 回目で強制終了。
        signal.notify_one();
        watching.await.expect("見張りが終わる");
        assert_eq!(
            graceful.load(Ordering::SeqCst),
            1,
            "穏やかなほうは 1 回だけ"
        );
        assert_eq!(forced.load(Ordering::SeqCst), 1);
    }

    /// 打ち切ったときは、走っている裏のスレッドを待ち続けない。
    #[test]
    fn 打ち切った後は裏のスレッドを待ち続けない() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("ランタイムを作れる");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        runtime.block_on(async move {
            // 固まった NFS や返事の来ない SMTP の代わり。
            tokio::task::spawn_blocking(move || {
                let _ = started_tx.send(());
                std::thread::sleep(Duration::from_secs(60));
            });
        });
        // 走り始めてから片付ける（待ち行列にあるだけの処理は、もともと捨てられる）。
        started_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("裏の処理が走り始める");

        // そのまま捨てると、tokio は 60 秒の処理が終わるまで待っていた。
        let started = std::time::Instant::now();
        finish(runtime, true, Duration::from_millis(100));
        assert!(
            started.elapsed() >= Duration::from_millis(100),
            "上限までは待つ"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "上限を過ぎたら待たない: {:?}",
            started.elapsed()
        );
    }

    /// 打ち切っていないときは、これまでどおり裏の処理が終わるまで待つ。
    #[test]
    fn 打ち切っていなければ裏の処理を待つ() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("ランタイムを作れる");
        let done = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let flag = done.clone();
        runtime.block_on(async move {
            tokio::task::spawn_blocking(move || {
                let _ = started_tx.send(());
                std::thread::sleep(Duration::from_millis(200));
                flag.store(true, Ordering::SeqCst);
            });
        });
        started_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("裏の処理が走り始める");

        // 上限（1 ミリ秒）は使われない。書き込みの後始末を途中で切らないため。
        finish(runtime, false, Duration::from_millis(1));
        assert!(done.load(Ordering::SeqCst), "終わるまで待つ");
    }

    #[test]
    fn 打ち切った後に待つのは数秒だけ() {
        // 長くすると、systemd の `TimeoutStopSec`（文書の例は上限 + 10 秒）を超える。
        assert!(CUT_OFF_GRACE <= Duration::from_secs(5));
        assert!(CUT_OFF_GRACE >= Duration::from_secs(1));
    }

    #[test]
    fn ログはwebsocketのチケットも伏せる() {
        let req = Request::new("GET", "/gateway").with_query("ticket=abc&v=1");
        assert_eq!(log_target(&req), "/gateway?ticket=***&v=1");
    }

    /// 実際に `127.0.0.1:0` で待ち受けて、本文を手で組んだ HTTP/1.1 で送る。
    ///
    /// テストのクライアントはメモリの本文を 1 かたまりで渡すので、hyper の本文
    /// （chunked・何回かに分けて届く・長さの申告）を通る道はここで確かめます。
    mod body {
        use super::*;
        use crate::http::Route;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        async fn length(req: Request) -> Result<Response> {
            Ok(Response::text(req.body().len().to_string()))
        }

        /// 少しずつ読み、合計のバイト数とかたまりの数を返す。
        async fn streamed(req: Request) -> Result<Response> {
            let mut body = req.body_stream()?;
            let (mut total, mut chunks) = (0, 0);
            while let Some(chunk) = body.chunk().await? {
                total += chunk.len();
                chunks += 1;
            }
            Ok(Response::text(format!("{total} {chunks}")))
        }

        async fn start() -> SocketAddr {
            let app = Application::configure()
                .with_routing(|r| {
                    r.web(|| {
                        Route::post("/buffered", length).max_body(10);
                        Route::post("/stream", streamed).stream_body().max_body(10);
                    })
                })
                .create();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let service = router(app, crate::websocket::ServerState::new(10));
            tokio::spawn(async move {
                let _ = axum::serve(listener, service).await;
            });
            addr
        }

        /// ヘッダーを送り、本文を `pieces` ごとに少し間を空けて送り、応答を全部読む。
        ///
        /// 書くのと読むのは**同時に**進めます。サーバーは途中で 413 を返して閉じることが
        /// あり、書き終えてから読むと、閉じた相手に書いた時点で接続がリセットされ
        /// （Windows）、届いていた応答まで読めなくなるためです。ブラウザも同じく
        /// 送りながら応答を読みます。途中で断られると書き込みは失敗するので、そのエラーは見ません。
        async fn send(addr: SocketAddr, head: &str, pieces: &[&[u8]]) -> (u16, String) {
            let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let (mut reader, mut writer) = stream.into_split();
            let mut outgoing = vec![head.as_bytes().to_vec()];
            outgoing.extend(pieces.iter().map(|piece| piece.to_vec()));
            let writing = tokio::spawn(async move {
                for piece in outgoing {
                    if writer.write_all(&piece).await.is_err() {
                        return;
                    }
                    let _ = writer.flush().await;
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
                // 書き終えても閉じない（`connection: close` なのでサーバーが閉じる）。
                std::future::pending::<()>().await;
            });
            let mut raw = Vec::new();
            let _ =
                tokio::time::timeout(Duration::from_secs(10), reader.read_to_end(&mut raw)).await;
            writing.abort();
            let text = String::from_utf8_lossy(&raw).into_owned();
            let status = text
                .split_whitespace()
                .nth(1)
                .and_then(|code| code.parse().ok())
                .unwrap_or(0);
            let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
            (status, body)
        }

        fn chunked(path: &str) -> String {
            format!(
                "POST {path} HTTP/1.1\r\nhost: localhost\r\ntransfer-encoding: chunked\r\n\
                 connection: close\r\n\r\n"
            )
        }

        #[tokio::test]
        async fn 分けて届いた本文を少しずつ読める() {
            let addr = start().await;
            let (status, body) = send(
                addr,
                &chunked("/stream"),
                &[
                    b"3\r\nabc\r\n",
                    b"3\r\ndef\r\n",
                    b"3\r\nghi\r\n",
                    b"0\r\n\r\n",
                ],
            )
            .await;
            assert_eq!(status, 200, "{body}");
            let (total, chunks) = body.split_once(' ').expect("合計とかたまりの数");
            assert_eq!(total, "9");
            assert!(chunks.parse::<u32>().unwrap() >= 1, "{body}");
        }

        #[tokio::test]
        async fn 少しずつ読んだ合計が上限を超えたら413() {
            let addr = start().await;
            let (status, _) = send(
                addr,
                &chunked("/stream"),
                &[b"6\r\nabcdef\r\n", b"6\r\nghijkl\r\n", b"0\r\n\r\n"],
            )
            .await;
            assert_eq!(status, 413);
        }

        #[tokio::test]
        async fn 申告の無い本文が上限を超えたら413() {
            // 以前は読み取りの失敗と見分けられず 400 にしていた。
            let addr = start().await;
            let (status, _) = send(
                addr,
                &chunked("/buffered"),
                &[b"6\r\nabcdef\r\n", b"6\r\nghijkl\r\n", b"0\r\n\r\n"],
            )
            .await;
            assert_eq!(status, 413);
        }

        #[tokio::test]
        async fn 申告が上限を超えていれば読まずに413() {
            let addr = start().await;
            let head = "POST /buffered HTTP/1.1\r\nhost: localhost\r\ncontent-length: 1000\r\n\
                        connection: close\r\n\r\n";
            let (status, _) = send(addr, head, &[]).await;
            assert_eq!(status, 413);
        }

        #[tokio::test]
        async fn 上限までの本文は全部読める() {
            let addr = start().await;
            let head = "POST /buffered HTTP/1.1\r\nhost: localhost\r\ncontent-length: 10\r\n\
                        connection: close\r\n\r\n";
            let (status, body) = send(addr, head, &[b"12345", b"67890"]).await;
            assert_eq!(status, 200);
            assert_eq!(body, "10");
        }
    }

    /// 実際に `127.0.0.1:0` で待ち受けて、tokio-tungstenite のクライアントで話す。
    #[cfg(feature = "websocket")]
    mod websocket {
        use super::*;
        use crate::http::Route;
        use crate::websocket::{Message, ServerState, WebSocket};
        use futures_util::{SinkExt, StreamExt};
        use std::sync::Arc;
        use tokio_tungstenite::tungstenite::{self, Message as WsMessage};

        type Client = tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >;

        const WAIT: Duration = Duration::from_secs(10);

        async fn echo(req: Request) -> Result<Response> {
            req.websocket()?
                .on_upgrade(|mut socket: WebSocket| async move {
                    while let Some(Ok(message)) = socket.recv().await {
                        let _ = match message {
                            Message::Text(text) => socket.send_text(text),
                            Message::Binary(bytes) => socket.send_binary(bytes),
                        };
                    }
                })
        }

        async fn boom(req: Request) -> Result<Response> {
            req.websocket()?.on_upgrade(|_socket| async move {
                panic!("わざと");
            })
        }

        /// 切り替えを載せずに 101 だけを返す（ミドルウェアが応答を作り直した場合の代わり）。
        async fn bare_101(_req: Request) -> Result<Response> {
            Ok(Response::new(101))
        }

        async fn joined(req: Request) -> Result<Response> {
            let channel = req.query("c").unwrap_or_default();
            req.websocket()?
                .on_upgrade(move |mut socket: WebSocket| async move {
                    socket.join(&channel);
                    let _ = socket.send_text("joined");
                    while socket.recv().await.is_some() {}
                })
        }

        /// サーバーを立てる。返すのはアドレスと WebSocket の状態。
        async fn start(limit: usize) -> (SocketAddr, Arc<ServerState>) {
            let app = Application::configure()
                .with_routing(|r| {
                    r.web(|| {
                        Route::get("/ws", echo);
                        Route::get("/boom", boom);
                        Route::get("/bare", bare_101);
                        Route::get("/join", joined);
                    })
                })
                .create();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let sockets = ServerState::new(limit);
            let service = router(app, sockets.clone());
            tokio::spawn(async move {
                let _ = axum::serve(listener, service).await;
            });
            (addr, sockets)
        }

        async fn connect(
            addr: SocketAddr,
            path: &str,
        ) -> std::result::Result<Client, tungstenite::Error> {
            tokio::time::timeout(
                WAIT,
                tokio_tungstenite::connect_async(format!("ws://{addr}{path}")),
            )
            .await
            .expect("時間内につながる")
            .map(|(client, _response)| client)
        }

        /// つなげなかったときの HTTP のステータス。
        fn http_status(error: tungstenite::Error) -> u16 {
            match error {
                tungstenite::Error::Http(response) => response.status().as_u16(),
                other => panic!("HTTP のエラーではありません: {other}"),
            }
        }

        async fn next(client: &mut Client) -> Option<WsMessage> {
            loop {
                let message = tokio::time::timeout(WAIT, client.next())
                    .await
                    .expect("時間内に届く")?
                    .ok()?;
                if !matches!(message, WsMessage::Ping(_) | WsMessage::Pong(_)) {
                    return Some(message);
                }
            }
        }

        async fn close_code(client: &mut Client) -> Option<u16> {
            loop {
                if let WsMessage::Close(frame) = next(client).await? {
                    // もう一度読むと、クライアントが Close の返事を送り出す。
                    let _ = tokio::time::timeout(WAIT, client.next()).await;
                    return frame.map(|frame| u16::from(frame.code));
                }
            }
        }

        async fn wait_until(mut done: impl FnMut() -> bool) {
            let started = std::time::Instant::now();
            while !done() {
                assert!(started.elapsed() < WAIT, "時間内に終わらない");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        #[tokio::test]
        async fn 実際のソケットで送受信できる() {
            let (addr, sockets) = start(10).await;
            let mut client = connect(addr, "/ws").await.expect("つながる");
            client.send(WsMessage::text("ping")).await.unwrap();
            assert_eq!(next(&mut client).await, Some(WsMessage::text("ping")));
            client.send(WsMessage::binary(vec![0, 1])).await.unwrap();
            assert_eq!(next(&mut client).await, Some(WsMessage::binary(vec![0, 1])));
            assert_eq!(sockets.active(), 1);

            client.close(None).await.unwrap();
            wait_until(|| sockets.active() == 0).await;
        }

        #[tokio::test]
        async fn 上限の数を超えたら503() {
            let (addr, sockets) = start(1).await;
            let mut first = connect(addr, "/ws").await.expect("1 本目はつながる");
            let second = connect(addr, "/ws").await.expect_err("2 本目は断る");
            assert_eq!(http_status(second), 503);

            first.close(None).await.unwrap();
            wait_until(|| sockets.active() == 0).await;
            let mut again = connect(addr, "/ws").await.expect("空いたらつながる");
            again.close(None).await.unwrap();
        }

        #[tokio::test]
        async fn パニックしてもほかの接続とサーバーは生きている() {
            let (addr, _sockets) = start(10).await;
            let mut alive = connect(addr, "/ws").await.unwrap();
            let mut broken = connect(addr, "/boom").await.unwrap();
            assert_eq!(close_code(&mut broken).await, Some(1011));

            alive.send(WsMessage::text("still")).await.unwrap();
            assert_eq!(next(&mut alive).await, Some(WsMessage::text("still")));
            let mut fresh = connect(addr, "/ws").await.expect("新しくつなげる");
            fresh.close(None).await.unwrap();
            alive.close(None).await.unwrap();
        }

        #[tokio::test]
        async fn 停止で全接続に1001を送り新しい切り替えは503() {
            let (addr, sockets) = start(10).await;
            let mut a = connect(addr, "/ws").await.unwrap();
            let mut b = connect(addr, "/ws").await.unwrap();
            wait_until(|| sockets.active() == 2).await;

            let stopping = {
                let sockets = sockets.clone();
                tokio::spawn(async move {
                    let started = std::time::Instant::now();
                    crate::websocket::shutdown(&sockets, Some(Duration::from_secs(30))).await;
                    started.elapsed()
                })
            };
            assert_eq!(close_code(&mut a).await, Some(1001));
            assert_eq!(close_code(&mut b).await, Some(1001));
            // クライアントは次に読むときに Close を返す。
            let _ = a.next().await;
            let _ = b.next().await;
            // 相手が Close を返したら、5 秒を待たずに終わる。
            let took = tokio::time::timeout(WAIT, stopping).await.unwrap().unwrap();
            assert!(took < Duration::from_secs(5), "{took:?}");
            assert_eq!(sockets.active(), 0);

            let refused = connect(addr, "/ws").await.expect_err("停止の途中は断る");
            assert_eq!(http_status(refused), 503);
        }

        #[tokio::test]
        async fn 切り替えが載っていない101は500にする() {
            let (addr, _sockets) = start(10).await;
            let error = connect(addr, "/bare").await.expect_err("切り替わらない");
            assert_eq!(http_status(error), 500);
        }

        #[tokio::test]
        async fn 配信でチャンネルの接続にだけ届きdisconnectで閉じる() {
            // fake と取り合わないように札を取る（Broadcast はプロセスで共有）。
            let _lock = crate::testing::exclusive().await;
            let (addr, _sockets) = start(10).await;

            let channel = format!("test.server.broadcast.{}", addr.port());
            let path = format!("/join?c={channel}");
            let mut a = connect(addr, &path).await.unwrap();
            let mut b = connect(addr, &path).await.unwrap();
            let mut outside = connect(addr, "/join?c=test.server.elsewhere")
                .await
                .unwrap();
            for client in [&mut a, &mut b, &mut outside] {
                assert_eq!(next(client).await, Some(WsMessage::text("joined")));
            }
            assert_eq!(crate::broadcast::Broadcast::local_count(&channel), 2);

            crate::broadcast::Broadcast::publish(&channel, "news")
                .await
                .unwrap();
            assert_eq!(next(&mut a).await, Some(WsMessage::text("news")));
            assert_eq!(next(&mut b).await, Some(WsMessage::text("news")));

            crate::broadcast::Broadcast::disconnect(&channel, 4001, "session revoked")
                .await
                .unwrap();
            assert_eq!(close_code(&mut a).await, Some(4001));
            assert_eq!(close_code(&mut b).await, Some(4001));
            let _ = a.next().await;
            let _ = b.next().await;
            wait_until(|| crate::broadcast::Broadcast::local_count(&channel) == 0).await;

            // 入っていない接続には届かず、生きている。
            outside.send(WsMessage::text("x")).await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(100), outside.next())
                    .await
                    .is_err(),
                "何も届かない"
            );
            outside.close(None).await.unwrap();
        }
    }
}
