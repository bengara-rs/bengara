//! `cargo artisan serve`：本体をビルドして起動し、変更があれば作り直す。
//!
//! - ビルドに失敗したときは、直前に動いていた版をそのまま動かし続けます。
//! - Windows では実行中の exe を上書きできないため、別の場所にコピーしてから起動します。
//! - 変更の監視は更新時刻の見張り（既定 0.4 秒ごと）で行います。OS ごとの通知の仕組みに
//!   頼らないので、依存クレートが増えず、どの OS でも同じように動きます。
//! - 見張るのは**本体のビルドに影響するファイルだけ**です（`.rs` と
//!   `resources/lang/*.toml`）。この2つだけなので、データベースのファイル
//!   （`.sqlite` や `.db-wal`）は自動的に外れます。
//!   `public/` も見張りません（デバッグビルドではディスクから読むので、再ビルドが要りません）。
//! - **`tests/` は見張りません**（[`WATCH_DIRS`] に入っていません）。`serve` は
//!   アプリを動かすだけで、テストは走らせないためです。テストを足したときは
//!   `cargo test` を実行してください。`build.rs` は `tests/` も見張っているので、
//!   そちらでは自動で見つかります。
//! - **`serve` は本体を即座に止めます。** `APP_SHUTDOWN_TIMEOUT` の猶予を確かめたいときは、
//!   `serve` を使わずに本体を直接動かしてください（`cargo run -- serve`）。

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

    // 置き場所は `storage:init` が作るが、無ければここで作る。
    // 中に入るのはアプリの実行ファイルそのものなので、所有者だけに絞る。
    let run_dir = root.join("storage/framework/serve");
    let created = !run_dir.is_dir();
    std::fs::create_dir_all(&run_dir)
        .map_err(|e| Error::msg(format!("{} を作れません: {e}", run_dir.display())))?;
    if created {
        crate::ops::restrict(&run_dir)?;
    }

    // コピーの名前に自分のプロセス id を入れる。別の serve が動いていても
    // 名前がぶつからず、ビルドのたびの片付けを自分のものだけに絞れる。
    let prefix = copy_prefix(&name, std::process::id());
    // **起動時の1回だけ**は、pid を抜いた前置きで片付ける。前回の `serve` の
    // コピーは別の pid なので、自分の pid に絞ると永久に残り、立ち上げ直すたびに
    // 実行ファイルが積み上がる。動いている `serve` のコピーは、Windows では
    // `remove_file` が失敗して自然に守られ、Unix では消しても動作に影響しない。
    clean_old_copies(&run_dir, &startup_prefix(&name));

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
            // 初回の失敗でも見張りは続ける。直せばまた試せる（2回目以降と同じ扱い）。
            match start(root, &run_dir, &exe, &prefix, generation, args) {
                Ok(started) => child = Some(started),
                Err(error) => eprintln!(
                    "起動できませんでした: {error}\nファイルを変更すると、もう一度試します。\n"
                ),
            }
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
                clean_old_copies(&run_dir, &prefix);
                generation += 1;
                // 失敗で見張りを終わらせない。直せばまた試せる。
                match start(root, &run_dir, &exe, &prefix, generation, args) {
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

/// このプロセスが作るコピーの、名前の始まり。
///
/// プロセス id を入れるのは、同じプロジェクトで `serve` を2つ動かしても
/// 名前がぶつからないようにするためです。世代番号は毎回 1 から始まるので、
/// これが無いと別の `serve` のコピーと同じ名前になります。
fn copy_prefix(name: &str, pid: u32) -> String {
    format!("{name}-{pid}-")
}

/// 起動時の片付けで使う、名前の始まり。
///
/// プロセス id を入れません。前回の `serve` が残したコピー（別の pid）まで
/// 消したいためです。**起動時の1回だけ**使います。
fn startup_prefix(name: &str) -> String {
    format!("{name}-")
}

/// できた実行ファイルを別の名前にコピーして起動する。
///
/// Windows は動いている exe を上書きできないため、毎回違う名前にします。
fn start(
    root: &Path,
    run_dir: &Path,
    exe: &Path,
    prefix: &str,
    generation: u32,
    args: &[String],
) -> Result<Child> {
    let extension = exe.extension().and_then(|e| e.to_str()).unwrap_or("");
    let file_name = if extension.is_empty() {
        format!("{prefix}{generation}")
    } else {
        format!("{prefix}{generation}.{extension}")
    };
    let destination = run_dir.join(file_name);
    std::fs::copy(exe, &destination).map_err(|e| {
        Error::msg(format!(
            "{} へコピーできません: {e}\n\
             前に起動した本体がまだ終わっていない可能性があります。",
            destination.display()
        ))
    })?;

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
        // `artisan` は tracing の購読を立てないので、eprintln! で出す。
        // すでに終わっていた場合はここに来ないので、来たときは知らせる価値がある。
        eprintln!("本体を止められませんでした: {e}");
    }
    let _ = running.wait();
}

/// 残っているコピーを消す（できる範囲で）。
///
/// 起動時と、本体を止めた直後に呼びます。動いている本体のコピーは消せませんが、
/// そのときは黙って残します（次の機会に消えます）。
///
/// 消す範囲は `prefix` で決めます。
///
/// | 呼ぶところ   | `prefix`                      | 消すもの               |
/// |--------------|-------------------------------|------------------------|
/// | 起動時に1回  | `startup_prefix`（pid 抜き）  | 前回の分も含めて全部   |
/// | ビルドのたび | `copy_prefix`（自分の pid）   | 自分が作った分だけ     |
fn clean_old_copies(run_dir: &Path, prefix: &str) {
    let Ok(entries) = std::fs::read_dir(run_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        if !file_name.to_string_lossy().starts_with(prefix) {
            continue;
        }
        let _ = std::fs::remove_file(entry.path());
    }
}

/// 見張る対象をまとめた2つの値。
///
/// 0.4 秒ごとに作り直すので、パスの一覧は持ちません。**ファイル数**と
/// **更新時刻の最大**だけを持ちます。追加と削除は数で分かり、中身の変更は
/// 最大時刻で分かります。確保がほぼ 0 になり、比較も2値の比較で済みます。
///
/// **割り切り**: 「数が同じで、最大の更新時刻も同じまま、中身だけが入れ替わった」
/// 場合は取りこぼします。実用上は起きにくいので受け入れます。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Snapshot {
    /// 見張っているファイルの数。
    count: usize,
    /// 更新時刻のうち、いちばん新しいもの。
    latest: Option<SystemTime>,
}

impl Snapshot {
    /// 1ファイル数える。更新時刻が読めたときは最大を更新する。
    fn add(&mut self, modified: Option<SystemTime>) {
        self.count += 1;
        if let Some(time) = modified {
            if self.latest.is_none_or(|latest| time > latest) {
                self.latest = Some(time);
            }
        }
    }
}

/// 見張る対象をひとまわり見て、数と最大の更新時刻を数える。
fn snapshot(root: &Path) -> Snapshot {
    let mut snapshot = Snapshot::default();
    for dir in WATCH_DIRS {
        walk(&root.join(dir), &mut snapshot);
    }
    for file in WATCH_FILES {
        if let Ok(metadata) = std::fs::metadata(root.join(file)) {
            snapshot.add(metadata.modified().ok());
        }
    }
    snapshot
}

fn walk(dir: &Path, snapshot: &mut Snapshot) {
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
                    walk(&path, snapshot);
                }
            }
            Ok(_) => {
                if is_watched_file(&path) {
                    // `read_dir` の結果を使い回す。`fs::metadata` だとパスをもう一度解決する。
                    if let Ok(metadata) = entry.metadata() {
                        snapshot.add(metadata.modified().ok());
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
/// `.rs` と、`resources/lang/` の `.toml` だけを見張ります。拡張子で絞るので、
/// データベースのファイル（`.sqlite` や `.db-wal`）はここで落ちます。
/// 名前で外す必要はありません。
fn is_watched_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    // 隠しファイルと、エディタが作る一時ファイルは見張らない。
    if name.starts_with('.') {
        return false;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => true,
        Some("toml") => in_lang_dir(path),
        _ => false,
    }
}

/// `resources/lang/` の直下にあるか。
fn in_lang_dir(path: &Path) -> bool {
    // `resources/lang/ja.toml` と `resources/lang/ja/messages.toml` の**どちらも**見張ります。
    // `bengara-build` は後者（Laravel 本来の置き方）も読むので、片方だけだと
    // 直しても作り直されません。
    let Some(parent) = path.parent() else {
        return false;
    };
    is_lang_root(parent) || parent.parent().is_some_and(is_lang_root)
}

/// そのディレクトリが `resources/lang` か。
fn is_lang_root(dir: &Path) -> bool {
    let named = |path: Option<&Path>, want: &str| {
        path.and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == want)
    };
    named(Some(dir), "lang") && named(dir.parent(), "resources")
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
        // Laravel 本来の置き方（言語ごとのディレクトリ）も `bengara-build` が読むので見張る。
        assert!(is_watched_file(Path::new(
            "resources/lang/ja/messages.toml"
        )));
        // 言語の置き場所の外にある toml は見張らない。
        assert!(!is_watched_file(Path::new("config/other.toml")));
        assert!(!is_watched_file(Path::new(
            "resources/lang/ja/sub/messages.toml"
        )));
        assert!(!is_watched_file(Path::new("resources/views/ja.toml")));
        // ビルドに関係ないもの。
        assert!(!is_watched_file(Path::new("public/css/app.css")));
        assert!(!is_watched_file(Path::new("app/.hidden.rs")));
    }

    #[test]
    fn コピーの名前にプロセスidが入る() {
        let prefix = copy_prefix("myapp", 4242);
        assert_eq!(prefix, "myapp-4242-");
        // 自分のものは消す対象。
        assert!("myapp-4242-1.exe".starts_with(&prefix));
        // 別の serve のものは触らない。
        assert!(!"myapp-9999-1.exe".starts_with(&prefix));
    }

    #[test]
    fn 起動時の片付けはプロセスidを問わない() {
        // 自分の pid に絞ると、前回の serve のコピーが永久に残って積み上がる。
        let prefix = startup_prefix("myapp");
        assert_eq!(prefix, "myapp-");
        assert!("myapp-4242-1.exe".starts_with(&prefix));
        assert!("myapp-9999-1.exe".starts_with(&prefix));
        // 別のプロジェクトの名前は含まない。
        assert!(!"other-9999-1.exe".starts_with(&prefix));
    }

    #[test]
    fn 起動時に別のプロセスidのコピーを消す() {
        let dir = std::env::temp_dir().join(format!("bengara-serve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 前回の serve が残したコピー（別の pid）と、関係ないファイル。
        for name in ["myapp-4242-1.exe", "myapp-9999-3.exe", "other-1-1.exe"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }

        clean_old_copies(&dir, &startup_prefix("myapp"));

        assert!(!dir.join("myapp-4242-1.exe").exists());
        assert!(!dir.join("myapp-9999-3.exe").exists());
        // 別のプロジェクトのものは残す。
        assert!(dir.join("other-1-1.exe").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 見張りは数と最大の更新時刻で比べる() {
        let base = SystemTime::UNIX_EPOCH;
        let later = base + Duration::from_secs(1);

        let mut a = Snapshot::default();
        a.add(Some(base));
        a.add(Some(later));

        // 同じ数・同じ最大時刻なら、順番が違っても同じと見る。
        let mut b = Snapshot::default();
        b.add(Some(later));
        b.add(Some(base));
        assert_eq!(a, b);

        // ファイルが増えれば数で分かる。
        let mut added = a;
        added.add(Some(base));
        assert_ne!(a, added);

        // 中身が新しくなれば最大時刻で分かる。
        let mut touched = Snapshot::default();
        touched.add(Some(base));
        touched.add(Some(later + Duration::from_secs(1)));
        assert_ne!(a, touched);
    }

    #[test]
    fn 更新時刻が読めないファイルも数える() {
        // 数が合わないと、消えたことにして毎回ビルドしてしまう。
        let mut s = Snapshot::default();
        s.add(None);
        assert_eq!(s.count, 1);
        assert_eq!(s.latest, None);
    }

    /// 拡張子で絞るので自動的に外れるが、意図の記録として残す。
    /// 見張る対象を広げたときに、ここで気づけるようにしておく。
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
