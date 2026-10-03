//! ルートの登録と照合。
//!
//! `routes/web.rs` の中で `Route::get(...)` を呼ぶと、組み立て中の一覧に積まれます。
//! 起動時に一度だけ固定し、以後は読むだけです。

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::error::{Error, Result};
use crate::http::handler::{Erased, ErasedHandler, Handler};

/// 1本のルート。
pub(crate) struct RouteDef {
    pub(crate) method: &'static str,
    pub(crate) path: String,
    pub(crate) name: Option<String>,
    pub(crate) handler: Arc<dyn ErasedHandler>,
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
            push(RouteDef {
                method: $method,
                path: normalize(path),
                name: None,
                handler: Arc::new(Erased::new(handler)),
            })
        }
    };
}

impl Route {
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

/// 登録したルートに名前を付けるための戻り値。
pub struct Registered {
    index: Option<usize>,
}

impl Registered {
    /// ルートに名前を付ける。`route("home")` で URL を引けるようになります。
    pub fn name(self, name: &str) {
        let Some(index) = self.index else { return };
        COLLECTING.with(|c| {
            if let Some(list) = c.borrow_mut().as_mut() {
                if let Some(def) = list.get_mut(index) {
                    def.name = Some(name.to_string());
                }
            }
        });
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
                if let Some(existing) = names.insert(name.clone(), def.path.clone()) {
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
        let params = matched
            .params
            .iter()
            .map(|(k, v)| (k.to_string(), super::request::percent_decode(v)))
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

static NAMES: OnceLock<HashMap<String, String>> = OnceLock::new();

/// 名前付きルートの一覧を、どこからでも引けるようにする。
pub(crate) fn install_names(names: HashMap<String, String>) {
    let _ = NAMES.set(names);
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
pub fn route_with(name: &str, params: &[(&str, &str)]) -> Result<String> {
    let names = NAMES
        .get()
        .ok_or_else(|| Error::msg("ルートがまだ組み立てられていません"))?;
    let pattern = names
        .get(name)
        .ok_or_else(|| Error::msg(format!("ルート名 `{name}` は登録されていません")))?;
    fill(pattern, params)
}

/// `/posts/{post}` の `{post}` を値で置き換える。
fn fill(pattern: &str, params: &[(&str, &str)]) -> Result<String> {
    let mut out = String::with_capacity(pattern.len());
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            return Err(Error::msg(format!(
                "ルート `{pattern}` の `{{` が閉じていません"
            )));
        };
        let key = after[..end].trim_start_matches('*');
        let value = params
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
            .ok_or_else(|| {
                Error::msg(format!(
                    "ルート `{pattern}` のパス引数 `{key}` が足りません"
                ))
            })?;
        out.push_str(value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
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
    fn パス引数を埋められる() {
        assert_eq!(
            fill("/posts/{post}", &[("post", "12")]).unwrap(),
            "/posts/12"
        );
        assert_eq!(fill("/", &[]).unwrap(), "/");
        assert!(fill("/posts/{post}", &[]).is_err());
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
    fn 収集の外での登録は無視される() {
        let r = Route::get("/outside", ok);
        r.name("outside");
    }
}
