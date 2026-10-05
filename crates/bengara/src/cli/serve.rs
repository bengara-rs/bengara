//! `cargo artisan serve`：本体をビルドして起動し、変更があれば作り直す。
//!
//! - ビルドに失敗したときは、直前に動いていた版をそのまま動かし続けます。
//! - Windows では実行中の exe を上書きできないため、別の場所にコピーしてから起動します。
//! - 変更の監視は更新時刻の見張り（既定 0.4 秒ごと）で行います。OS ごとの通知の仕組みに
//!   頼らないので、依存クレートが増えず、どの OS でも同じように動きます。
//! - 見張るのは**ビルドに影響するファイルだけ**です（`.rs` と `resources/lang/*.toml`）。
//!   データベースのファイルは、つないだだけで更新時刻が変わるため見張りません。
//!   `public/` も見張りません（デバッグビルドではディスクから読むので、再ビルドが要りません）。
//! - **`serve` は本体を即座に止めます。** `APP_SHUTDOWN_TIMEOUT` の猶予を確かめたいときは、
//!   `serve` を使わずに本体を直接動かしてください（`cargo run -- serve`）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime};

use super::cargo_command;
use super::cargo_toml::CargoToml;
use crate::error::{Error, Result};

/// 見張るディレクトリ。
///
/// `public/` は入れません。デバッグビルドでは `public/` をディスクから読むので、
/// CSS を1行直すたびに再ビルドと再起動が走るのは損だからです。
const WATCH_DIRS: &[&str] = &[
    "app",
    "bootstrap",
    "config",
    "database",
    "resources",
    "routes",
];

/// 見張るファイル。
const WATCH_FILES: &[&str] = &[".env", "Cargo.toml", "build.rs", "main.rs", "artisan.rs"];

/// 掘らないディレクトリの名前。重いものと、ビルドに関係ないもの。
const SKIP_DIRS: &[&str] = &["target", "node_modules", ".git", "storage"];

/// 見張らないファイルの終わり方。
///
/// データベースのファイルは、本体がつないだだけで `-wal` と `-shm` の更新時刻が
/// 変わります。見張ると、リクエストのたびに再起動してしまいます。
const DATABASE_SUFFIXES: &[&str] = &[
    ".sqlite",
    ".sqlite-wal",
    ".sqlite-shm",
    ".db",
    ".db-wal",
    ".db-shm",
];

/// 見張る間隔。
const POLL: Duration = Duration::from_millis(400);

/// 変更が続いている間は待つ（まとめて1回のビルドにするため）。
const SETTLE: Duration = Duration::from_millis(250);

pub(crate) fn run(root: &Path, args: &[String]) -> Result<()> {
    let manifest_path = root.join("Cargo.toml");
    let manifest = CargoToml::load(&manifest_path)?;
    let name = manifest
        .package_name()
        .ok_or_else(|| Error::msg("Cargo.toml の [package] に name がありません"))?;

    let run_dir = root.join("storage/framework/serve");
    std::fs::create_dir_all(&run_dir)
        .map_err(|e| Error::msg(format!("{} を作れません: {e}", run_dir.display())))?;
    clean_old_copies(&run_dir);

    println!("bengara serve: {name} を見張ります（Ctrl+C で終了）");
    println!(
        "serve は本体を即座に止めます。停止の猶予（APP_SHUTDOWN_TIMEOUT）を確かめるときは、\n\
         本体を直接動かしてください。\n"
    );

    let mut generation: u32 = 0;
    let mut child: Option<Child> = None;
    let mut watched = snapshot(root);

    match build(root, &name) {
        Ok(exe) => {
            generation += 1;
            child = Some(start(root, &run_dir, &exe, &name, generation, args)?);
        }
        Err(error) => {
            eprintln!("\nビルドに失敗しました: {error}\nファイルを直すと、もう一度ビルドします。\n")
        }
    }

    loop {
        std::thread::sleep(POLL);

        // 本体が自分で終わっていたら知らせる（ポートが使われているときなど）。
        if let Some(running) = child.as_mut() {
            if let Ok(Some(status)) = running.try_wait() {
                if status.success() {
                    println!("本体が終了しました。ファイルを変更すると、もう一度起動します。");
                } else {
                    eprintln!(
                        "本体が異常終了しました（終了コード {}）。ファイルを変更すると、もう一度起動します。",
                        status.code().unwrap_or(-1)
                    );
                }
                child = None;
            }
        }

        let mut current = snapshot(root);
        if current == watched {
            continue;
        }
        // 保存が続いている間は待って、1回のビルドにまとめる。
        loop {
            std::thread::sleep(SETTLE);
            let again = snapshot(root);
            if again == current {
                break;
            }
            current = again;
        }
        watched = current;

        println!("\n変更を検知しました。ビルドします…");
        match build(root, &name) {
            Ok(exe) => {
                stop(&mut child);
                // 止めた後なら、前の世代のコピーを消せる（Windows でも）。
                // 消せないものは次の起動時に片付けるので、ここでは気にしない。
                clean_old_copies(&run_dir);
                generation += 1;
                // 2回目以降の失敗で見張りを終わらせない。直せばまた試せる。
                match start(root, &run_dir, &exe, &name, generation, args) {
                    Ok(started) => child = Some(started),
                    Err(error) => eprintln!(
                        "起動できませんでした: {error}\nファイルを変更すると、もう一度試します。\n"
                    ),
                }
            }
            Err(error) => {
                if child.is_some() {
                    eprintln!("ビルドに失敗しました: {error}\n前の版をそのまま動かしています。\n");
                } else {
                    eprintln!("ビルドに失敗しました: {error}\n");
                }
            }
        }
    }
}

/// `cargo build` を実行し、できた実行ファイルのパスを返す。
fn build(root: &Path, name: &str) -> Result<PathBuf> {
    let cargo = cargo_command();
    let output = Command::new(&cargo)
        .current_dir(root)
        .arg("build")
        .arg("--bin")
        .arg(name)
        .arg("--message-format=json-render-diagnostics")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| Error::msg(format!("{} を実行できません: {e}", cargo.display())))?;

    if !output.status.success() {
        return Err(Error::msg("cargo build が失敗しました"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    executable_from(&stdout, name)
        .ok_or_else(|| Error::msg(format!("{name} の実行ファイルが見つかりません")))
}

/// `cargo build --message-format=json` の出力から実行ファイルを拾う。
fn executable_from(stdout: &str, name: &str) -> Option<PathBuf> {
    let mut found = None;
    for line in stdout.lines() {
        let message: serde_json::Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if message.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
            continue;
        }
        let Some(executable) = message.get("executable").and_then(|e| e.as_str()) else {
            continue;
        };
        if message.pointer("/target/name").and_then(|n| n.as_str()) == Some(name) {
            found = Some(PathBuf::from(executable));
        }
    }
    found
}

/// できた実行ファイルを別の名前にコピーして起動する。
///
/// Windows は動いている exe を上書きできないため、毎回違う名前にします。
fn start(
    root: &Path,
    run_dir: &Path,
    exe: &Path,
    name: &str,
    generation: u32,
    args: &[String],
) -> Result<Child> {
    let extension = exe.extension().and_then(|e| e.to_str()).unwrap_or("");
    let file_name = if extension.is_empty() {
        format!("{name}-{generation}")
    } else {
        format!("{name}-{generation}.{extension}")
    };
    let destination = run_dir.join(file_name);
    std::fs::copy(exe, &destination)
        .map_err(|e| Error::msg(format!("{} へコピーできません: {e}", destination.display())))?;

    let mut command = Command::new(&destination);
    command
        .current_dir(root)
        // コピー先から起動するので、基準ディレクトリを明示する。
        .env("APP_BASE_PATH", root)
        .arg("serve")
        .args(args);

    command
        .spawn()
        .map_err(|e| Error::msg(format!("{} を起動できません: {e}", destination.display())))
}

/// 動いている本体を止める。
///
/// **即座に止めます**（SIGKILL / TerminateProcess）。処理中のリクエストは待ちません。
/// `APP_SHUTDOWN_TIMEOUT` の猶予を確かめたいときは、本体を直接動かしてください。
fn stop(child: &mut Option<Child>) {
    let Some(mut running) = child.take() else {
        return;
    };
    if let Err(e) = running.kill() {
        // すでに終わっていた場合は気にしない。
        tracing::debug!("本体を止められませんでした: {e}");
    }
    let _ = running.wait();
}

/// 残っているコピーを消す（できる範囲で）。
///
/// 起動時と、本体を止めた直後に呼びます。動いている本体のコピーは消せませんが、
/// そのときは黙って残します（次の機会に消えます）。
fn clean_old_copies(run_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(run_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let _ = std::fs::remove_file(entry.path());
    }
}

/// 見張る対象の、パスと更新時刻の一覧。
fn snapshot(root: &Path) -> BTreeMap<PathBuf, SystemTime> {
    let mut map = BTreeMap::new();
    for dir in WATCH_DIRS {
        walk(&root.join(dir), &mut map);
    }
    for file in WATCH_FILES {
        let path = root.join(file);
        if let Some(time) = modified(&path) {
            map.insert(path, time);
        }
    }
    map
}

fn walk(dir: &Path, map: &mut BTreeMap<PathBuf, SystemTime>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => {
                if !skip_dir(&name) {
                    walk(&path, map);
                }
            }
            Ok(_) => {
                if is_watched_file(&path) {
                    if let Some(time) = modified(&path) {
                        map.insert(path, time);
                    }
                }
            }
            Err(_) => {}
        }
    }
}

/// 掘らないディレクトリか。隠しディレクトリと、重いもの。
fn skip_dir(name: &str) -> bool {
    name.starts_with('.') || SKIP_DIRS.contains(&name)
}

/// ビルドに影響するファイルか。
///
/// `.rs` と、`resources/lang/` の `.toml` だけを見張ります。
/// データベースのファイルは、`.rs` でなくても念のため名前でも外します。
fn is_watched_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    // 隠しファイルと、エディタが作る一時ファイルは見張らない。
    if name.starts_with('.') || is_database_file(name) {
        return false;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => true,
        Some("toml") => in_lang_dir(path),
        _ => false,
    }
}

/// データベースのファイルか（`-wal` と `-shm` を含む）。
fn is_database_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    DATABASE_SUFFIXES
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

/// `resources/lang/` の直下にあるか。
fn in_lang_dir(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let is_lang = parent
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "lang");
    let in_resources = parent
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "resources");
    is_lang && in_resources
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 実行ファイルのパスを拾える() {
        let stdout = concat!(
            r#"{"reason":"compiler-artifact","target":{"name":"other"},"executable":"/t/other"}"#,
            "\n",
            r#"{"reason":"compiler-artifact","target":{"name":"myapp"},"executable":"/t/myapp"}"#,
            "\n",
            r#"{"reason":"build-finished","success":true}"#,
            "\n",
        );
        assert_eq!(
            executable_from(stdout, "myapp"),
            Some(PathBuf::from("/t/myapp"))
        );
        assert_eq!(executable_from(stdout, "missing"), None);
    }

    #[test]
    fn 実行ファイルのないメッセージは飛ばす() {
        let stdout =
            r#"{"reason":"compiler-artifact","target":{"name":"myapp"},"executable":null}"#;
        assert_eq!(executable_from(stdout, "myapp"), None);
    }

    #[test]
    fn ソースと言語ファイルだけを見張る() {
        assert!(is_watched_file(Path::new(
            "app/Http/Controllers/HomeController.rs"
        )));
        assert!(is_watched_file(Path::new(
            "database/migrations/2026_01_01_000000_create_users_table.rs"
        )));
        assert!(is_watched_file(Path::new("resources/lang/ja.toml")));
        // 言語の置き場所の外にある toml は見張らない。
        assert!(!is_watched_file(Path::new("config/other.toml")));
        assert!(!is_watched_file(Path::new("resources/lang/sub/ja.toml")));
        // ビルドに関係ないもの。
        assert!(!is_watched_file(Path::new("public/css/app.css")));
        assert!(!is_watched_file(Path::new("app/.hidden.rs")));
    }

    #[test]
    fn データベースのファイルは見張らない() {
        for name in [
            "database/database.sqlite",
            "database/database.sqlite-wal",
            "database/database.sqlite-shm",
            "database/app.db",
            "database/app.db-wal",
            "database/app.db-shm",
            "database/DATABASE.SQLITE-WAL",
        ] {
            assert!(!is_watched_file(Path::new(name)), "{name} を見張っています");
        }
    }

    #[test]
    fn 重いディレクトリは掘らない() {
        assert!(skip_dir("target"));
        assert!(skip_dir("node_modules"));
        assert!(skip_dir(".git"));
        assert!(skip_dir(".idea"));
        assert!(skip_dir("storage"));
        assert!(!skip_dir("Controllers"));
        assert!(!skip_dir("migrations"));
    }
}
