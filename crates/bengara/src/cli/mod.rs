//! 開発用コマンド（`artisan`）と、`init` の入口。
//!
//! `artisan` は自動検出したモジュールを取り込まないので、アプリがコンパイルできない
//! 状態でも動きます。`serve` が再ビルドを担えるのはこのためです。

mod cargo_toml;
mod init;
mod serve;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};

use self::cargo_toml::CargoToml;
use crate::error::{Error, Result};

/// `init` の前の入口（`fn main() { bengara::run() }`）の中身。
pub(crate) fn run_before_init() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("");
    let result = match command {
        "init" => init::run(&project_root()),
        name if is_version(name) => {
            print_version();
            Ok(())
        }
        _ => {
            print_before_init_help(command);
            // 知らないコマンドは失敗で終える。0 で終えると、スクリプトや CI が
            // 打ち間違いに気づけない。一覧を聞いただけのときは 0 のまま。
            let code = before_init_exit_code(command);
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
    };
    if let Err(error) = result {
        eprintln!("エラー: {error}");
        std::process::exit(1);
    }
}

/// `init` の前の入口で、案内を出したあとの終了コード。
///
/// 引数なしと一覧を聞く言葉は 0、それ以外（知らないコマンド）は 1 です。
fn before_init_exit_code(command: &str) -> i32 {
    if command.is_empty() || is_help(command) {
        0
    } else {
        1
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
        name if is_help(name) => {
            print_artisan_help();
            // 実行時のコマンドは本体しか知らない（自作コマンドを含む）。
            // Laravel の `artisan list` と同じく、全部を1画面で出す。
            delegate_list(&root);
            Ok(())
        }
        // `version` も本体へ渡さずにここで答える。渡すと本体のビルドが要り、
        // アプリが壊れているときに版も出せない。`init` の前の入口とも同じ扱いになる。
        name if is_version(name) => {
            print_version();
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
    // 読めないときは `--bin` を付けずに進む。cargo が同じファイルを読んで理由を出す。
    let bin = CargoToml::load(&manifest).ok().and_then(|m| main_bin(&m));
    std::process::Command::new(&cargo)
        // `serve` と同じく、プロジェクトのルートで動かす。`--manifest-path` だけだと
        // 呼び出し元のカレントディレクトリが本体に渡ってしまう。
        .current_dir(root)
        .args(run_arguments(&manifest, bin.as_deref(), args))
        .stderr(if quiet {
            Stdio::null()
        } else {
            Stdio::inherit()
        })
        .status()
        .map_err(|e| Error::msg(format!("{} を実行できません: {e}", cargo.display())))
}

/// 本体（`main.rs`）の bin の名前。`Cargo.toml` の `[[bin]]` から読みます。
///
/// `serve` のビルドと、本体への受け渡しの両方で使います。bin の名前は
/// パッケージ名と同じとは限りません（`[[bin]] name = "server" path = "main.rs"`）。
pub(crate) fn main_bin(manifest: &CargoToml) -> Option<String> {
    manifest.bin_name_for_path("main.rs")
}

/// 本体を動かす `cargo run` の引数。
///
/// bin の名前が分かっているときは `--bin` で名指しします。`default-run` に頼ると、
/// それを書いていないプロジェクト（`init` は bin の名前がパッケージ名と違うとき
/// 書きません）で、cargo が「どの bin か決められない」と言って止まります。
/// bin は `artisan` と本体の2つが必ずあるためです。
///
/// 分からないとき（`main.rs` の `[[bin]]` が無い）は付けません。今までどおり
/// cargo の決め方に任せます。
fn run_arguments(manifest: &Path, bin: Option<&str>, args: &[String]) -> Vec<OsString> {
    let mut out: Vec<OsString> = vec!["run".into(), "-q".into()];
    out.push("--manifest-path".into());
    out.push(manifest.into());
    if let Some(bin) = bin {
        out.push("--bin".into());
        out.push(bin.into());
    }
    out.push("--".into());
    out.extend(args.iter().map(OsString::from));
    out
}

/// 版を聞く言葉か。2つの入口（`init` の前と `artisan`）で同じ一覧を使います。
fn is_version(command: &str) -> bool {
    matches!(command, "--version" | "-V" | "version")
}

/// 一覧（ヘルプ）を聞く言葉か。2つの入口で同じ一覧を使います。
///
/// 以前は入口ごとに別々に書いていたので、`init` の前の入口だけ `-h` と `list` を
/// 知らず、「`-h` はまだ使えません」と答えていました。
fn is_help(command: &str) -> bool {
    matches!(command, "list" | "help" | "--help" | "-h")
}

fn print_version() {
    println!("bengara {}", env!("CARGO_PKG_VERSION"));
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
    // 一覧を聞いただけのとき（`-h` や `list`）は、叱らずに案内だけ出す。
    if !command.is_empty() && !is_help(command) {
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
  version                 bengara の版を出す（--version / -V も同じ）

  serve は本体を即座に止めます。停止の猶予を確かめるときは本体を直接動かしてください。
",
        version = env!("CARGO_PKG_VERSION")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn 本体への受け渡しはbinを名指しする() {
        // `default-run` が無くても動くように、`--bin` を付ける。
        let args = run_arguments(
            Path::new("Cargo.toml"),
            Some("server"),
            &["migrate".to_string(), "--force".to_string()],
        );
        assert_eq!(
            strings(&args),
            [
                "run",
                "-q",
                "--manifest-path",
                "Cargo.toml",
                "--bin",
                "server",
                "--",
                "migrate",
                "--force"
            ]
        );
    }

    #[test]
    fn binが分からないときは名指ししない() {
        // 今までどおり cargo の決め方（`default-run`）に任せる。
        let args = run_arguments(Path::new("Cargo.toml"), None, &["list".to_string()]);
        assert_eq!(
            strings(&args),
            ["run", "-q", "--manifest-path", "Cargo.toml", "--", "list"]
        );
    }

    #[test]
    fn 本体のbinの名前はmain_rsのbinから読む() {
        let manifest = CargoToml::load_for_test(
            "[package]\nname = \"myapp\"\n\n[[bin]]\nname = \"server\"\npath = \"main.rs\"\n\n\
             [[bin]]\nname = \"artisan\"\npath = \"artisan.rs\"\n",
        );
        assert_eq!(main_bin(&manifest).as_deref(), Some("server"));
        // `init` の前（`[[bin]]` が無い）は分からない。
        let manifest = CargoToml::load_for_test("[package]\nname = \"myapp\"\n");
        assert_eq!(main_bin(&manifest), None);
    }

    #[test]
    fn 版と一覧の言葉は2つの入口で同じ() {
        for word in ["--version", "-V", "version"] {
            assert!(is_version(word), "{word}");
            assert!(!is_help(word), "{word}");
        }
        // `-h` と `list` も一覧として扱う。以前は `init` の前の入口が
        // 「`-h` はまだ使えません」と答えていた。
        for word in ["list", "help", "--help", "-h"] {
            assert!(is_help(word), "{word}");
            assert!(!is_version(word), "{word}");
        }
        for word in ["", "serve", "init", "migrate", "-v"] {
            assert!(!is_help(word) && !is_version(word), "{word}");
        }
    }

    #[test]
    fn init前の知らないコマンドは失敗で終える() {
        // 以前は案内を出して 0 で終えていた。CI が打ち間違いに気づけない。
        for word in ["serve", "migrate", "-v", "inti"] {
            assert_eq!(before_init_exit_code(word), 1, "{word}");
        }
        for word in ["", "list", "help", "--help", "-h"] {
            assert_eq!(before_init_exit_code(word), 0, "{word}");
        }
    }
}
