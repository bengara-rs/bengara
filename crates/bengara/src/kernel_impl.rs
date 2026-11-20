//! `bengara::app!()` が定義する `main` の中身。
//!
//! 本体のバイナリは、サーバーの起動と、実行時のコマンド（`route:list` など）を担います。
//! `serve` の監視と再ビルドは `artisan` 側です（`crate::cli`）。

use crate::application::Application;
use crate::config_registry::{self, Registry};
use crate::console::Command;
use crate::env_vars::{self, env};
use crate::error::Result;
use crate::{paths, server, Hooks};

/// 置き場所が無くても使えるコマンド。
///
/// `init` はこれから置き場所を作るところなので、`--version` と `--help` は
/// どこで実行しても答えられるべきなので、基準ディレクトリの確認も、
/// アプリの組み立て（`bootstrap/app.rs`）も飛ばします。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standalone {
    Init,
    Version,
    Help,
}

/// アプリを組み立ててから動かすコマンド。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppCommand {
    Serve,
    RouteList,
    KeyGenerate,
    SessionGc,
    StorageInit,
    About,
    /// DB のコマンド（`database/commands.rs` の一覧）。
    Database,
    /// 周辺のコマンド（`commands.rs` の一覧）。
    Extra,
    /// アプリが `app/Console/Commands/` に置いた自作コマンド。
    Custom,
}

impl AppCommand {
    /// アプリ（`bootstrap/app.rs`）を組み立ててから動かすか。
    ///
    /// `key:generate` と `storage:init` だけは組み立てません。使うのは `.env` と
    /// 置き場所（`paths`）だけで、ルートもイベントも見ないためです。
    /// 組み立ての中のパニック（ルートの重複、知らないミドルウェアの別名など）で、
    /// 新しい環境の準備に要るこの 2 つまで止まらないようにします。
    fn builds_application(self) -> bool {
        !matches!(self, Self::KeyGenerate | Self::StorageInit)
    }
}

/// 打たれた名前が指すもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Standalone(Standalone),
    App(AppCommand),
    /// どれでもない。
    Unknown,
}

/// このファイルが受け持つコマンド 1 つ。
struct Builtin {
    /// 受け付ける名前（別名を含む）。
    names: &'static [&'static str],
    target: Target,
    /// ヘルプの名前の欄に出す文字。
    label: &'static str,
    /// ヘルプに出す説明。
    description: &'static str,
    /// 説明の下に並べる旗（旗、説明）。
    flags: &'static [(&'static str, &'static str)],
}

/// このファイルが受け持つコマンドの表。**名前はここにだけ書きます。**
///
/// 置き場所が要るか（[`needs_base`]）・振り分け（[`resolve`]）・ヘルプ
/// （[`kernel_help`]）・同名の自作コマンドの検査（[`shadowed`]）は、全部
/// この表から決めます。以前は名前が別々の場所に散っていて、`list` `help`
/// `init` `version` だけが自作コマンドより**後ろ**で照合されていました。
/// `app/Console/Commands/List.rs` を置くと `cargo artisan list` が自作の
/// ほうを実行していました。
///
/// DB のコマンドと周辺のコマンドの名前は、それぞれのファイルの `COMMANDS` に
/// あります（[`builtin`] がまとめて見ます）。並びはヘルプに出す順です。
const BUILTINS: &[Builtin] = &[
    Builtin {
        names: &["serve"],
        target: Target::App(AppCommand::Serve),
        label: "(引数なし) / serve",
        // 名前を省いて旗だけでも起動する（`--port 9000`）。`split_command` を参照。
        description: "HTTP サーバーを起動する（名前は省ける。例: --port 9000）",
        flags: &[
            (
                "--host <ホスト>",
                "待ち受けるホスト（既定: APP_HOST か 127.0.0.1）",
            ),
            (
                "--port <ポート>",
                "待ち受けるポート（既定: APP_PORT か 8000）",
            ),
        ],
    },
    Builtin {
        names: &["route:list"],
        target: Target::App(AppCommand::RouteList),
        label: "route:list",
        description: "登録されているルートを一覧にする",
        flags: &[],
    },
    Builtin {
        names: &["key:generate"],
        target: Target::App(AppCommand::KeyGenerate),
        label: "key:generate",
        description: "APP_KEY を作って .env に書き込む（--force で上書き）",
        flags: &[],
    },
    Builtin {
        names: &["session:gc"],
        target: Target::App(AppCommand::SessionGc),
        label: "session:gc",
        description: "期限切れのセッションを消す",
        flags: &[],
    },
    Builtin {
        names: &["storage:init"],
        target: Target::App(AppCommand::StorageInit),
        label: "storage:init",
        description: "storage/ の下の書き込み先を作る（デプロイ時に1回）",
        flags: &[],
    },
    Builtin {
        names: &["about"],
        target: Target::App(AppCommand::About),
        label: "about",
        description: "いまの設定と置き場所を出す",
        flags: &[],
    },
    Builtin {
        names: &["init"],
        target: Target::Standalone(Standalone::Init),
        label: "init",
        description: "Laravel と同じ構成のファイルを作る（最初の1回だけ）",
        flags: &[],
    },
    Builtin {
        names: &["--version", "-V", "version"],
        target: Target::Standalone(Standalone::Version),
        label: "--version",
        description: "版番号を出す",
        flags: &[],
    },
    Builtin {
        names: &["--help", "-h", "help", "list"],
        target: Target::Standalone(Standalone::Help),
        label: "--help",
        description: "このヘルプを出す",
        flags: &[],
    },
];

/// フレームワークのコマンドなら、指すものを返す。
///
/// このファイルの表・DB のコマンド・周辺のコマンドの 3 つをまとめて見ます。
/// **フレームワークのコマンドかどうかは、ここだけで決めます。**
fn builtin(name: &str) -> Option<Target> {
    if let Some(found) = BUILTINS.iter().find(|b| b.names.contains(&name)) {
        return Some(found.target);
    }
    if crate::database::commands::is_command(name) {
        return Some(Target::App(AppCommand::Database));
    }
    if crate::commands::is_command(name) {
        return Some(Target::App(AppCommand::Extra));
    }
    None
}

/// 打たれた名前から、動かすものを決める。
///
/// **フレームワークのコマンドが必ず先です**（`docs/console-commands.md`）。
/// 自作コマンドを見るのは、フレームワークのどのコマンドでもなかったときだけです。
fn resolve(name: &str, custom: &[Command]) -> Target {
    if let Some(target) = builtin(name) {
        return target;
    }
    if crate::console::find(custom, name).is_some() {
        return Target::App(AppCommand::Custom);
    }
    Target::Unknown
}

/// フレームワークのコマンドと同じ名前の自作コマンド。**これらは呼ばれません。**
///
/// 黙って無視すると、`app/Console/Commands/List.rs` を書いた人が「動かない」
/// 理由を探すことになるので、起動のときに知らせます。
fn shadowed(custom: &[Command]) -> Vec<&'static str> {
    custom
        .iter()
        .map(|command| command.name)
        .filter(|name| builtin(name).is_some())
        .collect()
}

/// 基準ディレクトリが要るコマンドか。
///
/// 要らないのは [`Standalone`] だけです。知らない名前と自作コマンドは「要る」に
/// 倒します（置き場所が決まらないまま動き出さないため）。
fn needs_base(command: &str) -> bool {
    !matches!(builtin(command), Some(Target::Standalone(_)))
}

/// 本体の入口。`bengara::app!()` から呼ばれます。
pub fn main(hooks: Hooks, factory: fn() -> Application) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (command, _) = split_command(&args);

    // 知らない名前は、置き場所を確かめる**前**にヘルプを出して終える。
    // 先に確かめると、打ち間違いでも「基準ディレクトリが決まりません」と言われ、
    // 本当の原因（そんなコマンドは無い）が見えないため。
    if resolve(command, (hooks.commands)()) == Target::Unknown {
        eprintln!("`{command}` というコマンドはありません。");
        print_help(hooks);
        std::process::exit(1);
    }

    // 置き場所が決まらないまま動き出さない（決定記録 #064）。
    // **`.env` を読む前**に確かめる。読む場所そのものが決まっていないため。
    // つまり `APP_BASE_PATH` は `.env` に書いても効きません。
    let needs_base = needs_base(command);
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
    // フレームワークのコマンドと同じ名前の自作コマンドは呼ばれない。
    // 黙って無視せず、どのコマンドを動かしたときも知らせる。
    for name in shadowed((hooks.commands)()) {
        tracing::warn!("{}", shadowed_message(name));
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
    // 埋め込んだ resources/views/ も固定する。
    crate::view::install_embedded((hooks.views)());

    if let Err(error) = dispatch(&args, hooks, factory) {
        eprintln!("エラー: {error}");
        std::process::exit(1);
    }
}

/// 引数を「コマンドの名前」と「残り」に分ける。
///
/// - 引数なしは `serve`。
/// - 最初が `serve` の旗（`--host` / `--port`）なら、名前を省いた `serve` と見る。
///   ヘルプの「(引数なし) / serve」のとおり、`cargo run -- --port 9000` でも起動します。
///   以前は `--port` をコマンドの名前と読み、「ありません」で止まっていました。
fn split_command(args: &[String]) -> (&str, &[String]) {
    match args.split_first() {
        Some((first, _)) if is_serve_flag(first) => ("serve", args),
        Some((first, rest)) => (first.as_str(), rest),
        None => ("serve", &[]),
    }
}

/// `serve` の旗か（`--host` `--port` と、`=` で値を続けた形）。
fn is_serve_flag(arg: &str) -> bool {
    ["--host", "--port"]
        .iter()
        .any(|flag| arg == *flag || arg.strip_prefix(flag).is_some_and(|r| r.starts_with('=')))
}

fn dispatch(args: &[String], hooks: Hooks, factory: fn() -> Application) -> Result<()> {
    // 引数なしと、旗から始まる引数はサーバーの起動。
    let (command, rest) = split_command(args);

    // 振り分けは [`resolve`] に任せる。フレームワークのコマンドが必ず先。
    let kind = match resolve(command, (hooks.commands)()) {
        Target::Standalone(standalone) => return run_standalone(standalone, hooks),
        Target::Unknown => {
            eprintln!("`{command}` というコマンドはありません。");
            print_help(hooks);
            std::process::exit(1);
        }
        Target::App(kind) => kind,
    };

    // 組み立てずに動かすもの（[`AppCommand::builds_application`]）。
    if !kind.builds_application() {
        return run_without_application(kind, command, rest);
    }

    // アプリの組み立て（`bootstrap/app.rs`）は、ここで**1回だけ**行う。
    //
    // 組み立てのときにイベントの聞く側（`Event::listen`）が固定されるので、
    // 飛ばすと `queue:work` や自作コマンドの中の `Event::dispatch` が
    // 何も呼ばずに終わります（決定記録）。`serve` と `route:list` のほかにも
    // 要るので、上の 2 つを除く [`AppCommand`] の全部で組み立てます。
    let app = build(factory);

    match kind {
        AppCommand::Serve => {
            let options = ServeOptions::parse(rest)?;
            // 使う絶対パスを出す。設定の間違いはここを見れば分かる。
            crate::ops::log_paths();
            server::serve(app, &options.host, options.port)
        }
        AppCommand::RouteList => {
            crate::commands::no_flags(command, rest)?;
            print_routes(&app);
            Ok(())
        }
        AppCommand::KeyGenerate | AppCommand::StorageInit => {
            run_without_application(kind, command, rest)
        }
        AppCommand::SessionGc => {
            crate::commands::no_flags(command, rest)?;
            sweep_sessions()
        }
        AppCommand::About => {
            crate::commands::no_flags(command, rest)?;
            print_about(&hooks);
            Ok(())
        }
        // DB のコマンド（migrate / db:seed など）。
        AppCommand::Database => {
            crate::database::commands::run(command, rest, (hooks.migrations)(), (hooks.seeders)())
        }
        // 周辺のコマンド（cache:clear / queue:work / schedule:run など）。
        AppCommand::Extra => crate::commands::run(command, rest, hooks),
        AppCommand::Custom => crate::commands::run_custom(command, rest, hooks),
    }
}

/// 置き場所は要るが、アプリの組み立ては要らないコマンドを動かす。
///
/// 使うのは `.env` と置き場所（`paths`）だけです（[`AppCommand::builds_application`]）。
fn run_without_application(kind: AppCommand, command: &str, rest: &[String]) -> Result<()> {
    match kind {
        AppCommand::KeyGenerate => generate_key(rest),
        AppCommand::StorageInit => {
            crate::commands::no_flags(command, rest)?;
            crate::ops::storage_init()
        }
        other => Err(crate::Error::msg(format!(
            "`{command}` はアプリを組み立ててから動かすコマンドです（{other:?}）"
        ))),
    }
}

/// 置き場所もアプリの組み立ても要らないコマンドを動かす。
fn run_standalone(command: Standalone, hooks: Hooks) -> Result<()> {
    match command {
        Standalone::Init => crate::cli::init_here(),
        Standalone::Version => {
            println!("bengara {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Standalone::Help => {
            print_help(hooks);
            Ok(())
        }
    }
}

/// 同じ名前の自作コマンドがあるときの警告。
fn shadowed_message(name: &str) -> String {
    format!(
        "自作コマンド `{name}` は呼ばれません。フレームワークに同じ名前のコマンドがあります\
         （app/Console/Commands/ のファイル名を変えてください）"
    )
}

/// そのコマンドでアプリを組み立てるか。
///
/// 組み立てると、ルート・ミドルウェア・**イベントの聞く側**が固定されます。
/// `Event::dispatch` はどのコマンドからでも呼べることになっているので、
/// 次のものを除いてすべて真にします。
///
/// - 置き場所が要らないコマンド（`init` / `--version` / `--help` など）
/// - `key:generate` と `storage:init`（[`AppCommand::builds_application`]）
/// - 知らない名前（ヘルプを出して終わる）
///
/// 振り分けそのものは型（[`Standalone`] と [`AppCommand`]）で分かれています。
/// この関数は、その対応をテストで名前から確かめるためのものです。
#[cfg(test)]
fn needs_application(command: &str, custom: &[Command]) -> bool {
    matches!(resolve(command, custom), Target::App(kind) if kind.builds_application())
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

    if app_key_is_set(&body) && !force {
        println!(
            "APP_KEY はすでに設定されています。作り直すと、いまのセッションと\n\
             署名付き URL がすべて無効になります。それでもよければ --force を付けてください。"
        );
        return Ok(());
    }

    // 書き換える**前**の `.env` で見分ける（書いた後だと、新しい鍵と比べてしまう）。
    let real_key = real_app_key_present(&body);
    let key = crate::support::crypto::random_token();
    replace_file(&path, &write_app_key(&body, &key))?;
    println!("APP_KEY を {} に書き込みました。", path.display());
    if let Some(warning) = real_app_key_warning(real_key) {
        eprintln!("{warning}");
    }
    Ok(())
}

/// 本物の環境変数に `APP_KEY` が入っているか（`.env` から入れたものを除く）。
///
/// 起動のときに `.env` を読み込んでいるので、`std::env::var` だけでは見分けられません。
/// 書き換える前の `.env`（`body`）の値と同じなら、`.env` から来たものと見なします。
fn real_app_key_present(body: &str) -> bool {
    std::env::var("APP_KEY").is_ok_and(|value| is_real_app_key(&value, body))
}

/// 環境変数の値 `value` が、`.env`（`body`）から読み込んだものでないか。
fn is_real_app_key(value: &str, body: &str) -> bool {
    if value.trim().is_empty() {
        return false;
    }
    let from_dotenv = body
        .lines()
        .find_map(app_key_value)
        .map(env_vars::dotenv_value);
    from_dotenv.as_deref() != Some(value)
}

/// `.env` に書いても、本物の環境変数の `APP_KEY` が勝つときの警告。
///
/// `.env` は設定済みの変数を上書きしません。書いた鍵が使われないまま
/// 「書き込みました」で終わると、気づけないためです。
fn real_app_key_warning(present: bool) -> Option<&'static str> {
    present.then_some(
        "警告: 環境変数 APP_KEY が設定されています。環境変数 APP_KEY が優先されます。\n\
         .env に書いた鍵を使うには、環境変数 APP_KEY を外してください。",
    )
}

/// `.env` の中身で、`APP_KEY` がすでに設定されているか。
///
/// **`.env` の読み込みと同じ規則で値を読みます**（`env_vars::dotenv_value`）。
/// `=` の右側をそのまま見ていたころは、`APP_KEY=""` や `APP_KEY= # 後で` を
/// 「設定済み」と見て書き込みませんでした。読み込みではどちらも空なので、
/// 「設定されています」と言われたのに起動すると「設定されていません」になります。
///
/// 見るのは**最初の `APP_KEY` の行**です。読み込みは先に出てきた行を採るためです。
fn app_key_is_set(body: &str) -> bool {
    body.lines()
        .find_map(app_key_value)
        .is_some_and(|raw| !env_vars::dotenv_value(raw).trim().is_empty())
}

/// ファイルの中身を、一時ファイルに書いてから置き換える。
///
/// `std::fs::write` は切り詰めてから書くので、途中で落ちると `.env` が空か
/// 途中までになります。`.env` が壊れると、次の起動で設定が全部既定値に戻ります。
/// 同じディレクトリに書いてから `rename` で置き換えます
/// （`support::fs::write_atomic`。`schedule.rs` の前回時刻と同じ作りです）。
///
/// - **リンクなら、指している先を置き換えます。** リンクそのものを置き換えると、
///   共有の `.env` へのリンクが普通のファイルに変わってしまいます。
/// - **権限は元のファイルから引き継ぎます。** 一時ファイルは、中身を書く前に
///   所有者だけに絞ります（鍵が入るため）。
/// - 読み取り専用のファイルは、これまでどおり書き込まずにエラーにします。
fn replace_file(path: &std::path::Path, body: &str) -> Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let permissions = std::fs::metadata(&target)?.permissions();
    if permissions.readonly() {
        return Err(crate::Error::msg(format!(
            "{} は読み取り専用なので書き込めません",
            path.display()
        )));
    }

    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".env".to_string());
    let temp = target.with_file_name(format!("{name}.tmp.{}", std::process::id()));

    // 所有者だけに絞って書き、元の権限を付け直してから置き換える。
    // 失敗したときの書きかけは `write_atomic` が消します。
    crate::support::fs::write_atomic(
        &temp,
        &target,
        body.as_bytes(),
        crate::support::fs::Access::Keep(permissions),
    )?;
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
///
/// 先頭の BOM も外します（読み込みが外すため。Windows のメモ帳が付けます）。
/// 残すと 1 行目の `APP_KEY=` を見落とし、同じく末尾に 2 行目を足します。
///
/// キーの切り出しも読み込みと同じです（**最初の `=` で分け、キーの前後の空白を外す**）。
/// `APP_KEY=` で始まるかだけを見ていたころは、`APP_KEY = ` や `APP_KEY =値` を
/// 見落としていました。読み込みはどちらも `APP_KEY` と読むので、末尾に足した
/// 2 行目は使われず、空の鍵のまま「書き込みました」と出ていました。
fn app_key_value(line: &str) -> Option<&str> {
    let line = env_vars::strip_export(line.trim_start_matches('\u{feff}').trim());
    if line.starts_with('#') {
        return None;
    }
    let (key, rest) = line.split_once('=')?;
    (key.trim() == "APP_KEY").then_some(rest)
}

/// `APP_KEY` の行なら値を差し替える。そうでなければそのまま返す。
///
/// 最初の `=` までは残します（`export `・行頭の空白・`=` の前の空白）。
/// 利用者の書き方を変えないためです。
fn replace_app_key(line: &str, key: &str) -> String {
    if app_key_value(line).is_none() {
        return line.to_string();
    }
    match line.find('=') {
        Some(at) => format!("{}{key}", &line[..=at]),
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
        // 鍵そのものは出さない。署名に使えるか（あり／なし／短すぎる）だけを出す。
        ("APP_KEY", app_key_label(app)),
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
        // 書いた値ではなく、実際に使う方式を出す（知らない名前は file になる）。
        ("セッション", session_driver_label()),
        ("キャッシュ", cache_driver_label()),
        // いま実際に使う方式と強さ（`HASH_DRIVER`）。
        ("パスワード", crate::auth::hash_driver_label()),
        // `MAIL_MAILER` → `MAIL_DRIVER` の順に見る（`config/mail.rs` があればそちら）。
        ("メール", crate::mail::driver_label()),
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

/// `about` に出す `APP_KEY` の状態。署名に使うときと同じ判定（`signing_key`）を通す。
///
/// 以前は「空でなければあり」でした。短すぎる鍵も「あり」と出て、
/// 起動するとセッションが 500 になるのに気づけませんでした。
fn app_key_label(app: &crate::config_registry::AppConfig) -> String {
    match app.signing_key() {
        Ok(_) => "あり".to_string(),
        Err(_) if app.key.is_empty() => "なし".to_string(),
        Err(_) => format!("短すぎる（{} 文字）", app.key.chars().count()),
    }
}

/// 方式の名前を、実際に使うものに直す。
///
/// セッションもキャッシュも `memory`（別名 `array`）と `file` だけを知っていて、
/// 知らない名前は警告を出して `file` にします（`session/middleware.rs`・`cache/mod.rs`）。
fn driver_label(driver: &str) -> String {
    match driver {
        "memory" | "array" => "memory".to_string(),
        "file" => "file".to_string(),
        other => format!("file（`{other}` は知らないため）"),
    }
}

/// いま使うセッションの方式。`StartSession::from_env` と同じく `SESSION_DRIVER` を見る
/// （前後の空白を落とし、大文字小文字を区別しない）。
fn session_driver_label() -> String {
    driver_label(&env::<String>("SESSION_DRIVER", "file").to_ascii_lowercase())
}

/// いま使うキャッシュの方式。`config/cache.rs` があればその値、無ければ `CACHE_DRIVER`。
fn cache_driver_label() -> String {
    let driver = crate::try_config::<crate::cache::CacheConfig>()
        .map(|config| config.driver.clone())
        .unwrap_or_else(|| crate::cache::CacheConfig::default().driver);
    driver_label(&driver)
}

fn print_help(hooks: Hooks) {
    let describe = |list: &[(&str, &str)]| {
        list.iter()
            .map(|(name, description)| format!("  {name:<20} {description}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let kernel = kernel_help();
    let database = describe(crate::database::commands::COMMANDS);
    let extras = describe(crate::commands::COMMANDS);
    println!(
        "\
本体のコマンド

{kernel}
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

/// このファイルが受け持つコマンドの説明（ヘルプの最初のかたまり）。
///
/// [`BUILTINS`] から作ります。名前を表とヘルプの 2 か所に書かないためです。
/// 桁は端末での幅でそろえます（名前の欄に日本語が入るため）。
fn kernel_help() -> String {
    use crate::support::text;

    let mut out = String::new();
    for builtin in BUILTINS {
        out.push_str(&format!(
            "  {} {}\n",
            text::pad(builtin.label, 20),
            builtin.description
        ));
        for (flag, description) in builtin.flags {
            out.push_str(&format!("      {} {description}\n", text::pad(flag, 16)));
        }
    }
    out
}

/// ログの出力を用意する。
///
/// 出力の細かさは `RUST_LOG` で決めます。設定がなければ、`APP_DEBUG` が真なら
/// `debug`、そうでなければ `info` にします。
///
/// 機能フラグ `log-filter`（既定で入っています）があると、`RUST_LOG=myapp=debug` のような
/// 細かい絞り込みが使えます。外した場合は `trace` / `debug` / `info` / `warn` / `error` /
/// `off` の1語だけを見ます。
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
        let level = max_level(std::env::var("RUST_LOG").ok().as_deref(), default);
        tracing_subscriber::fmt().with_max_level(level)
    };

    // 二重に初期化しても落ちないようにする（テストから複数回呼ばれても安全）。
    let _ = builder.with_target(false).try_init();
}

/// `RUST_LOG` の値と既定から、ログの細かさを決める（`log-filter` を外したとき用）。
///
/// 読めない値と空は既定に落とします。`off` は「読めない」ではなく、
/// **何も出さない**という指定として扱います。
///
/// テストでも使うので、`log-filter` が入っているときもテストにだけ残します
/// （既定のビルドでは `log-filter` が入るので、外した側の枝は普段コンパイルされません）。
#[cfg(any(test, not(feature = "log-filter")))]
fn max_level(raw: Option<&str>, default: &str) -> tracing::level_filters::LevelFilter {
    raw.and_then(parse_level)
        .or_else(|| parse_level(default))
        .unwrap_or(tracing::level_filters::LevelFilter::INFO)
}

/// `info` などの1語をログの細かさに変える。読めなければ `None`。
///
/// 返すのは `Level` ではなく `LevelFilter` です。`Level` には「何も出さない」が
/// 無いので、以前は `off` を `None`（読めない）にするしかなく、既定の細かさに
/// 戻っていました。`RUST_LOG=off` と書いたのに `info` で出続ける形です。
#[cfg(any(test, not(feature = "log-filter")))]
fn parse_level(raw: &str) -> Option<tracing::level_filters::LevelFilter> {
    use tracing::level_filters::LevelFilter;

    match raw.trim().to_ascii_lowercase().as_str() {
        "trace" => Some(LevelFilter::TRACE),
        "debug" => Some(LevelFilter::DEBUG),
        "info" => Some(LevelFilter::INFO),
        "warn" | "warning" => Some(LevelFilter::WARN),
        "error" => Some(LevelFilter::ERROR),
        "off" | "none" => Some(LevelFilter::OFF),
        _ => None,
    }
}

/// テストから使う初期化。**何度呼んでも、中身が走るのは 1 回だけ**です。
///
/// `#[bengara::test]` が付いたテストは**1 件ごとに**ここを呼びます。`Once` で囲うのは
/// `env_vars::load_dotenv` が `std::env::set_var` を呼ぶからです。テストは複数の
/// スレッドで同時に走るので、別のテストが `env()` で `std::env::var` を読んでいる間に
/// 書き込むと未定義動作になります（`setenv`/`getenv` はスレッド安全ではありません。
/// Rust 2024 で `set_var` が unsafe になったのはこのためです）。
/// 中で呼ぶ他の処理（`install_test_mailer` など）はそれぞれ二重呼び出しに備えていましたが、
/// `load_dotenv` だけが無防備でした。
///
/// **この関数は `bengara` の単体テストから呼んではいけません。** 言語の表・埋め込んだ
/// `public/`・設定の一覧を `OnceLock` でプロセス全体に固定するので、1 回呼ぶだけで
/// 同じテストバイナリの他のテストを壊します（以前、ここを呼ぶテストを足したら
/// `lang::tests` が 6 件落ちました。空の言語表が先に固定され、`lang` 側の
/// `TABLE.set` が無視されたためです）。動きの確認はアプリ側の `#[bengara::test]`
/// が全件この道を通ることで足ります。
pub(crate) fn boot_for_tests(hooks: Hooks) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // メールはテストでは送らず、溜めるだけにする（`.env` を読む前に決める）。
        crate::mail::install_test_mailer();
        // パスワードのハッシュの回数を下げるのは **`.env` を読む前**に決める。
        // `.env` の値まで尊重すると、開発用に下げた値（や上げた値）でテストが走り、
        // 1 件ごとに数秒かかってしまう。環境変数で明示したときだけ従う。
        crate::auth::install_test_iterations();
        // `APP_ENV`・セッション・キャッシュのテスト用の既定も **`.env` を読む前**に入れる。
        // `.env` は設定済みの変数を上書きしないので、`.env` の `APP_ENV=local` に負けない。
        install_test_env();

        let base = paths::base_path().to_path_buf();
        let (_, warnings) = env_vars::load_dotenv(&base);
        init_tracing();
        for warning in warnings {
            tracing::warn!("{warning}");
        }
        // 設定を登録する**前**に入れる。`AppConfig` は登録のときに `APP_KEY` を読むため。
        install_test_app_key();
        if !config_registry::installed() {
            let mut registry = Registry::new();
            (hooks.configs)(&mut registry);
            config_registry::install(registry);
        }
        // 言語の表を固定する。
        crate::lang::install((hooks.lang)());
        // 埋め込んだ public/ を固定する。以後は読むだけ。
        crate::http::statics::install_embedded((hooks.public)());
        // 埋め込んだ resources/views/ も固定する。
        crate::view::install_embedded((hooks.views)());
        // マイグレーションの一覧を、あとで `refresh_database()` から使えるようにしておく。
        let _ = TEST_HOOKS.set(hooks);
    });
}

/// テストで `APP_KEY` が無いときに使う鍵。64 文字（`MIN_KEY_CHARS`）です。
///
/// 秘密ではありません。テストはソケットを開かず、Cookie も外に出ないので、
/// 固定の値で足ります。本物の環境変数や `.env` に値があればそちらを使います。
const TEST_APP_KEY: &str = "bengara-test-key-0123456789abcdef0123456789abcdef0123456789abcdef";

/// `.env` を読んだあとも `APP_KEY` が空なら、テスト用の鍵を入れる。
///
/// 取得したばかりのリポジトリには `.env` が無い。そのままだとセッションと署名付き URL を
/// 使う全テストが「`APP_KEY` が設定されていません」の 500 で落ちる。
/// メールの溜め込みやハッシュの回数と同じく、テストが自分で動ける状態にしておく。
///
/// `boot_for_tests` の `Once` の中（ほかのスレッドが環境変数を読む前）からだけ呼ぶ。
fn install_test_app_key() {
    let present = std::env::var("APP_KEY")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    if present {
        return;
    }
    // edition 2021 では set_var は安全な関数（`load_dotenv` と同じ理由）。
    std::env::set_var("APP_KEY", TEST_APP_KEY);
    tracing::debug!("APP_KEY が無いので、テスト用の固定の鍵を使います");
}

/// テストで使う環境変数の既定。Laravel の `phpunit.xml` の `<env>` と同じ役目です。
///
/// | 変数 | 値 | 理由 |
/// |---|---|---|
/// | `APP_ENV` | `testing` | 本体の既定は `production`。本番だけの振る舞いで結果が変わらないように |
/// | `SESSION_DRIVER` | `memory` | 開発用の `storage/framework/sessions/` にファイルを溜めない |
/// | `CACHE_DRIVER` | `memory` | 開発用の `storage/framework/cache/` を汚さない |
const TEST_ENV: &[(&str, &str)] = &[
    ("APP_ENV", "testing"),
    ("SESSION_DRIVER", "memory"),
    ("CACHE_DRIVER", "memory"),
];

/// 本物の環境変数に無いものだけ、[`TEST_ENV`] の値を入れる。
///
/// **`.env` を読む前**に呼びます。`.env` は設定済みの変数を上書きしないので、
/// 優先順位は「本物の環境変数 > テスト用の既定 > `.env`」になります
/// （Laravel の `phpunit.xml` と同じ）。以前は `.env` の後に入れていたので、
/// README どおり `APP_ENV=local` の `.env` を置くとテストが `local` で走りました。
///
/// 空の環境変数は「無い」と見なします（`.env` の読み込みと同じ規則）。
///
/// `boot_for_tests` の `Once` の中からだけ呼ぶ。
fn install_test_env() {
    for (key, value) in TEST_ENV {
        if env_vars::is_unset(std::env::var_os(key).as_deref()) {
            // edition 2021 では set_var は安全な関数（`load_dotenv` と同じ理由）。
            std::env::set_var(key, value);
        }
    }
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
    fn テスト用の鍵は必要な長さを満たす() {
        // 1 文字でも短いと、セッションを使う全テストが 500 で落ちる。
        assert!(TEST_APP_KEY.chars().count() >= config_registry::MIN_KEY_CHARS);
    }

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
            "session:gc",
            "about",
        ] {
            assert!(needs_application(command, &[]), "{command}");
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
            assert!(!needs_application(command, &[]), "{command}");
        }
    }

    #[test]
    fn 自作コマンドは組み立ててから動かす() {
        // `bengara-build` が生成できる名前（`app/Console/Commands/MyCommand.rs` → `my-command`）。
        // 以前のテストは `app:my-command` を使っていたが、これは生成できない名前で、
        // 表に無いので「知らない名前」に落ち、自作コマンドの道を通っていなかった。
        let commands = [custom("my-command")];
        assert_eq!(
            resolve("my-command", &commands),
            Target::App(AppCommand::Custom)
        );
        assert!(needs_application("my-command", &commands));
        assert!(needs_base("my-command"));
        // 登録していなければ知らない名前。組み立てずにヘルプを出して終わる。
        assert_eq!(resolve("my-command", &[]), Target::Unknown);
        assert!(!needs_application("my-command", &[]));
    }

    #[test]
    fn 準備のコマンドはアプリを組み立てない() {
        // `bootstrap/app.rs` の組み立てがパニックしても（ルートの重複など）、
        // 新しい環境の準備に要るこの 2 つは動く。置き場所は要る。
        for command in ["key:generate", "storage:init"] {
            assert!(matches!(resolve(command, &[]), Target::App(_)), "{command}");
            assert!(!needs_application(command, &[]), "{command}");
            assert!(needs_base(command), "{command}");
        }
    }

    #[test]
    fn 置き場所の要否と組み立ての要否はそろえる() {
        // 表にある名前の全部で、2 つの要否が一致する。
        // 例外は、置き場所だけが要る準備のコマンド（`key:generate` と `storage:init`）。
        for builtin in BUILTINS {
            for command in builtin.names {
                if matches!(builtin.target, Target::App(kind) if !kind.builds_application()) {
                    continue;
                }
                assert_eq!(
                    needs_base(command),
                    needs_application(command, &[]),
                    "{command}"
                );
            }
        }
        // 置き場所が要らないのは、この 8 つだけ。
        let without_base: Vec<&str> = BUILTINS
            .iter()
            .flat_map(|builtin| builtin.names.iter().copied())
            .filter(|command| !needs_base(command))
            .collect();
        assert_eq!(
            without_base,
            [
                "init",
                "--version",
                "-V",
                "version",
                "--help",
                "-h",
                "help",
                "list"
            ]
        );
        // 知らない名前と自作コマンドは「要る」に倒す。
        assert!(needs_base("greet"));
        assert!(needs_base("migrate"));
        assert!(needs_base("queue:work"));
    }

    /// テスト用の自作コマンド。
    fn custom(name: &'static str) -> Command {
        Command {
            name,
            description: "",
            handle: |_| Box::pin(async { Ok(()) }),
        }
    }

    #[test]
    fn 同じ名前の自作コマンドよりフレームワークが先() {
        // `app/Console/Commands/List.rs` などを置いた形。以前は `list` `help`
        // `init` `version` だけ自作コマンドの後ろで照合していて、自作のほうが動いた。
        let commands = [
            custom("list"),
            custom("help"),
            custom("init"),
            custom("version"),
            custom("serve"),
            custom("about"),
            custom("migrate"),
            custom("queue:work"),
            custom("greet"),
        ];
        assert_eq!(
            resolve("list", &commands),
            Target::Standalone(Standalone::Help)
        );
        assert_eq!(
            resolve("help", &commands),
            Target::Standalone(Standalone::Help)
        );
        assert_eq!(
            resolve("init", &commands),
            Target::Standalone(Standalone::Init)
        );
        assert_eq!(
            resolve("version", &commands),
            Target::Standalone(Standalone::Version)
        );
        assert_eq!(resolve("serve", &commands), Target::App(AppCommand::Serve));
        assert_eq!(resolve("about", &commands), Target::App(AppCommand::About));
        assert_eq!(
            resolve("migrate", &commands),
            Target::App(AppCommand::Database)
        );
        assert_eq!(
            resolve("queue:work", &commands),
            Target::App(AppCommand::Extra)
        );
        // ぶつからない名前は自作コマンドに届く。
        assert_eq!(resolve("greet", &commands), Target::App(AppCommand::Custom));
        assert_eq!(resolve("nope", &commands), Target::Unknown);
        assert_eq!(resolve("greet", &[]), Target::Unknown);

        // 同名の自作コマンドがあっても、置き場所の要否は変わらない
        // （以前は `list` が置き場所の確認を飛ばしたまま自作コマンドを動かした）。
        assert!(!needs_base("list"));
    }

    #[test]
    fn 呼ばれない自作コマンドを見つける() {
        let commands = [
            custom("greet"),
            custom("list"),
            custom("cache:clear"),
            custom("db:seed"),
            custom("send-report"),
        ];
        assert_eq!(shadowed(&commands), ["list", "cache:clear", "db:seed"]);
        assert!(shadowed(&[custom("greet")]).is_empty());
        assert!(shadowed(&[]).is_empty());

        let text = shadowed_message("list");
        assert!(text.contains("`list`"), "{text}");
        assert!(text.contains("呼ばれません"), "{text}");
        assert!(text.contains("app/Console/Commands/"), "{text}");
    }

    #[test]
    fn コマンドの名前は表の中で重ならない() {
        // 重なると、後ろのほうは決して選ばれない。
        let mut names: Vec<&str> = BUILTINS
            .iter()
            .flat_map(|builtin| builtin.names.iter().copied())
            .chain(crate::database::commands::COMMANDS.iter().map(|(n, _)| *n))
            .chain(crate::commands::COMMANDS.iter().map(|(n, _)| *n))
            .collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "同じ名前が 2 回出ている");
        // どの名前も、フレームワークのコマンドとして見つかる。
        for name in names {
            assert!(builtin(name).is_some(), "{name}");
        }
    }

    #[test]
    fn 旗から始まる引数はserveと見る() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // 引数なしは serve。
        assert_eq!(split_command(&[]), ("serve", &[] as &[String]));

        // 名前を省いて旗だけ。旗は残りとして serve に渡す。
        let given = args(&["--port", "9000"]);
        assert_eq!(split_command(&given), ("serve", given.as_slice()));
        let given = args(&["--host=0.0.0.0"]);
        assert_eq!(split_command(&given), ("serve", given.as_slice()));
        let given = args(&["--host", "0.0.0.0", "--port=9000"]);
        let (command, rest) = split_command(&given);
        assert_eq!(command, "serve");
        assert!(ServeOptions::parse(rest).is_ok_and(|o| o.port == 9000 && o.host == "0.0.0.0"));

        // 名前があればこれまでどおり。
        let given = args(&["serve", "--port", "9000"]);
        assert_eq!(split_command(&given), ("serve", &given[1..]));
        let given = args(&["migrate"]);
        assert_eq!(split_command(&given), ("migrate", &[] as &[String]));

        // 似ているだけの言葉は旗と見ない（知らないコマンドとして断る）。
        for word in ["--hostname", "--ports", "--help", "-p"] {
            assert!(!is_serve_flag(word), "{word}");
        }
        assert!(is_serve_flag("--port=8080"));
    }

    #[test]
    fn ヘルプは表から作る() {
        let help = kernel_help();
        // 表にあるものは、全部ヘルプに出る。
        for builtin in BUILTINS {
            assert!(help.contains(builtin.label), "{}", builtin.label);
            assert!(help.contains(builtin.description), "{}", builtin.label);
        }
        // 桁は端末での幅でそろえる（名前の欄に日本語が入る）。
        assert!(
            help.contains("  (引数なし) / serve   HTTP サーバーを起動する（"),
            "{help}"
        );
        assert!(
            help.contains("      --host <ホスト>  待ち受けるホスト"),
            "{help}"
        );
        assert!(
            help.contains("  route:list           登録されているルートを一覧にする\n"),
            "{help}"
        );
        assert!(
            help.contains("  --help               このヘルプを出す\n"),
            "{help}"
        );
    }

    #[test]
    fn 空の値やコメントだけの鍵は未設定と見る() {
        // `.env` の読み込みでは、どれも空になる。以前は「設定済み」と見て書き込まなかった。
        assert!(!app_key_is_set("APP_KEY=\n"));
        assert!(!app_key_is_set("APP_KEY=\"\"\n"));
        assert!(!app_key_is_set("APP_KEY=''\n"));
        assert!(!app_key_is_set("APP_KEY= # 後で\n"));
        assert!(!app_key_is_set("APP_KEY=#後で\n"));
        assert!(!app_key_is_set("APP_KEY=\"   \"\n"));
        assert!(!app_key_is_set("APP_NAME=myapp\n"), "行が無い");
        assert!(!app_key_is_set(""));

        assert!(app_key_is_set("APP_KEY=abc\n"));
        assert!(app_key_is_set("APP_KEY=\"abc\"\n"));
        assert!(app_key_is_set("export APP_KEY=abc # 本番用\n"));
        assert!(app_key_is_set("APP_NAME=myapp\r\nAPP_KEY=abc\r\n"));

        // 読み込みは先に出てきた行を採る。判定も同じ行を見る。
        assert!(!app_key_is_set("APP_KEY=\nAPP_KEY=abc\n"));
        assert!(app_key_is_set("APP_KEY=abc\nAPP_KEY=\n"));
    }

    #[test]
    fn bomつきの1行目の鍵も見つける() {
        // メモ帳が付ける BOM。見落とすと末尾に 2 行目を足し、空の 1 行目が勝つ。
        let body = "\u{feff}APP_KEY=\nAPP_NAME=myapp\n";
        assert_eq!(app_key_value("\u{feff}APP_KEY=abc"), Some("abc"));
        let updated = write_app_key(body, "new");
        assert_eq!(updated, "\u{feff}APP_KEY=new\nAPP_NAME=myapp\n");
        assert_eq!(updated.matches("APP_KEY=").count(), 1);
    }

    /// テスト用の空ディレクトリ。
    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bengara-kernel-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn envは一時ファイル経由で置き換える() {
        let dir = temp_dir("replace");
        let path = dir.join(".env");
        std::fs::write(&path, "APP_NAME=myapp\nAPP_KEY=old\n").unwrap();

        replace_file(&path, "APP_NAME=myapp\nAPP_KEY=new\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "APP_NAME=myapp\nAPP_KEY=new\n"
        );

        // 書きかけ（`.env.tmp.*`）が残らない。
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, [".env"], "残っている: {left:?}");

        // 無いファイルは作らない（`key:generate` は先に `.env` の有無を確かめる）。
        assert!(replace_file(&dir.join("無い"), "x").is_err());
        assert!(!dir.join("無い").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 読み取り専用のenvには書かない() {
        let dir = temp_dir("readonly");
        let path = dir.join(".env");
        std::fs::write(&path, "APP_KEY=old\n").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();

        // 置き換え（rename）は、読み取り専用でも通ってしまう OS がある。
        // 直接書いていたころと同じく、書かずにエラーにする。
        let error = replace_file(&path, "APP_KEY=new\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("読み取り専用"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "APP_KEY=old\n");

        // 後片付けのために書ける状態へ戻す。
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        let _ = std::fs::set_permissions(&path, permissions);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn offはログを止める指定として読む() {
        use tracing::level_filters::LevelFilter;

        // 以前は `off` が「読めない値」と同じ扱いで、既定の細かさに戻っていた。
        assert_eq!(parse_level("off"), Some(LevelFilter::OFF));
        assert_eq!(parse_level(" OFF "), Some(LevelFilter::OFF));
        assert_eq!(parse_level("none"), Some(LevelFilter::OFF));
        assert_eq!(max_level(Some("off"), "info"), LevelFilter::OFF);
        assert_eq!(max_level(Some("off"), "debug"), LevelFilter::OFF);

        // ほかの語はこれまでどおり。
        assert_eq!(max_level(Some("warn"), "info"), LevelFilter::WARN);
        assert_eq!(max_level(Some("TRACE"), "info"), LevelFilter::TRACE);
        // 読めない値・空・指定なしは既定に落ちる。
        assert_eq!(max_level(Some("myapp=debug"), "debug"), LevelFilter::DEBUG);
        assert_eq!(max_level(Some(""), "info"), LevelFilter::INFO);
        assert_eq!(max_level(None, "debug"), LevelFilter::DEBUG);
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
    fn イコールの前後に空白がある行も読み込みと同じに見つける() {
        // 読み込みは最初の `=` で分けてキーの空白を外す。判定と置き換えもそろえる。
        // 以前は見落として末尾に 2 行目を足し、空の 1 行目が勝っていた。
        assert_eq!(app_key_value("APP_KEY = "), Some(""));
        assert_eq!(app_key_value("APP_KEY =abc"), Some("abc"));
        assert_eq!(app_key_value("  export  APP_KEY\t= abc"), Some(" abc"));
        assert_eq!(app_key_value("APP_KEYS=abc"), None);
        assert_eq!(app_key_value("#APP_KEY=abc"), None);
        assert!(!app_key_is_set("APP_KEY = \n"));
        assert!(app_key_is_set("APP_KEY = abc\n"));

        // `=` までの書き方は残し、`APP_KEY` の行は 1 本のまま。
        let updated = write_app_key("APP_NAME=myapp\nAPP_KEY = \n", "new");
        assert_eq!(updated, "APP_NAME=myapp\nAPP_KEY =new\n");
        assert_eq!(updated.matches("APP_KEY").count(), 1);
        assert_eq!(
            write_app_key("export APP_KEY =old\n", "new"),
            "export APP_KEY =new\n"
        );
        // 値に `=` が入っていても、最初の `=` の後ろを差し替える。
        assert_eq!(write_app_key("APP_KEY=a=b\n", "new"), "APP_KEY=new\n");
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
    fn テスト用の既定はenvより先に入れる値() {
        // Laravel の phpunit.xml と同じ 3 つ。セッションとキャッシュは開発用の
        // storage/ に書かない方式にする。
        let get = |key: &str| TEST_ENV.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        assert_eq!(get("APP_ENV"), Some("testing"));
        assert_eq!(get("SESSION_DRIVER"), Some("memory"));
        assert_eq!(get("CACHE_DRIVER"), Some("memory"));
    }

    #[test]
    fn aboutの方式は実際に使うものを出す() {
        assert_eq!(driver_label("memory"), "memory");
        assert_eq!(driver_label("array"), "memory");
        assert_eq!(driver_label("file"), "file");
        // 知らない名前は file で動くので、書いた名前のまま出さない。
        let label = driver_label("redis");
        assert!(
            label.starts_with("file") && label.contains("redis"),
            "{label}"
        );
    }

    #[test]
    fn aboutのapp_keyは署名に使えるかで出す() {
        let with_key = |key: &str| crate::config_registry::AppConfig {
            name: String::new(),
            env: String::new(),
            debug: false,
            url: String::new(),
            key: key.to_string(),
        };
        assert_eq!(app_key_label(&with_key("")), "なし");
        assert_eq!(app_key_label(&with_key("short")), "短すぎる（5 文字）");
        assert_eq!(app_key_label(&with_key(TEST_APP_KEY)), "あり");
    }

    #[test]
    fn 本物の環境変数のapp_keyを見分ける() {
        // `.env` から読み込んだ値と同じなら、本物の環境変数ではない。
        assert!(!is_real_app_key("abc", "APP_KEY=abc\n"));
        assert!(!is_real_app_key("abc", "APP_KEY=\"abc\" # コメント\n"));
        // 違う値・`.env` に無い値は本物の環境変数。
        assert!(is_real_app_key("abc", "APP_KEY=xyz\n"));
        assert!(is_real_app_key("abc", "APP_NAME=x\n"));
        // 空は「無い」。
        assert!(!is_real_app_key("  ", "APP_NAME=x\n"));
        // 警告の文。
        assert!(real_app_key_warning(false).is_none());
        let warning = real_app_key_warning(true).unwrap();
        assert!(
            warning.contains("環境変数 APP_KEY が優先されます"),
            "{warning}"
        );
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
