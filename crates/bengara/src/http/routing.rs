//! ルートの登録と照合。
//!
//! `routes/web.rs` の中で `Route::get(...)` を呼ぶと、組み立て中の一覧に積まれます。
//! 起動時に一度だけ固定し、以後は読むだけです。

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::error::{Error, Result};
use crate::http::handler::{Erased, ErasedHandler, Handler};
use crate::http::middleware::Middleware;

/// 1本のルート。
pub(crate) struct RouteDef {
    pub(crate) method: &'static str,
    pub(crate) path: String,
    /// ルート名。`Arc<str>` なのは、`Request` へ渡すときに確保しないためです。
    pub(crate) name: Option<Arc<str>>,
    pub(crate) middleware: Vec<String>,
    pub(crate) handler: Arc<dyn ErasedHandler>,
    /// 起動時に組み立てた並び。`create()` で入ります。
    pub(crate) stack: Option<Arc<[Arc<dyn Middleware>]>>,
}

thread_local! {
    /// `routes/web.rs` を呼んでいる間だけ中身が入る。
    static COLLECTING: RefCell<Option<Vec<RouteDef>>> = const { RefCell::new(None) };
}

/// `routes/web.rs` の関数を呼び、その間に登録されたルートを集める。
pub(crate) fn collect(define: impl FnOnce()) -> Vec<RouteDef> {
    COLLECTING.with(|c| {
        let previous = c.borrow_mut().replace(Vec::new());
        // 入れ子で呼ばれても元に戻せるようにしておく。
        struct Guard(Option<Vec<RouteDef>>);
        impl Drop for Guard {
            fn drop(&mut self) {
                COLLECTING.with(|c| *c.borrow_mut() = self.0.take());
            }
        }
        let _guard = Guard(previous);
        define();
        COLLECTING
            .with(|c| c.borrow_mut().replace(Vec::new()))
            .unwrap_or_default()
    })
}

fn push(def: RouteDef) -> Registered {
    COLLECTING.with(|c| {
        let mut slot = c.borrow_mut();
        match slot.as_mut() {
            Some(list) => {
                list.push(def);
                Registered {
                    index: Some(list.len() - 1),
                }
            }
            None => {
                // `with_routing` の外で呼ばれた場合。落とさずに知らせる。
                tracing::error!(
                    "Route::{} {} は routes/*.rs の外で登録されたため無視します",
                    def.method.to_ascii_lowercase(),
                    def.path
                );
                Registered { index: None }
            }
        }
    })
}

/// ルートを登録する入口。
///
/// ```ignore
/// Route::get("/", HomeController::index).name("home");
/// ```
pub struct Route;

macro_rules! route_method {
    ($name:ident, $method:literal, $doc:literal) => {
        #[doc = $doc]
        pub fn $name<H, Args>(path: &str, handler: H) -> Registered
        where
            H: Handler<Args>,
            Args: 'static,
        {
            let group = folded();
            push(RouteDef {
                method: $method,
                path: normalize(&format!("{}{}", group.prefix, normalize(path))),
                name: None,
                middleware: group.middleware,
                handler: Arc::new(Erased::new(handler)),
                stack: None,
            })
        }
    };
}

impl Route {
    /// パスの頭をそろえるグループを作り始める。
    ///
    /// ```ignore
    /// Route::prefix("admin").group(|| { ... });
    /// ```
    pub fn prefix(prefix: &str) -> RouteGroup {
        RouteGroup::default().prefix(prefix)
    }

    /// ミドルウェアをまとめて付けるグループを作り始める。
    ///
    /// ```ignore
    /// Route::middleware("auth").group(|| { ... });
    /// ```
    pub fn middleware(name: &str) -> RouteGroup {
        RouteGroup::default().middleware(name)
    }

    /// ルート名の頭をそろえるグループを作り始める。点は自分で書きます。
    ///
    /// ```ignore
    /// Route::name("admin.").group(|| { ... });
    /// ```
    pub fn name(name: &str) -> RouteGroup {
        RouteGroup::default().name(name)
    }

    route_method!(
        get,
        "GET",
        "GET のルートを登録する（HEAD にも応答します）。"
    );
    route_method!(post, "POST", "POST のルートを登録する。");
    route_method!(put, "PUT", "PUT のルートを登録する。");
    route_method!(patch, "PATCH", "PATCH のルートを登録する。");
    route_method!(delete, "DELETE", "DELETE のルートを登録する。");
}

/// 登録したルートに名前やミドルウェアを足すための戻り値。
///
/// ```ignore
/// Route::get("/admin", AdminController::index)
///     .middleware("admin")
///     .name("admin.index");
/// ```
pub struct Registered {
    index: Option<usize>,
}

impl Registered {
    /// ルートに名前を付ける。`route("home")` で URL を引けるようになります。
    ///
    /// グループの中なら、外側の `name(...)` が頭に付きます。
    pub fn name(self, name: &str) -> Self {
        let full = format!("{}{name}", folded().name);
        self.edit(move |def| def.name = Some(full.into()))
    }

    /// ルートにミドルウェアを付ける。名前は `bootstrap/app.rs` の `alias` で登録したものです。
    ///
    /// 何回でも呼べます。書いた順に通ります。
    pub fn middleware(self, name: &str) -> Self {
        self.edit(|def| def.middleware.push(name.to_string()))
    }

    fn edit(self, change: impl FnOnce(&mut RouteDef)) -> Self {
        let Some(index) = self.index else { return self };
        COLLECTING.with(|c| {
            if let Some(list) = c.borrow_mut().as_mut() {
                if let Some(def) = list.get_mut(index) {
                    change(def);
                }
            }
        });
        self
    }
}

thread_local! {
    /// `group()` の中にいる間だけ積まれる、外側の設定。
    static GROUPS: RefCell<Vec<GroupConfig>> = const { RefCell::new(Vec::new()) };
}

/// グループ1段ぶんの設定。
#[derive(Default, Clone)]
struct GroupConfig {
    prefix: String,
    name: String,
    middleware: Vec<String>,
}

/// 積まれているグループを1つに畳む。
fn folded() -> GroupConfig {
    GROUPS.with(|g| {
        let mut out = GroupConfig::default();
        for level in g.borrow().iter() {
            out.prefix.push_str(&level.prefix);
            out.name.push_str(&level.name);
            out.middleware.extend(level.middleware.iter().cloned());
        }
        out
    })
}

/// ルートをまとめて設定するためのグループ。
///
/// ```ignore
/// Route::prefix("admin").middleware("auth").name("admin.").group(|| {
///     Route::get("/users", AdminController::users).name("users");
/// });
/// ```
#[derive(Default)]
pub struct RouteGroup {
    config: GroupConfig,
}

impl RouteGroup {
    /// パスの頭に足す文字列を決める。
    pub fn prefix(mut self, prefix: &str) -> Self {
        self.config.prefix.push_str(&prefix_segment(prefix));
        self
    }

    /// 中のルート全部に付けるミドルウェアを足す。
    pub fn middleware(mut self, name: &str) -> Self {
        self.config.middleware.push(name.to_string());
        self
    }

    /// ルート名の頭に足す文字列を決める。
    ///
    /// 点は自分で書きます（`name("admin.")`）。Laravel と同じです。
    pub fn name(mut self, name: &str) -> Self {
        self.config.name.push_str(name);
        self
    }

    /// グループを閉じる。中で登録したルートに、ここまでの設定が付きます。
    ///
    /// 入れ子にできます。外側の設定は内側へ引き継がれます。
    pub fn group(self, define: impl FnOnce()) {
        GROUPS.with(|g| g.borrow_mut().push(self.config));
        // 中でパニックしても必ず降ろす。
        struct Guard;
        impl Drop for Guard {
            fn drop(&mut self) {
                GROUPS.with(|g| {
                    g.borrow_mut().pop();
                });
            }
        }
        let _guard = Guard;
        define();
    }
}

/// プレフィックスを `/admin` の形にする。空なら何も足さない。
fn prefix_segment(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("/{trimmed}")
    }
}

/// パスの先頭に `/` を付け、末尾の余分な `/` を落とす。
pub(crate) fn normalize(path: &str) -> String {
    let trimmed = path.trim();
    let mut out = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    };
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// 照合に使う、固定されたルートの一覧。
pub(crate) struct Routes {
    by_method: HashMap<&'static str, matchit::Router<usize>>,
    defs: Vec<RouteDef>,
    names: HashMap<String, String>,
}

/// 照合の結果。
pub(crate) enum Matched<'a> {
    /// 当たった。
    Found {
        def: &'a RouteDef,
        params: Vec<(String, String)>,
    },
    /// パスは合うが、メソッドが違う。
    MethodNotAllowed { allowed: Vec<&'static str> },
    /// 見つからない。
    NotFound,
}

impl Routes {
    /// ルートの一覧を固定する。
    ///
    /// # パニック
    ///
    /// 同じメソッドとパスのルートが2本あるときは、起動時にパニックします。
    pub(crate) fn build(defs: Vec<RouteDef>) -> Self {
        let mut by_method: HashMap<&'static str, matchit::Router<usize>> = HashMap::new();
        let mut names: HashMap<String, String> = HashMap::new();

        for (index, def) in defs.iter().enumerate() {
            let router = by_method.entry(def.method).or_default();
            if let Err(e) = router.insert(def.path.clone(), index) {
                panic!(
                    "ルートの登録に失敗しました: {} {} ({e})。同じパスが2回登録されていないか確かめてください",
                    def.method, def.path
                );
            }
            if let Some(name) = &def.name {
                if let Some(existing) = names.insert(name.to_string(), def.path.clone()) {
                    panic!(
                        "ルート名 `{name}` が2回使われています（{existing} と {}）",
                        def.path
                    );
                }
            }
        }
        Self {
            by_method,
            defs,
            names,
        }
    }

    /// メソッドとパスからルートを探す。
    pub(crate) fn find(&self, method: &str, path: &str) -> Matched<'_> {
        // 登録側は末尾の `/` を落とす（`normalize`）ので、照合側でも落としてから引く。
        // そうしないと `GET /posts/` が 404 になります。リダイレクトはしません。
        // `/` 単独は落とさない（それ自体がルート）。
        if path.len() > 1 && path.ends_with('/') {
            let trimmed = path.trim_end_matches('/');
            // 全部 `/` だったときは `/` として引く。
            let trimmed = if trimmed.is_empty() { "/" } else { trimmed };
            return self.find_slashless(method, trimmed);
        }
        self.find_slashless(method, path)
    }

    /// 末尾の `/` を落としたパスで探す。
    fn find_slashless(&self, method: &str, path: &str) -> Matched<'_> {
        if let Some(found) = self.find_exact(method, path) {
            return found;
        }
        // HEAD は GET で応答する。
        if method == "HEAD" {
            if let Some(found) = self.find_exact("GET", path) {
                return found;
            }
        }
        let allowed: Vec<&'static str> = self
            .by_method
            .iter()
            .filter(|(_, router)| router.at(path).is_ok())
            .map(|(m, _)| *m)
            .collect();
        if allowed.is_empty() {
            Matched::NotFound
        } else {
            Matched::MethodNotAllowed { allowed }
        }
    }

    fn find_exact(&self, method: &str, path: &str) -> Option<Matched<'_>> {
        let router = self.by_method.get(method)?;
        let matched = router.at(path).ok()?;
        // 照合は符号化されたパスで行い、取り出した値だけを元に戻す。
        // 先に全体を戻すと、`%2F` が区切りの `/` と区別できなくなる。
        // `+` は空白にしない（`+` が空白なのはフォームの規則で、パスには当てはまらない）。
        let params = matched
            .params
            .iter()
            .map(|(k, v)| (k.to_string(), super::percent::decode_strict(v)))
            .collect();
        Some(Matched::Found {
            def: &self.defs[*matched.value],
            params,
        })
    }

    /// 登録されているルートの一覧（`route:list` の表示用）。
    pub(crate) fn all(&self) -> &[RouteDef] {
        &self.defs
    }

    /// 名前とパスの対応。
    pub(crate) fn names(&self) -> &HashMap<String, String> {
        &self.names
    }
}

/// 名前付きルートの一覧。**後から組み立てた `Application` で上書きします。**
///
/// 以前は `OnceLock` でした。2 つ目以降の `Application` を黙って無視するので、
/// `#[bengara::test]` のテストは 2 件目から 1 件目のルート表を見ていました。
/// 実行順によって `route()` が別のアプリの URL を返したり、
/// 「登録されていません」になったりします。
///
/// 読む側（`route()`）はリクエストのたびに通ります。`RwLock` の読みは原子的な
/// 加算 1 回で、`fill` が返す `String` の確保より軽いので、ここは `RwLock` で足ります。
static NAMES: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);

/// 名前付きルートの一覧を、どこからでも引けるようにする。
///
/// 2 回目以降は**上書き**します。テストは 1 件ごとに `Application` を組み立てるためです。
pub(crate) fn install_names(names: HashMap<String, String>) {
    let mut slot = NAMES.write().unwrap_or_else(|e| e.into_inner());
    *slot = Some(names);
}

/// 名前付きルートの URL を引く。
///
/// ```ignore
/// let url = route("home")?;
/// ```
pub fn route(name: &str) -> Result<String> {
    route_with(name, &[])
}

/// 名前付きルートの URL を、パス引数を埋めて引く。
///
/// ```ignore
/// let url = route_with("posts.show", &[("post", "12")])?;
/// ```
///
/// パスに出てこない引数は、Laravel と同じように**クエリ文字列として足します**。
/// `route_with("posts.index", &[("page", "2")])` は `/posts?page=2` になります。
pub fn route_with(name: &str, params: &[(&str, &str)]) -> Result<String> {
    let slot = NAMES.read().unwrap_or_else(|e| e.into_inner());
    let names = slot
        .as_ref()
        .ok_or_else(|| Error::msg("ルートがまだ組み立てられていません"))?;
    let pattern = names
        .get(name)
        .ok_or_else(|| Error::msg(format!("ルート名 `{name}` は登録されていません")))?;
    fill(pattern, params)
}

/// `/posts/{post}` の `{post}` を値で置き換える。
///
/// パスに出てこなかった引数はクエリ文字列として後ろに足します。**Laravel と同じ**です。
/// 以前は黙って捨てていたので、名前を打ち間違えた引数がどこにも現れませんでした。
///
/// `{` は必ずパス引数の始まりとして読みます。`{{` と書いて `{` そのものを表すことは
/// できません。以前は `{{post}}` を `{post` という名前の引数として読み、
/// 「パス引数 `{post` が足りません」という分かりにくいエラーになっていました。
/// Laravel のルートのパスにも `{` をそのまま書く書き方は無いので、逃がし方は作らず、
/// 理由の分かるエラーで知らせます。
fn fill(pattern: &str, params: &[(&str, &str)]) -> Result<String> {
    let mut out = String::with_capacity(pattern.len());
    // パスに埋めた引数の名前。残りをクエリ文字列に回すために覚えます。
    let mut filled: Vec<&str> = Vec::new();
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            return Err(Error::msg(format!(
                "ルート `{pattern}` の `{{` が閉じていません"
            )));
        };
        let name = &after[..end];
        if name.contains('{') {
            return Err(Error::msg(format!(
                "ルート `{pattern}` に `{{` が続いています。\
                 `{{` は必ずパス引数の始まりとして読むので、そのままの `{{` は書けません"
            )));
        }
        let key = name.trim_start_matches('*');
        if key.is_empty() {
            return Err(Error::msg(format!(
                "ルート `{pattern}` のパス引数の名前が空です"
            )));
        }
        let value = params
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
            .ok_or_else(|| {
                Error::msg(format!(
                    "ルート `{pattern}` のパス引数 `{key}` が足りません"
                ))
            })?;
        filled.push(key);
        // `{*path}` は複数のセグメントを表すので `/` を残す。
        out.push_str(&encode_segment(value, name.starts_with('*')));
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    append_query(&mut out, params, &filled);
    Ok(out)
}

/// パスに出てこなかった引数を、クエリ文字列として足す。
///
/// 並びは渡された順のままです。逃がす文字の規則は `http/percent.rs` にあり、
/// 署名の組み立て（`http/url.rs`）と**同じ関数**を使います。
fn append_query(out: &mut String, params: &[(&str, &str)], filled: &[&str]) {
    let mut first = true;
    for (key, value) in params.iter().filter(|(k, _)| !filled.contains(k)) {
        out.push(if first { '?' } else { '&' });
        first = false;
        out.push_str(&super::percent::encode(key, super::percent::unreserved));
        out.push('=');
        out.push_str(&super::percent::encode(value, super::percent::unreserved));
    }
}

/// パス引数の値を、1セグメント分としてパーセントエンコードする。
///
/// 照合は符号化されたパスで行うので、組み立てる側もそろえないと形が変わります。
/// そろえないと `("name", "a/b")` が `/hello/a/b` になり、別のルートになってしまいます。
/// 逃がす文字の規則は `http/percent.rs` にあり、`http/url.rs` の署名の組み立てと
/// **同じ関数**を使います（以前は同じ規則を2か所に書いていました）。
fn encode_segment(value: &str, keep_slash: bool) -> String {
    let keep = if keep_slash {
        super::percent::unreserved_or_slash
    } else {
        super::percent::unreserved
    };
    super::percent::encode(value, keep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::Response;

    #[test]
    fn パスを整える() {
        assert_eq!(normalize("/"), "/");
        assert_eq!(normalize("posts"), "/posts");
        assert_eq!(normalize("/posts/"), "/posts");
        assert_eq!(normalize("  /posts  "), "/posts");
    }

    #[test]
    fn ルート名の表は後から入れたもので上書きする() {
        // 以前は `OnceLock` で、2 つ目以降の `Application` を黙って無視していた。
        // テストは 1 件ごとに組み立てるので、2 件目から 1 件目の表を見ていた。
        //
        // このテストはプロセス全体の表を差し替えます。`route()` を読む単体テストは
        // ここだけなので、他のテストには影響しません。足すときは気をつけてください。
        let mut first = HashMap::new();
        first.insert("home".to_string(), "/".to_string());
        install_names(first);
        assert_eq!(route("home").unwrap(), "/");

        let mut second = HashMap::new();
        second.insert("about".to_string(), "/about".to_string());
        install_names(second);
        assert_eq!(route("about").unwrap(), "/about");
        // 前の表はもう見ない。
        assert!(route("home").is_err());
    }

    #[test]
    fn 余ったパス引数はクエリ文字列にする() {
        // Laravel と同じ。以前は黙って捨てていたので、打ち間違えに気づけなかった。
        assert_eq!(fill("/posts", &[("page", "2")]).unwrap(), "/posts?page=2");
        assert_eq!(
            fill(
                "/posts/{post}",
                &[("post", "12"), ("page", "2"), ("q", "あ")]
            )
            .unwrap(),
            "/posts/12?page=2&q=%E3%81%82"
        );
        // 埋めた引数はクエリに重ねない。
        assert_eq!(
            fill("/posts/{post}", &[("post", "12")]).unwrap(),
            "/posts/12"
        );
    }

    #[test]
    fn 二重の波かっこは理由の分かるエラーにする() {
        // 以前は `{post` という名前の引数として読み、必ず「足りません」になっていた。
        let error = fill("/posts/{{post}}", &[("post", "12")]).unwrap_err();
        assert!(error.to_string().contains("`{`"), "{error}");
        // 名前が空のときも知らせる。
        assert!(fill("/posts/{}", &[]).is_err());
    }

    #[test]
    fn パス引数を埋められる() {
        assert_eq!(
            fill("/posts/{post}", &[("post", "12")]).unwrap(),
            "/posts/12"
        );
        assert_eq!(fill("/", &[]).unwrap(), "/");
        assert!(fill("/posts/{post}", &[]).is_err());
    }

    #[test]
    fn パス引数はエンコードして埋める() {
        // `/` を残すとルートの形が変わるので `%2F` にする。
        assert_eq!(
            fill("/posts/{post}", &[("post", "a/b")]).unwrap(),
            "/posts/a%2Fb"
        );
        // ワイルドカードは複数のセグメントを表すので `/` を残す。
        assert_eq!(
            fill("/files/{*path}", &[("path", "a/b.txt")]).unwrap(),
            "/files/a/b.txt"
        );
        // 日本語も逃がす。
        assert_eq!(
            fill("/hello/{name}", &[("name", "あ")]).unwrap(),
            "/hello/%E3%81%82"
        );
        // 記号も逃がす。`-` `_` `.` `~` はそのまま。
        assert_eq!(
            fill("/q/{term}", &[("term", "a b?c#d")]).unwrap(),
            "/q/a%20b%3Fc%23d"
        );
        assert_eq!(
            fill("/q/{term}", &[("term", "a-b_c.d~e")]).unwrap(),
            "/q/a-b_c.d~e"
        );
    }

    #[test]
    fn 埋めた値はそのまま照合できる() {
        let defs = collect(|| {
            Route::get("/hello/{name}", ok);
        });
        let routes = Routes::build(defs);
        let path = fill("/hello/{name}", &[("name", "a/b")]).unwrap();
        match routes.find("GET", &path) {
            Matched::Found { params, .. } => {
                assert_eq!(params, vec![("name".to_string(), "a/b".to_string())]);
            }
            _ => panic!("当たらなかった"),
        }
    }

    async fn ok() -> Result<Response> {
        Ok(Response::text("ok"))
    }

    #[test]
    fn 収集と照合ができる() {
        let defs = collect(|| {
            Route::get("/", ok).name("home");
            Route::post("/posts", ok);
        });
        assert_eq!(defs.len(), 2);
        let routes = Routes::build(defs);
        assert!(matches!(routes.find("GET", "/"), Matched::Found { .. }));
        assert!(matches!(routes.find("HEAD", "/"), Matched::Found { .. }));
        assert!(matches!(
            routes.find("DELETE", "/"),
            Matched::MethodNotAllowed { .. }
        ));
        assert!(matches!(routes.find("GET", "/missing"), Matched::NotFound));
        assert_eq!(routes.names().get("home").map(String::as_str), Some("/"));
    }

    #[test]
    fn 末尾のスラッシュは同じルートに当たる() {
        let defs = collect(|| {
            Route::get("/posts", ok).name("posts.index");
            Route::get("/", ok).name("home");
        });
        let routes = Routes::build(defs);
        // 登録側は末尾の `/` を落とすので、照合側でも落としてから引く。
        assert!(matches!(
            routes.find("GET", "/posts"),
            Matched::Found { .. }
        ));
        assert!(matches!(
            routes.find("GET", "/posts/"),
            Matched::Found { .. }
        ));
        match routes.find("GET", "/posts/") {
            Matched::Found { def, .. } => assert_eq!(def.path, "/posts"),
            _ => panic!("当たらなかった"),
        }
        // `/` 単独は壊れない。
        assert!(matches!(routes.find("GET", "/"), Matched::Found { .. }));
        // 無いパスは末尾に `/` が付いても 404 のまま。
        assert!(matches!(routes.find("GET", "/missing/"), Matched::NotFound));
    }

    #[test]
    fn グループでパスと名前とミドルウェアが畳まれる() {
        let defs = collect(|| {
            Route::prefix("admin")
                .middleware("auth")
                .name("admin.")
                .group(|| {
                    Route::get("/", ok).name("index");
                    Route::get("/users", ok).name("users");
                });
        });
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].path, "/admin");
        assert_eq!(defs[0].name.as_deref(), Some("admin.index"));
        assert_eq!(defs[0].middleware, vec!["auth".to_string()]);
        assert_eq!(defs[1].path, "/admin/users");
        assert_eq!(defs[1].name.as_deref(), Some("admin.users"));
    }

    #[test]
    fn 入れ子のグループは外側を引き継ぐ() {
        let defs = collect(|| {
            Route::prefix("admin")
                .middleware("auth")
                .name("admin.")
                .group(|| {
                    Route::prefix("posts")
                        .middleware("can")
                        .name("posts.")
                        .group(|| {
                            Route::get("/{post}", ok).name("show");
                        });
                });
        });
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].path, "/admin/posts/{post}");
        assert_eq!(defs[0].name.as_deref(), Some("admin.posts.show"));
        // 外側が先、内側が後。
        assert_eq!(
            defs[0].middleware,
            vec!["auth".to_string(), "can".to_string()]
        );
    }

    #[test]
    fn グループを抜けたら元に戻る() {
        let defs = collect(|| {
            Route::prefix("admin").middleware("auth").group(|| {
                Route::get("/inside", ok);
            });
            Route::get("/outside", ok).name("outside");
        });
        assert_eq!(defs[0].path, "/admin/inside");
        assert_eq!(defs[1].path, "/outside");
        assert!(defs[1].middleware.is_empty());
        assert_eq!(defs[1].name.as_deref(), Some("outside"));
    }

    #[test]
    fn 空のプレフィックスは何も足さない() {
        let defs = collect(|| {
            Route::prefix("").group(|| {
                Route::get("/users", ok);
            });
            Route::prefix("/").group(|| {
                Route::get("/posts", ok);
            });
        });
        assert_eq!(defs[0].path, "/users");
        assert_eq!(defs[1].path, "/posts");
    }

    #[test]
    fn プレフィックスの余分なスラッシュは落ちる() {
        let defs = collect(|| {
            Route::prefix("/admin/").group(|| {
                Route::get("users", ok);
            });
        });
        assert_eq!(defs[0].path, "/admin/users");
    }

    #[test]
    fn グループの中でパニックしても積んだ設定は降りる() {
        let caught = std::panic::catch_unwind(|| {
            collect(|| {
                Route::prefix("boom").group(|| panic!("わざと"));
            });
        });
        assert!(caught.is_err());
        // 次の収集に前のグループが残っていないこと。
        let defs = collect(|| {
            Route::get("/after", ok);
        });
        assert_eq!(defs[0].path, "/after");
    }

    #[test]
    fn ミドルウェアだけのグループも書ける() {
        let defs = collect(|| {
            Route::middleware("auth").group(|| {
                Route::post("/posts", ok);
            });
        });
        assert_eq!(defs[0].path, "/posts");
        assert_eq!(defs[0].middleware, vec!["auth".to_string()]);
    }
    #[test]
    fn パス引数はパーセントデコードされる() {
        let defs = collect(|| {
            Route::get("/hello/{name}", ok);
        });
        let routes = Routes::build(defs);
        match routes.find("GET", "/hello/%E3%81%82") {
            Matched::Found { params, .. } => {
                assert_eq!(params, vec![("name".to_string(), "あ".to_string())]);
            }
            _ => panic!("当たらなかった"),
        }
    }

    #[test]
    fn パス引数のプラスは空白にしない() {
        let defs = collect(|| {
            Route::get("/hello/{name}", ok);
        });
        let routes = Routes::build(defs);
        match routes.find("GET", "/hello/a+b") {
            Matched::Found { params, .. } => {
                assert_eq!(params, vec![("name".to_string(), "a+b".to_string())]);
            }
            _ => panic!("当たらなかった"),
        }
    }

    #[test]
    fn 収集の外での登録は無視される() {
        let r = Route::get("/outside", ok);
        r.name("outside");
    }
}
