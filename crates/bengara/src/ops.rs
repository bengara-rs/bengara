//! 本番で動かすための小道具。
//!
//! - `storage:init` … 書き込み先のディレクトリを作る
//! - 起動時に、使う絶対パスをログに出す
//!
//! **起動時にディレクトリを自動で作りません**（決定記録 #023）。
//! 設定を間違えたプロセスが、空の `storage/` を作って動き出すのを防ぐためです。

use std::path::{Path, PathBuf};

use crate::error::Result;

/// `storage:init` が作るディレクトリ。`storage/` からの相対パスです。
///
/// 親から順に並べます。`create_dir_all` を使うので順番に意味はありませんが、
/// 一覧に出たときに読みやすいようにしてあります。
pub(crate) const STORAGE_DIRECTORIES: &[&str] = &[
    "app",
    "app/public",
    "framework",
    "framework/cache",
    "framework/sessions",
    "logs",
];

/// 書き込み先のディレクトリを作る。
///
/// すでにあるものは触りません。何度実行しても同じ結果になります。
pub(crate) fn storage_init() -> Result<()> {
    let root = crate::paths::storage_root().to_path_buf();
    println!("storage: {}", root.display());

    let mut created = Vec::new();
    let mut kept = Vec::new();

    // `storage/` 自身も作る。
    for relative in std::iter::once("").chain(STORAGE_DIRECTORIES.iter().copied()) {
        let path = if relative.is_empty() {
            root.clone()
        } else {
            join(&root, relative)
        };
        if path.is_dir() {
            kept.push(relative.to_string());
            continue;
        }
        std::fs::create_dir_all(&path)?;
        restrict(&path)?;
        created.push(relative.to_string());
    }

    report("作成", &created);
    report("そのまま", &kept);
    if created.is_empty() {
        println!("すべて揃っています。");
    }
    Ok(())
}

/// `a/b` の形の相対パスを、OS の区切りでつなぐ。
fn join(root: &Path, relative: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in relative.split('/') {
        path.push(part);
    }
    path
}

/// 所有者だけが読み書きできるようにする（Unix のみ）。
///
/// Windows では何もしません。既定の継承に任せます。
#[cfg(unix)]
fn restrict(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict(_path: &Path) -> Result<()> {
    Ok(())
}

fn report(label: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    println!("\n{label}（{}件）", items.len());
    for item in items {
        if item.is_empty() {
            println!("  storage/");
        } else {
            println!("  storage/{item}");
        }
    }
}

/// 起動時に、使う絶対パスを出す。
///
/// 設定の間違いは、たいていここを見れば分かります。
pub(crate) fn log_paths() {
    tracing::info!(
        "基準ディレクトリ {}（{}）",
        crate::paths::base_path().display(),
        crate::paths::base_source().label()
    );
    let storage = crate::paths::storage_root();
    tracing::info!("storage {}", storage.display());
    if !storage.is_dir() {
        tracing::warn!(
            "{} がありません。`cargo artisan storage:init` を実行してください",
            storage.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 相対パスをつなぐ() {
        let root = Path::new("/tmp/storage");
        assert_eq!(join(root, "app"), root.join("app"));
        assert_eq!(join(root, "framework/cache"), root.join("framework/cache"));
    }

    #[test]
    fn 作るディレクトリに重なりがない() {
        let mut sorted = STORAGE_DIRECTORIES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), STORAGE_DIRECTORIES.len());
    }

    #[test]
    fn 作るディレクトリは相対パス() {
        for dir in STORAGE_DIRECTORIES {
            assert!(!dir.starts_with('/'), "{dir}");
            assert!(!dir.contains(".."), "{dir}");
            assert!(!dir.is_empty());
        }
    }

    #[test]
    fn 置き場所と同じ名前を使っている() {
        // storage.rs / cache / session が使う場所と揃っていること。
        assert!(STORAGE_DIRECTORIES.contains(&"app"));
        assert!(STORAGE_DIRECTORIES.contains(&"app/public"));
        assert!(STORAGE_DIRECTORIES.contains(&"framework/cache"));
        assert!(STORAGE_DIRECTORIES.contains(&"framework/sessions"));
        assert!(STORAGE_DIRECTORIES.contains(&"logs"));
    }
}
