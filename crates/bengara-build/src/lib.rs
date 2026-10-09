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
/// - `reexport`: 先頭が大文字のファイルから、同じ名前の型を再エクスポートする。
///   ただし関数を置く場所（`seeders`・`factories`・`jobs`・`listeners`・`commands`）は除く
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

/// マイグレーションの置き場所（**モジュール名**で書きます）。
const MIGRATIONS_DIR: &[&str] = &["database", "migrations"];
/// シーダーの置き場所。
const SEEDERS_DIR: &[&str] = &["database", "seeders"];
/// ファクトリの置き場所。
const FACTORIES_DIR: &[&str] = &["database", "factories"];
/// ジョブの置き場所。
const JOBS_DIR: &[&str] = &["app", "jobs"];
/// 聞く側（リスナ）の置き場所。
const LISTENERS_DIR: &[&str] = &["app", "listeners"];
/// 自作コマンドの置き場所。
const COMMANDS_DIR: &[&str] = &["app", "console", "commands"];

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

    let mut generator = Generator::default();
    generator.run(&root);

    let code = generator.finish();
    write_if_changed(&out_dir.join(OUT_FILE), &code);
}

/// 中身が変わったときだけ書く。
///
/// 無条件に書くと更新時刻だけが進みます。`main.rs` の `include!` を通じて
/// アプリのクレートが丸ごと再コンパイルされるので、1バイトも変わっていないなら触りません。
fn write_if_changed(out_path: &Path, code: &str) {
    if fs::read_to_string(out_path).is_ok_and(|old| old == code) {
        return;
    }
    if let Err(e) = fs::write(out_path, code) {
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

/// バイト数を MiB の文字にする。小数1桁。
///
/// 切り捨てで出すと、8.9 MiB のファイルを「8 MiB あります」と言ってしまうためです。
fn mib(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))
}

/// ディレクトリの中の1件。`DirEntry` から取れる分を先に持っておきます。
///
/// 後から `is_dir()` を呼び直すと、そのたびに OS に聞きに行きます。
///
/// ファイルの大きさは持ちません。使うのは `public/` を埋め込むときだけなので、
/// ここで取ると `app/` や `tests/` のすべてのファイルで無駄に OS へ聞きに行きます。
struct Entry {
    path: PathBuf,
    is_dir: bool,
}

/// 一覧に入れる1件（マイグレーション・シーダー・ジョブ・コマンド）。
struct Item {
    /// 一覧に出る名前。並べ替えと名前の重なりの判定もこれで行います。
    ///
    /// マイグレーション・シーダー・ジョブはファイル名そのまま、コマンドは
    /// `cargo artisan` で打つ名前（`SendReport` → `send-report`）です。
    name: String,
    /// クレート直下に置いたときのモジュール名。
    flat: String,
    /// 元のファイル。名前がぶつかったときに知らせるために持ちます。
    path: PathBuf,
}

/// 一覧の定数を書き出したか。`finish()` が参照するかどうかの判断に使います。
#[derive(Default)]
struct Emitted {
    migrations: bool,
    seeders: bool,
    jobs: bool,
    commands: bool,
}

#[derive(Default)]
struct Generator {
    /// クレート直下に平らに並べる `#[path]` 付きのモジュール宣言。
    flat: String,
    /// 再エクスポートだけで組み立てる名前空間。
    tree: String,
    /// 今たどっているディレクトリのモジュール名（平らな名前を作るのに使う）。
    stack: Vec<String>,
    /// すでに使った平らな名前と、最初にその名前になったファイル。
    taken_flat: BTreeMap<String, PathBuf>,
    /// `config/` 直下で見つかった設定モジュール（登録順）。
    configs: Vec<String>,
    /// `routes/console.rs` が見つかったか。
    has_console: bool,
    /// `cargo:rerun-if-changed` に出したディレクトリ（テストで確かめるために持ちます）。
    watched: Vec<PathBuf>,
    /// `database/migrations/` の下で集めたマイグレーション（入れ子も含む）。
    migrations: Vec<Item>,
    /// `database/seeders/` の下で集めたシーダー。
    seeders: Vec<Item>,
    /// `app/Jobs/` の下で集めたジョブ。
    jobs: Vec<Item>,
    /// `app/Console/Commands/` の下で集めた自作コマンド。
    commands: Vec<Item>,
    /// 一覧の定数を書いたか。
    emitted: Emitted,
    /// `resources/lang/` から読んだ文字（言語ごと）。
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
            if !self.watch(&dir) {
                continue;
            }
            let Some(module) = self.module_name(target.name, &dir) else {
                continue;
            };
            if target.test_only {
                self.tree.push_str("#[cfg(test)]\n");
            }
            self.emit_dir(&dir, module, target, 0);
        }
        self.read_lang(root);
        self.read_public(root);
    }

    /// `dir` の変化を見張る。無いときは見張らず、警告だけ出す。
    ///
    /// 無いディレクトリを `cargo:rerun-if-changed` に出すと、cargo は
    /// 「常に古い」と見なしてビルドスクリプトを毎回走らせます。
    /// かといって代わりにプロジェクトのルートを見張るのも駄目です。cargo は
    /// ディレクトリを渡されると**中を再帰的にたどって**いちばん新しい更新時刻を取るので、
    /// ルートの下の `target/` を毎回見ることになります。`target/` はビルドごとに
    /// 書き換わるため、避けたかったこと（毎回走る）がそのまま起きます。
    ///
    /// そこで、無いディレクトリは見張らずに1行知らせます。後から作ったときは
    /// `touch build.rs` で作り直してください。
    ///
    /// 戻り値は「そのディレクトリがあったか」です。
    fn watch(&mut self, dir: &Path) -> bool {
        if dir.is_dir() {
            self.watch_existing(dir);
            return true;
        }
        warn(&format!(
            "{} が見つかりませんでした。作った後は `touch build.rs` で作り直してください",
            dir.display()
        ));
        false
    }

    /// あると分かっているディレクトリを見張る。
    ///
    /// ディレクトリ自体の変化（ファイルの追加・削除）を見ます。取り込んだ `.rs` の
    /// 中身の変化は、`#[path]` で取り込んだソースとして rustc 側が追跡します。
    fn watch_existing(&mut self, dir: &Path) {
        self.watched.push(dir.to_path_buf());
        println!("cargo:rerun-if-changed={}", dir.display());
    }

    /// `public/` の中身を、バイナリに埋め込む一覧にする。
    ///
    /// **リリースビルドのときだけ**集めます。デバッグビルドではディスクから読むので、
    /// ファイルを直したらすぐ反映されます。
    fn read_public(&mut self, root: &Path) {
        let dir = root.join("public");
        // 見張りはデバッグビルドでも出す。release に切り替えたときに集め直せるようにする。
        if !self.watch(&dir) {
            return;
        }
        // PROFILE は cargo がビルドスクリプトに渡す（"debug" か "release"）。
        if env::var("PROFILE").as_deref() != Ok("release") {
            return;
        }

        // 大きくなりすぎたら気づけるようにする。止めはしない。
        let mut total: u64 = 0;
        self.collect_public(&dir, "", &mut total);
        if total > MAX_EMBED_TOTAL {
            warn(&format!(
                "public/ の合計が {} MiB あります。バイナリがその分大きくなります",
                mib(total)
            ));
        }
    }

    /// `public/` を下までたどる。`prefix` は配信するときの鍵の頭（`/` 区切り）。
    ///
    /// ファイルごとの `cargo:rerun-if-changed` は出しません。中身は生成コードの
    /// `include_bytes!` で取り込むので、rustc 側が追跡します（`public/` の指定とも重複します）。
    fn collect_public(&mut self, dir: &Path, prefix: &str, total: &mut u64) {
        let Some(entries) = self.read_dir_sorted(dir) else {
            return;
        };
        for entry in entries {
            let Some(name) = entry.path.file_name().and_then(|n| n.to_str()) else {
                self.errors.push(format!(
                    "{} の名前が UTF-8 ではありません",
                    entry.path.display()
                ));
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
            if entry.is_dir {
                self.watch_existing(&entry.path);
                self.collect_public(&entry.path, &key, total);
                continue;
            }
            // 大きさを OS に聞くのはここだけ。埋め込む `public/` のためだけに使います。
            let len = entry.path.metadata().map_or(0, |m| m.len());
            *total += len;
            if len > MAX_EMBED_FILE {
                warn(&format!(
                    "public/{key} は {} MiB あります。バイナリに埋め込みます",
                    mib(len)
                ));
            }
            self.public.push((key, entry.path));
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

    /// `resources/lang/` を読んで、文字の表を作る。
    ///
    /// 読む形は2つです。どちらも使えます。
    ///
    /// | 置き方 | `[x] y = ".."` の鍵 |
    /// |---|---|
    /// | `resources/lang/ja.toml` | `x.y` |
    /// | `resources/lang/ja/messages.toml` | `messages.x.y` |
    ///
    /// 下が Laravel 本来の置き方です。ファイル名が鍵の頭に付きます。
    /// 同じ言語で両方を置いてもよく、1つの表にまとまります。
    ///
    /// **ビルド時に読み込みます。** 実行時にファイルを読まないので、本番に配る必要はありません。
    fn read_lang(&mut self, root: &Path) {
        let dir = root.join("resources/lang");
        if !self.watch(&dir) {
            return;
        }
        let Some(entries) = self.read_dir_sorted(&dir) else {
            return;
        };

        // 言語ごとにまとめる。3つめの要素は、鍵が重なったときに知らせるための元のファイル。
        let mut table: BTreeMap<String, Vec<(String, String, String)>> = BTreeMap::new();

        for entry in entries {
            let Some(stem) = file_stem(&entry.path) else {
                // 言語の名前は生成コードの文字になるので、UTF-8 でないと使えません。
                if is_toml(&entry.path) {
                    self.errors.push(format!(
                        "{} の名前が UTF-8 ではありません",
                        entry.path.display()
                    ));
                }
                continue;
            };
            if stem.starts_with('.') {
                continue;
            }
            let locale = stem.to_string();
            if entry.is_dir {
                self.read_lang_dir(&entry.path, &locale, &mut table);
                continue;
            }
            if !is_toml(&entry.path) {
                continue;
            }
            let origin = entry.path.display().to_string();
            let pairs = self.read_toml(&entry.path);
            let into = table.entry(locale).or_default();
            for (key, value) in pairs {
                into.push((key, value, origin.clone()));
            }
        }

        for (locale, mut pairs) in table {
            // 鍵の昇順（`str` の辞書順）に並べる。実行時はこの並びを前提に引きます。
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            // 同じ鍵が2回あったら教える。黙って先に書いた方が勝つと、直し方が
            // 分からないためです。`ja.toml` と `ja/` を混ぜたときにも起こります。
            for pair in pairs.windows(2) {
                if pair[0].0 == pair[1].0 {
                    self.errors.push(format!(
                        "`{locale}` に鍵 `{}` が2回書かれています: {} と {}",
                        pair[0].0, pair[0].2, pair[1].2
                    ));
                }
            }
            let pairs = pairs.into_iter().map(|(k, v, _)| (k, v)).collect();
            self.lang.push((locale, pairs));
        }
    }

    /// `resources/lang/<言語>/*.toml` を読む。鍵の頭にファイル名が付きます。
    fn read_lang_dir(
        &mut self,
        dir: &Path,
        locale: &str,
        table: &mut BTreeMap<String, Vec<(String, String, String)>>,
    ) {
        self.watch_existing(dir);
        let Some(entries) = self.read_dir_sorted(dir) else {
            return;
        };
        for entry in entries {
            if entry.is_dir {
                // ここより下は読みません（Laravel も1階層だけ）。黙って無視しないで知らせます。
                warn(&format!(
                    "{} は読みません。言語ファイルは resources/lang/{locale}/ の直下に置いてください",
                    entry.path.display()
                ));
                continue;
            }
            if !is_toml(&entry.path) {
                continue;
            }
            let Some(group) = file_stem(&entry.path) else {
                // ファイル名が鍵の頭になるので、UTF-8 でないと使えません。
                self.errors.push(format!(
                    "{} の名前が UTF-8 ではありません",
                    entry.path.display()
                ));
                continue;
            };
            if group.starts_with('.') {
                continue;
            }
            let group = group.to_string();
            let origin = entry.path.display().to_string();
            let pairs = self.read_toml(&entry.path);
            let into = table.entry(locale.to_string()).or_default();
            for (key, value) in pairs {
                into.push((format!("{group}.{key}"), value, origin.clone()));
            }
        }
    }

    /// 1つの TOML を読む。読めないときはエラーを記録して空を返す。
    fn read_toml(&mut self, path: &Path) -> Vec<(String, String)> {
        // 中身が変わったら作り直す。ビルド時に読むので rustc は追跡しません。
        println!("cargo:rerun-if-changed={}", path.display());
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                self.errors
                    .push(format!("{} を読めませんでした: {e}", path.display()));
                return Vec::new();
            }
        };
        match toml::parse(&text) {
            Ok(pairs) => pairs,
            Err(reason) => {
                self.errors.push(format!("{} の {reason}", path.display()));
                Vec::new()
            }
        }
    }

    /// 1つのディレクトリを `pub mod ... { ... }` として書き出す。再帰する。
    ///
    /// `module` は呼ぶ側が作ります。ここで作り直すと同じ変換が二度走り、
    /// 名前にできないディレクトリでは同じエラーを2回積むことになります。
    fn emit_dir(&mut self, dir: &Path, module: String, target: &Target, depth: usize) {
        let _ = writeln!(self.tree, "#[allow(unused_imports)] pub mod {module} {{");
        self.stack.push(module);
        // 入れ子のディレクトリも見張る。親だけ見張っても、下にファイルを足したときに
        // 親の更新時刻は動かないので気づけません（いちばん上は `run` が見張っています）。
        if depth > 0 {
            self.watch_existing(dir);
        }

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

        // 置き場所は**モジュール名の並び**で確かめます。`finish()` が
        // `crate::app::jobs::JOBS` のような決まったパスを参照するので、
        // 生のディレクトリ名で判定すると、`app/JOBS/`（モジュール名は `j_o_b_s`）
        // のときに解決できない生成コードになります。
        //
        // 「置き場所の**下にあるか**」で見ます。`app/Jobs/Mail/SendWelcome.rs` の
        // ように入れ子にしても一覧に入ります。集めたものは親に持ち上げて1つの定数にします。
        let in_migrations = under(&self.stack, MIGRATIONS_DIR);
        let in_seeders = under(&self.stack, SEEDERS_DIR);
        let in_factories = under(&self.stack, FACTORIES_DIR);
        let in_jobs = under(&self.stack, JOBS_DIR);
        let in_listeners = under(&self.stack, LISTENERS_DIR);
        let in_commands = under(&self.stack, COMMANDS_DIR);
        let function_dir = in_seeders || in_factories || in_jobs || in_commands || in_listeners;

        for entry in entries {
            let path = entry.path;
            let Some(stem) = file_stem(&path) else {
                // 名前が UTF-8 でないと `#[path]` に書けません。`collect_public` と
                // 同じ文言で知らせます。黙って飛ばすと、置いた人には
                // 「型が見つからない」としか見えないためです。
                if is_rust_file(&path) {
                    self.errors
                        .push(format!("{} の名前が UTF-8 ではありません", path.display()));
                }
                continue;
            };

            if entry.is_dir {
                if skip_dir(stem) {
                    continue;
                }
                let stem = stem.to_string();
                let Some(module) = self.module_name(&stem, &path) else {
                    // 名前にできないディレクトリは、エラーを記録して中身を見ない。
                    continue;
                };
                self.check_duplicate(&mut taken, &module, &stem, dir);
                self.emit_dir(&path, module, target, depth + 1);
                continue;
            }

            if !is_rust_file(&path) {
                continue;
            }
            if skip_file(stem) {
                // `mod.rs` は Rust の癖で意味を持つ名前なので取り込みません。
                // 黙って無視すると、置いた人には何も伝わらないので知らせます。
                if stem == "mod" {
                    warn(&format!(
                        "{} は取り込みません。bengara はファイル名をそのままモジュール名にするので、\
                         mod.rs は要りません",
                        path.display()
                    ));
                }
                continue;
            }
            let Some(module) = self.module_name(stem, &path) else {
                continue;
            };
            self.check_duplicate(&mut taken, &module, stem, dir);

            // 平らな名前は、パスをつないで作る。テストの出力などで読めるようにするため。
            let flat = self.flat_name(&module, &path);
            let cfg = if target.test_only {
                "#[cfg(test)] "
            } else {
                ""
            };
            // 平らな名前はエラーの文やバックトレースに出ます。元のファイルを
            // すぐ引けるように、宣言の直前に元のパスをコメントで添えます。
            let _ = writeln!(self.flat, "// {}", path.display());
            let _ = writeln!(
                self.flat,
                "{cfg}#[path = {:?}] #[doc(hidden)] #[allow(dead_code)] pub mod {flat};",
                path.display().to_string()
            );
            let _ = writeln!(self.tree, "pub use crate::{flat} as {module};");

            // PSR-4 と同じ約束。先頭が大文字のファイルは、同じ名前の型を公開する。
            // ただし seeders と factories は「関数を置く場所」なので、型は探さない。
            if target.reexport && starts_upper(stem) && !function_dir {
                // ファイル名をそのまま型の名前として書くので、識別子に使えるか確かめる。
                // `My-Model.rs` はモジュール名にはできるが、型の名前にはできない。
                if is_ident(stem) {
                    let _ = writeln!(self.tree, "pub use crate::{flat}::{stem};");
                } else {
                    self.errors.push(format!(
                        "{} の名前は Rust の型の名前に使えません。`-` や空白を使わず、\
                         英数字と `_` だけにしてください",
                        path.display()
                    ));
                }
            }
            // database/migrations/ の、日付で始まるファイルはマイグレーション。
            if in_migrations {
                if starts_digit(stem) {
                    self.migrations.push(Item {
                        name: stem.to_string(),
                        flat: flat.clone(),
                        path: path.clone(),
                    });
                } else {
                    warn(&format!(
                        "{} は日付で始まらないので、マイグレーションの一覧に入りません",
                        path.display()
                    ));
                }
            }
            // database/seeders/ の、大文字で始まるファイルはシーダー。
            if in_seeders {
                collect_upper(&path, stem, "シーダー", &mut self.seeders, &flat);
            }
            // app/Jobs/ の、大文字で始まるファイルはジョブ。
            if in_jobs {
                collect_upper(&path, stem, "ジョブ", &mut self.jobs, &flat);
            }
            // app/Console/Commands/ の、大文字で始まるファイルは自作コマンド。
            //
            // 一覧に入れる名前は**変換後のコマンド名**です。並べ替えと重なりの判定も
            // その名前で行うので、ここで作ってから入れます。ファイル名で判定すると、
            // `SendReport.rs` と `Sub/Send_Report.rs` がどちらも `send-report` になるのに
            // 何も知らせず、後のほうは呼べないままになります。
            if in_commands {
                if !starts_upper(stem) {
                    warn(&format!(
                        "{} は大文字で始まらないので、コマンドの一覧に入りません",
                        path.display()
                    ));
                } else {
                    match to_command_name(stem) {
                        Ok(name) => self.commands.push(Item {
                            name,
                            flat: flat.clone(),
                            path: path.clone(),
                        }),
                        Err(reason) => self.errors.push(format!(
                            "{} はコマンドの名前にできません（{reason}）",
                            path.display()
                        )),
                    }
                }
            }
            // routes/console.rs があれば、定期処理の登録を呼ぶ。
            if target.name == "routes" && depth == 0 && stem == "console" {
                self.has_console = true;
            }
            // config/ の直下にある小文字のファイルは、設定として自動登録する。
            if target.name == "config" && depth == 0 && !starts_upper(stem) {
                self.configs.push(module.clone());
            }
        }

        // 一覧の定数は、置き場所そのもの（`database/migrations` など）に1つだけ書く。
        // 入れ子で集めたものは、ここまで持ち上がってきています。
        self.emit_lists();

        self.stack.pop();
        self.tree.push_str("}\n");
    }

    /// 集めた一覧を、置き場所そのもののディレクトリで定数にする。
    fn emit_lists(&mut self) {
        if is_exactly(&self.stack, MIGRATIONS_DIR) {
            let items = std::mem::take(&mut self.migrations);
            let list = self.join_items(items, "マイグレーション", |item| {
                let (name, flat) = (&item.name, &item.flat);
                format!(
                    "::bengara::Migration {{ name: {name:?}, \
                     up: crate::{flat}::up, down: crate::{flat}::down }}"
                )
            });
            let _ = writeln!(
                self.tree,
                "pub const MIGRATIONS: &[::bengara::Migration] = &[{list}];"
            );
            self.emitted.migrations = true;
        }
        if is_exactly(&self.stack, SEEDERS_DIR) {
            let items = std::mem::take(&mut self.seeders);
            let list = self.join_items(items, "シーダー", |item| {
                let (name, flat) = (&item.name, &item.flat);
                format!(
                    "::bengara::Seeder {{ name: {name:?}, \
                     run: || ::std::boxed::Box::pin(crate::{flat}::run()) }}"
                )
            });
            let _ = writeln!(
                self.tree,
                "pub const SEEDERS: &[::bengara::Seeder] = &[{list}];"
            );
            self.emitted.seeders = true;
        }
        if is_exactly(&self.stack, JOBS_DIR) {
            let items = std::mem::take(&mut self.jobs);
            let list = self.join_items(items, "ジョブ", |item| {
                let (name, flat) = (&item.name, &item.flat);
                format!(
                    "::bengara::Job {{ name: {name:?}, \
                     handle: |__payload| ::std::boxed::Box::pin(\
                     crate::{flat}::handle(__payload)) }}"
                )
            });
            let _ = writeln!(self.tree, "pub const JOBS: &[::bengara::Job] = &[{list}];");
            self.emitted.jobs = true;
        }
        if is_exactly(&self.stack, COMMANDS_DIR) {
            let items = std::mem::take(&mut self.commands);
            // 名前はファイル名を小文字とハイフンにしたもの。集めたときに作ってあります。
            let list = self.join_items(items, "コマンド", |item| {
                let (name, flat) = (&item.name, &item.flat);
                format!(
                    "::bengara::Command {{ name: {name:?}, \
                     description: crate::{flat}::DESCRIPTION, \
                     handle: |__args| ::std::boxed::Box::pin(async move {{ \
                     crate::{flat}::handle(&__args).await }}) }}"
                )
            });
            let _ = writeln!(
                self.tree,
                "pub const COMMANDS: &[::bengara::Command] = &[{list}];"
            );
            self.emitted.commands = true;
        }
    }

    /// 一覧を名前順に並べ、名前の重なりを調べてから、生成コードの並びにする。
    ///
    /// 入れ子のディレクトリに置いても1つの一覧にまとめるので、名前が全体で
    /// 重なっていないかをここで確かめます。
    fn join_items(
        &mut self,
        mut items: Vec<Item>,
        kind: &str,
        render: impl Fn(&Item) -> String,
    ) -> String {
        items.sort_by(|a, b| a.name.cmp(&b.name));
        for pair in items.windows(2) {
            if pair[0].name == pair[1].name {
                self.errors.push(format!(
                    "{kind}の名前 `{}` が2つあります: {} と {}。\
                     入れ子のディレクトリに置いても1つの一覧にまとまるので、\
                     名前は全体で重ならないようにしてください",
                    pair[0].name,
                    pair[0].path.display(),
                    pair[1].path.display()
                ));
            }
        }
        items.iter().map(render).collect::<Vec<_>>().join(", ")
    }

    /// クレート直下に置くときの名前。`tests/Feature/HomeTest.rs` なら
    /// `__bengara_tests_feature_home_test` になります。
    fn flat_name(&mut self, module: &str, path: &Path) -> String {
        let mut name = String::from("__bengara");
        for segment in &self.stack {
            name.push('_');
            name.push_str(segment);
        }
        name.push('_');
        name.push_str(module);

        if !self.taken_flat.contains_key(&name) {
            self.taken_flat.insert(name.clone(), path.to_path_buf());
            return name;
        }

        // 万一ぶつかったら、空いている名前が見つかるまで番号を上げる。
        // 作った名前も登録する。登録しないと、素でその名前になるファイルと
        // 二重定義になり、利用者が書いていない名前でエラーが出ます。
        let mut number = 2;
        let mut unique = format!("{name}_{number}");
        while self.taken_flat.contains_key(&unique) {
            number += 1;
            unique = format!("{name}_{number}");
        }
        // 黙って変えると、エラーの文に出た名前からどのファイルか分からなくなるので、
        // 元の2つのファイルを添えて1行知らせる。
        let first = self.taken_flat[&name].display().to_string();
        warn(&format!(
            "平らなモジュール名 `{name}` が重なったので `{unique}` にしました。\
             つなげると同じ名前になるファイルがあります: {first} と {}",
            path.display()
        ));
        self.taken_flat.insert(unique.clone(), path.to_path_buf());
        unique
    }

    /// ディレクトリの中身を名前順に並べて返す。読めなければエラーを記録する。
    fn read_dir_sorted(&mut self, dir: &Path) -> Option<Vec<Entry>> {
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
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    self.errors
                        .push(format!("{} の一覧取得に失敗しました: {e}", dir.display()));
                    return None;
                }
            };
            // 種類は `DirEntry` から取る。あとで `is_dir()` を呼び直すと、
            // そのたびに OS に聞きに行きます。
            let file_type = entry.file_type().ok();
            let is_dir = match file_type {
                Some(kind) => kind.is_dir(),
                None => entry.path().is_dir(),
            };
            // `file_type()` はリンクを辿りません。ディレクトリへのリンク
            // （Windows のジャンクションも同じ）は `is_dir` が偽になり、
            // `.rs` でもないので黙って飛ばされます。辿らないのは循環で落ちないという
            // 良い面があるのでそのままにして、気づけるように1行知らせます。
            if file_type.is_some_and(|kind| kind.is_symlink()) {
                warn(&format!(
                    "{} はリンクなのでたどりません。中のファイルは取り込まれません",
                    entry.path().display()
                ));
            }
            entries.push(Entry {
                path: entry.path(),
                is_dir,
            });
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
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
    ///
    /// **並びの約束**：鍵の表は鍵の昇順、言語の表は言語名の昇順です。
    /// 実行時の引き当て（`lang.rs`）がこの並びを前提にします。
    /// 鍵の並べ替えは `read_lang` で、言語の並べ替えはここで行います。
    fn lang_code(&self) -> String {
        let mut code = String::new();
        let mut locales = Vec::new();

        let mut sorted: Vec<&(String, Vec<(String, String)>)> = self.lang.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));

        for (index, (locale, pairs)) in sorted.into_iter().enumerate() {
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
        let migrations = if self.emitted.migrations {
            "|| crate::database::migrations::MIGRATIONS"
        } else {
            "|| &[]"
        };
        let seeders = if self.emitted.seeders {
            "|| crate::database::seeders::SEEDERS"
        } else {
            "|| &[]"
        };
        // ジョブ・自作コマンド・定期処理。無ければ何もしない形にする。
        let jobs = if self.emitted.jobs {
            "|| crate::app::jobs::JOBS"
        } else {
            "|| &[]"
        };
        let commands = if self.emitted.commands {
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
        // `const _` は名前がぶつからないので、何個でもそのまま並べられる。
        self.errors.sort();
        self.errors.dedup();
        for message in &self.errors {
            let _ = writeln!(
                code,
                "const _: () = {{ compile_error!({:?}); }};",
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

fn file_stem(path: &Path) -> Option<&str> {
    path.file_stem()?.to_str()
}

fn is_rust_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "rs")
}

fn is_toml(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "toml")
}

/// 取り込まないファイル。隠しファイルと、Rust の慣習で意味を持つ名前。
fn skip_file(stem: &str) -> bool {
    stem.starts_with('.') || stem == "mod" || stem == "main" || stem == "lib"
}

/// 取り込まないディレクトリ。
fn skip_dir(name: &str) -> bool {
    name.starts_with('.') || name == "target"
}

/// 大文字で始まるファイルだけを一覧に入れる。外れたものは警告で知らせる。
///
/// 黙って外すと `cargo artisan migrate` などが「何もしない」だけになり、
/// 理由がどこにも出ないためです（取り込み自体はできているので、ビルドは止めません）。
fn collect_upper(path: &Path, stem: &str, kind: &str, list: &mut Vec<Item>, flat: &str) {
    if starts_upper(stem) {
        list.push(Item {
            name: stem.to_string(),
            flat: flat.to_string(),
            path: path.to_path_buf(),
        });
    } else {
        warn(&format!(
            "{} は大文字で始まらないので、{kind}の一覧に入りません",
            path.display()
        ));
    }
}

/// いまたどっているモジュール名の並びが、置き場所の**下にあるか**。
///
/// `["app", "jobs", "mail"]` は `["app", "jobs"]` の下にあります。
/// 入れ子にしたジョブなども一覧に入れるために、これで判定します。
fn under(stack: &[String], names: &[&str]) -> bool {
    stack.len() >= names.len() && stack.iter().zip(names).all(|(a, b)| a == b)
}

/// いまたどっているモジュール名の並びが、置き場所**そのもの**か。
fn is_exactly(stack: &[String], names: &[&str]) -> bool {
    stack.len() == names.len() && under(stack, names)
}

/// Rust の識別子として使える名前か（型名の再エクスポートに使う）。
fn is_ident(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_') && !is_keyword(name)
}

fn starts_upper(stem: &str) -> bool {
    stem.starts_with(|c: char| c.is_ascii_uppercase())
}

/// ファイル名をコマンド名にする。`SendReport` → `send-report`。
///
/// 英数字と `_` `-` 以外の文字が入っていたら断ります。`My Report.rs` を通すと
/// `my -report` という名前になり、`cargo artisan` の引数は空白で切れているので
/// 打ちようがありません。`list` には出るのに呼べない、という形になります。
fn to_command_name(stem: &str) -> Result<String, &'static str> {
    let mut out = String::with_capacity(stem.len() + 2);
    for (i, c) in stem.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 && !out.ends_with('-') {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else if c == '_' {
            out.push('-');
        } else if c.is_ascii_alphanumeric() || c == '-' {
            out.push(c);
        } else {
            return Err("コマンドのファイル名に使えるのは英数字・`_`・`-` だけです");
        }
    }
    if out.is_empty() {
        return Err("コマンドの名前が空になります");
    }
    Ok(out)
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

    /// テスト用の作業場所。テストごとに別の名前にする。
    fn temp_root(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!(
            "bengara_build_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn touch(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn stack_of(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

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
    fn 置き場所はモジュール名の並びで見る() {
        // 置き場所そのもの。
        assert!(is_exactly(
            &stack_of(&["app", "console", "commands"]),
            COMMANDS_DIR
        ));
        // 下にあるものも一覧に入れる。
        assert!(under(&stack_of(&["app", "jobs", "mail"]), JOBS_DIR));
        assert!(!is_exactly(&stack_of(&["app", "jobs", "mail"]), JOBS_DIR));
        // 別の場所は入らない。
        assert!(!under(&stack_of(&["app", "foo", "commands"]), COMMANDS_DIR));
        assert!(!under(&stack_of(&["tests", "migrations"]), MIGRATIONS_DIR));
        assert!(!under(&stack_of(&["database"]), MIGRATIONS_DIR));
    }

    #[test]
    fn 大文字だけのディレクトリ名は置き場所にならない() {
        // `app/JOBS/` のモジュール名は `j_o_b_s`。生のディレクトリ名で判定すると
        // ここが真になり、`crate::app::jobs::JOBS` を参照する壊れたコードができる。
        assert_eq!(to_module_name("JOBS").unwrap(), "j_o_b_s");
        assert!(!under(&stack_of(&["app", "j_o_b_s"]), JOBS_DIR));
        // `app/Jobs/` は `jobs` になるので一致する。
        assert_eq!(to_module_name("Jobs").unwrap(), "jobs");
        assert!(under(&stack_of(&["app", "jobs"]), JOBS_DIR));
    }

    #[test]
    fn 型の名前に使える名前だけを見分ける() {
        assert!(is_ident("HomeController"));
        assert!(is_ident("User2"));
        assert!(!is_ident("My-Model"));
        assert!(!is_ident("My Model"));
        assert!(!is_ident("2Model"));
        assert!(!is_ident("type"));
        assert!(!is_ident(""));
    }

    #[test]
    fn 言語の表は言語名の昇順になる() {
        let mut generator = Generator::default();
        generator.lang.push((
            "ja".to_string(),
            vec![("m.a".to_string(), "値".to_string())],
        ));
        generator.lang.push(("en".to_string(), Vec::new()));
        let code = generator.lang_code();
        let table = code.lines().last().unwrap().to_string();
        assert!(table.find("\"en\"").unwrap() < table.find("\"ja\"").unwrap());
    }

    #[test]
    fn 鍵は昇順に並び重複は知らせる() {
        let root = temp_root("lang");
        let dir = root.join("resources/lang");
        touch(&dir.join("ja.toml"), "[m]\nb = \"2\"\na = \"1\"\n");
        touch(&dir.join("en.toml"), "[m]\na = \"1\"\na = \"2\"\n");

        let mut generator = Generator::default();
        generator.read_lang(&root);

        let ja = generator
            .lang
            .iter()
            .find(|(locale, _)| locale == "ja")
            .unwrap();
        let keys: Vec<&str> = ja.1.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, ["m.a", "m.b"]);
        assert!(generator
            .errors
            .iter()
            .any(|e| e.contains("m.a") && e.contains("2回")));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 言語は入れ子のディレクトリも読む() {
        let root = temp_root("lang_dir");
        let dir = root.join("resources/lang");
        // Laravel 本来の置き方。鍵の頭にファイル名が付く。
        touch(&dir.join("ja/messages.toml"), "[x]\ny = \"値\"\n");
        touch(&dir.join("ja/validation.toml"), "required = \"必須です\"\n");
        // 直下の ja.toml もこれまでどおり読める。1つの表にまとまる。
        touch(&dir.join("ja.toml"), "[top]\nz = \"直下\"\n");

        let mut generator = Generator::default();
        generator.read_lang(&root);

        assert_eq!(generator.lang.len(), 1, "言語は ja だけ");
        let keys: Vec<&str> = generator.lang[0]
            .1
            .iter()
            .map(|(key, _)| key.as_str())
            .collect();
        assert_eq!(
            keys,
            ["messages.x.y", "top.z", "validation.required"],
            "鍵の昇順で並ぶ"
        );
        assert!(generator.errors.is_empty(), "{:?}", generator.errors);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 入れ子のシーダーとマイグレーションも一覧に入る() {
        let root = temp_root("nested");
        touch(&root.join("database/seeders/UserSeeder.rs"), "");
        touch(&root.join("database/seeders/Demo/AdminSeeder.rs"), "");
        touch(&root.join("database/migrations/2026_01_01_000000_a.rs"), "");
        touch(
            &root.join("database/migrations/2027/2027_01_01_000000_b.rs"),
            "",
        );

        let mut generator = Generator::default();
        generator.run(&root);
        let code = generator.finish();

        // 一覧は1つだけ。入れ子で集めたものが親に持ち上がっている。
        assert_eq!(code.matches("pub const SEEDERS").count(), 1);
        assert_eq!(code.matches("pub const MIGRATIONS").count(), 1);

        let seeders = code
            .lines()
            .find(|l| l.contains("pub const SEEDERS"))
            .unwrap();
        // 名前順に並ぶ。
        assert!(
            seeders.find("\"AdminSeeder\"").unwrap() < seeders.find("\"UserSeeder\"").unwrap(),
            "{seeders}"
        );
        assert!(seeders.contains("crate::__bengara_database_seeders_demo_admin_seeder::run()"));

        let migrations = code
            .lines()
            .find(|l| l.contains("pub const MIGRATIONS"))
            .unwrap();
        assert!(migrations.contains("\"2026_01_01_000000_a\""));
        assert!(migrations.contains("\"2027_01_01_000000_b\""));

        // 空の一覧ではなく、生成した定数を参照している。
        assert!(code.contains("seeders: || crate::database::seeders::SEEDERS"));
        assert!(code.contains("migrations: || crate::database::migrations::MIGRATIONS"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 入れ子で一覧の名前がぶつかったら知らせる() {
        let root = temp_root("dup_seeder");
        touch(&root.join("database/seeders/UserSeeder.rs"), "");
        touch(&root.join("database/seeders/Demo/UserSeeder.rs"), "");

        let mut generator = Generator::default();
        generator.run(&root);

        assert!(
            generator
                .errors
                .iter()
                .any(|e| e.contains("シーダー") && e.contains("UserSeeder")),
            "{:?}",
            generator.errors
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 平らな名前は連番も登録する() {
        let mut generator = Generator::default();
        generator.stack.push("app".to_string());

        let first = generator.flat_name("foo_bar", Path::new("app/foo_bar.rs"));
        assert_eq!(first, "__bengara_app_foo_bar");
        // つなげると同じ名前になる2つめは連番になる。
        let second = generator.flat_name("foo_bar", Path::new("app/FooBar.rs"));
        assert_eq!(second, "__bengara_app_foo_bar_2");
        // 連番も登録されているので、素で `foo_bar_2` になるファイルは別の名前になる。
        // 登録していないと、ここで `__bengara_app_foo_bar_2` と二重定義になる。
        let third = generator.flat_name("foo_bar_2", Path::new("app/foo_bar_2.rs"));
        assert_eq!(third, "__bengara_app_foo_bar_2_2");
        assert_eq!(generator.taken_flat.len(), 3);
    }

    #[test]
    fn 中身が同じなら書き直さない() {
        let root = temp_root("write");
        fs::create_dir_all(&root).unwrap();
        let path = root.join(OUT_FILE);

        write_if_changed(&path, "a");
        let first = fs::metadata(&path).unwrap().modified().unwrap();

        // 同じ中身なら触らない（更新時刻が進まない）。
        write_if_changed(&path, "a");
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), first);

        // 違う中身なら書く。
        write_if_changed(&path, "b");
        assert_eq!(fs::read_to_string(&path).unwrap(), "b");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 大きさは小数1桁で出す() {
        // 切り捨てると 8.9 MiB が「8 MiB」になってしまう。
        assert_eq!(mib(8 * 1024 * 1024 + 900 * 1024), "8.9");
        assert_eq!(mib(0), "0.0");
        assert_eq!(mib(1024 * 1024), "1.0");
    }

    #[test]
    fn 無い置き場所は見張らない() {
        let root = temp_root("missing");
        let mut generator = Generator::default();
        generator.run(&root);

        // 無いだけならエラーにしない。生成コードも壊れない。
        assert!(generator.errors.is_empty(), "{:?}", generator.errors);
        // 1件も `rerun-if-changed` を出さない。無いディレクトリを出すと毎回走り、
        // 代わりにルートを出しても、cargo が中の `target/` までたどるので毎回走る。
        assert!(generator.watched.is_empty(), "{:?}", generator.watched);
        let code = generator.finish();
        assert!(code.contains("migrations: || &[]"));
    }

    #[test]
    fn あるディレクトリだけを見張る() {
        let root = temp_root("watch_some");
        touch(&root.join("app/Models/User.rs"), "");

        let mut generator = Generator::default();
        generator.run(&root);

        // 置いた分は見張る。無いものとルートは出さない。
        assert!(generator.watched.contains(&root.join("app")));
        assert!(generator.watched.contains(&root.join("app/Models")));
        assert!(!generator.watched.contains(&root.to_path_buf()));
        assert!(!generator.watched.contains(&root.join("config")));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 入れ子で同じコマンド名になったら知らせる() {
        let root = temp_root("dup_command");
        // ファイル名は違うが、どちらもコマンド名は `send-report` になる。
        touch(&root.join("app/Console/Commands/SendReport.rs"), "");
        touch(&root.join("app/Console/Commands/Sub/Send_Report.rs"), "");

        let mut generator = Generator::default();
        generator.run(&root);

        assert!(
            generator
                .errors
                .iter()
                .any(|e| e.contains("コマンド") && e.contains("send-report")),
            "{:?}",
            generator.errors
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn コマンド名はファイル名から作る() {
        assert_eq!(to_command_name("SendReport").unwrap(), "send-report");
        assert_eq!(to_command_name("Send_Report").unwrap(), "send-report");
        assert_eq!(to_command_name("Cleanup").unwrap(), "cleanup");
        // 打ちようのない名前になる文字は断る。
        assert!(to_command_name("My Report").is_err());
        assert!(to_command_name("Send.Report").is_err());
        assert!(to_command_name("日本語").is_err());
    }

    #[test]
    fn 打てないコマンド名はコンパイルエラーにする() {
        let root = temp_root("bad_command");
        // `My Report` は `my -report` になり、`cargo artisan` では打てない。
        touch(&root.join("app/Console/Commands/My Report.rs"), "");

        let mut generator = Generator::default();
        generator.run(&root);

        assert!(
            generator
                .errors
                .iter()
                .any(|e| e.contains("コマンドの名前にできません")),
            "{:?}",
            generator.errors
        );
        let code = generator.finish();
        assert!(code.contains("compile_error!"));
        // 打てない名前は一覧にも入らない。
        assert!(!code.contains("my -report"), "{code}");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 名前にできないディレクトリのエラーは1件だけ() {
        let root = temp_root("bad_dir");
        touch(&root.join("app/type/Foo.rs"), "");

        let mut generator = Generator::default();
        generator.run(&root);

        let hits = generator
            .errors
            .iter()
            .filter(|e| e.contains("予約語"))
            .count();
        assert_eq!(hits, 1, "{:?}", generator.errors);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 予約語と使えない文字は断る() {
        assert!(to_module_name("type").is_err());
        assert!(to_module_name("日本語").is_err());
        assert!(to_module_name("").is_err());
        assert!(to_module_name("_").is_err());
    }
}
