//! テンプレート（`resources/views/*.html`）。Laravel の `view()` に当たります。
//!
//! ```ignore
//! // 機能フラグが要ります: bengara = { version = "0.1", features = ["view"] }
//! use bengara::serde_json::json;
//!
//! async fn index(req: Request) -> Result<Response> {
//!     req.view("posts.index", json!({ "title": "記事" }))
//! }
//! ```
//!
//! 書き方は Jinja 系（中身は minijinja）です。**Blade の構文ではありません。**
//! minijinja の型は公開しません。渡すのは `serde::Serialize` の値だけです。
//!
//! ## 名前
//!
//! | 名前 | 読むファイル |
//! |---|---|
//! | `posts.index`・`posts/index` | `resources/views/posts/index.html` |
//! | `layouts/app.html` | `resources/views/layouts/app.html` |
//!
//! `{% extends %}` と `{% include %}` に書く名前も同じ規則です。
//!
//! ## どこから読むか
//!
//! | ビルド | 読み方 |
//! |---|---|
//! | リリース | `bengara-build` がバイナリに埋め込んだものだけ。起動時に 1 回だけ組み立て、以後は読むだけ |
//! | デバッグ | 描くたびにディスクの `resources/views/` から読む。直せば次のリクエストから反映される |
//!
//! ## フラグが無いとき
//!
//! 関数はそのままあります。`view` / `Request::view` / `render` は「features に "view" を
//! 足してください」の 500 になり、`exists` は偽を返します。

use serde::Serialize;

use crate::error::Result;
use crate::http::Response;

/// テンプレートを描いて、HTML のレスポンスで返す（200、`text/html; charset=utf-8`）。
///
/// ```ignore
/// bengara::view("posts.index", json!({ "posts": posts }))
/// ```
///
/// `context` は構造体か `json!({ .. })` のオブジェクト、または `()`（空）です。
/// CSRF のトークンや前の入力をテンプレートで使うときは [`Request::view`](crate::Request::view) を使います。
///
/// # エラー
///
/// 名前が規則に合わない・テンプレートが無い・描けない（未定義の変数など）ときは 500 です。
pub fn view(name: &str, context: impl Serialize) -> Result<Response> {
    crate::html(render(name, context)?)
}

/// テンプレートを描いて、文字列だけを返す（メールの本文などに）。
///
/// ```ignore
/// let body = bengara::view::render("emails.welcome", json!({ "name": name }))?;
/// ```
pub fn render(name: &str, context: impl Serialize) -> Result<String> {
    imp::render(name, &context, None)
}

/// テンプレートがあるか（Laravel の `View::exists`）。
///
/// 名前が規則に合わないときと、`view` フラグが無いときは偽です。
pub fn exists(name: &str) -> bool {
    imp::exists(name)
}

/// `Request::view` の中身。セッションがあれば、リクエストの関数に中身が入ります。
pub(crate) fn request_view(
    session: Option<&crate::session::Session>,
    name: &str,
    context: impl Serialize,
) -> Result<Response> {
    crate::html(imp::render(name, &context, Some(session))?)
}

/// 埋め込んだ一覧を固定する。`bengara::app!()` が生成したものを渡します。
///
/// 2 回目は黙って無視します（テストから複数回呼ばれても落ちないように）。
pub(crate) fn install_embedded(views: &'static [(&'static str, &'static str)]) {
    imp::install_embedded(views);
}

/// 埋め込んだテンプレートを全部読み込んで、構文を確かめる。`serve` の最初に呼びます。
///
/// 埋め込みが空のとき（デバッグビルド）と、`view` フラグが無いときは何もしません。
pub(crate) fn check_embedded() -> Result<()> {
    imp::check_embedded()
}

#[cfg(feature = "view")]
mod imp {
    use std::borrow::Cow;
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    use minijinja::value::{Rest, Serde, ValueKind};
    use minijinja::{AutoEscape, Environment, ErrorKind, UndefinedBehavior, Value};
    use serde::Serialize;

    use crate::error::{Error, Result};
    use crate::session::Session;

    /// リクエストの関数の名前。利用者の `context` には使えません。
    const RESERVED: [&str; 3] = ["csrf_token", "csrf_field", "old"];

    /// 埋め込んだ一覧。入っていなければ空（デバッグビルド）。
    static EMBEDDED: OnceLock<&'static [(&'static str, &'static str)]> = OnceLock::new();

    /// 埋め込みから作った共有の環境。最初に使うときに 1 回だけ作ります。
    ///
    /// 構文の誤りがあったときは、その文を持ちます。`serve` は起動時に気づいて止まり、
    /// ほかのコマンド（キューなど）は描くたびに同じ 500 を返します。
    static SHARED: OnceLock<std::result::Result<Environment<'static>, String>> = OnceLock::new();

    pub(super) fn install_embedded(views: &'static [(&'static str, &'static str)]) {
        let _ = EMBEDDED.set(views);
    }

    fn embedded() -> &'static [(&'static str, &'static str)] {
        EMBEDDED.get().copied().unwrap_or(&[])
    }

    pub(super) fn check_embedded() -> Result<()> {
        if embedded().is_empty() {
            return Ok(());
        }
        shared_env().map(|_| ())
    }

    fn shared_env() -> Result<&'static Environment<'static>> {
        SHARED
            .get_or_init(|| embedded_env(embedded()))
            .as_ref()
            .map_err(|message| Error::msg(message.clone()))
    }

    pub(super) fn render(
        name: &str,
        context: &impl Serialize,
        request: Option<Option<&Session>>,
    ) -> Result<String> {
        let context = to_context(context, request)?;
        let views = embedded();
        if views.is_empty() {
            render_in(&disk_env(views_dir()), name, context)
        } else {
            render_in(shared_env()?, name, context)
        }
    }

    pub(super) fn exists(name: &str) -> bool {
        let Some(path) = resolve(name) else {
            return false;
        };
        let views = embedded();
        if views.is_empty() {
            views_dir().join(&path).is_file()
        } else {
            views.iter().any(|(key, _)| *key == path)
        }
    }

    /// デバッグビルドで読む場所（`base_path()/resources/views/`）。
    fn views_dir() -> PathBuf {
        crate::base_path().join("resources").join("views")
    }

    /// 名前を `resources/views/` からの相対パスにする。規則に合わなければ `None`。
    ///
    /// | 名前 | パス |
    /// |---|---|
    /// | `.html` で終わらない（`posts.index`・`posts/index`） | `.` と `/` で区切って `/` でつなぎ、`.html` を付ける |
    /// | `.html` で終わる（`layouts/app.html`） | そのまま |
    ///
    /// 区切りに使える文字を絞るので、`..` で外へ出る名前や絶対パスは通りません。
    pub(super) fn resolve(name: &str) -> Option<String> {
        if name.ends_with(".html") {
            let valid = name.split('/').all(|part| {
                !part.is_empty()
                    && part != "."
                    && part != ".."
                    && part
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            });
            return valid.then(|| name.to_string());
        }
        let parts: Vec<&str> = name.split(['.', '/']).collect();
        let valid = parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        });
        valid.then(|| format!("{}.html", parts.join("/")))
    }

    fn bad_name(name: &str) -> String {
        format!("テンプレート名 `{name}` は使えません（例: posts.index）")
    }

    /// テンプレートの中の `{% include %}` などで、見つからなかった・名前が悪いときのエラー。
    ///
    /// 名前は `set_path_join_callback` を通った後のものです。規則に合う名前は
    /// もうパスになっています。合わない名前はそのまま届きます。
    fn load_error(name: &str) -> minijinja::Error {
        if resolve(name).as_deref() == Some(name) {
            // 見つからないときは TemplateNotFound のままにする（`ignore missing` が効くように）。
            minijinja::Error::new(
                ErrorKind::TemplateNotFound,
                format!("テンプレート `{name}`（resources/views/{name}）がありません"),
            )
        } else {
            minijinja::Error::new(ErrorKind::InvalidOperation, bad_name(name))
        }
    }

    fn template_error(message: impl Into<Cow<'static, str>>) -> minijinja::Error {
        minijinja::Error::new(ErrorKind::InvalidOperation, message)
    }

    /// どちらの環境にも入れる設定と関数。
    fn base_env() -> Environment<'static> {
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        // `.html` しか扱わないので、常に HTML の逃がし方。
        env.set_auto_escape_callback(|_| AutoEscape::Html);
        // `{% extends "layouts.app" %}` を `layouts/app.html` にする。
        // 規則に合わない名前はそのまま渡し、探す側（`load_error`）で断る。
        env.set_path_join_callback(|name, _parent| match resolve(name) {
            Some(path) => Cow::Owned(path),
            None => Cow::Borrowed(name),
        });

        env.add_function("route", route);
        env.add_function("url", |path: String| {
            // `url()` は渡したパスを逃がさない。属性の外へ出られないよう、ここで逃がしてから
            // 安全な文字列にする（`&` は `&amp;` になるが、属性の中では同じ意味）。
            safe_url(&crate::url(&path))
        });
        env.add_function(
            "__",
            |key: String, values: Option<Value>| -> std::result::Result<String, minijinja::Error> {
                match values.filter(|v| !v.is_none() && !v.is_undefined()) {
                    None => Ok(crate::__(&key)),
                    Some(values) => {
                        let pairs = to_pairs("__", &values)?;
                        let borrowed: Vec<(&str, &str)> = pairs
                            .iter()
                            .map(|(k, v)| (k.as_str(), v.as_str()))
                            .collect();
                        Ok(crate::__with(&key, &borrowed))
                    }
                }
            },
        );
        // リクエストの関数。`req.view(..)` は描画の文脈で本物に上書きする。
        for name in RESERVED {
            env.add_function(
                name,
                move |_: Rest<Value>| -> std::result::Result<Value, minijinja::Error> {
                    Err(template_error(format!(
                        "{name}() は req.view(..) の中でだけ使えます"
                    )))
                },
            );
        }
        env
    }

    /// `route(name, params)`。
    fn route(name: String, params: Option<Value>) -> std::result::Result<Value, minijinja::Error> {
        let pairs = match params.filter(|v| !v.is_none() && !v.is_undefined()) {
            None => Vec::new(),
            Some(params) => to_pairs("route", &params)?,
        };
        let borrowed: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        // `url()` と同じく逃がしてから安全な文字列にする。値はパーセントエンコードされるが、
        // 引数をクエリ文字列に回したときの区切りの `&` はそのまま出てくるため
        // （`&amp;` になるが、属性の中では同じ意味）。
        crate::route_with(&name, &borrowed)
            .map(|url| safe_url(&url))
            .map_err(|e| template_error(e.to_string()))
    }

    /// URL を逃がしてから、テンプレートにそのまま出せる値にする（`url()` と `route()`）。
    pub(super) fn safe_url(url: &str) -> Value {
        Value::from_safe_string(crate::escape_html(url))
    }

    /// `{'post': 1}` を `[("post", "1")]` にする。値は文字列か数だけ。
    fn to_pairs(
        function: &str,
        value: &Value,
    ) -> std::result::Result<Vec<(String, String)>, minijinja::Error> {
        let shape = || {
            template_error(format!(
                "{function}() の 2 つ目の引数は {{'post': 1}} のように、文字列か数の値を持つオブジェクトにしてください"
            ))
        };
        if value.kind() != ValueKind::Map {
            return Err(shape());
        }
        let mut pairs = Vec::new();
        for key in value.try_iter()? {
            let item = value.get_item(&key)?;
            let text = match item.kind() {
                ValueKind::String | ValueKind::Number => item.to_string(),
                _ => return Err(shape()),
            };
            let Some(key) = key.as_str() else {
                return Err(shape());
            };
            pairs.push((key.to_string(), text));
        }
        Ok(pairs)
    }

    /// 埋め込みから環境を作る。全部のテンプレートを入れ、構文を確かめます。
    pub(super) fn embedded_env(
        views: &'static [(&'static str, &'static str)],
    ) -> std::result::Result<Environment<'static>, String> {
        let mut env = base_env();
        for (key, source) in views {
            // `add_template` は入れるときに構文を確かめる。
            env.add_template(key, source)
                .map_err(|e| format!("テンプレート `{key}` を読めません: {e}"))?;
        }
        // 埋め込みに無い名前は、ここに来る。ディスクは読まない。
        env.set_loader(|name| Err::<Option<String>, _>(load_error(name)));
        Ok(env)
    }

    /// ディスクから読む環境を作る。描くたびに作り直すので、キャッシュもロックも持ちません。
    pub(super) fn disk_env(dir: PathBuf) -> Environment<'static> {
        let mut env = base_env();
        env.set_loader(move |name| load_from_disk(&dir, name));
        env
    }

    fn load_from_disk(
        dir: &Path,
        name: &str,
    ) -> std::result::Result<Option<String>, minijinja::Error> {
        // 規則に合う名前だけを読む。`..` や絶対パスはここで止まる。
        if resolve(name).as_deref() != Some(name) {
            return Err(load_error(name));
        }
        match std::fs::read_to_string(dir.join(name)) {
            Ok(source) => Ok(Some(source)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(load_error(name)),
            Err(e) => Err(template_error(format!(
                "resources/views/{name} を読めません: {e}"
            ))),
        }
    }

    /// 名前を確かめて描く。
    pub(super) fn render_in(
        env: &Environment<'static>,
        name: &str,
        context: Value,
    ) -> Result<String> {
        let path = resolve(name).ok_or_else(|| Error::msg(bad_name(name)))?;
        let template = env.get_template(&path).map_err(|e| {
            if e.kind() == ErrorKind::TemplateNotFound {
                Error::msg(format!(
                    "テンプレート `{name}`（resources/views/{path}）がありません"
                ))
            } else {
                draw_error(name, &e)
            }
        })?;
        template.render(context).map_err(|e| draw_error(name, &e))
    }

    fn draw_error(name: &str, error: &minijinja::Error) -> Error {
        Error::msg(format!("テンプレート `{name}` を描けません: {error}"))
    }

    /// 利用者の `context` を描画の文脈にする。
    ///
    /// `request` が `Some` のとき（`Request::view`）は、リクエストの関数を足します。
    /// 中の `Option` はセッションがあるかどうかです。
    pub(super) fn to_context(
        context: &impl Serialize,
        request: Option<Option<&Session>>,
    ) -> Result<Value> {
        let value = Value::from(Serde(context));
        let user =
            match value.kind() {
                ValueKind::Map => value,
                // `()` は空の文脈。
                ValueKind::None => Value::from_pairs(std::iter::empty::<(Value, Value)>()),
                ValueKind::Invalid => {
                    return Err(Error::msg(format!(
                        "テンプレートに渡す値を変換できません: {value}"
                    )))
                }
                _ => return Err(Error::msg(
                    "テンプレートに渡す値は、構造体か json!({ ... }) のオブジェクトにしてください",
                )),
            };
        let keys: Vec<Value> = user
            .try_iter()
            .map_err(|e| Error::msg(format!("テンプレートに渡す値を読めません: {e}")))?
            .collect();
        for key in &keys {
            if let Some(key) = key.as_str().filter(|k| RESERVED.contains(k)) {
                return Err(Error::msg(format!(
                    "名前 `{key}` はテンプレートの関数に使っています。別の名前にしてください"
                )));
            }
        }
        let Some(session) = request else {
            return Ok(user);
        };

        // リクエストの関数は文脈の値として渡す。共有の環境は書き換えない。
        let mut pairs: Vec<(Value, Value)> = Vec::with_capacity(keys.len() + RESERVED.len());
        for key in keys {
            let item = user.get_item(&key).unwrap_or(Value::UNDEFINED);
            pairs.push((key, item));
        }
        let session = session.cloned();
        let for_token = session.clone();
        pairs.push((
            Value::from("csrf_token"),
            Value::from_function(move || -> std::result::Result<String, minijinja::Error> {
                token(for_token.as_ref(), "csrf_token")
            }),
        ));
        let for_field = session.clone();
        pairs.push((
            Value::from("csrf_field"),
            Value::from_function(move || -> std::result::Result<Value, minijinja::Error> {
                let token = token(for_field.as_ref(), "csrf_field")?;
                Ok(Value::from_safe_string(format!(
                    "<input type=\"hidden\" name=\"_token\" value=\"{}\">",
                    crate::escape_html(&token)
                )))
            }),
        ));
        pairs.push((
            Value::from("old"),
            Value::from_function(move |key: String, default: Option<String>| -> String {
                session
                    .as_ref()
                    .and_then(|s| s.old(&key))
                    .unwrap_or_else(|| default.unwrap_or_default())
            }),
        ));
        Ok(Value::from_pairs(pairs))
    }

    fn token(
        session: Option<&Session>,
        function: &str,
    ) -> std::result::Result<String, minijinja::Error> {
        let session = session.ok_or_else(|| {
            template_error(format!(
                "{function}() には StartSession が要ります。bootstrap/app.rs の with_middleware で登録してください"
            ))
        })?;
        Ok(crate::session::csrf_token(session))
    }
}

#[cfg(not(feature = "view"))]
mod imp {
    use serde::Serialize;

    use crate::error::{Error, Result};
    use crate::session::Session;

    const NO_FEATURE: &str =
        "テンプレートを使うには、Cargo.toml の bengara の features に \"view\" を足してください";

    pub(super) fn render(
        _name: &str,
        _context: &impl Serialize,
        _request: Option<Option<&Session>>,
    ) -> Result<String> {
        Err(Error::msg(NO_FEATURE))
    }

    pub(super) fn exists(_name: &str) -> bool {
        false
    }

    pub(super) fn install_embedded(_views: &'static [(&'static str, &'static str)]) {}

    pub(super) fn check_embedded() -> Result<()> {
        Ok(())
    }
}

#[cfg(all(test, feature = "view"))]
mod tests {
    use super::imp::*;
    use crate::http::Request;
    use crate::session::Session;
    use minijinja::Value;
    use serde_json::json;

    /// 埋め込みの一覧から描く（リリースビルドと同じ道）。
    fn draw(
        views: &'static [(&'static str, &'static str)],
        name: &str,
        context: serde_json::Value,
    ) -> crate::Result<String> {
        let env = embedded_env(views).expect("構文の誤りは無いはず");
        render_in(&env, name, to_context(&context, None)?)
    }

    static LAYOUT: &[(&str, &str)] = &[
        (
            "layouts/app.html",
            "<title>{% block title %}{% endblock %}</title>{% include \"partials.nav\" %}<main>{% block body %}{% endblock %}</main>",
        ),
        ("partials/nav.html", "<nav>{{ title }}</nav>"),
        (
            "posts/index.html",
            "{% extends \"layouts.app\" %}{% block title %}{{ title }}{% endblock %}{% block body %}{% for p in posts %}<p>{{ p }}</p>{% endfor %}{% endblock %}",
        ),
    ];

    #[test]
    fn 継承とincludeが使える() {
        let out = draw(
            LAYOUT,
            "posts.index",
            json!({ "title": "記事", "posts": ["a", "b"] }),
        )
        .unwrap();
        assert_eq!(
            out,
            "<title>記事</title><nav>記事</nav><main><p>a</p><p>b</p></main>"
        );
    }

    #[test]
    fn 空のforとis_definedとマクロが使える() {
        static VIEWS: &[(&str, &str)] = &[(
            "a.html",
            "{% macro b(x) %}<b>{{ x }}</b>{% endmacro %}{% for p in posts %}{{ p }}{% else %}none{% endfor %}|{% if title is defined %}t{% else %}u{% endif %}|{{ b('m') }}",
        )];
        let out = draw(VIEWS, "a", json!({ "posts": [] })).unwrap();
        assert_eq!(out, "none|u|<b>m</b>");
    }

    #[test]
    fn 値は自動で逃がされsafeで逃がさない() {
        static VIEWS: &[(&str, &str)] = &[("a.html", "{{ x }}|{{ x|safe }}")];
        let out = draw(VIEWS, "a", json!({ "x": "<script>" })).unwrap();
        assert_eq!(out, "&lt;script&gt;|<script>");
    }

    #[test]
    fn 未定義の変数はファイル名と行の入ったエラーになる() {
        static VIEWS: &[(&str, &str)] = &[("posts/index.html", "1\n2\n{{ missing }}")];
        let err = draw(VIEWS, "posts.index", json!({}))
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("テンプレート `posts.index` を描けません: "),
            "{err}"
        );
        assert!(err.contains("posts/index.html:3"), "{err}");
    }

    #[test]
    fn 名前の規則() {
        assert_eq!(resolve("posts.index").as_deref(), Some("posts/index.html"));
        assert_eq!(resolve("posts/index").as_deref(), Some("posts/index.html"));
        assert_eq!(resolve("a-b_c.d1").as_deref(), Some("a-b_c/d1.html"));
        assert_eq!(
            resolve("layouts/app.html").as_deref(),
            Some("layouts/app.html")
        );
        assert_eq!(resolve("a/b.c.html").as_deref(), Some("a/b.c.html"));
        for bad in [
            "",
            "posts..index",
            ".posts",
            "posts.",
            "posts index",
            "../x",
            "/x.html",
            "\\x.html",
            "a/../b.html",
            "./a.html",
            "a//b.html",
            "記事",
            "a\\b.html",
        ] {
            assert_eq!(resolve(bad), None, "{bad}");
        }
    }

    #[test]
    fn 使えない名前と無いテンプレートは500の文になる() {
        static VIEWS: &[(&str, &str)] = &[("a.html", "x")];
        let err = draw(VIEWS, "../a", json!({})).unwrap_err().to_string();
        assert_eq!(err, "テンプレート名 `../a` は使えません（例: posts.index）");
        let err = draw(VIEWS, "posts.show", json!({}))
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "テンプレート `posts.show`（resources/views/posts/show.html）がありません"
        );
        assert_eq!(draw(VIEWS, "a.html", json!({})).unwrap(), "x");
    }

    #[test]
    fn includeの中の名前も確かめる() {
        static VIEWS: &[(&str, &str)] = &[
            ("bad.html", "{% include \"../secret\" %}"),
            ("missing.html", "{% include \"partials.none\" %}"),
            (
                "ignored.html",
                "[{% include \"partials.none\" ignore missing %}]",
            ),
        ];
        let err = draw(VIEWS, "bad", json!({})).unwrap_err().to_string();
        assert!(
            err.contains("テンプレート名 `../secret` は使えません"),
            "{err}"
        );
        let err = draw(VIEWS, "missing", json!({})).unwrap_err().to_string();
        // 見つからないときの文は minijinja が書き直す（元の名前が入る）。
        assert!(err.contains("partials.none"), "{err}");
        assert_eq!(draw(VIEWS, "ignored", json!({})).unwrap(), "[]");
    }

    #[test]
    fn 文脈は配列だと500で空でも描ける() {
        static VIEWS: &[(&str, &str)] = &[("a.html", "ok")];
        let env = embedded_env(VIEWS).unwrap();
        let err = to_context(&[1, 2], None).unwrap_err().to_string();
        assert_eq!(
            err,
            "テンプレートに渡す値は、構造体か json!({ ... }) のオブジェクトにしてください"
        );
        assert!(to_context(&3, None).is_err());
        assert_eq!(
            render_in(&env, "a", to_context(&(), None).unwrap()).unwrap(),
            "ok"
        );

        // 構造体も渡せる。
        #[derive(serde::Serialize)]
        struct Page {
            title: &'static str,
        }
        static TITLE: &[(&str, &str)] = &[("t.html", "{{ title }}")];
        let env = embedded_env(TITLE).unwrap();
        let ctx = to_context(&Page { title: "題" }, None).unwrap();
        assert_eq!(render_in(&env, "t", ctx).unwrap(), "題");
    }

    #[test]
    fn urlと翻訳が使える() {
        static VIEWS: &[(&str, &str)] = &[(
            "a.html",
            "{{ url('/posts') }}|{{ url('/q?a=\"x\"') }}|{{ __('posts.empty') }}|{{ __('posts.empty', {'n': 1}) }}",
        )];
        let out = draw(VIEWS, "a", json!({})).unwrap();
        let parts: Vec<&str> = out.split('|').collect();
        assert!(parts[0].ends_with("/posts"), "{out}");
        // url() は逃がしてから出す（属性の外へ出られない）。
        assert!(parts[1].ends_with("/q?a=&quot;x&quot;"), "{out}");
        // 表に無い鍵は鍵そのもの。
        assert_eq!(parts[2], "posts.empty");
        assert_eq!(parts[3], "posts.empty");
    }

    #[test]
    fn routeとurlはアンパサンドも逃がす() {
        // クエリ文字列に回した引数の区切り。属性の中では `&amp;` で同じ意味になる。
        assert_eq!(
            safe_url("/posts?page=2&sort=new").to_string(),
            "/posts?page=2&amp;sort=new"
        );
        assert_eq!(safe_url("/a\"b").to_string(), "/a&quot;b");
    }

    #[test]
    fn routeの失敗は描画のエラーになる() {
        // ルート名の表はプロセス全体のものなので、ここでは入れない（routing.rs のテストを参照）。
        // 登録されていない名前と、形の悪い引数だけを確かめる。
        static VIEWS: &[(&str, &str)] = &[
            ("a.html", "{{ route('__bengara_no_such_route') }}"),
            ("b.html", "{{ route('posts.show', [1]) }}"),
            ("c.html", "{{ route('posts.show', {'post': [1]}) }}"),
        ];
        let err = draw(VIEWS, "a", json!({})).unwrap_err().to_string();
        assert!(err.contains("テンプレート `a` を描けません"), "{err}");
        assert!(err.contains("a.html:1"), "{err}");
        for name in ["b", "c"] {
            let err = draw(VIEWS, name, json!({})).unwrap_err().to_string();
            assert!(err.contains("文字列か数の値を持つオブジェクト"), "{err}");
        }
    }

    static FORM: &[(&str, &str)] = &[(
        "form.html",
        "{{ csrf_field() }}|{{ csrf_token() }}|{{ old('title') }}|{{ old('none', '既定') }}",
    )];

    #[test]
    fn csrf_fieldとoldはreq_viewで使える() {
        let session = Session::empty();
        session.flash_input(&[("title".into(), "<b>".into())]);
        let mut req = Request::new("get", "/");
        req.set_session(session.clone());

        let env = embedded_env(FORM).unwrap();
        let ctx = to_context(&json!({}), Some(req.try_session())).unwrap();
        let out = render_in(&env, "form", ctx).unwrap();
        let token = crate::session::csrf_token(&session);
        assert_eq!(
            out,
            format!(
                "<input type=\"hidden\" name=\"_token\" value=\"{token}\">|{token}|&lt;b&gt;|既定"
            )
        );
    }

    #[test]
    fn セッションが無いとcsrfはエラーでoldは既定値() {
        static OLD: &[(&str, &str)] = &[("old.html", "[{{ old('title') }}]")];
        let env = embedded_env(OLD).unwrap();
        let ctx = to_context(&json!({}), Some(None)).unwrap();
        assert_eq!(render_in(&env, "old", ctx).unwrap(), "[]");

        let env = embedded_env(FORM).unwrap();
        let ctx = to_context(&json!({}), Some(None)).unwrap();
        let err = render_in(&env, "form", ctx).unwrap_err().to_string();
        assert!(err.contains("StartSession が要ります"), "{err}");
    }

    #[test]
    fn req_viewの外ではリクエストの関数はエラー() {
        let err = draw(FORM, "form", json!({})).unwrap_err().to_string();
        assert!(
            err.contains("csrf_field() は req.view(..) の中でだけ使えます"),
            "{err}"
        );
    }

    #[test]
    fn 予約の鍵が文脈にあると500() {
        for key in ["csrf_token", "csrf_field", "old"] {
            let err = to_context(&json!({ key: 1 }), None)
                .unwrap_err()
                .to_string();
            assert_eq!(
                err,
                format!("名前 `{key}` はテンプレートの関数に使っています。別の名前にしてください")
            );
        }
    }

    #[test]
    fn 構文の誤りは組み立てるときに分かる() {
        static VIEWS: &[(&str, &str)] = &[("ok.html", "ok"), ("posts/bad.html", "{% if %}")];
        let err = embedded_env(VIEWS).err().unwrap();
        assert!(
            err.starts_with("テンプレート `posts/bad.html` を読めません: "),
            "{err}"
        );
    }

    #[test]
    fn ディスクから読む() {
        let dir = std::env::temp_dir().join(format!("bengara-views-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("layouts")).unwrap();
        std::fs::write(
            dir.join("layouts/app.html"),
            "<b>{% block body %}{% endblock %}</b>",
        )
        .unwrap();
        std::fs::write(
            dir.join("home.html"),
            "{% extends \"layouts/app.html\" %}{% block body %}{{ name }}{% endblock %}",
        )
        .unwrap();
        let env = disk_env(dir.clone());
        let ctx = to_context(&json!({ "name": "<x>" }), None).unwrap();
        assert_eq!(render_in(&env, "home", ctx).unwrap(), "<b>&lt;x&gt;</b>");

        // 直したら、作り直した環境で反映される。
        std::fs::write(dir.join("home.html"), "new").unwrap();
        let env = disk_env(dir.clone());
        assert_eq!(render_in(&env, "home", Value::from(())).unwrap(), "new");

        let err = render_in(&env, "none", Value::from(()))
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "テンプレート `none`（resources/views/none.html）がありません"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(all(test, not(feature = "view")))]
mod tests {
    #[test]
    fn フラグが無いと500の文を返しexistsは偽() {
        let err = super::render("posts.index", ()).unwrap_err().to_string();
        assert_eq!(
            err,
            "テンプレートを使うには、Cargo.toml の bengara の features に \"view\" を足してください"
        );
        assert!(super::view("posts.index", ()).is_err());
        assert!(!super::exists("posts.index"));
        assert!(super::check_embedded().is_ok());
    }
}
