//! HTTP の入口。リクエスト、レスポンス、ルーティング。

pub(crate) mod handler;
pub(crate) mod middleware;
pub(crate) mod request;
pub(crate) mod response;
pub(crate) mod routing;
pub(crate) mod statics;

pub use handler::{BoxFuture, Handler};
pub use middleware::{Middleware, Middlewares, Next};
pub use request::Request;
pub use response::{
    abort, abort_with, escape_html, html, json, redirect, text, Redirect, Response,
};
pub use routing::{route, route_with, Registered, Route, RouteGroup};
