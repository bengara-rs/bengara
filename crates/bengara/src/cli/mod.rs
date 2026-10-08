//! 開発用コマンド（`artisan`）と、`init` の入口。
//!
//! `artisan` は自動検出したモジュールを取り込まないので、アプリがコンパイルできない
//! 状態でも動きます。`serve` が再ビルドを担えるのはこのためです。

mod cargo_toml;
mod init;
mod serve;

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};

use crate::error::{Error, Result};

/// `init` の前の入口（`fn main() { bengara::run() }`）の中身。
pub(crate) fn run_before_init() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("");
    let result = match command {
        "init" => init::run(&project_root()),
        "--version" | "-V" | "version" => {
            println!("bengara {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            print_before_init_help(command);
            Ok(())
        }
    };
    if let Err(error) = result {
        eprintln!("エラー: {error}");
        std::process::exit(1);
    }
}

/// `artisan.rs`（`fn main() { bengara::artisan() }`）の中身。
pub(crate) fn artisan() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = project_root();
    let (command, rest) = match args.split_first() {
        Some((first, rest)) => (first.as_str(), rest),
        None => ("list", &[] as &[String]),
    };

    let result = match command {
        "list" | "help" | "--help" | "-h" => {
            print_artisan_help();
            // 実行時のコマンドは本体しか知らない（自作コマンドを含む）。
            // Laravel の `artisan list` と同じく、全部を1画面で出す。
            delegate_list(&root);
            Ok(())
        }
        "--version" | "-V" => {
            println!("bengara {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "init" => init::run(&root),
        "serve" => serve::run(&root, rest),
        // 実行時のコマンドは本体に渡す。
        _ => delegate(&root, &args),
    };

    if let Err(error) = result {
        eprintln!("エラー: {error}");
        std::process::exit(1);
    }
}

/// 本体のバイナリから呼ばれる `init`。
pub(crate) fn init_here() -> Result<()> {
    init::run(&project_root())
}

/// プロジェクトのルート。cargo が渡す `CARGO_MANIFEST_DIR` を使います。
///
/// サブディレクトリから `cargo artisan` を実行しても、正しいルートになります。
fn project_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("CARGO_MANIFEST_DIR") {
        return PathBuf::from(dir);
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// `migrate` などの実行時コマンドを本体に渡す。
fn delegate(root: &Path, args: &[String]) -> Result<()> {
    let status = spawn_app(root, args, false)?;
    if status.success() {
        Ok(())
    } else {
        std::process::exit(status.code().unwrap_or(1));
    }
}

/// `list` のときだけ使う、本体への受け渡し。
///
/// アプリがビルドできないときは、何も言わずに諦めます。アプリが壊れていても
/// `artisan` が動くことが、`artisan` を別のバイナリにしている理由だからです。
fn delegate_list(root: &Path) {
    let _ = spawn_app(root, &["list".to_string()], true);
}

/// 本体（`cargo run -- ...`）を動かして、終了の状態を返す。
///
/// `quiet` のときは、ビルドの失敗を画面に出しません。
fn spawn_app(root: &Path, args: &[String], quiet: bool) -> Result<ExitStatus> {
    let cargo = cargo_command();
    let manifest = root.join("Cargo.toml");
    std::process::Command::new(&cargo)
        // `serve` と同じく、プロジェクトのルートで動かす。`--manifest-path` だけだと
        // 呼び出し元のカレントディレクトリが本体に渡ってしまう。
        .current_dir(root)
        .arg("run")
        .arg("-q")
        .arg("--manifest-path")
        .arg(&manifest)
        .stderr(if quiet {
            Stdio::null()
        } else {
            Stdio::inherit()
        })
        .arg("--")
        .args(args)
        .status()
        .map_err(|e| Error::msg(format!("{} を実行できません: {e}", cargo.display())))
}

/// 実行中の cargo のパス。同じツールチェーンを使うためです。
pub(crate) fn cargo_command() -> PathBuf {
    std::env::var_os("CARGO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cargo"))
}

/// `bengara` に依存を書くときの版指定（`0.1` のような形）。
pub(crate) fn version_requirement() -> String {
    let major = env!("CARGO_PKG_VERSION_MAJOR");
    let minor = env!("CARGO_PKG_VERSION_MINOR");
    if major == "0" {
        format!("0.{minor}")
    } else {
        major.to_string()
    }
}

fn print_before_init_help(command: &str) {
    if !command.is_empty() && command != "help" && command != "--help" {
        eprintln!("`{command}` はまだ使えません。先に init を実行してください。\n");
    }
    println!(
        "\
bengara {version}

このプロジェクトはまだ初期化されていません。次を実行してください。

  cargo run -- init

init は Laravel と同じ構成のディレクトリとファイルを作り、プロジェクト直下に
`bengara::app!();` の1行だけの main.rs を置きます。その後は次のように使えます。

  cargo artisan serve     サーバーを起動して、変更を見張る
  cargo artisan list      コマンドの一覧
",
        version = env!("CARGO_PKG_VERSION")
    );
}

fn print_artisan_help() {
    println!(
        "\
bengara {version} — cargo artisan

開発用のコマンド（artisan が自分で処理します）

  serve [--host <ホスト>] [--port <ポート>]
                          サーバーを起動し、変更があれば再ビルドして再起動する
  init                    Laravel と同じ構成のファイルを作る（最初の1回だけ）
  list                    この一覧を出す

  serve は本体を即座に止めます。停止の猶予を確かめるときは本体を直接動かしてください。
",
        version = env!("CARGO_PKG_VERSION")
    );
}
