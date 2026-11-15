//! `cargo run -- init`：Laravel と同じ構成のディレクトリとファイルを作る。
//!
//! 何度実行しても壊れないようにしてあります。すでにあるファイルには触らず、
//! 足りないものだけを作ります。
//!
//! ただし次の3つは例外です。`cargo new` や利用者がすでに持っていることが多く、
//! そのまま残すと `init` の目的が果たせないからです。
//!
//! | ファイル | すでにあるとき |
//! |---|---|
//! | `.gitignore` | 雛形の行のうち、足りない行を足す |
//! | `.cargo/config.toml` | `artisan` の別名が無ければ足す |
//! | `build.rs` | `bengara_build::discover()` が無ければ警告を出す（書き換えない） |
//!
//! 中身を確かめるファイルは、**何かを書く前に全部読みます**（[`Existing`]）。
//! 読めないファイル（UTF-8 でないものなど）があれば、何も書き換えずに止まります。

use std::io::ErrorKind;
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
    // `storage/` の下は入れない。`storage/.gitignore` が `*` で全部無視するので、
    // `.gitkeep` を置いても git には入らない（作る意味が無い）。
    // 中のディレクトリは `storage:init` が作る。
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

    // 中身を確かめるファイルを、書き始める前に全部読む。
    // 読めないものがあれば、ここで止まる（まだ何も書き換えていない）。
    let existing = Existing::read(root)?;

    println!("bengara init: {} を用意します\n", root.display());

    let mut report = Report::default();

    for dir in DIRECTORIES {
        create_dir(root, dir, &mut report)?;
    }
    create_storage_dirs(root, &mut report)?;
    for dir in KEEP {
        // 中身があるディレクトリには置かない。git は空のディレクトリだけを無視するため。
        if is_empty_dir(&root.join(dir)) {
            write_if_missing(root, &format!("{dir}/.gitkeep"), "", &mut report)?;
        }
    }

    // 利用者が手で書くことになっている入口と、数行の設定ファイル。
    // この3つは「すでにある」ことが多いので、そのまま残さず中身を確かめる。
    merge_cargo_config(root, existing.cargo_config, &mut report)?;
    check_build_rs(root, existing.build_rs.as_deref(), &mut report)?;
    merge_gitignore(root, existing.gitignore.as_deref(), &mut report)?;
    write_if_missing(root, "artisan.rs", stub("artisan_rs"), &mut report)?;
    write_if_missing(
        root,
        "storage/.gitignore",
        stub("storage_gitignore"),
        &mut report,
    )?;
    write_if_missing(root, "public/robots.txt", stub("robots"), &mut report)?;

    let env_text = stub("env").replace("{{name}}", &name);
    // `.env` には、あとで `key:generate` が `APP_KEY` を書き込む。
    // 秘密を持つファイルなので、所有者だけが読めるようにする。
    // `.env.example` は秘密を持たないので、ふつうの権限のままにする。
    write_secret_if_missing(root, ".env", &env_text, &mut report)?;
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
    update_main(root, existing.main_rs.as_deref(), &mut report)?;
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

/// すでにあるファイルの中身。`init` が書き始める前に読みます。
///
/// `init` は「あれば中身を確かめ、無ければ雛形を書く」ファイルをいくつか持ちます。
/// 以前は読み込みの失敗をすべて「無い」と同じに扱っていたので、**読めないだけ**の
/// ファイルを雛形で上書きしていました。たとえば Windows PowerShell 5.1 の `>` で
/// 作った UTF-16 の `.gitignore` です。手で書いた中身が確認なしで消え、一覧には
/// 「作成」と出ていました。
///
/// 「無い」と見なすのは `ErrorKind::NotFound` のときだけです。それ以外はファイル名を
/// 添えたエラーにします。先にまとめて読むのは、途中まで書いてから止まらないためです。
struct Existing {
    cargo_config: Option<CargoToml>,
    build_rs: Option<String>,
    gitignore: Option<String>,
    main_rs: Option<String>,
}

impl Existing {
    fn read(root: &Path) -> Result<Self> {
        let cargo_config = root.join(".cargo/config.toml");
        Ok(Self {
            // 先に `read_optional` で確かめる。エラーの文をほかのファイルとそろえるため。
            cargo_config: match read_optional(&cargo_config)? {
                Some(_) => Some(CargoToml::load(&cargo_config)?),
                None => None,
            },
            build_rs: read_optional(&root.join("build.rs"))?,
            gitignore: read_optional(&root.join(".gitignore"))?,
            main_rs: read_optional(&root.join("main.rs"))?,
        })
    }
}

/// ファイルを読む。**無いときだけ** `None` を返す。
///
/// 読めないとき（UTF-8 でない・権限が無い・ディレクトリだった）はエラーです。
/// 「無い」と同じに扱うと、呼ぶ側が雛形で上書きしてしまいます。
fn read_optional(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::msg(format!(
            "{} を読めません: {error}\n\
             init は何も書き換えていません。読めるように直してから（文字コードは UTF-8）、\
             もう一度実行してください",
            path.display()
        ))),
    }
}

/// ディレクトリを作る。戻り値は「いま作ったか」です。
fn create_dir(root: &Path, relative: &str, report: &mut Report) -> Result<bool> {
    let path = root.join(relative);
    if path.is_dir() {
        return Ok(false);
    }
    std::fs::create_dir_all(&path)
        .map_err(|e| Error::msg(format!("{} を作れません: {e}", path.display())))?;
    report.created.push(format!("{relative}/"));
    Ok(true)
}

/// `storage/` とその下を作る。`storage:init` と同じ一覧・同じ権限にします。
///
/// 作ったものは所有者だけに絞ります（Unix では 0700。`ops::restrict`）。
/// 以前は `create_dir_all` だけだったので umask まかせになり、`storage:init` は
/// 「すでにある」ものを触らないので、`init` したプロジェクトではセッションや
/// ログの置き場所が他のアカウントから読めるまま残っていました。
///
/// `storage/` 自身も先に作って絞ります。下のディレクトリを作るついでに
/// できた `storage/` は、絞られないまま残るためです。
///
/// すでにあるものは触りません（`storage:init` と同じ）。
fn create_storage_dirs(root: &Path, report: &mut Report) -> Result<()> {
    for dir in std::iter::once("storage".to_string()).chain(storage_directories()) {
        if create_dir(root, &dir, report)? {
            let path = root.join(&dir);
            crate::ops::restrict(&path)
                .map_err(|e| Error::msg(format!("{} の権限を絞れません: {e}", path.display())))?;
        }
    }
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

/// 秘密を持つファイルを書く。中身は `write_if_missing` と同じで、権限だけ絞ります。
fn write_secret_if_missing(
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
    restrict_file(&path)?;
    report.created.push(relative.to_string());
    Ok(())
}

/// 所有者だけが読み書きできるようにする（Unix のみ）。
///
/// `std::fs::write` だけでは umask まかせ（ふつう 0644）で、共用ホストでは
/// 同じマシンの他のアカウントから `APP_KEY` が読めます。`storage/` は 0700 に
/// 絞ってあるので、秘密を持つファイルのほうが緩いのは不釣り合いです。
///
/// Windows では何もしません。既定の継承に任せます（`ops::restrict` と同じ方針）。
#[cfg(unix)]
fn restrict_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| Error::msg(format!("{} の権限を絞れません: {e}", path.display())))
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) -> Result<()> {
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

/// `.gitignore` に、雛形の行のうち足りないものを足す。
///
/// **そのまま残してはいけません。** `cargo new` は `/target` の1行だけの `.gitignore` を
/// 必ず作ります。「ある」と見て残すと `/.env` が入らず、`key:generate` で作った
/// `APP_KEY` 入りの `.env` が git に入ります。
///
/// `current` は、すでにあるファイルの中身です（無ければ `None`）。読むのは
/// [`Existing::read`] で、読めないファイルはここまで来ません。
fn merge_gitignore(root: &Path, current: Option<&str>, report: &mut Report) -> Result<()> {
    let path = root.join(".gitignore");
    let wanted = stub("gitignore");

    let Some(current) = current else {
        write_file(&path, wanted)?;
        report.created.push(".gitignore".to_string());
        println!(".env を git の管理から外しました。\n");
        return Ok(());
    };

    let missing = missing_lines(current, wanted);
    if missing.is_empty() {
        report.kept.push(".gitignore".to_string());
        return Ok(());
    }

    // 1行足すだけで全行が差分になるのを避けるため、元の改行に合わせる。
    let newline = if current.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut lines: Vec<String> = current.lines().map(str::to_string).collect();
    lines.extend(missing.iter().cloned());
    let mut text = lines.join(newline);
    text.push_str(newline);
    std::fs::write(&path, text)
        .map_err(|e| Error::msg(format!("{} を書けません: {e}", path.display())))?;

    report.updated.push(".gitignore".to_string());
    // 鍵が git に入りかけていたので、気づけるように1行出す。
    if missing.iter().any(|line| line == "/.env") {
        println!(".env を git の管理から外しました。\n");
    }
    Ok(())
}

/// 雛形にあって既存のファイルに無い行。
///
/// コメント行と空行は足しません。無くても害が無く、足すと読みにくくなるためです。
fn missing_lines(current: &str, wanted: &str) -> Vec<String> {
    // 先頭の BOM は行の一部ではない。外さないと 1 行目（`/target` など）が
    // 「無い」と見えて、同じ行をもう 1 つ足してしまう。
    let current = current.trim_start_matches('\u{feff}');
    let mut missing: Vec<String> = Vec::new();
    for line in wanted.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let in_current = current.lines().any(|l| l.trim() == line);
        if in_current || missing.iter().any(|l| l == line) {
            continue;
        }
        missing.push(line.to_string());
    }
    missing
}

/// `.cargo/config.toml` に `artisan` の別名が無ければ足す。
///
/// リンカの指定などで自前の `.cargo/config.toml` を持っている人は多いです。
/// そのまま残すと `cargo artisan` が「そんなサブコマンドは無い」で終わり、
/// `init` が最後に案内する「次の一歩」がそのまま動きません。
///
/// `config` は、すでにあるファイルです（無ければ `None`）。
fn merge_cargo_config(root: &Path, config: Option<CargoToml>, report: &mut Report) -> Result<()> {
    let relative = ".cargo/config.toml";
    let path = root.join(relative);
    // `CargoToml` は Cargo.toml 用の名前だが、中身は「行の並び」として扱う汎用の作り。
    let Some(mut config) = config else {
        write_file(&path, stub("cargo_config"))?;
        report.created.push(relative.to_string());
        return Ok(());
    };

    if config.has_key("alias", "artisan") {
        report.kept.push(relative.to_string());
        return Ok(());
    }
    config.add_line("alias", artisan_alias_line());
    if config.save(&path)? {
        report.updated.push(relative.to_string());
        println!("cargo artisan が使えるように、.cargo/config.toml に別名を足しました。\n");
    }
    Ok(())
}

/// 雛形から `artisan` の別名の行を取り出す。
///
/// 同じ行を2か所に書くと必ずずれるので、雛形を唯一の出どころにします。
fn artisan_alias_line() -> &'static str {
    stub("cargo_config")
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("artisan"))
        .unwrap_or(r#"artisan = ["run", "-q", "--bin", "artisan", "--"]"#)
}

/// `build.rs` が `bengara_build::discover()` を呼んでいるか確かめる。
///
/// 呼んでいないと `bengara::app!()` が `bengara_app.rs` を読めず、原因の分からない
/// エラーになります。自前の `build.rs` を壊さないよう、書き換えずに案内だけ出します。
///
/// `current` は、すでにあるファイルの中身です（無ければ `None`）。
fn check_build_rs(root: &Path, current: Option<&str>, report: &mut Report) -> Result<()> {
    let path = root.join("build.rs");
    let Some(current) = current else {
        write_file(&path, stub("build_rs"))?;
        report.created.push("build.rs".to_string());
        return Ok(());
    };

    report.kept.push("build.rs".to_string());
    if !current.contains("bengara_build::discover") {
        println!(
            "build.rs が bengara_build::discover() を呼んでいません。\n\
             このままでは bengara::app!() が bengara_app.rs を読めません。\n\
             fn main() の中に次の1行を足してください。\n\n  \
             bengara_build::discover()\n"
        );
    }
    Ok(())
}

/// プロジェクト直下の `main.rs` を `bengara::app!();` にする。
///
/// `cargo new` が作った雛形と、`fn main() { bengara::run() }` の1行だけを置き換えます。
/// それ以外が書かれていたら、触らずに案内だけ出します。
///
/// `current` は、すでにある `main.rs` の中身です（無ければ `None`）。
fn update_main(root: &Path, current: Option<&str>, report: &mut Report) -> Result<()> {
    let path = root.join("main.rs");
    let wanted = stub("main_rs");

    let Some(current) = current else {
        // 直下に無ければ、cargo new が作った src/main.rs を引き継ぐ。
        //
        // こちらは読めなくても止めない。`src/main.rs` は上書きも削除もしないので
        // （`clean_src` は読めないものを残す）、失うものが無い。読めないものは
        // 「手で書いたコード」として案内する。
        let legacy = std::fs::read_to_string(root.join("src/main.rs"));
        let hand_written = match &legacy {
            Ok(text) => !text.is_empty() && !is_replaceable_main(text),
            Err(error) => error.kind() != ErrorKind::NotFound,
        };
        if hand_written {
            println!(
                "src/main.rs に手で書いたコードがあります。中身を {} に移してから src/ を消してください。\n",
                root.join("main.rs").display()
            );
        }
        write_file(&path, wanted)?;
        report.created.push("main.rs".to_string());
        return Ok(());
    };

    if current.contains("bengara::app!") {
        report.kept.push("main.rs".to_string());
        return Ok(());
    }

    if is_replaceable_main(current) {
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
///
/// 空白の書き方（空白・タブ・改行）と行コメントは無視して比べます。
/// `bengara::run()` は `;` があっても無くても同じ意味なので、どちらも認めます。
/// 以前は `;` つきを「手で書いたコード」と見て、置き換えずに案内だけ出していました。
fn is_replaceable_main(text: &str) -> bool {
    // 先頭の BOM は中身ではない。
    let body: String = text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .flat_map(str::chars)
        .filter(|c| !c.is_whitespace())
        .collect();
    // cargo new の雛形 か、bengara::run() の1行。
    matches!(
        body.as_str(),
        "fnmain(){println!(\"Hello,world!\");}"
            | "fnmain(){bengara::run()}"
            | "fnmain(){bengara::run();}"
    )
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
    // 入口はプロジェクト直下の main.rs と artisan.rs。src/ は使わない。
    if !manifest.has_key("package", "autobins") {
        manifest.add_line("package", "autobins = false");
    }
    // `tests/` の直下に置いた `.rs` を、cargo が**独立したテスト**として登録しない
    // ようにする。登録されると、そのクレートには `__bengara_hooks` も `bootstrap` も
    // 無いので、`#[bengara::test]` が展開したコードが「そんな関数は無い」で落ちる。
    // テストは bengara-build が本体の中に取り込むので、cargo 側の自動検出は要らない。
    //
    // `autoexamples` と `autobenches` は足さない。`examples/` と `benches/` は
    // bengara が走査しない場所なので二重にならず、cargo の自動検出がそのまま役に立つ。
    if !manifest.has_key("package", "autotests") {
        manifest.add_line("package", "autotests = false");
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

    // `default-run` は、その名前の bin が本当にあるときだけ書く。だから
    // `[[bin]]` を足したあとに見る。
    //
    // すでに `[[bin]] name = "server" path = "main.rs"` を持つプロジェクトでは、
    // `main.rs` の bin が「すでにある」ので上では足されない。それでも
    // `default-run = "myapp"` を書くと、`cargo build` が
    // `default-run target 'myapp' not found` で落ちていた。
    //
    // 無いときは書かない（勝手に2つめの bin を足さない）。bin が2つあって
    // `default-run` が無いと `cargo run` が「どれを動かすか決められない」と
    // 言うだけで、こちらは直し方が分かるエラーになる。
    if !manifest.has_key("package", "default-run") {
        if manifest.has_bin_name(name) {
            manifest.add_line("package", &format!("default-run = \"{name}\""));
        } else {
            println!(
                "[[bin]] に name = \"{name}\" が無いので、default-run は書きませんでした。\n\
                 `cargo run` で動かす bin を [package] default-run に書いてください。\n"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用の空のプロジェクト。テストごとに別の名前にする。
    fn temp_project(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("bengara-init-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"myapp\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        root
    }

    /// UTF-16LE（BOM つき）の中身。Windows PowerShell 5.1 の `>` が作る形。
    fn utf16(text: &str) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn 読めないファイルは上書きせずに止まる() {
        // 以前は「読めない」を「無い」と同じに扱い、雛形で上書きしていた。
        for file in [".gitignore", "build.rs", "main.rs", ".cargo/config.toml"] {
            let tag = file.replace(['.', '/'], "_");
            let root = temp_project(&format!("unreadable{tag}"));
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let original = utf16("/my-secret-dir\r\n");
            std::fs::write(&path, &original).unwrap();
            let manifest = std::fs::read(root.join("Cargo.toml")).unwrap();

            let error = run(&root).expect_err("読めないファイルがあるのに進みました");
            let message = error.to_string();
            // どのファイルかが分かる。
            assert!(message.contains(&path.display().to_string()), "{message}");

            // 中身はそのまま。
            assert_eq!(std::fs::read(&path).unwrap(), original, "{file}");
            // ほかのものも、何も書き換えていない。
            assert_eq!(std::fs::read(root.join("Cargo.toml")).unwrap(), manifest);
            for untouched in ["app", "storage", "artisan.rs", ".env", "routes"] {
                assert!(
                    !root.join(untouched).exists(),
                    "{file}: {untouched} を作っています"
                );
            }

            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn 無いファイルは雛形で作る() {
        // 「無い」（NotFound）のときは、今までどおり作る。
        let root = temp_project("fresh");
        run(&root).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join(".gitignore")).unwrap(),
            stub("gitignore")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("build.rs")).unwrap(),
            stub("build_rs")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("main.rs")).unwrap(),
            stub("main_rs")
        );
        assert_eq!(
            std::fs::read_to_string(root.join(".cargo/config.toml")).unwrap(),
            stub("cargo_config")
        );
        // storage/ の下は `storage:init` と同じ一覧で揃う。
        for dir in storage_directories() {
            assert!(root.join(&dir).is_dir(), "{dir}");
        }

        // 2回目は何も変えない。
        let before = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        run(&root).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("Cargo.toml")).unwrap(),
            before
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn 無いと読めないを区別する() {
        let root = temp_project("optional");
        assert_eq!(read_optional(&root.join("nothing.txt")).unwrap(), None);

        std::fs::write(root.join("ok.txt"), "中身\n").unwrap();
        assert_eq!(
            read_optional(&root.join("ok.txt")).unwrap().as_deref(),
            Some("中身\n")
        );

        // UTF-8 でないファイルは「無い」ではない。
        std::fs::write(root.join("utf16.txt"), utf16("/target\r\n")).unwrap();
        let error = read_optional(&root.join("utf16.txt"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("utf16.txt"), "{error}");
        assert!(error.contains("何も書き換えていません"), "{error}");

        // ディレクトリも「無い」ではない。
        std::fs::create_dir(root.join("dir")).unwrap();
        assert!(read_optional(&root.join("dir")).is_err());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn 読めないsrc_mainは消さずに案内する() {
        // `src/main.rs` は上書きも削除もしないので、読めなくても止めない。
        let root = temp_project("legacy");
        std::fs::create_dir(root.join("src")).unwrap();
        let original = utf16("fn main() {}\r\n");
        std::fs::write(root.join("src/main.rs"), &original).unwrap();

        run(&root).unwrap();

        assert_eq!(std::fs::read(root.join("src/main.rs")).unwrap(), original);
        assert_eq!(
            std::fs::read_to_string(root.join("main.rs")).unwrap(),
            stub("main_rs")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn storageは自身も含めて作る() {
        let root = temp_project("storage");
        let mut report = Report::default();
        create_storage_dirs(&root, &mut report).unwrap();

        assert!(root.join("storage").is_dir());
        for dir in storage_directories() {
            assert!(root.join(&dir).is_dir(), "{dir}");
        }
        // `storage/` 自身も一覧に出る（下のついでに黙って作らない）。
        assert_eq!(report.created.first().map(String::as_str), Some("storage/"));

        // 2回目は何も作らない。
        let mut report = Report::default();
        create_storage_dirs(&root, &mut report).unwrap();
        assert!(report.created.is_empty(), "{:?}", report.created);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// `storage:init` と同じく、所有者だけに絞る。
    #[cfg(unix)]
    #[test]
    fn initが作るstorageは0700になる() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_project("storage-mode");
        let mut report = Report::default();
        create_storage_dirs(&root, &mut report).unwrap();

        for dir in std::iter::once("storage".to_string()).chain(storage_directories()) {
            let mode = std::fs::metadata(root.join(&dir))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "{dir}: {mode:o}");
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bomつきのgitignoreでも同じ行を足さない() {
        // 先頭の BOM を行の一部と見ると、1 行目の `/target` が「無い」ことになる。
        let missing = missing_lines("\u{feff}/target\n", stub("gitignore"));
        assert!(!missing.iter().any(|l| l == "/target"), "{missing:?}");
        assert!(missing.iter().any(|l| l == "/.env"), "{missing:?}");
    }

    /// `.env` には `APP_KEY` が入るので、所有者だけが読める権限にする。
    #[cfg(unix)]
    #[test]
    fn 秘密を持つファイルは0600になる() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("bengara-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let mut report = Report::default();
        write_secret_if_missing(&root, ".env", "APP_KEY=\n", &mut report).unwrap();

        let mode = std::fs::metadata(root.join(".env"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
        assert_eq!(report.created, vec![".env".to_string()]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn 置き換えてよいmainを見分ける() {
        assert!(is_replaceable_main(
            "fn main() {\n    println!(\"Hello, world!\");\n}\n"
        ));
        assert!(is_replaceable_main("fn main() { bengara::run() }\n"));
        assert!(is_replaceable_main(
            "// コメント\nfn main() { bengara::run() }\n"
        ));
        // `;` つきと、空白の書き方の違い（タブ・改行・詰めた形）も同じと見る。
        assert!(is_replaceable_main("fn main() { bengara::run(); }\n"));
        assert!(is_replaceable_main("fn main() {\n\tbengara::run();\n}\n"));
        assert!(is_replaceable_main("fn main(){bengara::run()}"));
        assert!(is_replaceable_main(
            "\u{feff}fn main() {\r\n    bengara::run()\r\n}\r\n"
        ));
        // 1文でも足してあれば、手で書いたコードと見る。
        assert!(!is_replaceable_main(
            "fn main() { setup(); bengara::run(); }\n"
        ));
        assert!(!is_replaceable_main(""));
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
    fn 実在しないbinはdefault_runに書かない() {
        // すでに `[[bin]] name = "server" path = "main.rs"` を持つプロジェクト。
        // `main.rs` の bin は足されないので、`default-run = "myapp"` を書くと
        // `cargo build` が `default-run target 'myapp' not found` で落ちていた。
        let mut manifest = CargoToml::load_for_test(
            "[package]\nname = \"myapp\"\n\n[[bin]]\nname = \"server\"\npath = \"main.rs\"\n",
        );
        update_manifest(&mut manifest, "myapp");
        assert!(!manifest.contains("default-run"));

        // 素のプロジェクトでは、足した `[[bin]]` に合わせて書く。
        let mut manifest = CargoToml::load_for_test("[package]\nname = \"myapp\"\n");
        update_manifest(&mut manifest, "myapp");
        assert!(manifest.contains("default-run = \"myapp\""));
        // `tests/` の直下の `.rs` を cargo が独立したテストにしないようにする。
        assert!(manifest.contains("autotests = false"));
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
    fn cargo_newのgitignoreに足りない行を足す() {
        // cargo new は `/target` の1行だけを作る。ここで `/.env` が入らないと
        // APP_KEY 入りの .env が git に入る。
        let missing = missing_lines("/target\n", stub("gitignore"));
        assert!(missing.iter().any(|l| l == "/.env"), "{missing:?}");
        assert!(missing.iter().any(|l| l == "/storage/framework"));
        assert!(missing.iter().any(|l| l == "/database/*.sqlite"));
        // すでにある行は足さない。
        assert!(!missing.iter().any(|l| l == "/target"));
        // コメント行と空行は足さない。
        assert!(!missing.iter().any(|l| l.starts_with('#') || l.is_empty()));
    }

    #[test]
    fn 揃っているgitignoreには足さない() {
        assert!(missing_lines(stub("gitignore"), stub("gitignore")).is_empty());
        // 前後の空白と並びの違いは無視する。
        let current = "  /.env  \n/database/*.sqlite-wal\n/storage/framework\n\
                       /database/*.sqlite\n/database/*.sqlite-shm\n/target\n";
        assert!(missing_lines(current, stub("gitignore")).is_empty());
    }

    #[test]
    fn artisanの別名は雛形から取る() {
        let line = artisan_alias_line();
        assert!(line.starts_with("artisan"), "{line}");
        assert!(line.contains("--bin"), "{line}");
        // 既存の .cargo/config.toml に足したとき、そのまま別名として読める形になる。
        let mut config = CargoToml::load_for_test("[build]\nrustflags = []\n");
        assert!(!config.has_key("alias", "artisan"));
        config.add_line("alias", line);
        assert!(config.has_key("alias", "artisan"));
    }

    #[test]
    fn 版指定の形() {
        let requirement = version_requirement();
        assert!(requirement.starts_with('0') || requirement.parse::<u32>().is_ok());
    }
}
