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
pub mod auth;
pub mod cache;
mod cli;
pub(crate) mod commands;
mod config_registry;
pub mod console;
pub mod database;
mod env_vars;
mod error;
pub mod events;
mod http;
mod kernel_impl;
pub mod lang;
pub mod mail;
pub(crate) mod ops;
mod paths;
pub mod queue;
pub mod schedule;
mod server;
pub mod session;
mod storage;
mod support;
pub mod validation;

pub mod testing;

pub use application::{Application, ApplicationBuilder, Exceptions, Routing};
pub use auth::{
    authorize, authorize_with, decrypt, encrypt, Auth, Authenticatable, Authenticate, Hash,
    HashConfig, PasswordReset,
};
pub use cache::{Cache, CacheConfig, CacheStore, FileCache, MemoryCache};
pub use config_registry::{config, try_config, AppConfig, Registry};
pub use console::{Command, CommandFuture};
pub use database::{
    now, Affected, Blueprint, ConnectionConfig, DatabaseConfig, Driver, Migration, Model,
    ModelQuery, Paginator, QueryBuilder, Row, Schema, Seeder, SeederFuture, Transaction, Value, DB,
};
pub use env_vars::{env, FromEnv};
pub use error::{Error, Result};
pub use events::{Event, Events, Listener, ListenerFuture};
pub use http::{
    abort, abort_with, escape_html, has_valid_signature, html, json, redirect, route, route_url,
    route_url_with, route_with, signed_url, temporary_signed_url, text, url, BoxFuture, Cookie,
    Handler, Middleware, Middlewares, Next, Redirect, Registered, Request, Response, Route,
    RouteGroup, SameSite, Throttle,
};
pub use lang::{__with, Lang, LangTable, __};
pub use mail::{Mail, MailConfig, Mailer, Message};
pub use paths::{app_path, base_path, public_path, storage_path};
pub use queue::{Job, JobFuture, Queue};
pub use schedule::{Every, Schedule, Task};
pub use storage::{DiskConfig, Storage, StorageConfig};

/// `#[bengara::test]`：`tests/` 以下のテスト関数に付ける。
pub use bengara_macros::test;

/// `#[derive(Model)]`：`app/Models/` の構造体に付けて、表と結びつける。
pub use bengara_macros::Model;

/// serde と tracing をそのまま使えるようにしておく。
///
/// アプリ側で `Cargo.toml` に書かなくても `bengara::serde` / `bengara::serde_json` /
/// `bengara::tracing` で使えます。
/// フレームワークと同じ版が使われるので、版がずれません。
///
/// ```ignore
/// json(&bengara::serde_json::json!({ "status": "ok" }))
/// ```
pub use {serde, serde_json, tracing};

/// `bengara::app!()` から渡される、アプリ側の自動検出の結果。
///
/// 中身は `bengara-build` が生成します。利用者が直接作ることはありません。
#[derive(Clone, Copy)]
pub struct Hooks {
    /// `config/` 直下の各ファイルの `config()` を呼んで登録する。
    pub configs: fn(&mut Registry),
    /// `database/migrations/` のマイグレーションの一覧（名前順）。
    pub migrations: fn() -> &'static [Migration],
    /// `database/seeders/` のシーダーの一覧（名前順）。
    pub seeders: fn() -> &'static [Seeder],
    /// `app/Jobs/` のジョブの一覧（名前順）。
    pub jobs: fn() -> &'static [Job],
    /// `app/Console/Commands/` の自作コマンドの一覧（名前順）。
    pub commands: fn() -> &'static [Command],
    /// `resources/lang/*.toml` から読んだ文字の表。
    pub lang: fn() -> LangTable,
    /// リリースビルドでバイナリに埋め込んだ `public/` の中身。
    ///
    /// 鍵は `public/` からの相対パスで、区切りは常に `/` です。
    /// デバッグビルドでは空になり、ディスクの `public/` を読みます。
    pub public: fn() -> &'static [(&'static str, &'static [u8])],
    /// `routes/console.rs` の `schedule()` を呼んで登録する。
    pub schedule: fn(&mut Schedule),
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            configs: |_| {},
            migrations: || &[],
            seeders: || &[],
            jobs: || &[],
            commands: || &[],
            lang: || &[],
            public: || &[],
            schedule: |_| {},
        }
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
        abort, abort_with, app_path, base_path, config, env, escape_html, has_valid_signature,
        html, json, public_path, redirect, route, route_url, route_url_with, route_with,
        signed_url, storage_path, temporary_signed_url, text, try_config, url, AppConfig,
        Application, BoxFuture, Cookie, Error, Exceptions, Middleware, Middlewares, Next, Redirect,
        Registered, Request, Response, Result, Route, RouteGroup, Routing, SameSite, Throttle,
    };

    // セッションまわりは `session::` のまま使うと長いので、よく使う型だけ入れておきます。
    pub use crate::session::{Session, SessionConfig, StartSession, VerifyCsrfToken};
    pub use crate::validation::{Validated, ValidationErrors};

    // データベース。`#[derive(Model)]` とトレイトの `Model` は名前が同じですが、
    // Rust では別の種類の名前なので両方使えます。
    pub use crate::database::{
        now, Affected, Blueprint, ConnectionConfig, DatabaseConfig, Driver, Migration, Model,
        ModelQuery, Paginator, QueryBuilder, Row, Schema, Seeder, Transaction, Value, DB,
    };
    pub use bengara_macros::Model;

    // 認証と認可。
    pub use crate::auth::{
        authorize, authorize_with, decrypt, encrypt, Auth, Authenticatable, Authenticate, Hash,
        HashConfig, PasswordReset,
    };

    // 周辺（キャッシュ・ファイル・キュー・スケジューラ・イベント・メール・多言語）。
    pub use crate::cache::{Cache, CacheConfig, CacheStore};
    pub use crate::console::Command;
    pub use crate::events::{Event, Events};
    pub use crate::lang::{__with, Lang, __};
    pub use crate::mail::{Mail, MailConfig, Message};
    pub use crate::queue::{Job, Queue};
    pub use crate::schedule::{Every, Schedule, Task};
    pub use crate::storage::{DiskConfig, Storage, StorageConfig};
}
