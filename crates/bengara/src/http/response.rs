//! レスポンス。

use std::borrow::Cow;

use serde::Serialize;

use crate::error::{reason_phrase, Error, Result};

/// クライアントに返す内容。
///
/// 本文は `Cow` で持ちます。バイナリに埋め込んだ `public/` を配信するときに、
/// `&'static [u8]` をコピーせずそのまま返せるようにするためです。
#[derive(Debug, Clone)]
pub struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Cow<'static, [u8]>,
}

impl Response {
    /// ステータスだけを決めた空のレスポンス。
    ///
    /// HTTP として使える範囲（100〜599）の外は、警告して 500 にします。
    pub fn new(status: u16) -> Self {
        Self {
            status: normalize_status(status),
            headers: Vec::new(),
            body: Cow::Borrowed(&[]),
        }
    }

    /// `text/plain` を返す。
    pub fn text(body: impl Into<String>) -> Self {
        Self::new(200)
            .with_header("content-type", "text/plain; charset=utf-8")
            .with_body(body.into().into_bytes())
    }

    /// `text/html` を返す。
    pub fn html(body: impl Into<String>) -> Self {
        Self::new(200)
            .with_header("content-type", "text/html; charset=utf-8")
            .with_body(body.into().into_bytes())
    }

    /// `application/json` を返す。
    pub fn json<T: Serialize>(value: &T) -> Result<Self> {
        let body = serde_json::to_vec(value)?;
        Ok(Self::new(200)
            .with_header("content-type", "application/json")
            .with_body(body))
    }

    /// 本文のない 204 を返す。
    pub fn no_content() -> Self {
        Self::new(204)
    }

    /// 任意のバイト列を返す。
    pub fn bytes(content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self::new(200)
            .with_header("content-type", content_type)
            .with_body(body)
    }

    /// バイナリに埋め込んだ中身を、コピーせずに返す。
    ///
    /// `public/` の埋め込み配信で使います。`&'static [u8]` をそのまま持つので、
    /// 1本のアセットを返すたびに全文をコピーすることがなくなります。
    pub(crate) fn static_bytes(content_type: &str, body: &'static [u8]) -> Self {
        let mut response = Self::new(200).with_header("content-type", content_type);
        response.body = Cow::Borrowed(body);
        response
    }

    /// ステータスを変える。
    ///
    /// HTTP として使える範囲（100〜599）の外は、警告して 500 にします。
    pub fn with_status(mut self, status: u16) -> Self {
        self.status = normalize_status(status);
        self
    }

    /// ヘッダーを**置き換える**。同じ名前がすでにあれば、消してから入れます。
    ///
    /// Laravel の `header()` と同じ既定です。同じ名前を何本も送りたいときは
    /// `with_added_header` を使ってください。
    ///
    /// 値に使えない文字（改行など）が入っていたときは、**その1本だけ**落とします。
    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Self {
        // 照合は確保せずに行い、格納する名前は小文字にそろえる。
        self.headers
            .retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
        let value = value.into();
        if !header_value_ok(name, &value) {
            return self;
        }
        self.headers.push((name.to_ascii_lowercase(), value));
        self
    }

    /// ヘッダーを**足す**。同じ名前がすでにあっても消しません。
    ///
    /// `set-cookie` のように、同じ名前を何本も送る必要があるときに使います。
    ///
    /// 値に使えない文字（改行など）が入っていたときは、**その1本だけ**落とします。
    pub fn with_added_header(mut self, name: &str, value: impl Into<String>) -> Self {
        let value = value.into();
        if !header_value_ok(name, &value) {
            return self;
        }
        self.headers.push((name.to_ascii_lowercase(), value));
        self
    }

    /// Cookie を1本足す。何本でも足せます。
    pub fn with_cookie(self, cookie: &super::cookie::Cookie) -> Self {
        self.with_added_header("set-cookie", cookie.to_header_value())
    }

    /// 本文を差し替える。
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Cow::Owned(body.into());
        self
    }

    /// 本文を取り出す。
    ///
    /// 返すときにもう一度コピーしないための入口です。埋め込んだ `public/` は
    /// `Cow::Borrowed` のまま返るので、下回りへ渡すときもコピーが起きません。
    pub(crate) fn into_body(self) -> Cow<'static, [u8]> {
        self.body
    }

    /// ステータス。
    pub fn status(&self) -> u16 {
        self.status
    }

    /// ヘッダーの値（最初に見つかったもの）。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// ヘッダーの一覧。
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// 本文。
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// 本文を文字列として見る（UTF-8 でない部分は置き換える）。
    pub fn body_text(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }
}

/// ヘッダーの値として使えるか。使えないときは1行だけ残して偽を返します。
///
/// 改行・復帰・NUL とその他の制御文字が入っていると `HeaderValue` を作れません。
/// 以前はそれが応答の組み立て全体を失敗させ、**ヘッダーを全部捨てた素の 500** に
/// なっていました。直前にセッションが付けた `Set-Cookie` まで消えていたので、
/// ログインや CSRF トークンの更新が失われました
/// （`redirect().to(req.input("next"))` に `?next=/a%0Ab` が来ると起きました）。
/// 黙って落とすので、落としたときだけ1行だけ残します。`Cookie` の属性
/// （`cookie::sanitize_attribute`）と同じやり方です。
fn header_value_ok(name: &str, value: &str) -> bool {
    // 水平タブだけは HTTP でも値に使えるので通す。
    if !value.chars().any(|c| c.is_control() && c != '\t') {
        return true;
    }
    tracing::warn!("ヘッダー {name} の値に使えない文字があったので落としました");
    false
}

/// HTTP として使えるステータスに直す。範囲（100〜599）の外は 500 に寄せます。
///
/// `abort(1000)` や `Response::new(0)` をそのまま通すと、応答を組み立てられずに
/// ヘッダーごと失われていました。入口で丸めます。
fn normalize_status(status: u16) -> u16 {
    if (100..=599).contains(&status) {
        return status;
    }
    tracing::warn!("ステータス {status} は HTTP の範囲外なので 500 にします");
    500
}

/// 本文を持てないステータスか（1xx・204・304）。
pub(crate) fn body_forbidden(status: u16) -> bool {
    matches!(status, 100..=199 | 204 | 304)
}

/// 本文を持てないステータスなら、本文と種類・長さの申告を落とす。
///
/// 以前は `abort(204)` や `abort(304)` でも HTML / JSON の本文を作っていたので、
/// 本文を持てない応答に本文が付き、枠組みの長さが相手と食い違っていました。
///
/// エラーの道（`error_response`）と、`Application::handle` の最後の両方から呼びます。
/// 利用者が `Response::new(204).with_body(..)` と自分で組んだ応答も落とすためです。
pub(crate) fn without_forbidden_body(mut response: Response) -> Response {
    if !body_forbidden(response.status) {
        return response;
    }
    response.body = Cow::Borrowed(&[]);
    response.headers.retain(|(name, _)| {
        !name.eq_ignore_ascii_case("content-type") && !name.eq_ignore_ascii_case("content-length")
    });
    response
}

/// `text/plain` のレスポンスを作る。
pub fn text(body: impl Into<String>) -> Result<Response> {
    Ok(Response::text(body))
}

/// `text/html` のレスポンスを作る。
pub fn html(body: impl Into<String>) -> Result<Response> {
    Ok(Response::html(body))
}

/// JSON のレスポンスを作る。
pub fn json<T: Serialize>(value: &T) -> Result<Response> {
    Response::json(value)
}

/// リダイレクトの組み立て。
///
/// ```ignore
/// redirect().to("/login")
/// redirect().route("home")
/// ```
///
/// 行き先に**外から来た文字列**（`req.input("next")` など）を渡すときは、
/// `to` がエラーを返しうることに注意してください。`?` で上に返せば
/// 普段のエラーの道を通るので、直前のミドルウェアが付けた `Set-Cookie` は
/// 残ったまま 500 になります。
#[derive(Debug, Clone, Copy)]
pub struct Redirect {
    status: u16,
}

/// リダイレクトを作り始める。既定は 302。
pub fn redirect() -> Redirect {
    Redirect { status: 302 }
}

impl Redirect {
    /// 301（恒久的な移動）にする。
    pub fn permanent(mut self) -> Self {
        self.status = 301;
        self
    }

    /// ステータスを自分で決める。
    pub fn with_status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }

    /// パスへリダイレクトする。
    ///
    /// 行き先に `Location` ヘッダーとして使えない文字（改行などの制御文字）が
    /// 入っていたら**エラーにします**。`?next=/a%0Ab` のように外から来た文字列を
    /// そのまま渡すと起きます。
    ///
    /// ここで落とすのは、`Location` の無い 302 を返さないためです。それだと
    /// ブラウザは何も表示せず、白い画面になって原因を追いにくくなります。
    /// エラーにすれば普段のエラーの道を通るので、直前のミドルウェアが付けた
    /// `Set-Cookie` は残ったまま 500 になります。
    pub fn to(self, path: impl Into<String>) -> Result<Response> {
        let path = path.into();
        if path.chars().any(|c| c.is_control()) {
            return Err(Error::msg(
                "転送先に使えない文字が入っています（改行などの制御文字は Location ヘッダーに入れられません）",
            ));
        }
        Ok(Response::new(self.status).with_header("location", path))
    }

    /// 名前付きルートへリダイレクトする。
    pub fn route(self, name: &str) -> Result<Response> {
        self.to(super::routing::route(name)?)
    }

    /// 名前付きルートへ、パス引数を埋めてリダイレクトする。
    pub fn route_with(self, name: &str, params: &[(&str, &str)]) -> Result<Response> {
        self.to(super::routing::route_with(name, params)?)
    }
}

/// そのステータスで処理を打ち切る。
///
/// ```ignore
/// if !allowed { return abort(403); }
/// ```
pub fn abort<T>(status: u16) -> Result<T> {
    Err(Error::Http {
        status,
        message: String::new(),
    })
}

/// メッセージ付きで処理を打ち切る。
pub fn abort_with<T>(status: u16, message: impl Into<String>) -> Result<T> {
    Err(Error::Http {
        status,
        message: message.into(),
    })
}

/// エラーをどう見せるかの指定。起動時とリクエストごとに決まり、以後は変わりません。
///
/// `Arc` を1つ持つだけなので、クローンは安いです。
#[derive(Clone)]
pub(crate) struct RenderOptions {
    /// 詳しい内容を出すか（`APP_DEBUG`）。
    pub(crate) debug: bool,
    /// クライアントが JSON を欲しがっているか。
    pub(crate) wants_json: bool,
    /// 利用者が差し替えたエラーの見せ方。
    pub(crate) exceptions: Option<std::sync::Arc<crate::application::Exceptions>>,
}

impl RenderOptions {
    pub(crate) fn new(debug: bool, wants_json: bool) -> Self {
        Self {
            debug,
            wants_json,
            exceptions: None,
        }
    }

    pub(crate) fn with_exceptions(
        mut self,
        exceptions: Option<std::sync::Arc<crate::application::Exceptions>>,
    ) -> Self {
        self.exceptions = exceptions;
        self
    }
}

/// エラーをレスポンスに変える。
///
/// `wants_json` なら JSON、そうでなければ HTML の画面を返します。
/// 本文を持てないステータス（1xx・204・304）では、本文を付けません。
pub(crate) fn error_response(error: &Error, options: RenderOptions) -> Response {
    without_forbidden_body(build_error_response(error, options))
}

fn build_error_response(error: &Error, options: RenderOptions) -> Response {
    // まず、利用者が差し替えた見せ方を試す。
    if let Some(exceptions) = &options.exceptions {
        if let Some(response) = exceptions.apply(error) {
            return response;
        }
    }

    let status = error.status();

    // 入力の検査だけは、どの項目が駄目かを機械が読める形で返す。
    if let Error::Validation(errors) = error {
        if options.wants_json {
            return Response::json(&errors.to_json())
                .unwrap_or_else(|_| Response::text("入力に誤りがあります。"))
                .with_status(status);
        }
        let detail = errors
            .all()
            .iter()
            .map(|(field, reasons)| format!("{field}: {}", reasons.join(" / ")))
            .collect::<Vec<_>>()
            .join("\n");
        return Response::html(error_page(
            &format!("{status} {}", reason_phrase(status)),
            &detail,
            options.debug,
        ))
        .with_status(status);
    }

    let detail = if options.debug {
        let mut detail = error.to_string();
        let mut source = std::error::Error::source(error);
        while let Some(e) = source {
            detail.push_str(&format!("\n  原因: {e}"));
            source = e.source();
        }
        detail
    } else {
        error.public_message()
    };

    if options.wants_json {
        let body = serde_json::json!({ "message": detail });
        return Response::json(&body)
            .unwrap_or_else(|_| Response::text(detail.clone()))
            .with_status(status);
    }

    let title = format!("{status} {}", reason_phrase(status));
    Response::html(error_page(&title, &detail, options.debug)).with_status(status)
}
fn error_page(title: &str, detail: &str, debug: bool) -> String {
    let detail = escape_html(detail);
    let hint = if debug {
        "<p class=\"hint\">この詳細は APP_DEBUG=true のときだけ表示されます。</p>"
    } else {
        ""
    };
    format!(
        "<!doctype html>\n\
         <html lang=\"ja\"><head><meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{title}</title>\n\
         <style>\n\
         :root {{ color-scheme: light dark; }}\n\
         body {{ font-family: system-ui, sans-serif; margin: 0; display: grid; place-items: center; min-height: 100vh; }}\n\
         main {{ max-width: 44rem; padding: 2rem; }}\n\
         h1 {{ font-size: 1.5rem; margin: 0 0 1rem; }}\n\
         pre {{ background: rgba(127,127,127,.12); padding: 1rem; border-radius: .5rem; white-space: pre-wrap; word-break: break-word; }}\n\
         .hint {{ opacity: .6; font-size: .85rem; }}\n\
         </style></head>\n\
         <body><main><h1>{title}</h1><pre>{detail}</pre>{hint}</main></body></html>\n"
    )
}

/// HTML に埋め込める形にする。
///
/// 利用者から来た文字列をそのまま HTML に入れると、意図しないタグを書き込まれます
/// （クロスサイトスクリプティング）。画面に出す前にこれを通してください。
///
/// ```ignore
/// html(format!("<h1>{} さん、こんにちは</h1>", escape_html(name)))
/// ```
pub fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// ハンドラやミドルウェアの戻り値を、必ずレスポンスにする。
///
/// エラーはログに残してから、ステータス付きのレスポンスに変えます。
pub(crate) fn render(result: crate::error::Result<Response>, options: RenderOptions) -> Response {
    match result {
        Ok(response) => response,
        Err(error) => {
            if error.status() >= 500 {
                tracing::error!("{error}");
            } else {
                tracing::debug!("{error}");
            }
            error_response(&error, options)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_headerは置き換えwith_added_headerは足す() {
        let r = Response::text("x")
            .with_header("x-a", "1")
            .with_header("X-A", "2");
        assert_eq!(r.header("x-a"), Some("2"));
        assert_eq!(r.header("X-A"), Some("2"), "探すときも区別しない");
        assert_eq!(
            r.headers().iter().filter(|(k, _)| k == "x-a").count(),
            1,
            "格納する名前は小文字にそろえる"
        );

        let r = Response::text("x")
            .with_added_header("set-cookie", "a=1")
            .with_added_header("set-cookie", "b=2");
        assert_eq!(
            r.headers()
                .iter()
                .filter(|(k, _)| k == "set-cookie")
                .count(),
            2
        );
    }

    #[test]
    fn テキストのレスポンス() {
        let res = Response::text("hello");
        assert_eq!(res.status(), 200);
        assert_eq!(
            res.header("Content-Type"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(res.body_text(), "hello");
    }

    #[test]
    fn jsonのレスポンス() {
        let res = Response::json(&serde_json::json!({"a": 1})).unwrap();
        assert_eq!(res.header("content-type"), Some("application/json"));
        assert_eq!(res.body_text(), "{\"a\":1}");
    }

    #[test]
    fn リダイレクト() {
        let res = redirect().to("/login").unwrap();
        assert_eq!(res.status(), 302);
        assert_eq!(res.header("location"), Some("/login"));
        let res = redirect().permanent().to("/new").unwrap();
        assert_eq!(res.status(), 301);
    }

    #[test]
    fn abortはエラーになる() {
        let err = abort::<Response>(403).unwrap_err();
        assert_eq!(err.status(), 403);
    }

    #[test]
    fn 転送先に使えない文字があればエラーにする() {
        // `?next=/a%0Ab` のように、復号で本物の改行ができたときの形。
        // `Location` の無い 302（白い画面）を返さないよう、入口で落とす。
        for bad in ["/a\nb", "/a\rb", "/a\0b", "/a\tb", "/a\u{1}b"] {
            let err = redirect().to(bad).unwrap_err();
            assert_eq!(err.status(), 500, "{bad:?}");
            assert!(
                err.to_string().contains("転送先に使えない文字"),
                "何が悪いか分かる文面にする: {err}"
            );
        }
        // 普通の行き先はそのまま通る。
        let res = redirect().to("/a b.html?x=1#y").unwrap();
        assert_eq!(res.header("location"), Some("/a b.html?x=1#y"));
    }

    #[test]
    fn 転送先のエラーでもset_cookieは残る() {
        // ミドルウェアが付けた `Set-Cookie` を持つ応答を、エラーの道で作り直す。
        // 以前は `Location` の組み立てに失敗すると、ヘッダーごと捨てられていました。
        let error = redirect().to("/a\nb").unwrap_err();
        let response = error_response(&error, RenderOptions::new(false, false))
            .with_cookie(&crate::http::cookie::Cookie::new("session", "abc"));

        assert_eq!(response.status(), 500, "302 ではなく 500 で返る");
        assert!(
            response
                .header("set-cookie")
                .is_some_and(|v| v.starts_with("session=abc")),
            "Set-Cookie は残る"
        );
        assert_eq!(response.header("location"), None);
        assert!(!response.body().is_empty(), "500 の画面が出る");
    }

    #[test]
    fn ヘッダーの値が不正なら1本だけ落とす() {
        // `?next=/a%0Ab` のように、復号で本物の改行ができたときの形。
        let res = Response::text("x")
            .with_cookie(&crate::http::cookie::Cookie::new("session", "abc"))
            .with_header("location", "/a\nb");
        assert_eq!(res.header("location"), None, "改行の入った1本は落とす");
        assert!(
            res.header("set-cookie").is_some(),
            "直前の Set-Cookie は残る"
        );
        assert_eq!(
            res.header("content-type"),
            Some("text/plain; charset=utf-8"),
            "他のヘッダーも残る"
        );

        // 復帰・NUL・その他の制御文字も同じ。
        for bad in ["a\rb", "a\0b", "a\u{7f}b", "a\u{1}b"] {
            assert_eq!(
                Response::text("x").with_header("x-a", bad).header("x-a"),
                None
            );
        }
        // 水平タブは HTTP でも値に使えるので通す。
        assert_eq!(
            Response::text("x").with_header("x-a", "a\tb").header("x-a"),
            Some("a\tb")
        );

        // `with_added_header` も同じ。良い1本は残る。
        let res = Response::text("x")
            .with_added_header("set-cookie", "a=1")
            .with_added_header("set-cookie", "b=\n2");
        assert_eq!(
            res.headers()
                .iter()
                .filter(|(k, _)| k == "set-cookie")
                .count(),
            1
        );
    }

    #[test]
    fn 範囲外のステータスは500に寄せる() {
        // `abort(1000)` や `Response::new(0)` でも応答は組み立てられる。
        assert_eq!(Response::new(0).status(), 500);
        assert_eq!(Response::new(1000).status(), 500);
        assert_eq!(Response::text("x").with_status(600).status(), 500);
        // 使える範囲はそのまま。
        assert_eq!(Response::new(100).status(), 100);
        assert_eq!(Response::new(599).status(), 599);
        assert_eq!(Response::new(204).status(), 204);
    }

    #[test]
    fn 本文を持てない状態では本文を付けない() {
        for status in [100, 199, 204, 304] {
            let error = Error::Http {
                status,
                message: String::new(),
            };
            // HTML と JSON の両方。
            for wants_json in [false, true] {
                let res = error_response(&error, RenderOptions::new(false, wants_json));
                assert_eq!(res.status(), status);
                assert!(res.body().is_empty(), "{status} に本文は付けない");
                assert_eq!(res.header("content-type"), None, "{status}");
                assert_eq!(res.header("content-length"), None, "{status}");
            }
        }
        // 本文を持てる状態では、これまでどおり本文を作る。
        let error = Error::Http {
            status: 404,
            message: String::new(),
        };
        let res = error_response(&error, RenderOptions::new(false, false));
        assert!(!res.body().is_empty());
        assert!(res.header("content-type").is_some());
    }

    #[test]
    fn htmlエスケープ() {
        assert_eq!(
            escape_html("<a href=\"x\">&</a>"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&lt;/a&gt;"
        );
    }
}
