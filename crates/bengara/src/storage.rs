//! ファイルの置き場所。Laravel の `Storage` に当たります。
//!
//! ```ignore
//! Storage::put("notes/a.txt", "本文").await?;
//! let body = Storage::get("notes/a.txt").await?;
//! Storage::disk("public").put("avatars/1.png", bytes).await?;
//! ```
//!
//! | 名前 | 実際の場所 |
//! |---|---|
//! | `local`（既定） | `storage/app/` |
//! | `public` | `storage/app/public/` |

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::error::{Error, Result};

/// 既定の置き場所の名前。
const DEFAULT_DISK: &str = "local";

/// 置き場所の設定。`config/storage.rs` が返します。
///
/// ```ignore
/// pub fn config() -> StorageConfig {
///     StorageConfig {
///         default: env("STORAGE_DISK", "local"),
///         disks: vec![
///             DiskConfig::new("local", "app"),
///             DiskConfig::new("public", "app/public"),
///         ],
///     }
/// }
/// ```
#[derive(Debug, Clone)]
pub struct StorageConfig {
    /// 既定で使う置き場所の名前。
    pub default: String,
    /// 置き場所の一覧。
    pub disks: Vec<DiskConfig>,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            default: crate::env("STORAGE_DISK", DEFAULT_DISK),
            disks: vec![
                DiskConfig::new("local", "app"),
                DiskConfig::new("public", "app/public"),
            ],
        }
    }
}

/// 置き場所1つぶんの設定。
#[derive(Debug, Clone)]
pub struct DiskConfig {
    /// 名前（`Storage::disk("public")` で指す名前）。
    pub name: String,
    /// `storage/` からの相対パス。
    pub root: String,
}

impl DiskConfig {
    /// 名前と場所を決めて作る。
    pub fn new(name: impl Into<String>, root: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            root: root.into(),
        }
    }
}

/// いまの設定。
///
/// `try_config` は `&'static` を返すので**借りて回します**。クローンすると
/// `Storage::get` 1 回につき `StorageConfig` のコピーが何度も走ります。
fn config() -> &'static StorageConfig {
    static FALLBACK: OnceLock<StorageConfig> = OnceLock::new();
    crate::try_config::<StorageConfig>()
        .unwrap_or_else(|| FALLBACK.get_or_init(StorageConfig::default))
}

/// ファイルの読み書き。`Storage::disk("名前")` で置き場所を選べます。
pub struct Storage {
    root: PathBuf,
}

impl Storage {
    /// 既定の置き場所。
    ///
    /// 既定の置き場所は**1回だけ組み立てて覚えます**。`Storage::put` のような
    /// 静的メソッドは毎回ここを通るので、設定の走査とパスの組み立てを
    /// 呼ぶ回数ぶん繰り返さないようにするためです。
    /// 設定は起動時に決まって変わらない前提です。
    pub fn default_disk() -> Result<Self> {
        // エラーも覚える。`Error` はクローンできないので文字列で持ちます。
        static ROOT: OnceLock<std::result::Result<PathBuf, String>> = OnceLock::new();
        match ROOT.get_or_init(|| {
            Self::named(&config().default)
                .map(|disk| disk.root)
                .map_err(|e| e.to_string())
        }) {
            Ok(root) => Ok(Self { root: root.clone() }),
            Err(message) => Err(Error::msg(message.clone())),
        }
    }

    /// 名前で置き場所を選ぶ。
    ///
    /// # パニック
    ///
    /// 知らない名前のときはパニックします。設定の書き間違いに
    /// すぐ気づけるようにするためです。読み書きの前に分かります。
    pub fn disk(name: &str) -> Self {
        Self::named(name).unwrap_or_else(|e| panic!("{e}"))
    }

    fn named(name: &str) -> Result<Self> {
        let config = config();
        let disk = config
            .disks
            .iter()
            .find(|d| d.name == name)
            .ok_or_else(|| {
                Error::msg(format!(
                    "`{name}` という置き場所は設定にありません（ある置き場所: {}）",
                    config
                        .disks
                        .iter()
                        .map(|d| d.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
        Ok(Self {
            root: crate::paths::storage_path(&disk.root),
        })
    }

    /// `public` の置き場所そのもの（`/storage/...` の配信に使う）。
    ///
    /// 設定に `public` が無ければ `storage/app/public` を使います。
    /// 起動時に1回だけ呼び、以後は読むだけにします。
    pub(crate) fn public_root() -> PathBuf {
        Self::named("public")
            .map(|disk| disk.root)
            .unwrap_or_else(|_| crate::paths::storage_path("app/public"))
    }

    /// 文字列を書く。途中のディレクトリは作ります。
    pub async fn put(path: &str, contents: impl Into<String>) -> Result<()> {
        Self::default_disk()?
            .write(path, contents.into().into_bytes())
            .await
    }

    /// バイト列を書く。
    pub async fn put_bytes(path: &str, contents: Vec<u8>) -> Result<()> {
        Self::default_disk()?.write(path, contents).await
    }

    /// 文字列として読む。無ければ `None`。
    pub async fn get(path: &str) -> Result<Option<String>> {
        Self::default_disk()?.read(path).await
    }

    /// バイト列として読む。
    pub async fn get_bytes(path: &str) -> Result<Option<Vec<u8>>> {
        Self::default_disk()?.read_bytes(path).await
    }

    /// あるか。
    pub async fn exists(path: &str) -> Result<bool> {
        Self::default_disk()?.has(path).await
    }

    /// 無いか。
    pub async fn missing(path: &str) -> Result<bool> {
        Ok(!Self::exists(path).await?)
    }

    /// 消す。
    pub async fn delete(path: &str) -> Result<()> {
        Self::default_disk()?.remove(path).await
    }

    // ---- 置き場所を指定したとき ----

    /// 文字列を書く。
    pub async fn write(&self, path: &str, contents: Vec<u8>) -> Result<()> {
        let full = self.absolute(path)?;
        crate::support::blocking(move || {
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&full, contents).map_err(Error::Io)
        })
        .await?
    }

    /// 文字列として読む。
    pub async fn read(&self, path: &str) -> Result<Option<String>> {
        let Some(bytes) = self.read_bytes(path).await? else {
            return Ok(None);
        };
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| Error::msg(format!("`{path}` は文字列として読めません")))
    }

    /// バイト列として読む。
    pub async fn read_bytes(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let full = self.absolute(path)?;
        crate::support::blocking(move || match std::fs::read(&full) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::Io(e)),
        })
        .await?
    }

    /// あるか。
    ///
    /// `is_file()` 1 回だけなので、裏のスレッド（`spawn_blocking`）を経由しません。
    /// 往復のほうが調べるより高く付きます。
    pub async fn has(&self, path: &str) -> Result<bool> {
        Ok(self.absolute(path)?.is_file())
    }

    /// 消す。無くてもエラーにはしません。
    pub async fn remove(&self, path: &str) -> Result<()> {
        let full = self.absolute(path)?;
        crate::support::blocking(move || match std::fs::remove_file(&full) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Io(e)),
        })
        .await?
    }

    /// バイト数。無ければ `None`。
    ///
    /// `metadata()` 1 回だけなので、`has` と同じく裏のスレッドを経由しません。
    pub async fn size(&self, path: &str) -> Result<Option<u64>> {
        let full = self.absolute(path)?;
        match std::fs::metadata(&full) {
            Ok(meta) => Ok(Some(meta.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::Io(e)),
        }
    }

    /// その下にあるファイルの一覧（置き場所からの相対パス）。
    pub async fn files(&self, prefix: &str) -> Result<Vec<String>> {
        let base = self.root.clone();
        let full = self.absolute(prefix)?;
        crate::support::blocking(move || {
            let mut out = Vec::new();
            collect_files(&full, &base, &mut out)?;
            out.sort();
            Ok(out)
        })
        .await?
    }

    /// 実際の絶対パス。
    pub fn path(&self, path: &str) -> Result<PathBuf> {
        self.absolute(path)
    }

    /// 置き場所の中の絶対パスにする。
    ///
    /// 次の 2 つを**断ります**。
    ///
    /// - `..` と絶対パス（`:` を含むもの）。置き場所の外を触らせないため。
    /// - 装置の名前（`nul` など）と、末尾が空白・`.` の名前。[`usable_name`] を見てください。
    fn absolute(&self, path: &str) -> Result<PathBuf> {
        let trimmed = path.trim_start_matches(['/', '\\']);
        if trimmed.is_empty() {
            return Ok(self.root.clone());
        }
        for part in trimmed.split(['/', '\\']) {
            if part.is_empty() || part == "." {
                continue;
            }
            if part == ".." || part.contains(':') {
                return Err(Error::msg(format!("`{path}` は置き場所の外を指しています")));
            }
            if let Err(reason) = usable_name(part) {
                return Err(Error::msg(format!(
                    "`{path}` は使えない名前です（`{part}` が{reason}）"
                )));
            }
        }
        Ok(self.root.join(trimmed.replace('\\', "/")))
    }
}

/// Windows が**どのディレクトリの下でも**装置として解決してしまう名前。
///
/// `storage/app/nul` へ書くと、`std::fs::write` は成功を返すのに中身は
/// 0 バイトになります（画面に出さずに捨てられます）。`con` は読み戻しで止まります。
const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// パスの 1 要素として使える名前か。使えないときは理由を返します。
///
/// **`#[cfg(windows)]` で分けません。** OS で挙動を変えると、Linux で通った
/// コードが Windows で黙ってデータを捨てます。全 OS で同じ名前を断ります。
fn usable_name(part: &str) -> std::result::Result<(), &'static str> {
    // 装置の名前は拡張子を付けても装置のまま（`nul.txt` も `nul`）。
    // 最初の `.` より前だけを見ます。
    let stem = part.split('.').next().unwrap_or(part);
    if RESERVED_NAMES
        .iter()
        .any(|name| stem.eq_ignore_ascii_case(name))
    {
        return Err("装置の名前です");
    }
    // Windows は末尾の空白と `.` を落とすので、`a.txt ` と `a.txt` が
    // 同じファイルになります。別の鍵のつもりが同じ中身を指します。
    if part.ends_with(' ') || part.ends_with('.') {
        return Err("末尾が空白か `.` です");
    }
    Ok(())
}

/// ディレクトリの中のファイルを集める（再帰）。
fn collect_files(dir: &Path, base: &Path, out: &mut Vec<String>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(Error::Io(e)),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, base, out)?;
            continue;
        }
        if let Ok(relative) = path.strip_prefix(base) {
            out.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テストごとに別のディレクトリを使う（並行して走るため）。
    fn disk(name: &str) -> Storage {
        Storage {
            root: std::env::temp_dir()
                .join(format!("bengara-storage-{}-{name}", std::process::id())),
        }
    }

    #[test]
    fn 既定の設定は2つの置き場所を持つ() {
        let config = StorageConfig::default();
        assert_eq!(config.default, "local");
        assert_eq!(config.disks.len(), 2);
        assert_eq!(config.disks[0].root, "app");
        assert_eq!(config.disks[1].root, "app/public");
    }

    #[test]
    fn 置き場所の外は指せない() {
        let disk = disk("paths");
        for bad in [
            "../secret",
            "a/../../b",
            "C:/windows/system32",
            "a/..",
            "..",
        ] {
            assert!(disk.absolute(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn 装置の名前は断る() {
        let disk = disk("device");
        // 実機で確かめたもの。`nul` に書くと成功を返すのに 0 バイトになる。
        for bad in [
            "nul",
            "NUL",
            "nul.txt",
            "notes/nul",
            "notes/con/a.txt",
            "aux",
            "prn",
            "com1",
            "COM9.log",
            "lpt1",
            "Nul.tar.gz",
        ] {
            let error = disk.absolute(bad).unwrap_err().to_string();
            assert!(error.contains("使えない名前"), "{bad}: {error}");
        }

        // 装置の名前を含むだけの名前は通す。
        for ok in ["null", "nula", "console", "com10", "com", "lpt0", "a.nul"] {
            assert!(disk.absolute(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn 末尾の空白とドットは断る() {
        let disk = disk("trailing");
        // Windows は末尾を落とすので、`a.txt ` と `a.txt` が同じファイルになる。
        for bad in ["a.txt ", "a.txt.", "notes /a.txt", "notes./a.txt", "a..."] {
            let error = disk.absolute(bad).unwrap_err().to_string();
            assert!(error.contains("使えない名前"), "{bad}: {error}");
        }
        // 先頭や途中の空白・`.` は通す。
        assert!(disk.absolute("a b.txt").is_ok());
        assert!(disk.absolute(" a.txt").is_ok());
    }

    #[test]
    fn 置き場所の外と使えない名前はエラー文で分かれる() {
        let disk = disk("reason");
        assert!(disk
            .absolute("../secret")
            .unwrap_err()
            .to_string()
            .contains("置き場所の外"));
        assert!(disk
            .absolute("nul")
            .unwrap_err()
            .to_string()
            .contains("使えない名前"));
    }

    #[test]
    fn 普通のパスは置き場所の下になる() {
        let disk = disk("paths2");
        let full = disk.absolute("notes/a.txt").unwrap();
        assert!(full.starts_with(&disk.root));
        assert!(full.ends_with("notes/a.txt"));

        // 先頭の / は落とす。
        assert_eq!(disk.absolute("/notes/a.txt").unwrap(), full);
        // 空のパスは置き場所そのもの。
        assert_eq!(disk.absolute("").unwrap(), disk.root);
    }

    #[tokio::test]
    async fn 書いて読んで消せる() {
        let disk = disk("rw");
        let _ = std::fs::remove_dir_all(&disk.root);

        disk.write("notes/a.txt", "本文".as_bytes().to_vec())
            .await
            .unwrap();
        assert_eq!(
            disk.read("notes/a.txt").await.unwrap().as_deref(),
            Some("本文")
        );
        assert!(disk.has("notes/a.txt").await.unwrap());
        assert_eq!(disk.size("notes/a.txt").await.unwrap(), Some(6));

        assert_eq!(disk.read("none.txt").await.unwrap(), None);
        assert!(!disk.has("none.txt").await.unwrap());
        assert_eq!(disk.size("none.txt").await.unwrap(), None);

        disk.write("notes/b.txt", b"2".to_vec()).await.unwrap();
        assert_eq!(
            disk.files("notes").await.unwrap(),
            vec!["notes/a.txt".to_string(), "notes/b.txt".to_string()]
        );

        disk.remove("notes/a.txt").await.unwrap();
        assert!(!disk.has("notes/a.txt").await.unwrap());
        // 無いものを消してもエラーにしない。
        disk.remove("notes/a.txt").await.unwrap();

        let _ = std::fs::remove_dir_all(&disk.root);
    }

    #[tokio::test]
    async fn バイト列も扱える() {
        let disk = disk("bytes");
        let _ = std::fs::remove_dir_all(&disk.root);

        disk.write("bin/a.dat", vec![0, 1, 2, 255]).await.unwrap();
        assert_eq!(
            disk.read_bytes("bin/a.dat").await.unwrap(),
            Some(vec![0, 1, 2, 255])
        );
        // 文字列として読もうとすると分かるエラーになる。
        assert!(disk.read("bin/a.dat").await.is_err());

        let _ = std::fs::remove_dir_all(&disk.root);
    }

    #[tokio::test]
    async fn 無いディレクトリの一覧は空() {
        let disk = disk("empty");
        let _ = std::fs::remove_dir_all(&disk.root);
        assert!(disk.files("none").await.unwrap().is_empty());
    }
}
