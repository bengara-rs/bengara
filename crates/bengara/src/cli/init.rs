//! `cargo run -- init`：Laravel と同じ構成のディレクトリとファイルを作る。
//!
//! 何度実行しても壊れないようにしてあります。すでにあるファイルには触らず、
//! 足りないものだけを作ります。

use std::path::{Path, PathBuf};

use super::cargo_toml::CargoToml;
use super::version_requirement;
use crate::error::{Error, Result};

/// 作るディレクトリ。空のままになるものには `.gitkeep` を置きます。
const DIRECTORIES: &[&str] = &[
    "app/Console/Commands",
    "app/Http/Controllers",
    "app/Http/Middleware",
    "app/Jobs",
    "app/Listeners",
    "app/Models",
    "app/Policies",
    "bootstrap",
    "config",
    "database/factories",
    "database/migrations",
    "database/seeders",
    "public",
    "resources/lang",
    "resources/views",
    "routes",
    "tests/Feature",
    "tests/Unit",
];

/// `storage/` の下に作るディレクトリ。
///
/// `storage:init` と同じ一覧を使います。2か所に書くと必ずずれるためです。
fn storage_directories() -> Vec<String> {
    crate::ops::STORAGE_DIRECTORIES
        .iter()
        .map(|d| format!("storage/{d}"))
        .collect()
}

/// `.gitkeep` を置くディレクトリ（今はまだ中身が無いもの）。
const KEEP: &[&str] = &[
    "app/Console/Commands",
    "app/Http/Middleware",
    "app/Jobs",
    "app/Listeners",
    "app/Models",
    "app/Policies",
    "database/factories",
    "database/migrations",
    "resources/views",
    "storage/logs", // storage/ の中は storage_directories() が作る
    "tests/Unit",
];

pub(crate) fn run(root: &Path) -> Result<()> {
    let manifest_path = root.join("Cargo.toml");
    if !manifest_path.is_file() {
        return Err(Error::msg(format!(
            "{} が見つかりません。cargo new で作ったプロジェクトの中で実行してください",
            manifest_path.display()
        )));
    }

    let mut manifest = CargoToml::load(&manifest_path)?;
    let name = manifest
        .package_name()
        .ok_or_else(|| Error::msg("Cargo.toml の [package] に name がありません"))?;

    println!("bengara init: {} を用意します\n", root.display());

    let mut report = Report::default();

    for dir in DIRECTORIES {
        create_dir(root, dir, &mut report)?;
    }
    // storage/ の下は `storage:init` と同じ一覧で作る。
    for dir in storage_directories() {
        create_dir(root, &dir, &mut report)?;
    }
    for dir in KEEP {
        // 中身があるディレクトリには置かない。git は空のディレクトリだけを無視するため。
        if is_empty_dir(&root.join(dir)) {
            write_if_missing(root, &format!("{dir}/.gitkeep"), "", &mut report)?;
        }
    }

    // 利用者が手で書くことになっている入口と、数行の設定ファイル。
    write_if_missing(
        root,
        ".cargo/config.toml",
        stub("cargo_config"),
        &mut report,
    )?;
    write_if_missing(root, "build.rs", stub("build_rs"), &mut report)?;
    write_if_missing(root, "artisan.rs", stub("artisan_rs"), &mut report)?;
    write_if_missing(root, ".gitignore", stub("gitignore"), &mut report)?;
    write_if_missing(
        root,
        "storage/.gitignore",
        stub("storage_gitignore"),
        &mut report,
    )?;
    write_if_missing(root, "public/robots.txt", stub("robots"), &mut report)?;

    let env_text = stub("env").replace("{{name}}", &name);
    write_if_missing(root, ".env", &env_text, &mut report)?;
    write_if_missing(root, ".env.example", &env_text, &mut report)?;

    // アプリの骨組み。
    write_if_missing(root, "bootstrap/app.rs", stub("bootstrap_app"), &mut report)?;
    write_if_missing(
        root,
        "config/app.rs",
        &stub("config_app").replace("{{name}}", &name),
        &mut report,
    )?;
    write_if_missing(
        root,
        "config/database.rs",
        stub("config_database"),
        &mut report,
    )?;
    write_if_missing(
        root,
        "database/seeders/DatabaseSeeder.rs",
        stub("seeder_database"),
        &mut report,
    )?;
    write_if_missing(root, "routes/web.rs", stub("routes_web"), &mut report)?;
    write_if_missing(
        root,
        "routes/console.rs",
        stub("routes_console"),
        &mut report,
    )?;
    write_if_missing(root, "resources/lang/ja.toml", stub("lang_ja"), &mut report)?;
    write_if_missing(
        root,
        "app/Http/Controllers/HomeController.rs",
        stub("controller_home"),
        &mut report,
    )?;
    write_if_missing(
        root,
        "tests/Feature/HomeTest.rs",
        &stub("test_home").replace("{{name}}", &name),
        &mut report,
    )?;

    // 入口（main.rs）を用意し、cargo new が作った src/ を片付ける。
    update_main(root, &mut report)?;
    clean_src(root, &mut report)?;

    // Cargo.toml に足りない行を足す。
    update_manifest(&mut manifest, &name);
    if manifest.save(&manifest_path)? {
        report.updated.push("Cargo.toml".to_string());
    }

    report.print();
    println!(
        "\n次の一歩\n\n  cargo artisan serve     サーバーを起動して、変更を見張る\n  cargo test              テストを走らせる\n  cargo artisan list      コマンドの一覧\n"
    );
    Ok(())
}

/// 雛形の中身。フレームワークと同じ版のものが必ず使われます。
fn stub(name: &str) -> &'static str {
    match name {
        "cargo_config" => include_str!("stubs/cargo_config.stub"),
        "build_rs" => include_str!("stubs/build_rs.stub"),
        "main_rs" => include_str!("stubs/main_rs.stub"),
        "artisan_rs" => include_str!("stubs/artisan_rs.stub"),
        "env" => include_str!("stubs/env.stub"),
        "gitignore" => include_str!("stubs/gitignore.stub"),
        "storage_gitignore" => include_str!("stubs/storage_gitignore.stub"),
        "robots" => include_str!("stubs/robots.stub"),
        "bootstrap_app" => include_str!("stubs/bootstrap_app.stub"),
        "config_app" => include_str!("stubs/config_app.stub"),
        "config_database" => include_str!("stubs/config_database.stub"),
        "seeder_database" => include_str!("stubs/seeder_database.stub"),
        "routes_web" => include_str!("stubs/routes_web.stub"),
        "routes_console" => include_str!("stubs/routes_console.stub"),
        "lang_ja" => include_str!("stubs/lang_ja.stub"),
        "controller_home" => include_str!("stubs/controller_home.stub"),
        "test_home" => include_str!("stubs/test_home.stub"),
        other => unreachable!("雛形 {other} はありません"),
    }
}

#[derive(Default)]
struct Report {
    created: Vec<String>,
    kept: Vec<String>,
    updated: Vec<String>,
    removed: Vec<String>,
}

impl Report {
    fn print(&self) {
        print_list("作成", &self.created);
        print_list("更新", &self.updated);
        print_list("削除", &self.removed);
        print_list("そのまま", &self.kept);
        if self.created.is_empty() && self.updated.is_empty() && self.removed.is_empty() {
            println!("すべて揃っています。");
        }
    }
}

fn print_list(label: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    println!("{label}（{}件）", items.len());
    for item in items {
        println!("  {item}");
    }
    println!();
}

fn create_dir(root: &Path, relative: &str, report: &mut Report) -> Result<()> {
    let path = root.join(relative);
    if path.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(&path)
        .map_err(|e| Error::msg(format!("{} を作れません: {e}", path.display())))?;
    report.created.push(format!("{relative}/"));
    Ok(())
}

/// 中身が無いディレクトリか（`.gitkeep` は数えない）。
fn is_empty_dir(path: &Path) -> bool {
    let Ok(mut entries) = std::fs::read_dir(path) else {
        // 読めない・無いときは「空」として扱い、.gitkeep を置かせる。
        return true;
    };
    !entries.any(|entry| {
        entry
            .map(|e| e.file_name() != std::ffi::OsStr::new(".gitkeep"))
            .unwrap_or(false)
    })
}

fn write_if_missing(
    root: &Path,
    relative: &str,
    contents: &str,
    report: &mut Report,
) -> Result<()> {
    let path = root.join(relative);
    if path.exists() {
        report.kept.push(relative.to_string());
        return Ok(());
    }
    write_file(&path, contents)?;
    report.created.push(relative.to_string());
    Ok(())
}

fn write_file(path: &PathBuf, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::msg(format!("{} を作れません: {e}", parent.display())))?;
    }
    std::fs::write(path, contents)
        .map_err(|e| Error::msg(format!("{} を書けません: {e}", path.display())))
}

/// プロジェクト直下の `main.rs` を `bengara::app!();` にする。
///
/// `cargo new` が作った雛形と、`fn main() { bengara::run() }` の1行だけを置き換えます。
/// それ以外が書かれていたら、触らずに案内だけ出します。
fn update_main(root: &Path, report: &mut Report) -> Result<()> {
    let path = root.join("main.rs");
    let wanted = stub("main_rs");

    let current = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) => {
            // 直下に無ければ、cargo new が作った src/main.rs を引き継ぐ。
            let legacy = std::fs::read_to_string(root.join("src/main.rs")).unwrap_or_default();
            if !legacy.is_empty() && !is_replaceable_main(&legacy) {
                println!(
                    "src/main.rs に手で書いたコードがあります。中身を {} に移してから src/ を消してください。\n",
                    root.join("main.rs").display()
                );
            }
            write_file(&path, wanted)?;
            report.created.push("main.rs".to_string());
            return Ok(());
        }
    };

    if current.contains("bengara::app!") {
        report.kept.push("main.rs".to_string());
        return Ok(());
    }

    if is_replaceable_main(&current) {
        write_file(&path, wanted)?;
        report.updated.push("main.rs".to_string());
        return Ok(());
    }

    println!(
        "main.rs には手で書いたコードがあるため、そのままにしました。\n\
         次の1行だけの内容に置き換えてください。\n\n  {}\n",
        wanted.trim()
    );
    report.kept.push("main.rs".to_string());
    Ok(())
}

/// `cargo new` が作った `src/` を片付ける。
///
/// 入口は直下の `main.rs` と `artisan.rs` なので、`src/` は使いません。
/// 手で書いたコードが入っている場合は消さずに案内だけ出します。
fn clean_src(root: &Path, report: &mut Report) -> Result<()> {
    let src = root.join("src");
    if !src.is_dir() {
        return Ok(());
    }
    let main = src.join("main.rs");
    if main.is_file() {
        let text = std::fs::read_to_string(&main).unwrap_or_default();
        if !is_replaceable_main(&text) && !text.contains("bengara::app!") {
            println!("src/main.rs は使われません。中身を確かめてから消してください。\n");
            return Ok(());
        }
        std::fs::remove_file(&main)
            .map_err(|e| Error::msg(format!("{} を消せません: {e}", main.display())))?;
        report.removed.push("src/main.rs".to_string());
    }
    // 空になったディレクトリだけを消す（中身があれば残す）。
    let _ = std::fs::remove_dir(src.join("bin"));
    if std::fs::remove_dir(&src).is_ok() {
        report.removed.push("src/".to_string());
    }
    Ok(())
}

/// 置き換えてよい入口のファイルか。
fn is_replaceable_main(text: &str) -> bool {
    let body: String = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .collect::<Vec<_>>()
        .join("");
    let body = body.replace(' ', "");
    // cargo new の雛形 か、bengara::run() の1行。
    body == "fnmain(){println!(\"Hello,world!\");}" || body == "fnmain(){bengara::run()}"
}

/// `Cargo.toml` に足りない行を足す。すでにある行には触りません。
fn update_manifest(manifest: &mut CargoToml, name: &str) {
    let requirement = version_requirement();

    // bengara が動く Rust の下限。利用者の手元で古い版を使って失敗しないように書く。
    if !manifest.has_key("package", "rust-version") {
        manifest.add_line(
            "package",
            &format!("rust-version = \"{}\"", env!("CARGO_PKG_RUST_VERSION")),
        );
    }
    if !manifest.has_key("package", "default-run") {
        manifest.add_line("package", &format!("default-run = \"{name}\""));
    }
    // 入口はプロジェクト直下の main.rs と artisan.rs。src/ は使わない。
    if !manifest.has_key("package", "autobins") {
        manifest.add_line("package", "autobins = false");
    }
    // データベースは機能フラグで出し入れする。Laravel と同じく SQLite から始める。
    if !manifest.has_key("dependencies", "bengara") {
        manifest.add_line(
            "dependencies",
            &format!("bengara = {{ version = \"{requirement}\", features = [\"sqlite\"] }}"),
        );
    }
    if !manifest.has_key("build-dependencies", "bengara-build") {
        manifest.add_line(
            "build-dependencies",
            &format!("bengara-build = \"{requirement}\""),
        );
    }
    // シングルバイナリを小さく速くする設定。
    if !manifest.has_section("profile.release") {
        manifest.add_line("profile.release", "lto = \"fat\"");
        manifest.add_line("profile.release", "codegen-units = 1");
        manifest.add_line("profile.release", "strip = true");
    }
    // 繰り返しの節なので、末尾にまとめて足す。
    // 空白と引用符の書き方が違っても二重に足さないよう、値で見る。
    if !manifest.has_path("main.rs") {
        manifest.append_block(&[
            "[[bin]]",
            &format!("name = \"{name}\""),
            "path = \"main.rs\"",
        ]);
    }
    if !manifest.has_path("artisan.rs") {
        manifest.append_block(&["[[bin]]", "name = \"artisan\"", "path = \"artisan.rs\""]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 置き換えてよいmainを見分ける() {
        assert!(is_replaceable_main(
            "fn main() {\n    println!(\"Hello, world!\");\n}\n"
        ));
        assert!(is_replaceable_main("fn main() { bengara::run() }\n"));
        assert!(is_replaceable_main(
            "// コメント\nfn main() { bengara::run() }\n"
        ));
        assert!(!is_replaceable_main(
            "fn main() {\n    my_app::start();\n}\n"
        ));
    }

    #[test]
    fn 雛形がすべて揃っている() {
        for name in [
            "cargo_config",
            "build_rs",
            "main_rs",
            "artisan_rs",
            "env",
            "gitignore",
            "storage_gitignore",
            "robots",
            "bootstrap_app",
            "config_app",
            "config_database",
            "seeder_database",
            "routes_web",
            "routes_console",
            "lang_ja",
            "controller_home",
            "test_home",
        ] {
            assert!(!stub(name).is_empty(), "{name} が空です");
        }
    }

    #[test]
    fn 雛形の依存にはsqliteが入る() {
        let mut manifest = CargoToml::load_for_test("[package]\nname = \"myapp\"\n");
        update_manifest(&mut manifest, "myapp");
        assert!(manifest.contains("features = [\"sqlite\"]"));
    }

    #[test]
    fn 書き方が違うbinは二重に足さない() {
        // 空白なしと単引用符。どちらも「すでにある」と見なす。
        let mut manifest = CargoToml::load_for_test(
            "[package]\nname = \"myapp\"\n\n[[bin]]\nname = \"myapp\"\npath=\"main.rs\"\n\n[[bin]]\nname = \"artisan\"\npath = 'artisan.rs'\n",
        );
        update_manifest(&mut manifest, "myapp");
        assert_eq!(manifest.count_lines("[[bin]]"), 2);
    }

    #[test]
    fn 節の形の依存は二重に足さない() {
        let mut manifest = CargoToml::load_for_test(
            "[package]\nname = \"myapp\"\n\n[dependencies.bengara]\nversion = \"0.1\"\nfeatures = [\"sqlite\"]\n",
        );
        update_manifest(&mut manifest, "myapp");
        assert_eq!(manifest.count_lines("bengara = {"), 0);
    }

    #[test]
    fn 版指定の形() {
        let requirement = version_requirement();
        assert!(requirement.starts_with('0') || requirement.parse::<u32>().is_ok());
    }
}
