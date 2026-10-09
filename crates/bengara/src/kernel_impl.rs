//! `bengara::app!()` が定義する `main` の中身。
//!
//! 本体のバイナリは、サーバーの起動と、実行時のコマンド（`route:list` など）を担います。
//! `serve` の監視と再ビルドは `artisan` 側です（`crate::cli`）。

use crate::application::Application;
use crate::config_registry::{self, Registry};
use crate::env_vars::{self, env};
use crate::error::Result;
use crate::{paths, server, Hooks};

/// 置き場所が無くても使えるコマンド。
///
/// `init` はこれから置き場所を作るところなので、`--version` と `--help` は
/// どこで実行しても答えられるべきなので、基準ディレクトリの確認を飛ばします。
const WITHOUT_BASE: &[&str] = &[
    "init",
    "--version",
    "-V",
    "version",
    "--help",
    "-h",
    "help",
    "list",
];

/// 本体の入口。`bengara::app!()` から呼ばれます。
pub fn main(hooks: Hooks, factory: fn() -> Application) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("serve");

    // 置き場所が決まらないまま動き出さない（決定記録 #064）。
    // **`.env` を読む前**に確かめる。読む場所そのものが決まっていないため。
    // つまり `APP_BASE_PATH` は `.env` に書いても効きません。
    let needs_base = !WITHOUT_BASE.contains(&command);
    if needs_base {
        if let Err(error) = paths::guard() {
            eprintln!("エラー: {error}");
            std::process::exit(1);
        }
    }

    let base = paths::base_path().to_path_buf();
    // `.env` の読み込みは、ログの購読より先に済ませる（`init_tracing` が
    // `APP_DEBUG` を見るため）。そのため警告はその場では出せないので、
    // 一覧で受け取って購読の**後**に流し直す。
    let (dotenv, warnings) = env_vars::load_dotenv(&base);
    init_tracing();
    if let Some(path) = &dotenv {
        tracing::debug!("{} を読み込みました", path.display());
    }
    for warning in warnings {
        tracing::warn!("{warning}");
    }

    // `APP_STORAGE_PATH` は `.env` に書いても効くので、読み込んだ**後**に確かめる。
    if needs_base {
        if let Err(error) = paths::guard_storage() {
            eprintln!("エラー: {error}");
            std::process::exit(1);
        }
    }

    let mut registry = Registry::new();
    (hooks.configs)(&mut registry);
    config_registry::install(registry);
    // 言語の表を固定する。以後は読むだけ。
    crate::lang::install((hooks.lang)());
    // 埋め込んだ public/ を固定する。以後は読むだけ。
    crate::http::statics::install_embedded((hooks.public)());

    if let Err(error) = dispatch(&args, hooks, factory) {
        eprintln!("エラー: {error}");
        std::process::exit(1);
    }
}

fn dispatch(args: &[String], hooks: Hooks, factory: fn() -> Application) -> Result<()> {
    let (command, rest) = match args.split_first() {
        Some((first, rest)) => (first.as_str(), rest),
        // 引数なしはサーバーの起動。
        None => ("serve", &[] as &[String]),
    };

    // アプリの組み立て（`bootstrap/app.rs`）は、ここで**1回だけ**行う。
    //
    // 組み立てのときにイベントの聞く側（`Event::listen`）が固定されるので、
    // 飛ばすと `queue:work` や自作コマンドの中の `Event::dispatch` が
    // 何も呼ばずに終わります（決定記録）。`serve` と `route:list` のほかにも
    // 要るので、`needs_application` で広く真にしてあります。
    let app = if needs_application(command) {
        Some(build(factory))
    } else {
        None
    };

    match command {
        "serve" => {
            let options = ServeOptions::parse(rest)?;
            let app = app.unwrap_or_else(|| build(factory));
            // 使う絶対パスを出す。設定の間違いはここを見れば分かる。
            crate::ops::log_paths();
            server::serve(app, &options.host, options.port)
        }
        "route:list" => {
            print_routes(&app.unwrap_or_else(|| build(factory)));
            Ok(())
        }
        "key:generate" => generate_key(rest),
        "session:gc" => sweep_sessions(),
        "storage:init" => crate::ops::storage_init(),
        "about" => {
            print_about(&hooks);
            Ok(())
        }
        // DB のコマンド（migrate / db:seed など）。
        name if crate::database::commands::is_command(name) => {
            crate::database::commands::run(name, rest, (hooks.migrations)(), (hooks.seeders)())
        }
        // 周辺のコマンド（cache:clear / queue:work / schedule:run など）。
        name if crate::commands::is_command(name) => crate::commands::run(name, rest, hooks),
        // アプリが `app/Console/Commands/` に置いた自作コマンド。
        name if crate::console::find((hooks.commands)(), name).is_some() => {
            crate::commands::run_custom(name, rest, hooks)
        }
        "init" => crate::cli::init_here(),
        "--version" | "-V" | "version" => {
            println!("bengara {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "--help" | "-h" | "help" | "list" => {
            print_help(hooks);
            Ok(())
        }
        other => {
            eprintln!("`{other}` というコマンドはありません。");
            print_help(hooks);
            std::process::exit(1);
        }
    }
}

/// そのコマンドでアプリを組み立てるか。
///
/// 組み立てると、ルート・ミドルウェア・**イベントの聞く側**が固定されます。
/// `Event::dispatch` はどのコマンドからでも呼べることになっているので、
/// 置き場所が要らないコマンド（`init` / `--version` / `--help` など）を除いて
/// すべて真にします。`WITHOUT_BASE` と同じ並びを使うのは、置き場所が無ければ
/// `bootstrap/app.rs` も読めないためです。
fn needs_application(command: &str) -> bool {
    !WITHOUT_BASE.contains(&command)
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

/// `APP_KEY` を作って `.env` に書き込む。
///
/// すでにある場合は、`--force` が無いかぎり上書きしません。
/// 鍵を変えると、いま動いているセッションと署名付き URL が全部無効になるためです。
fn generate_key(rest: &[String]) -> Result<()> {
    let force = parse_force(rest)?;
    let path = paths::base_path().join(".env");

    if !path.is_file() {
        return Err(crate::Error::msg(format!(
            "{} がありません。先に .env を作ってください",
            path.display()
        )));
    }

    let body = std::fs::read_to_string(&path)?;
    let already_set = body
        .lines()
        .any(|line| app_key_value(line).is_some_and(|value| !value.trim().is_empty()));

    if already_set && !force {
        println!(
            "APP_KEY はすでに設定されています。作り直すと、いまのセッションと\n\
             署名付き URL がすべて無効になります。それでもよければ --force を付けてください。"
        );
        return Ok(());
    }

    let key = crate::support::crypto::random_token();
    std::fs::write(&path, write_app_key(&body, &key))?;
    println!("APP_KEY を {} に書き込みました。", path.display());
    Ok(())
}

/// `.env` の中身に `APP_KEY` を入れた文字列を作る。
///
/// すでに行があれば差し替え、無ければ末尾に足します。改行は元のままです。
fn write_app_key(body: &str, key: &str) -> String {
    let newline = newline_of(body);
    if body.lines().any(|line| app_key_value(line).is_some()) {
        let mut out: Vec<String> = body
            .lines()
            .map(|line| replace_app_key(line, key))
            .collect();
        out.push(String::new());
        return out.join(newline);
    }
    let mut out = body.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(newline);
    }
    out.push_str("APP_KEY=");
    out.push_str(key);
    out.push_str(newline);
    out
}

/// `key:generate` の旗を読む。`--force` だけを受け付けます。
///
/// **受け取った引数だけを見ます。** `std::env::args()` を読み直すと、
/// `--forse` のような打ち間違いが黙って「上書きしない」側に落ちます。
fn parse_force(rest: &[String]) -> Result<bool> {
    let mut force = false;
    for arg in rest {
        if arg == "--force" {
            force = true;
        } else {
            return Err(crate::Error::msg(format!(
                "key:generate は `{arg}` を受け付けません（使えるのは --force だけです）"
            )));
        }
    }
    Ok(force)
}

/// 書き戻すときの改行を決める。
///
/// 元の改行を保ちます。`.gitignore` と `Cargo.toml` の書き換えと同じ方針です。
/// LF にそろえると、CRLF の `.env` が git の差分で全行変わって見えます。
fn newline_of(body: &str) -> &'static str {
    if body.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// `.env` の1行が `APP_KEY` の行なら、その値を返す。
///
/// `.env` の読み込みと**同じ前処理**をします（前後の空白と先頭の `export` を外す）。
/// そろえないと `export APP_KEY=…` を見落とし、末尾に 2 行目を足してしまいます。
/// 読み込みは先に出てくる値を採るので、鍵は変わらないのに「書き込みました」と出ます。
fn app_key_value(line: &str) -> Option<&str> {
    env_vars::strip_export(line.trim()).strip_prefix("APP_KEY=")
}

/// `APP_KEY` の行なら値を差し替える。そうでなければそのまま返す。
///
/// `export ` と行頭の空白は残します。利用者の書き方を変えないためです。
fn replace_app_key(line: &str, key: &str) -> String {
    if app_key_value(line).is_none() {
        return line.to_string();
    }
    match line.find("APP_KEY=") {
        Some(at) => format!("{}APP_KEY={key}", &line[..at]),
        None => line.to_string(),
    }
}

fn sweep_sessions() -> Result<()> {
    let dir = paths::storage_path("framework/sessions");
    let removed = crate::session::sweep_expired(&dir)?;
    println!(
        "期限切れのセッションを {removed} 件消しました（{}）",
        dir.display()
    );
    Ok(())
}

/// 登録されているルートを表で出す。
fn print_routes(app: &Application) {
    let routes = app.routes();
    if routes.is_empty() {
        println!("ルートは1本も登録されていません。routes/web.rs を確かめてください。");
        return;
    }
    // 桁は**端末での幅**でそろえる。文字数で数えると、日本語のルート名で崩れます
    // （`about` や `schedule:list` と同じ数え方にそろえてあります）。
    use crate::support::text;

    let method_width = routes
        .iter()
        .map(|r| text::width(r.method))
        .max()
        .unwrap_or(6)
        .max(6);
    let path_width = routes
        .iter()
        .map(|r| text::width(&r.path))
        .max()
        .unwrap_or(4)
        .max(4);
    let name_width = routes
        .iter()
        .map(|r| text::width(r.name.as_deref().unwrap_or("")))
        .max()
        .unwrap_or(4)
        .max(4);
    // ミドルウェアが1本も無いなら、列ごと出さない。
    let any_middleware = routes.iter().any(|r| !r.middleware.is_empty());
    if any_middleware {
        println!(
            "{}  {}  {}  MIDDLEWARE",
            text::pad("METHOD", method_width),
            text::pad("URI", path_width),
            text::pad("NAME", name_width)
        );
    } else {
        println!(
            "{}  {}  NAME",
            text::pad("METHOD", method_width),
            text::pad("URI", path_width)
        );
    }
    for route in routes {
        let name = route.name.as_deref().unwrap_or("");
        if any_middleware {
            println!(
                "{}  {}  {}  {}",
                text::pad(route.method, method_width),
                text::pad(&route.path, path_width),
                text::pad(name, name_width),
                route.middleware.join(", ")
            );
        } else {
            println!(
                "{}  {}  {}",
                text::pad(route.method, method_width),
                text::pad(&route.path, path_width),
                name
            );
        }
    }
    println!("\n{} 本", routes.len());
}

/// `about`：いまの設定と置き場所を1画面で出す。
///
/// 本番で「どこを見ているのか」を確かめるためのコマンドです。
/// **秘密は出しません**（`APP_KEY` は設定の有無だけ）。
fn print_about(hooks: &Hooks) {
    use crate::support::text;

    let app = crate::config_registry::app_config();
    let storage = paths::storage_root();

    let rows: Vec<(&str, String)> = vec![
        ("bengara", env!("CARGO_PKG_VERSION").to_string()),
        ("アプリ名", app.name.clone()),
        ("環境", app.env.clone()),
        ("デバッグ表示", yes_no(app.debug)),
        ("URL", app.url.clone()),
        // 鍵そのものは出さない。設定されているかだけを出す。
        ("APP_KEY", yes_no(!app.key.trim().is_empty())),
        (
            "基準ディレクトリ",
            format!(
                "{}（{}）",
                paths::base_path().display(),
                paths::base_source().label()
            ),
        ),
        (
            "storage",
            format!(
                "{}{}",
                storage.display(),
                if storage.is_dir() {
                    ""
                } else {
                    "  ← ありません（storage:init）"
                }
            ),
        ),
        ("public", paths::app_path("public").display().to_string()),
        (
            "言語",
            format!(
                "{} / {:?}",
                crate::Lang::current(),
                crate::Lang::available()
            ),
        ),
        ("セッション", env("SESSION_DRIVER", "file".to_string())),
        ("キャッシュ", env("CACHE_DRIVER", "file".to_string())),
        ("メール", env("MAIL_DRIVER", "log".to_string())),
        ("停止の上限", crate::server::shutdown_timeout_label()),
        ("マイグレーション", (hooks.migrations)().len().to_string()),
        ("シーダー", (hooks.seeders)().len().to_string()),
        ("ジョブ", (hooks.jobs)().len().to_string()),
        ("自作コマンド", (hooks.commands)().len().to_string()),
    ];

    let width = rows
        .iter()
        .map(|(label, _)| text::width(label))
        .max()
        .unwrap_or(4);
    for (label, value) in &rows {
        println!("{}  {value}", text::pad(label, width));
    }
}

/// 真偽を日本語にする。
fn yes_no(value: bool) -> String {
    if value { "あり" } else { "なし" }.to_string()
}

fn print_help(hooks: Hooks) {
    let describe = |list: &[(&str, &str)]| {
        list.iter()
            .map(|(name, description)| format!("  {name:<20} {description}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let database = describe(crate::database::commands::COMMANDS);
    let extras = describe(crate::commands::COMMANDS);
    println!(
        "\
本体のコマンド

  (引数なし) / serve   HTTP サーバーを起動する
      --host <ホスト>  待ち受けるホスト（既定: APP_HOST か 127.0.0.1）
      --port <ポート>  待ち受けるポート（既定: APP_PORT か 8000）
  route:list           登録されているルートを一覧にする
  key:generate         APP_KEY を作って .env に書き込む（--force で上書き）
  session:gc           期限切れのセッションを消す
  storage:init         storage/ の下の書き込み先を作る（デプロイ時に1回）
  about                いまの設定と置き場所を出す
  init                 Laravel と同じ構成のファイルを作る（最初の1回だけ）
  --version            版番号を出す
  --help               このヘルプを出す

データベースのコマンド
{database}
  どれにも --database=<接続の名前> を付けられます。

周辺機能のコマンド
{extras}

開発用のコマンド（serve の自動再ビルドなど）は `cargo artisan list` に出ます。
"
    );
    // アプリが app/Console/Commands/ に置いたものも出す。
    crate::console::print_list((hooks.commands)());
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
        // sqlx は投げた SQL を1本ずつ出すので、既定では黙らせる。
        // 見たいときは `RUST_LOG=info,sqlx=debug` のように指定する。
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new(format!("{default},hyper=warn,tower=warn,sqlx=warn"))
        });
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
    // メールはテストでは送らず、溜めるだけにする（`.env` を読む前に決める）。
    crate::mail::install_test_mailer();
    // パスワードのハッシュの回数を下げるのは **`.env` を読む前**に決める。
    // `.env` の値まで尊重すると、開発用に下げた値（や上げた値）でテストが走り、
    // 1 件ごとに数秒かかってしまう。環境変数で明示したときだけ従う。
    crate::auth::install_test_iterations();

    let base = paths::base_path().to_path_buf();
    let (_, warnings) = env_vars::load_dotenv(&base);
    init_tracing();
    for warning in warnings {
        tracing::warn!("{warning}");
    }
    if !config_registry::installed() {
        let mut registry = Registry::new();
        (hooks.configs)(&mut registry);
        config_registry::install(registry);
    }
    // 言語の表を固定する。
    crate::lang::install((hooks.lang)());
    // 埋め込んだ public/ を固定する。以後は読むだけ。
    crate::http::statics::install_embedded((hooks.public)());
    // マイグレーションの一覧を、あとで `refresh_database()` から使えるようにしておく。
    let _ = TEST_HOOKS.set(hooks);
}

static TEST_HOOKS: std::sync::OnceLock<Hooks> = std::sync::OnceLock::new();

/// テストのときに覚えておいた `Hooks`。
pub(crate) fn test_hooks() -> Hooks {
    TEST_HOOKS.get().copied().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn イベントの聞く側が要るコマンドは組み立てる() {
        // `Event::dispatch` はどのコマンドからでも呼べる。組み立てを飛ばすと
        // 聞く側が登録されず、何も起きずに終わっていた。
        for command in [
            "serve",
            "route:list",
            "queue:work",
            "schedule:run",
            "migrate",
            "db:seed",
            "cache:clear",
            "app:my-command",
        ] {
            assert!(needs_application(command), "{command}");
        }
        // 置き場所が要らないコマンドは組み立てない。
        for command in [
            "init",
            "--version",
            "-V",
            "version",
            "--help",
            "-h",
            "help",
            "list",
        ] {
            assert!(!needs_application(command), "{command}");
        }
    }

    #[test]
    fn 置き場所の要否と組み立ての要否はそろえる() {
        for command in WITHOUT_BASE {
            assert!(!needs_application(command), "{command}");
        }
    }

    #[test]
    fn exportつきの鍵の行も見つける() {
        assert_eq!(app_key_value("APP_KEY=abc"), Some("abc"));
        assert_eq!(app_key_value("export APP_KEY=abc"), Some("abc"));
        assert_eq!(app_key_value("export\tAPP_KEY=abc"), Some("abc"));
        assert_eq!(app_key_value("  APP_KEY=abc"), Some("abc"));
        assert_eq!(app_key_value("APP_KEY="), Some(""));
        // コメントと別のキーは触らない。
        assert_eq!(app_key_value("# APP_KEY=abc"), None);
        assert_eq!(app_key_value("APP_KEY_OLD=abc"), None);
    }

    #[test]
    fn exportつきの行が置き換わる() {
        // 以前は見落として末尾に2行目を足し、先に出てくる古い値が勝っていた。
        let body = "APP_NAME=myapp\nexport APP_KEY=old\n";
        let updated = write_app_key(body, "new");
        assert_eq!(updated, "APP_NAME=myapp\nexport APP_KEY=new\n");
        // `APP_KEY` の行は1本だけ。
        assert_eq!(updated.matches("APP_KEY=").count(), 1);
    }

    #[test]
    fn 行が無ければ末尾に足す() {
        assert_eq!(write_app_key("A=1\n", "new"), "A=1\nAPP_KEY=new\n");
        // 改行で終わっていない中身にも足せる。
        assert_eq!(write_app_key("A=1", "new"), "A=1\nAPP_KEY=new\n");
    }

    #[test]
    fn crlfのenvはcrlfのまま保つ() {
        assert_eq!(newline_of("A=1\r\n"), "\r\n");
        assert_eq!(newline_of("A=1\n"), "\n");
        let body = "APP_NAME=myapp\r\nAPP_KEY=old\r\n";
        assert_eq!(
            write_app_key(body, "new"),
            "APP_NAME=myapp\r\nAPP_KEY=new\r\n"
        );
        // 足すときも元の改行にそろえる。
        assert_eq!(write_app_key("A=1\r\n", "new"), "A=1\r\nAPP_KEY=new\r\n");
    }

    #[test]
    fn 知らない旗はエラーにする() {
        assert!(!parse_force(&[]).unwrap());
        assert!(parse_force(&["--force".to_string()]).unwrap());
        // 打ち間違いが黙って通らないこと。
        let error = parse_force(&["--forse".to_string()]).unwrap_err();
        assert!(error.to_string().contains("--forse"), "{error}");
    }
}
