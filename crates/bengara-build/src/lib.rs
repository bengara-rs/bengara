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
        // マイグレーション（日付で始まるファイル）の一覧。
        let mut migrations: Vec<String> = Vec::new();

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
            if target.reexport && starts_upper(&stem) {
                let _ = writeln!(self.tree, "pub use crate::{flat}::{stem};");
            }
            if starts_digit(&stem) {
                migrations.push(stem.clone());
            }
            // config/ の直下にある小文字のファイルは、設定として自動登録する。
            if target.name == "config" && depth == 0 && !starts_upper(&stem) {
                self.configs.push(module.clone());
            }
        }

        if !migrations.is_empty() {
            let list = migrations
                .iter()
                .map(|m| format!("{m:?}"))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(self.tree, "pub const MIGRATIONS: &[&str] = &[{list}];");
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
        let _ = write!(
            code,
            "#[doc(hidden)]\n\
             pub fn __bengara_hooks() -> ::bengara::Hooks {{\n\
             \x20   ::bengara::Hooks {{\n\
             \x20       configs: |__registry| {{\n\
             {body}\
             \x20       }},\n\
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
    fn 連続する大文字で余計なアンダースコアを入れない() {
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
