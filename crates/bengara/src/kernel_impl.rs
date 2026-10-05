//! `bengara::app!()` が定義する `main` の中身。
//!
//! 本体のバイナリは、サーバーの起動と、実行時のコマンド（`route:list` など）を担います。
//! `serve` の監視と再ビルドは `artisan` 側です（`crate::cli`）。

use crate::application::Application;
use crate::config_registry::{self, Registry};
use crate::env_vars::{self, env};
use crate::error::Result;
use crate::{paths, server, Hooks};

/// 本体の入口。`bengara::app!()` から呼ばれます。
pub fn main(hooks: Hooks, factory: fn() -> Application) {
    let base = paths::base_path().to_path_buf();
    let dotenv = env_vars::load_dotenv(&base);
    init_tracing();
    if let Some(path) = &dotenv {
        tracing::debug!("{} を読み込みました", path.display());
    }

    let mut registry = Registry::new();
    (hooks.configs)(&mut registry);
    config_registry::install(registry);

    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(error) = dispatch(&args, factory) {
        eprintln!("エラー: {error}");
        std::process::exit(1);
    }
}

fn dispatch(args: &[String], factory: fn() -> Application) -> Result<()> {
    let (command, rest) = match args.split_first() {
        Some((first, rest)) => (first.as_str(), rest),
        // 引数なしはサーバーの起動。
        None => ("serve", &[] as &[String]),
    };

    match command {
        "serve" => {
            let options = ServeOptions::parse(rest)?;
            let app = build(factory);
            server::serve(app, &options.host, options.port)
        }
        "route:list" => {
            print_routes(&build(factory));
            Ok(())
        }
        "init" => crate::cli::init_here(),
        "--version" | "-V" | "version" => {
            println!("bengara {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "--help" | "-h" | "help" | "list" => {
            print_help();
            Ok(())
        }
        other => {
            eprintln!("`{other}` というコマンドはありません。");
            print_help();
            std::process::exit(1);
        }
    }
}

/// `bootstrap/app.rs` を呼んでアプリを固定する。
fn build(factory: fn() -> Application) -> Application {
    let app = factory();
    app.install_names();
    app
}

/// `serve` の引数。
struct ServeOptions {
    host: String,
    port: u16,
}

impl ServeOptions {
    fn parse(args: &[String]) -> Result<Self> {
        let mut options = Self {
            host: env("APP_HOST", "127.0.0.1"),
            port: env("APP_PORT", 8000u16),
        };
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--host" => {
                    options.host = next_value(&mut iter, "--host")?;
                }
                "--port" => {
                    let raw = next_value(&mut iter, "--port")?;
                    options.port = raw.parse().map_err(|_| {
                        crate::Error::msg(format!("--port の値 `{raw}` が数値ではありません"))
                    })?;
                }
                other => {
                    if let Some(value) = other.strip_prefix("--host=") {
                        options.host = value.to_string();
                    } else if let Some(value) = other.strip_prefix("--port=") {
                        options.port = value.parse().map_err(|_| {
                            crate::Error::msg(format!("--port の値 `{value}` が数値ではありません"))
                        })?;
                    } else {
                        return Err(crate::Error::msg(format!(
                            "serve は `{other}` を受け付けません"
                        )));
                    }
                }
            }
        }
        Ok(options)
    }
}

fn next_value<'a>(iter: &mut impl Iterator<Item = &'a String>, name: &str) -> Result<String> {
    iter.next()
        .map(|v| v.to_string())
        .ok_or_else(|| crate::Error::msg(format!("{name} の値がありません")))
}

/// 登録されているルートを表で出す。
fn print_routes(app: &Application) {
    let routes = app.routes();
    if routes.is_empty() {
        println!("ルートは1本も登録されていません。routes/web.rs を確かめてください。");
        return;
    }
    let method_width = routes
        .iter()
        .map(|r| r.method.len())
        .max()
        .unwrap_or(6)
        .max(6);
    let path_width = routes
        .iter()
        .map(|r| r.path.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let name_width = routes
        .iter()
        .map(|r| r.name.as_deref().unwrap_or("").len())
        .max()
        .unwrap_or(4)
        .max(4);
    // ミドルウェアが1本も無いなら、列ごと出さない。
    let any_middleware = routes.iter().any(|r| !r.middleware.is_empty());
    if any_middleware {
        println!(
            "{:<method_width$}  {:<path_width$}  {:<name_width$}  MIDDLEWARE",
            "METHOD", "URI", "NAME"
        );
    } else {
        println!("{:<method_width$}  {:<path_width$}  NAME", "METHOD", "URI");
    }
    for route in routes {
        let name = route.name.as_deref().unwrap_or("");
        if any_middleware {
            println!(
                "{:<method_width$}  {:<path_width$}  {:<name_width$}  {}",
                route.method,
                route.path,
                name,
                route.middleware.join(", ")
            );
        } else {
            println!(
                "{:<method_width$}  {:<path_width$}  {}",
                route.method, route.path, name
            );
        }
    }
    println!("\n{} 本", routes.len());
}

fn print_help() {
    println!(
        "\
本体のコマンド

  (引数なし) / serve   HTTP サーバーを起動する
      --host <ホスト>  待ち受けるホスト（既定: APP_HOST か 127.0.0.1）
      --port <ポート>  待ち受けるポート（既定: APP_PORT か 8000）
  route:list           登録されているルートを一覧にする
  init                 Laravel と同じ構成のファイルを作る（最初の1回だけ）
  --version            版番号を出す
  --help               このヘルプを出す

開発用のコマンド（init と、serve の自動再ビルド）は artisan 側です。

  cargo artisan list
"
    );
}

/// ログの出力を用意する。
///
/// 出力の細かさは `RUST_LOG` で決めます。設定がなければ、`APP_DEBUG` が真なら
/// `debug`、そうでなければ `info` にします。
///
/// 機能フラグ `log-filter`（既定で入っています）があると、`RUST_LOG=myapp=debug` のような
/// 細かい絞り込みが使えます。外した場合は `trace` / `debug` / `info` / `warn` / `error` の
/// 1語だけを見ます。
fn init_tracing() {
    let default = if env("APP_DEBUG", false) {
        "debug"
    } else {
        "info"
    };

    #[cfg(feature = "log-filter")]
    let builder = {
        use tracing_subscriber::EnvFilter;
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(format!("{default},hyper=warn,tower=warn")));
        tracing_subscriber::fmt().with_env_filter(filter)
    };

    #[cfg(not(feature = "log-filter"))]
    let builder = {
        let level = std::env::var("RUST_LOG")
            .ok()
            .and_then(|raw| parse_level(&raw))
            .or_else(|| parse_level(default))
            .unwrap_or(tracing::Level::INFO);
        tracing_subscriber::fmt().with_max_level(level)
    };

    // 二重に初期化しても落ちないようにする（テストから複数回呼ばれても安全）。
    let _ = builder.with_target(false).try_init();
}

/// `info` などの1語をログの細かさに変える。
#[cfg(not(feature = "log-filter"))]
fn parse_level(raw: &str) -> Option<tracing::Level> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "trace" => Some(tracing::Level::TRACE),
        "debug" => Some(tracing::Level::DEBUG),
        "info" => Some(tracing::Level::INFO),
        "warn" | "warning" => Some(tracing::Level::WARN),
        "error" => Some(tracing::Level::ERROR),
        "off" | "none" => None,
        _ => None,
    }
}

/// テストから使う初期化。`#[bengara::test]` が1回だけ呼びます。
pub(crate) fn boot_for_tests(hooks: Hooks) {
    let base = paths::base_path().to_path_buf();
    env_vars::load_dotenv(&base);
    init_tracing();
    if !config_registry::installed() {
        let mut registry = Registry::new();
        (hooks.configs)(&mut registry);
        config_registry::install(registry);
    }
}
