//! `cargo artisan serve`：本体をビルドして起動し、変更があれば作り直す。
//!
//! - ビルドに失敗したときは、直前に動いていた版をそのまま動かし続けます。
//! - Windows では実行中の exe を上書きできないため、別の場所にコピーしてから起動します。
//! - 変更の監視は更新時刻の見張り（既定 0.4 秒ごと）で行います。OS ごとの通知の仕組みに
//!   頼らないので、依存クレートが増えず、どの OS でも同じように動きます。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime};

use super::cargo_command;
use super::cargo_toml::CargoToml;
use crate::error::{Error, Result};

/// 見張るディレクトリ。
const WATCH_DIRS: &[&str] = &[
    "app",
    "bootstrap",
    "config",
    "database",
    "public",
    "resources",
    "routes",
];

/// 見張るファイル。
const WATCH_FILES: &[&str] = &[".env", "Cargo.toml", "build.rs", "main.rs", "artisan.rs"];

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

    println!("bengara serve: {name} を見張ります（Ctrl+C で終了）\n");

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
                generation += 1;
                child = Some(start(root, &run_dir, &exe, &name, generation, args)?);
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

/// 前回の `serve` が残したコピーを消す（できる範囲で）。
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
        // 隠しファイルとビルド結果は見張らない。
        if name.starts_with('.') || name == "target" {
            continue;
        }
        match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => walk(&path, map),
            Ok(_) => {
                if let Some(time) = modified(&path) {
                    map.insert(path, time);
                }
            }
            Err(_) => {}
        }
    }
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
}
