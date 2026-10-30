//! ファイルの置き場所。Laravel の `Storage` に当たります。
//!
//! ```ignore
//! Storage::put("notes/a.txt", "本文").await?;
//! let body = Storage::get("notes/a.txt").await?;
//! Storage::disk("public").write("avatars/1.png", bytes).await?;
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
    ///
    /// **原子的ではありません。** 書いている途中でプロセスが落ちると、
    /// 切れたファイルが残ります（Laravel の `put` も同じです）。
    /// 途中の中身を読まれたくないときは、別の名前に書いてから自分で
    /// 置き換えてください。
    pub async fn write(&self, path: &str, contents: Vec<u8>) -> Result<()> {
        let full = self.file_path(path)?;
        self.ensure_storage(&full)?;
        crate::support::blocking(move || {
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&full, contents).map_err(Error::Io)
        })
        .await?
    }

    /// `storage/` そのものが無ければ、作らずに知らせる（決定記録 #023）。
    ///
    /// `create_dir_all` は祖先をすべて作るので、置き場所が `storage/` の下に
    /// あるときは `storage/` ごと作っていました。セッションやキャッシュは
    /// 「無ければ作らずエラー」なのに、`Storage` だけが設定を間違えたプロセスに
    /// 空の `storage/` を作らせていました。`storage/` の外にある置き場所
    /// （テスト用の一時ディレクトリなど）には口を出しません。
    fn ensure_storage(&self, full: &Path) -> Result<()> {
        let storage = crate::paths::storage_path("");
        if full.starts_with(&storage) && !storage.is_dir() {
            return Err(Error::msg(format!(
                "{} がありません。`cargo artisan storage:init` で作ってください",
                storage.display()
            )));
        }
        Ok(())
    }

    /// ファイル 1 つを指すパスにする。空と `.` は断る。
    ///
    /// [`absolute`](Self::absolute) は空のパスを置き場所そのものに写します
    /// （`files("")` が一覧の起点にするため）。ファイルの読み書きに渡すと
    /// ディレクトリを開く分かりにくいエラーになるので、ここで断ります。
    fn file_path(&self, path: &str) -> Result<PathBuf> {
        let full = self.absolute(path)?;
        if full == self.root {
            return Err(Error::msg("パスが空です。ファイルの名前を渡してください"));
        }
        Ok(full)
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
        let full = self.file_path(path)?;
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
        Ok(self.file_path(path)?.is_file())
    }

    /// 消す。無くてもエラーにはしません。
    pub async fn remove(&self, path: &str) -> Result<()> {
        let full = self.file_path(path)?;
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
        let full = self.file_path(path)?;
        match std::fs::metadata(&full) {
            Ok(meta) => Ok(Some(meta.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::Io(e)),
        }
    }

    /// その下にあるファイルの一覧（置き場所からの相対パス）。
    ///
    /// **深さは `prefix` から数えます**（`MAX_DEPTH` まで）。置き場所の
    /// 根からではありません。
    ///
    /// 名前が使えない（`..` など）ときだけエラーになります。読めないものは
    /// 飛ばして `tracing::warn!` で知らせます（決定記録 #167）。
    ///
    /// - `prefix` が無い・ディレクトリでない → 空の一覧
    /// - 権限などで読めないディレクトリ → その枝だけ飛ばす
    /// - 深さを超えた・UTF-8 でない名前 → そこだけ飛ばす
    pub async fn files(&self, prefix: &str) -> Result<Vec<String>> {
        let base = self.root.clone();
        let full = self.absolute(prefix)?;
        crate::support::blocking(move || {
            let mut out = Vec::new();
            collect_files(&full, &base, &mut out);
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
    /// - `..` と絶対パス（`/` か `\` で始まるもの、`:` を含むもの）。置き場所の外を触らせないため。
    /// - 装置の名前（`nul` など）、末尾が空白・`.` の名前、`~1` のような
    ///   Windows の短い名前の形。[`usable_name`] を見てください。
    ///
    /// 先頭の `/` は以前は黙って落として置き場所の中に写していました。
    /// 公開ドキュメントは「絶対パスはエラー」と書いていたので、書いてある
    /// 約束のほうにそろえます。`/etc/passwd` を渡した人は、置き場所の中の
    /// `etc/passwd` が読まれるより、断られるほうが間違いに気づけます。
    fn absolute(&self, path: &str) -> Result<PathBuf> {
        if path.starts_with(['/', '\\']) {
            return Err(Error::msg(format!(
                "`{path}` は置き場所の外を指しています（絶対パスは渡せません）"
            )));
        }
        let trimmed = path;
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
///
/// キャッシュの鍵（`cache/mod.rs` の `normalize_key`）からも使います。
/// 判定を 1 か所にまとめるためです。
pub(crate) fn usable_name(part: &str) -> std::result::Result<(), &'static str> {
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
    // Windows がファイル名に使えない記号。断っていなかったころ、
    // `Storage::put("notes/a?b.txt", ..)` は Linux で成功し、Windows では
    // `Err(InvalidInput)` になりました。この関数の方針が防ぎたかった形です。
    // `:` は入れません。キャッシュの鍵が `:` を使い、`Storage` 側は
    // `absolute` が別に断っています。
    if part.contains(['<', '>', '"', '|', '?', '*']) {
        return Err("Windows で使えない記号（`< > \" | ? *`）が入っています");
    }
    // 制御文字も同じです。Linux では名前に使えますが Windows では使えません。
    if part.chars().any(|c| c <= '\x1f') {
        return Err("制御文字が入っています");
    }
    // `~` の直後が数字の形も断ります。NTFS は長い名前に短い名前
    // （`<先頭6文字>~<数字>`）を自動で付けます。`storage/app/avatars/` に
    // `alice.profile.photo.jpg` があるとき、実機（Windows 10 / NTFS）では
    // `avatars/alicep~1.jpg` でそのファイルを読み・上書き・削除できました。
    // 置き場所の外には出られませんが、別の名前のつもりが同じ中身を指します。
    // バイトで見てかまいません。UTF-8 では ASCII 以外のバイトは必ず 0x80 以上で、
    // `~` と数字に化けることがありません。
    if part
        .as_bytes()
        .windows(2)
        .any(|pair| pair[0] == b'~' && pair[1].is_ascii_digit())
    {
        return Err("`~1` のような形は Windows の短い名前と区別できません");
    }
    Ok(())
}

/// どこまで深く潜るか。これより深いディレクトリは見ません。
///
/// 印（シンボリックリンク・ジャンクション）は飛ばすので輪にはなりませんが、
/// 深いだけのディレクトリでもスタックを使い切らないよう上限を置きます。
const MAX_DEPTH: usize = 32;

/// ディレクトリの中のファイルを集める（再帰）。
///
/// 深さは `dir`（`files` の `prefix`）を 0 として数えます。
fn collect_files(dir: &Path, base: &Path, out: &mut Vec<String>) {
    collect_files_at(dir, base, out, 0);
}

/// 深さを数えながら集める。
///
/// **印（シンボリックリンク・ジャンクション）はたどりません。**
/// `Path::is_dir()` は印の先を見るので、`storage/app/` の下に親を指す印が
/// 1 つあるだけで再帰が止まらず、`Err` にもならずプロセスが落ちます。
/// `storage:link` は作らない決まり（決定記録 #047）なので、印をたどる
/// 前提は要りません。`entry.file_type()` は追加の問い合わせを出さない点でも
/// `is_dir()` より軽いです。
///
/// **読めないものは飛ばします。エラーにしません**（決定記録 #167）。
/// 非 UTF-8 の名前と深さ超過を飛ばすのに、`read_dir` の失敗だけ `Err` に
/// していたころは、権限で読めないサブディレクトリ 1 つで一覧の全体が
/// 落ちました。ファイルを `prefix` に渡しただけでもエラーでした。
fn collect_files_at(dir: &Path, base: &Path, out: &mut Vec<String>, depth: usize) {
    if depth >= MAX_DEPTH {
        // 黙って打ち切ると、一覧に出ないファイルがある理由が分かりません。
        tracing::warn!(
            "{} は深さ {MAX_DEPTH} を超えたので一覧から飛ばします",
            dir.display()
        );
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // 無いものは空の一覧にします。一覧を頼むだけで作ったりはしません。
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            // ディレクトリでない・権限が無い、など。その枝だけ飛ばします。
            tracing::warn!("{} を読めないので一覧から飛ばします: {e}", dir.display());
            return;
        }
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        let path = entry.path();
        if kind.is_dir() {
            collect_files_at(&path, base, out, depth + 1);
            continue;
        }
        if let Ok(relative) = path.strip_prefix(base) {
            // **`to_string_lossy` は使いません。** Linux では非 UTF-8 の名前が
            // 作れます。置き換え文字（U+FFFD）入りの文字列を返すと、それを
            // `read` や `delete` に渡しても見つかりません。飛ばして知らせます。
            match relative.to_str() {
                Some(text) => out.push(text.replace('\\', "/")),
                None => tracing::warn!(
                    "{} は UTF-8 でない名前なので一覧から飛ばします",
                    relative.display()
                ),
            }
        }
    }
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
            // 絶対パス。以前は先頭の `/` を落として中に写していたが、
            // 公開ドキュメントの「絶対パスはエラー」にそろえた。
            "/etc/passwd",
            "\\windows\\win.ini",
            "/",
        ] {
            assert!(disk.absolute(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn 空のパスはファイルとして扱わない() {
        let disk = disk("empty-path");
        // 一覧の起点としては置き場所そのもの。
        assert_eq!(disk.absolute("").unwrap(), disk.root);
        assert_eq!(disk.absolute(".").unwrap(), disk.root);
        // ファイルの読み書きに渡すと、ディレクトリを開く分かりにくいエラーに
        // なっていた。先に断る。
        for empty in ["", ".", "./"] {
            let error = disk.file_path(empty).unwrap_err().to_string();
            assert!(error.contains("パスが空です"), "{empty:?}: {error}");
        }
        assert!(disk.file_path("a.txt").is_ok());
    }

    #[test]
    fn storageが無ければ作らない() {
        // `storage/` の下を指す置き場所。テストでは `storage/` が無い。
        let storage = crate::paths::storage_path("");
        let disk = Storage {
            root: crate::paths::storage_path("app"),
        };
        if storage.is_dir() {
            // 手元に `storage/` がある環境では確かめようがないので飛ばす。
            return;
        }
        // 以前は `create_dir_all` が `storage/` ごと作っていた（決定記録 #023 に反する）。
        let error = disk
            .ensure_storage(&disk.absolute("notes/a.txt").unwrap())
            .unwrap_err()
            .to_string();
        assert!(error.contains("storage:init"), "{error}");
        assert!(!storage.exists(), "作らない");

        // `storage/` の外の置き場所には口を出さない。
        let outside = self::disk("outside");
        assert!(outside
            .ensure_storage(&outside.absolute("a.txt").unwrap())
            .is_ok());
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
    fn windowsで使えない記号は断る() {
        let disk = disk("symbols");
        // 断っていなかったころ、`Storage::put("notes/a?b.txt", ..)` は
        // Linux で成功し、Windows では `Err(InvalidInput)` になった。
        for bad in [
            "a?b.txt",
            "notes/a*b.txt",
            "a<b.txt",
            "a>b.txt",
            "a\"b.txt",
            "a|b.txt",
            "notes/*/a.txt",
            // 制御文字。Linux では名前に使えるが Windows では使えない。
            "a\nb.txt",
            "a\tb.txt",
            "a\u{0}b.txt",
            "a\u{1f}b.txt",
        ] {
            let error = disk.absolute(bad).unwrap_err().to_string();
            assert!(error.contains("使えない名前"), "{bad:?}: {error}");
        }

        // 普通の名前はそのまま通る。`:` は別の理由（置き場所の外）で断る。
        for ok in ["a-b.txt", "a_b.txt", "日本語.txt", "a b.txt", "a!b.txt"] {
            assert!(disk.absolute(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn 短い名前の形は断る() {
        let disk = disk("shortname");
        // NTFS は長い名前に短い名前（`<先頭6文字>~<数字>`）を自動で付ける。
        // 実機では `avatars/alicep~1.jpg` が `alice.profile.photo.jpg` に当たり、
        // 読み・上書き・削除ができた。
        for bad in [
            "avatars/alicep~1.jpg",
            "postsc~1.aut",
            "a~9",
            "notes/x~12/a.txt",
        ] {
            let error = disk.absolute(bad).unwrap_err().to_string();
            assert!(error.contains("使えない名前"), "{bad}: {error}");
            assert!(error.contains("短い名前"), "{bad}: {error}");
        }

        // `~` の後ろが数字でなければ通す。短い名前は必ず `~<数字>` の形。
        for ok in ["file~name.txt", "a~.txt", "~b", "a~", "~"] {
            assert!(disk.absolute(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn キャッシュの鍵で使う記号は断らない() {
        // `cache/mod.rs` の `normalize_key` も `usable_name` を使う。
        // `:` そのものは断らない（`Storage` 側は `absolute` が別に断る）。
        // ただし `:` を含む鍵は `normalize_key` がハッシュ側に回すので、
        // 素通しした鍵に `~` は出てこない。`~1` を断っても噛み合う。
        assert!(usable_name("user:1").is_ok());
        assert!(usable_name("a.b-c_1").is_ok());
        assert!(usable_name("user~1").is_err());
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

        // 区切りは `\` でもよい。
        assert_eq!(disk.absolute("notes\\a.txt").unwrap(), full);
        // 空のパスは置き場所そのもの（一覧の起点）。
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

    #[test]
    fn 一覧は深さの上限で止まる() {
        let dir = std::env::temp_dir().join(format!("bengara-depth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        // 上限より深いところにもファイルを置く。
        let mut deep = dir.clone();
        for i in 0..(MAX_DEPTH + 2) {
            deep = deep.join(format!("d{i}"));
            std::fs::create_dir_all(&deep).unwrap();
            std::fs::write(deep.join("a.txt"), "x").unwrap();
        }

        let mut out = Vec::new();
        collect_files(&dir, &dir, &mut out);
        // 上限の手前までしか見ない（`d0` の 1 件目は深さ 1 で数える）。
        assert_eq!(out.len(), MAX_DEPTH - 1);
        assert!(out.iter().all(|p| p.ends_with("a.txt")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn 無いディレクトリの一覧は空() {
        let disk = disk("empty");
        let _ = std::fs::remove_dir_all(&disk.root);
        assert!(disk.files("none").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn 無い起点とファイルの起点は空の一覧() {
        let disk = disk("files-prefix");
        let _ = std::fs::remove_dir_all(&disk.root);
        std::fs::create_dir_all(&disk.root).unwrap();

        // 置き場所はあるが、その下に無い起点。
        assert!(disk.files("none/deep").await.unwrap().is_empty());

        // ファイルを起点に渡しても落ちない。前は `Err` になっていた。
        disk.write("a.txt", b"1".to_vec()).await.unwrap();
        assert!(disk.files("a.txt").await.unwrap().is_empty());

        // 名前が使えないときだけエラーにする。
        assert!(disk.files("../secret").await.is_err());

        let _ = std::fs::remove_dir_all(&disk.root);
    }
}
