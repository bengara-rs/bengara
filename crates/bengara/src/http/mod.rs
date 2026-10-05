//! HTTP の入口。リクエスト、レスポンス、ルーティング。

pub(crate) mod cookie;
pub(crate) mod handler;
pub(crate) mod middleware;
pub(crate) mod request;
pub(crate) mod response;
pub(crate) mod routing;
pub(crate) mod statics;
pub(crate) mod throttle;
pub(crate) mod url;

pub use cookie::{Cookie, SameSite};
pub use handler::{BoxFuture, Handler};
pub use middleware::{Middleware, Middlewares, Next};
pub use request::Request;
pub use response::{
    abort, abort_with, escape_html, html, json, redirect, text, Redirect, Response,
};
pub use routing::{route, route_with, Registered, Route, RouteGroup};
pub use throttle::Throttle;
pub use url::{
    has_valid_signature, route_url, route_url_with, signed_url, temporary_signed_url, url,
};
