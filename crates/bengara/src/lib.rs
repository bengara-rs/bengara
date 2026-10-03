//! # bengara
//!
//! Laravel の構成とそのままの書き味で使える、Rust 製の Web アプリケーションフレームワークです。
//!
//! - ディレクトリとファイルの名前は Laravel と同じ（`app/Http/Controllers/HomeController.rs` など）
//! - `cargo build --release` で実行ファイルが1つできる
//! - コマンドは `cargo artisan ...`。グローバルなインストールは要らない
//!
//! Laravel 公式とは無関係のプロジェクトです。
//!
//! ## 利用者が書く入口
//!
//! ```ignore
//! // main.rs
//! bengara::app!();
//!
//! // artisan.rs
//! fn main() { bengara::artisan() }
//!
//! // build.rs
//! fn main() { bengara_build::discover() }
//! ```

mod application;
mod cli;
mod config_registry;
mod env_vars;
mod error;
mod http;
mod kernel_impl;
mod paths;
mod server;

pub mod testing;

pub use application::{Application, ApplicationBuilder, Routing};
pub use config_registry::{config, try_config, AppConfig, Registry};
pub use env_vars::{env, FromEnv};
pub use error::{Error, Result};
pub use http::{
    abort, abort_with, escape_html, html, json, redirect, route, route_with, text, Handler,
    Redirect, Registered, Request, Response, Route,
};
pub use paths::{app_path, base_path, public_path, storage_path};

/// `#[bengara::test]`：`tests/` 以下のテスト関数に付ける。
pub use bengara_macros::test;

/// serde をそのまま使えるようにしておく。
///
/// アプリ側で `Cargo.toml` に書かなくても `bengara::serde` / `bengara::serde_json` で使えます。
/// フレームワークと同じ版が使われるので、版がずれません。
///
/// ```ignore
/// json(&bengara::serde_json::json!({ "status": "ok" }))
/// ```
pub use {serde, serde_json};

/// `bengara::app!()` から渡される、アプリ側の自動検出の結果。
///
/// 中身は `bengara-build` が生成します。利用者が直接作ることはありません。
#[derive(Clone, Copy)]
pub struct Hooks {
    /// `config/` 直下の各ファイルの `config()` を呼んで登録する。
    pub configs: fn(&mut Registry),
}

impl Default for Hooks {
    fn default() -> Self {
        Self { configs: |_| {} }
    }
}

/// `init` の前の `main.rs`（または `src/main.rs`）の中身。
///
/// ```ignore
/// fn main() { bengara::run() }
/// ```
///
/// この状態では `init` だけを受け付けます。
pub fn run() {
    cli::run_before_init();
}

/// `artisan.rs` の中身。開発用コマンドの入口です。
///
/// ```ignore
/// fn main() { bengara::artisan() }
/// ```
pub fn artisan() {
    cli::artisan();
}

/// `bengara::app!()` の実体。利用者が直接呼ぶことはありません。
#[doc(hidden)]
pub mod kernel {
    pub use crate::kernel_impl::main;
}

/// `main.rs` に書く1行。自動検出の結果を取り込み、`main` を定義します。
///
/// ```ignore
/// bengara::app!();
/// ```
#[allow(clippy::crate_in_macro_def)] // crate:: は意図どおり、呼び出し側のクレートを指す
#[macro_export]
macro_rules! app {
    () => {
        // bengara-build が OUT_DIR に書き出したモジュール宣言を取り込む。
        include!(concat!(env!("OUT_DIR"), "/bengara_app.rs"));

        fn main() {
            $crate::kernel::main(crate::__bengara_hooks(), || crate::bootstrap::app::app());
        }
    };
}

/// よく使うものをまとめた入口。
///
/// ```ignore
/// use bengara::prelude::*;
/// ```
pub mod prelude {
    pub use crate::{
        abort, abort_with, app_path, base_path, config, env, escape_html, html, json, public_path,
        redirect, route, route_with, storage_path, text, try_config, AppConfig, Application, Error,
        Redirect, Registered, Request, Response, Result, Route, Routing,
    };
}
