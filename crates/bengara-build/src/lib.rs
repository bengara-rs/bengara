//! `build.rs` から呼ぶファイル自動検出。
//!
//! Laravel と同じ名前のディレクトリ（`app/`、`bootstrap/`、`config/`、`database/`、
//! `routes/`、`tests/`）を走査し、Rust のモジュール宣言を `OUT_DIR/bengara_app.rs`
//! に書き出します。プロジェクト直下の `main.rs` にある `bengara::app!()` が、それを取り込みます。
//!
//! 設計上の約束
//!
//! - 書き出すのは `OUT_DIR` だけ（Cargo のルール）。
//! - パニックしない。問題は生成コードの中の `compile_error!` で知らせる。
//!   ビルドスクリプトが失敗すると `artisan` もビルドできなくなるため。
//! - 依存クレートを持たない。
//!
//! 生成されるコードの形は、ファイルの実体をクレート直下に平らに並べ、
//! 名前空間は再エクスポートだけで組み立てます。入れ子のモジュールの中に
//! `#[path]` を書くと rust-analyzer がパスを解決できないためです。

mod toml;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{env, fs};

/// 走査するディレクトリ。
///
/// - `reexport`: 先頭が大文字のファイルから、同じ名前の型を再エクスポートする
/// - `test_only`: `#[cfg(test)]` を付けて取り込む
struct Target {
    name: &'static str,
    reexport: bool,
    test_only: bool,
}

const TARGETS: &[Target] = &[
    Target {
        name: "app",
        reexport: true,
        test_only: false,
    },
    Target {
        name: "bootstrap",
        reexport: true,
        test_only: false,
    },
    Target {
        name: "config",
        reexport: true,
        test_only: false,
    },
    Target {
        name: "database",
        reexport: true,
        test_only: false,
    },
    Target {
        name: "routes",
        reexport: true,
        test_only: false,
    },
    Target {
        name: "tests",
        reexport: false,
        test_only: true,
    },
];

/// 生成するファイル名（`OUT_DIR` の下）。
const OUT_FILE: &str = "bengara_app.rs";

/// この大きさを超えるファイルを埋め込むときは警告を出す（8 MiB）。
const MAX_EMBED_FILE: u64 = 8 * 1024 * 1024;

/// `public/` の合計がこれを超えるときは警告を出す（32 MiB）。
const MAX_EMBED_TOTAL: u64 = 32 * 1024 * 1024;

/// `build.rs` から呼ぶ入口。
///
/// ```ignore
/// fn main() { bengara_build::discover() }
/// ```
pub fn discover() {
    let Some(root) = env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from) else {
        warn("CARGO_MANIFEST_DIR が設定されていません。自動検出を飛ばします");
        return;
    };
    let Some(out_dir) = env::var_os("OUT_DIR").map(PathBuf::from) else {
        warn("OUT_DIR が設定されていません。自動検出を飛ばします");
        return;
    };

    let mut gen = Generator::default();
    gen.run(&root);

    let code = gen.finish();
    let out_path = out_dir.join(OUT_FILE);
    if let Err(e) = fs::write(&out_path, code) {
        // ここで失敗すると compile_error! も届けられないので、警告だけ出して続ける。
        warn(&format!(
            "{} を書き出せませんでした: {e}",
            out_path.display()
        ));
    }
}

/// `cargo:warning=` を出す。
fn warn(message: &str) {
    println!("cargo:warning=bengara-build: {message}");
}

#[derive(Default)]
struct Generator {
    /// クレート直下に平らに並べる `#[path]` 付きのモジュール宣言。
    flat: String,
    /// 再エクスポートだけで組み立てる名前空間。
    tree: String,
    /// 今たどっているディレクトリのモジュール名（平らな名前を作るのに使う）。
    stack: Vec<String>,
    /// すでに使った平らな名前。重なったときに連番を足すため。
    taken_flat: BTreeMap<String, usize>,
    /// `config/` 直下で見つかった設定モジュール（登録順）。
    configs: Vec<String>,
    /// `database/migrations/` が見つかったか。
    has_migrations: bool,
    /// `database/seeders/` が見つかったか。
    has_seeders: bool,
    /// `app/Jobs/` が見つかったか。
    has_jobs: bool,
    /// `app/Console/Commands/` が見つかったか。
    has_commands: bool,
    /// `routes/console.rs` が見つかったか。
    has_console: bool,
    /// `resources/lang/*.toml` から読んだ文字（言語ごと）。
    lang: Vec<(String, Vec<(String, String)>)>,
    /// `public/` の中のファイル（配信するときの鍵と、実ファイルのパス）。
    ///
    /// リリースビルドのときだけ中身が入ります。
    public: Vec<(String, PathBuf)>,
    /// 生成コードに埋め込むエラー（`compile_error!` にする）。
    errors: Vec<String>,
}

impl Generator {
    fn run(&mut self, root: &Path) {
        for target in TARGETS {
            let dir = root.join(target.name);
            // ディレクトリ自体の変化（ファイルの追加・削除）を監視する。
            // ファイルの内容の変化は、取り込んだソースとして rustc 側で追跡される。
            println!("cargo:rerun-if-changed={}", dir.display());
            if !dir.is_dir() {
                continue;
            }
            if target.test_only {
                self.tree.push_str("#[cfg(test)]\n");
            }
            self.emit_dir(&dir, target.name, target, 0);
        }
        self.read_lang(root);
        self.read_public(root);
    }

    /// `public/` の中身を、バイナリに埋め込む一覧にする。
    ///
    /// **リリースビルドのときだけ**集めます。デバッグビルドではディスクから読むので、
    /// ファイルを直したらすぐ反映されます。
    fn read_public(&mut self, root: &Path) {
        let dir = root.join("public");
        println!("cargo:rerun-if-changed={}", dir.display());

        // PROFILE は cargo がビルドスクリプトに渡す（"debug" か "release"）。
        if env::var("PROFILE").as_deref() != Ok("release") {
            return;
        }
        if !dir.is_dir() {
            return;
        }
        self.collect_public(&dir, "");

        // 大きくなりすぎたら気づけるようにする。止めはしない。
        let mut total: u64 = 0;
        for (key, path) in &self.public {
            let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            total += size;
            if size > MAX_EMBED_FILE {
                warn(&format!(
                    "public/{key} は {} MiB あります。バイナリに埋め込みます",
                    size / 1024 / 1024
                ));
            }
        }
        if total > MAX_EMBED_TOTAL {
            warn(&format!(
                "public/ の合計が {} MiB あります。バイナリがその分大きくなります",
                total / 1024 / 1024
            ));
        }
    }

    /// `public/` を下までたどる。`prefix` は配信するときの鍵の頭（`/` 区切り）。
    fn collect_public(&mut self, dir: &Path, prefix: &str) {
        let Some(entries) = self.read_dir_sorted(dir) else {
            return;
        };
        for path in entries {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                self.errors
                    .push(format!("{} の名前が UTF-8 ではありません", path.display()));
                continue;
            };
            // 隠しファイルは配信しない。
            if name.starts_with('.') {
                continue;
            }
            // 鍵の区切りは常に `/`。Windows で作っても Linux と同じ鍵になる。
            let key = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            if path.is_dir() {
                self.collect_public(&path, &key);
            } else {
                println!("cargo:rerun-if-changed={}", path.display());
                self.public.push((key, path));
            }
        }
    }

    /// 埋め込んだ `public/` を Rust の定数として書き出す。
    ///
    /// ```ignore
    /// static __BENGARA_PUBLIC: &[(&str, &[u8])] = &[("robots.txt", include_bytes!("..."))];
    /// ```
    fn public_code(&self) -> String {
        let mut list = String::new();
        for (key, path) in &self.public {
            let _ = write!(
                list,
                "({:?}, include_bytes!({:?})),",
                key,
                path.to_string_lossy()
            );
        }
        format!(
            "#[doc(hidden)]\n\
             static __BENGARA_PUBLIC: &[(&str, &[u8])] = &[{list}];\n"
        )
    }

    /// `resources/lang/*.toml` を読んで、文字の表を作る。
    ///
    /// **ビルド時に読み込みます。** 実行時にファイルを読まないので、本番に配る必要はありません。
    fn read_lang(&mut self, root: &Path) {
        let dir = root.join("resources/lang");
        println!("cargo:rerun-if-changed={}", dir.display());
        if !dir.is_dir() {
            return;
        }
        let Some(entries) = self.read_dir_sorted(&dir) else {
            return;
        };

        for path in entries {
            if path.extension().is_none_or(|e| e != "toml") {
                continue;
            }
            // 中身が変わったら作り直す。
            println!("cargo:rerun-if-changed={}", path.display());
            let Some(locale) = file_stem(&path) else {
                continue;
            };
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(e) => {
                    self.errors
                        .push(format!("{} を読めませんでした: {e}", path.display()));
                    continue;
                }
            };
            match toml::parse(&text) {
                Ok(pairs) => self.lang.push((locale, pairs)),
                Err(reason) => self.errors.push(format!("{} の {reason}", path.display())),
            }
        }
    }

    /// 1つのディレクトリを `pub mod ... { ... }` として書き出す。再帰する。
    fn emit_dir(&mut self, dir: &Path, dir_name: &str, target: &Target, depth: usize) {
        let module = match self.module_name(dir_name, dir) {
            Some(m) => m,
            // 名前にできないディレクトリは、エラーを記録して中身を見ない。
            None => return,
        };
        let _ = writeln!(self.tree, "#[allow(unused_imports)] pub mod {module} {{");
        self.stack.push(module);

        let entries = match self.read_dir_sorted(dir) {
            Some(entries) => entries,
            None => {
                self.stack.pop();
                self.tree.push_str("}\n");
                return;
            }
        };

        // 同じディレクトリの中でモジュール名がぶつかっていないかを調べる。
        // 例: `User.rs` と `user.rs` はどちらも `user` になる。
        let mut taken: BTreeMap<String, String> = BTreeMap::new();
        // `database/migrations/` と `database/seeders/` の中身。
        // どちらも (ファイル名, 平らなモジュール名) で覚える。
        let in_database = target.name == "database" && depth == 1;
        let in_migrations = in_database && dir_name.eq_ignore_ascii_case("migrations");
        let in_seeders = in_database && dir_name.eq_ignore_ascii_case("seeders");
        let in_factories = in_database && dir_name.eq_ignore_ascii_case("factories");
        // `app/Jobs/` と `app/Console/Commands/` も、関数を置く場所として扱う。
        let in_jobs = target.name == "app" && depth == 1 && dir_name.eq_ignore_ascii_case("jobs");
        let in_listeners =
            target.name == "app" && depth == 1 && dir_name.eq_ignore_ascii_case("listeners");
        let in_commands =
            target.name == "app" && depth == 2 && dir_name.eq_ignore_ascii_case("commands");
        let function_dir = in_seeders || in_factories || in_jobs || in_commands || in_listeners;
        let mut migrations: Vec<(String, String)> = Vec::new();
        let mut seeders: Vec<(String, String)> = Vec::new();
        let mut jobs: Vec<(String, String)> = Vec::new();
        let mut commands: Vec<(String, String)> = Vec::new();

        for path in entries {
            let Some(stem) = file_stem(&path) else {
                continue;
            };

            if path.is_dir() {
                if skip_dir(&stem) {
                    continue;
                }
                if let Some(module) = self.module_name(&stem, &path) {
                    self.check_duplicate(&mut taken, &module, &stem, dir);
                }
                self.emit_dir(&path, &stem, target, depth + 1);
                continue;
            }

            if !is_rust_file(&path) || skip_file(&stem) {
                continue;
            }
            let Some(module) = self.module_name(&stem, &path) else {
                continue;
            };
            self.check_duplicate(&mut taken, &module, &stem, dir);

            // 平らな名前は、パスをつないで作る。テストの出力などで読めるようにするため。
            let flat = self.flat_name(&module);
            let cfg = if target.test_only {
                "#[cfg(test)] "
            } else {
                ""
            };
            let _ = writeln!(
                self.flat,
                "{cfg}#[path = {:?}] #[doc(hidden)] #[allow(dead_code)] pub mod {flat};",
                path.display().to_string()
            );
            let _ = writeln!(self.tree, "pub use crate::{flat} as {module};");

            // PSR-4 と同じ約束。先頭が大文字のファイルは、同じ名前の型を公開する。
            // ただし seeders と factories は「関数を置く場所」なので、型は探さない。
            if target.reexport && starts_upper(&stem) && !function_dir {
                let _ = writeln!(self.tree, "pub use crate::{flat}::{stem};");
            }
            // database/migrations/ の、日付で始まるファイルはマイグレーション。
            if in_migrations && starts_digit(&stem) {
                migrations.push((stem.clone(), flat.clone()));
            }
            // database/seeders/ の、大文字で始まるファイルはシーダー。
            if in_seeders && starts_upper(&stem) {
                seeders.push((stem.clone(), flat.clone()));
            }
            // app/Jobs/ の、大文字で始まるファイルはジョブ。
            if in_jobs && starts_upper(&stem) {
                jobs.push((stem.clone(), flat.clone()));
            }
            // app/Console/Commands/ の、大文字で始まるファイルは自作コマンド。
            if in_commands && starts_upper(&stem) {
                commands.push((stem.clone(), flat.clone()));
            }
            // routes/console.rs があれば、定期処理の登録を呼ぶ。
            if target.name == "routes" && depth == 0 && stem == "console" {
                self.has_console = true;
            }
            // config/ の直下にある小文字のファイルは、設定として自動登録する。
            if target.name == "config" && depth == 0 && !starts_upper(&stem) {
                self.configs.push(module.clone());
            }
        }

        // マイグレーションの一覧。名前順に並ぶ（読み込みの時点で並べてある）。
        if in_migrations {
            let list = migrations
                .iter()
                .map(|(stem, flat)| {
                    format!(
                        "::bengara::Migration {{ name: {stem:?}, \
                         up: crate::{flat}::up, down: crate::{flat}::down }}"
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                self.tree,
                "pub const MIGRATIONS: &[::bengara::Migration] = &[{list}];"
            );
            self.has_migrations = true;
        }
        // シーダーの一覧。
        if in_seeders {
            let list = seeders
                .iter()
                .map(|(stem, flat)| {
                    format!(
                        "::bengara::Seeder {{ name: {stem:?}, \
                         run: || ::std::boxed::Box::pin(crate::{flat}::run()) }}"
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                self.tree,
                "pub const SEEDERS: &[::bengara::Seeder] = &[{list}];"
            );
            self.has_seeders = true;
        }
        // ジョブの一覧。
        if in_jobs {
            let list = jobs
                .iter()
                .map(|(stem, flat)| {
                    format!(
                        "::bengara::Job {{ name: {stem:?}, \
                         handle: |__payload| ::std::boxed::Box::pin(\
                         crate::{flat}::handle(__payload)) }}"
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(self.tree, "pub const JOBS: &[::bengara::Job] = &[{list}];");
            self.has_jobs = true;
        }
        // 自作コマンドの一覧。ファイル名を小文字とハイフンにした名前で呼べる。
        if in_commands {
            let list = commands
                .iter()
                .map(|(stem, flat)| {
                    let name = to_command_name(stem);
                    format!(
                        "::bengara::Command {{ name: {name:?}, \
                         description: crate::{flat}::DESCRIPTION, \
                         handle: |__args| ::std::boxed::Box::pin(async move {{ \
                         crate::{flat}::handle(&__args).await }}) }}"
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                self.tree,
                "pub const COMMANDS: &[::bengara::Command] = &[{list}];"
            );
            self.has_commands = true;
        }
        self.stack.pop();
        self.tree.push_str("}\n");
    }

    /// クレート直下に置くときの名前。`tests/Feature/HomeTest.rs` なら
    /// `__bengara_tests_feature_home_test` になります。
    fn flat_name(&mut self, module: &str) -> String {
        let mut name = String::from("__bengara");
        for segment in &self.stack {
            name.push('_');
            name.push_str(segment);
        }
        name.push('_');
        name.push_str(module);

        // 万一ぶつかったら連番を足す。
        let count = self.taken_flat.entry(name.clone()).or_insert(0);
        *count += 1;
        if *count == 1 {
            name
        } else {
            format!("{name}_{count}")
        }
    }

    /// ディレクトリの中身を名前順に並べて返す。読めなければエラーを記録する。
    fn read_dir_sorted(&mut self, dir: &Path) -> Option<Vec<PathBuf>> {
        let read = match fs::read_dir(dir) {
            Ok(read) => read,
            Err(e) => {
                self.errors
                    .push(format!("{} を読めませんでした: {e}", dir.display()));
                return None;
            }
        };
        let mut entries = Vec::new();
        for entry in read {
            match entry {
                Ok(entry) => entries.push(entry.path()),
                Err(e) => {
                    self.errors
                        .push(format!("{} の一覧取得に失敗しました: {e}", dir.display()));
                    return None;
                }
            }
        }
        entries.sort();
        Some(entries)
    }

    /// ファイル名・ディレクトリ名から Rust のモジュール名を作る。
    /// 作れない場合はエラーを記録して `None` を返す。
    fn module_name(&mut self, stem: &str, path: &Path) -> Option<String> {
        match to_module_name(stem) {
            Ok(name) => Some(name),
            Err(reason) => {
                self.errors
                    .push(format!("{} は取り込めません（{reason}）", path.display()));
                None
            }
        }
    }

    fn check_duplicate(
        &mut self,
        taken: &mut BTreeMap<String, String>,
        module: &str,
        stem: &str,
        dir: &Path,
    ) {
        if let Some(first) = taken.get(module) {
            self.errors.push(format!(
                "{} の中で名前がぶつかっています。`{first}` と `{stem}` はどちらもモジュール `{module}` になります",
                dir.display()
            ));
        } else {
            taken.insert(module.to_string(), stem.to_string());
        }
    }

    /// 言語の表を Rust の定数として書き出す。
    ///
    /// ```ignore
    /// static __BENGARA_LANG_JA: &[(&str, &str)] = &[("messages.welcome", "ようこそ")];
    /// static __BENGARA_LANG: ::bengara::LangTable = &[("ja", __BENGARA_LANG_JA)];
    /// ```
    fn lang_code(&self) -> String {
        let mut code = String::new();
        let mut locales = Vec::new();

        for (index, (locale, pairs)) in self.lang.iter().enumerate() {
            let name = format!("__BENGARA_LANG_{index}");
            let entries = pairs
                .iter()
                .map(|(key, value)| format!("({key:?}, {value:?})"))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                code,
                "#[doc(hidden)] static {name}: &[(&str, &str)] = &[{entries}];"
            );
            locales.push(format!("({locale:?}, {name})"));
        }
        let _ = writeln!(
            code,
            "#[doc(hidden)] static __BENGARA_LANG: ::bengara::LangTable = &[{}];",
            locales.join(", ")
        );
        code
    }

    fn finish(mut self) -> String {
        let mut code = String::from("// @generated by bengara-build. 手で編集しないでください。\n");
        code.push_str(&self.flat);
        code.push_str(&self.tree);

        // 設定の自動登録。config/ が無いときは空の本体になる。
        let mut body = String::new();
        for module in &self.configs {
            let _ = writeln!(
                body,
                "        __registry.set(crate::config::{module}::config());"
            );
        }
        // マイグレーションとシーダーの一覧。無ければ空の一覧を渡す。
        let migrations = if self.has_migrations {
            "|| crate::database::migrations::MIGRATIONS"
        } else {
            "|| &[]"
        };
        let seeders = if self.has_seeders {
            "|| crate::database::seeders::SEEDERS"
        } else {
            "|| &[]"
        };
        // ジョブ・自作コマンド・定期処理。無ければ何もしない形にする。
        let jobs = if self.has_jobs {
            "|| crate::app::jobs::JOBS"
        } else {
            "|| &[]"
        };
        let commands = if self.has_commands {
            "|| crate::app::console::commands::COMMANDS"
        } else {
            "|| &[]"
        };
        let schedule = if self.has_console {
            "|__schedule| crate::routes::console::schedule(__schedule)"
        } else {
            "|_| {}"
        };
        // 言語の表と、埋め込んだ public/。どちらもビルド時に読んだ中身をそのまま入れる。
        let lang = self.lang_code();
        let public = self.public_code();

        let _ = write!(
            code,
            "{lang}{public}\
             #[doc(hidden)]\n\
             pub fn __bengara_hooks() -> ::bengara::Hooks {{\n\
             \x20   ::bengara::Hooks {{\n\
             \x20       configs: |__registry| {{\n\
             {body}\
             \x20       }},\n\
             \x20       migrations: {migrations},\n\
             \x20       seeders: {seeders},\n\
             \x20       jobs: {jobs},\n\
             \x20       commands: {commands},\n\
             \x20       lang: || __BENGARA_LANG,\n\
             \x20       public: || __BENGARA_PUBLIC,\n\
             \x20       schedule: {schedule},\n\
             \x20   }}\n\
             }}\n"
        );

        // 問題はビルドを止めずに、生成コードの中で知らせる。
        self.errors.sort();
        self.errors.dedup();
        for (i, message) in self.errors.iter().enumerate() {
            let _ = writeln!(
                code,
                "const _: () = {{ compile_error!({:?}); }}; // {i}",
                format!("bengara-build: {message}")
            );
        }
        code
    }
}

/// `HomeController` → `home_controller`、`Http` → `http`、
/// `2026_09_29_create_users_table` → `m2026_09_29_create_users_table`。
fn to_module_name(stem: &str) -> Result<String, &'static str> {
    if stem.is_empty() {
        return Err("名前が空です");
    }
    let mut out = String::with_capacity(stem.len() + 2);
    for (i, c) in stem.chars().enumerate() {
        match c {
            'A'..='Z' => {
                if i > 0 && !out.ends_with('_') {
                    out.push('_');
                }
                out.push(c.to_ascii_lowercase());
            }
            'a'..='z' | '0'..='9' | '_' => out.push(c),
            '-' | ' ' | '.' => out.push('_'),
            _ => return Err("ファイル名に使えるのは英数字・`_`・`-` だけです"),
        }
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 'm');
    }
    if out.chars().all(|c| c == '_') {
        return Err("モジュール名が `_` だけになります");
    }
    if is_keyword(&out) {
        return Err("Rust の予約語と同じ名前になります");
    }
    Ok(out)
}

fn file_stem(path: &Path) -> Option<String> {
    Some(path.file_stem()?.to_str()?.to_string())
}

fn is_rust_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "rs")
}

/// 取り込まないファイル。隠しファイルと、Rust の慣習で意味を持つ名前。
fn skip_file(stem: &str) -> bool {
    stem.starts_with('.') || stem == "mod" || stem == "main" || stem == "lib"
}

/// 取り込まないディレクトリ。
fn skip_dir(name: &str) -> bool {
    name.starts_with('.') || name == "target"
}

fn starts_upper(stem: &str) -> bool {
    stem.starts_with(|c: char| c.is_ascii_uppercase())
}

/// ファイル名をコマンド名にする。`SendReport` → `send-report`。
fn to_command_name(stem: &str) -> String {
    let mut out = String::with_capacity(stem.len() + 2);
    for (i, c) in stem.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 && !out.ends_with('-') {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else if c == '_' {
            out.push('-');
        } else {
            out.push(c);
        }
    }
    out
}

fn starts_digit(stem: &str) -> bool {
    stem.starts_with(|c: char| c.is_ascii_digit())
}

/// Rust の予約語（将来の予約語を含む）。
fn is_keyword(name: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "as", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false",
        "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
        "ref", "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe",
        "use", "where", "while", "async", "await", "gen", "abstract", "become", "box", "do",
        "final", "macro", "override", "priv", "try", "typeof", "unsized", "virtual", "yield",
    ];
    KEYWORDS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 大文字始まりはスネークケースになる() {
        assert_eq!(to_module_name("HomeController").unwrap(), "home_controller");
        assert_eq!(to_module_name("Http").unwrap(), "http");
        assert_eq!(to_module_name("EnsureAdmin").unwrap(), "ensure_admin");
    }

    #[test]
    fn 小文字はそのまま() {
        assert_eq!(to_module_name("web").unwrap(), "web");
        assert_eq!(to_module_name("app").unwrap(), "app");
    }

    #[test]
    fn 日付で始まる名前にはmが付く() {
        assert_eq!(
            to_module_name("2026_09_29_000000_create_users_table").unwrap(),
            "m2026_09_29_000000_create_users_table"
        );
    }

    #[test]
    fn ハイフンはアンダースコアになる() {
        assert_eq!(to_module_name("my-file").unwrap(), "my_file");
    }

    #[test]
    fn 連続する大文字は1文字ずつ区切る() {
        assert_eq!(to_module_name("API").unwrap(), "a_p_i");
    }

    #[test]
    fn 予約語と使えない文字は断る() {
        assert!(to_module_name("type").is_err());
        assert!(to_module_name("日本語").is_err());
        assert!(to_module_name("").is_err());
        assert!(to_module_name("_").is_err());
    }
}
