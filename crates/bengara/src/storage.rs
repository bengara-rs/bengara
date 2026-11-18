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
use std::sync::{Arc, OnceLock};

use crate::error::{Error, Result};

/// 既定の置き場所の名前。
const DEFAULT_DISK: &str = "local";

/// 置き場所の設定。`config/filesystems.rs` が返します。
///
/// ファイル名は自由です。見分けるのは名前ではなく、`config()` が返す型です。
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
    /// `root` の実体のパス（リンクを解いたもの）。初めて要るときに求めて覚える。
    ///
    /// 同じ置き場所の `Storage` どうしで共有します（`real_root_cell`）。
    /// 読み書きのたびに `root` を解き直さないためです。
    real_root: Arc<OnceLock<PathBuf>>,
}

/// 置き場所の根ごとの「実体のパス」の覚え場所。
///
/// 設定の置き場所は起動後に変わらないので、最初に 1 回だけ表を作り、以後は読むだけです。
/// 表に無い根（テストで手で作ったものなど）には、その場で新しい覚え場所を渡します。
fn real_root_cell(root: &Path) -> Arc<OnceLock<PathBuf>> {
    static TABLE: OnceLock<Vec<(PathBuf, Arc<OnceLock<PathBuf>>)>> = OnceLock::new();
    TABLE
        .get_or_init(|| {
            config()
                .disks
                .iter()
                .map(|disk| (crate::paths::storage_path(&disk.root), Arc::default()))
                .collect()
        })
        .iter()
        .find(|(known, _)| known == root)
        .map(|(_, cell)| Arc::clone(cell))
        .unwrap_or_default()
}

impl Storage {
    /// 根から作る。実体のパスの覚え場所は、同じ根のものを共有します。
    fn at(root: PathBuf) -> Self {
        let real_root = real_root_cell(&root);
        Self { root, real_root }
    }

    /// 実体のパスで置き場所の中に収まっているかを確かめる道具を作る。
    ///
    /// 裏のスレッド（`spawn_blocking`）へ持ち込めるように、`self` から切り離します。
    fn jail(&self) -> Jail {
        Jail {
            root: self.root.clone(),
            real_root: Arc::clone(&self.real_root),
        }
    }

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
            Ok(root) => Ok(Self::at(root.clone())),
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
        Ok(Self::at(crate::paths::storage_path(&disk.root)))
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

    /// 文字列を末尾に書き足す。ファイルがあれば、間に改行（`\n`）を挟みます。
    ///
    /// Laravel の `Storage::append` と同じ決まりです（Laravel の改行は OS ごとに
    /// 違いますが、bengara はどの OS でも `\n` です）。ファイルが無ければ作ります。
    ///
    /// 何も挟まずにつなげたいとき（バイナリのかたまりなど）は、
    /// [`append_bytes`](Self::append_bytes) を使ってください。
    ///
    /// **同時に書き足すと、改行の有無がずれることがあります。** あるかどうかを
    /// 見てから書くまでの間に、別の書き足しが入るためです。何本ものリクエストから
    /// 1 行ずつ足すときは、自分で `\n` を付けて `append_bytes` を使ってください。
    pub async fn append(path: &str, contents: impl Into<String>) -> Result<()> {
        Self::default_disk()?
            .append_line(path, contents.into())
            .await
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
        let jail = self.jail();
        crate::support::blocking(move || {
            jail.check(&full, true)?;
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&full, contents).map_err(Error::Io)
        })
        .await?
    }

    /// `Storage::append` の中身。ファイルがあれば改行を挟んで書き足す。
    ///
    /// 置き場所を指定した形は公開しません。静的な `append` と同じ名前にできない
    /// （Rust は同じ型に同じ名前の関数を 2 つ置けない）ためで、`put` と `write` の関係と同じです。
    async fn append_line(&self, path: &str, contents: String) -> Result<()> {
        let full = self.file_path(path)?;
        self.ensure_storage(&full)?;
        let jail = self.jail();
        crate::support::blocking(move || {
            jail.check(&full, true)?;
            let exists = full.is_file();
            let mut bytes = Vec::with_capacity(contents.len() + 1);
            if exists {
                bytes.push(b'\n');
            }
            bytes.extend_from_slice(contents.as_bytes());
            append_to(&full, &bytes)
        })
        .await?
    }

    /// バイト列を末尾にそのまま書き足す（間に何も挟みません）。
    ///
    /// ファイルが無ければ作ります。途中のディレクトリも作ります。
    /// 少しずつ届くデータ（WebSocket のバイナリ、`body_stream()` のかたまり）を
    /// 1 つのファイルにつなげるときに使います。
    ///
    /// ファイル全体を読み直さず、末尾にだけ書きます（OS の追記モード）。
    /// 1 回の書き足しが途中で切れることはありますが、前の中身は壊しません。
    pub async fn append_bytes(&self, path: &str, contents: Vec<u8>) -> Result<()> {
        let full = self.file_path(path)?;
        self.ensure_storage(&full)?;
        let jail = self.jail();
        crate::support::blocking(move || {
            jail.check(&full, true)?;
            append_to(&full, &contents)
        })
        .await?
    }

    /// 少しずつ届く本文（`req.body_stream()?`）を、そのままファイルに書く。
    /// 書いたバイト数を返します。
    ///
    /// ```ignore
    /// // routes/web.rs: Route::post("/uploads", ..).stream_body().max_body(512 * 1024 * 1024)
    /// let written = Storage::disk("local")
    ///     .write_stream("uploads/movie.mp4", req.body_stream()?)
    ///     .await?;
    /// ```
    ///
    /// Laravel の `writeStream` に当たります。本文を全部メモリに載せずに書けます。
    ///
    /// **最後まで受け取れたときだけ、その名前のファイルになります。** 書いている
    /// 間は同じディレクトリの別の名前（`.名前.<乱数>.part`）に書き、終わったら
    /// 置き換えます。途中で失敗したとき（上限超えの 413・時間切れの 408・切断の 400）は
    /// 書きかけを消し、前からあったファイルはそのまま残します。エラーは
    /// 本文と同じ状態コードで返るので、`?` で返せばその応答になります。
    ///
    /// 待つのをやめたとき（`tokio::time::timeout` で包んだ、キューの `--timeout` で
    /// 打ち切られた、など）も書きかけを消します。ただし、ちょうど裏のスレッドで
    /// 書いている最中だと Windows では消せず、`tracing::warn!` で知らせて残ります。
    ///
    /// 書いている間の別の名前は、[`files`](Self::files) の一覧にも見えます。
    pub async fn write_stream(&self, path: &str, mut body: crate::http::BodyStream) -> Result<u64> {
        let full = self.file_path(path)?;
        self.ensure_storage(&full)?;
        let temp = part_path(&full)?;

        let opened = {
            let (full, temp) = (full.clone(), temp.clone());
            let jail = self.jail();
            crate::support::blocking(move || -> Result<std::fs::File> {
                jail.check(&full, true)?;
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temp)
                    .map_err(Error::Io)
            })
            .await?
        };
        let mut file = opened?;

        // ここから先は、失敗しても待つのをやめられても、書きかけを消す。
        // 置き換えが済んだら外す（`disarm`）。
        //
        // 宣言の順に意味があります。後に作る `async` の中身（ファイルを持っている）が
        // 先に捨てられ、ファイルが閉じてから、この見張りが消します。
        let mut guard = PartGuard(Some(temp.clone()));

        let written = {
            let (temp, full) = (temp.clone(), full.clone());
            async move {
                let mut written: u64 = 0;
                while let Some(chunk) = body.chunk().await? {
                    written = written.saturating_add(chunk.len() as u64);
                    // ファイルを裏のスレッドへ渡して書き、返してもらう。
                    file = crate::support::blocking(move || -> Result<std::fs::File> {
                        use std::io::Write;
                        file.write_all(&chunk)?;
                        Ok(file)
                    })
                    .await??;
                }
                crate::support::blocking(move || -> Result<()> {
                    use std::io::Write;
                    file.flush()?;
                    // Windows は開いたままのファイルを置き換えられないので、先に閉じる。
                    drop(file);
                    crate::support::fs::rename_with_retry(&temp, &full).map_err(Error::Io)
                })
                .await??;
                Ok::<u64, Error>(written)
            }
            .await
        };

        match written {
            Ok(written) => {
                // 置き換え済み。書きかけの名前はもう無いので、消さない。
                guard.disarm();
                Ok(written)
            }
            Err(error) => {
                // 消すのは裏のスレッドで行う（`Drop` の中で消すと、ここで待たされる）。
                // 消せなくても、元のエラーを返す（消せない理由はログに出す）。
                if let Some(temp) = guard.disarm() {
                    let _ = crate::support::blocking(move || remove_part(&temp)).await;
                }
                Err(error)
            }
        }
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
            return Err(Error::msg(crate::paths::missing_storage_message(&storage)));
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
        let jail = self.jail();
        crate::support::blocking(move || {
            jail.check(&full, true)?;
            match std::fs::read(&full) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(Error::Io(e)),
            }
        })
        .await?
    }

    /// あるか。
    ///
    /// 問い合わせが少ないので、裏のスレッド（`spawn_blocking`）を経由しません。
    /// 往復のほうが調べるより高く付きます。
    pub async fn has(&self, path: &str) -> Result<bool> {
        let full = self.file_path(path)?;
        self.jail().check(&full, true)?;
        Ok(full.is_file())
    }

    /// 消す。無くてもエラーにはしません。
    pub async fn remove(&self, path: &str) -> Result<()> {
        let full = self.file_path(path)?;
        let jail = self.jail();
        crate::support::blocking(move || {
            // 消すのはリンクそのもので、先はたどらない。親のディレクトリだけ確かめる。
            jail.check(&full, false)?;
            match std::fs::remove_file(&full) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(Error::Io(e)),
            }
        })
        .await?
    }

    /// バイト数。無ければ `None`。
    ///
    /// `metadata()` 1 回だけなので、`has` と同じく裏のスレッドを経由しません。
    pub async fn size(&self, path: &str) -> Result<Option<u64>> {
        let full = self.file_path(path)?;
        self.jail().check(&full, true)?;
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
        let jail = self.jail();
        crate::support::blocking(move || {
            jail.check(&full, true)?;
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
    ///
    /// ここで見るのは**文字列だけ**です。置き場所の中のリンク（シンボリックリンク・
    /// ジャンクション）が外を指していないかは、読み書きの直前に [`Jail::check`] が
    /// 実体のパスで確かめます。
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

/// 実体のパスで、置き場所の中に収まっているかを確かめる。
///
/// `absolute` の文字列の検査だけでは、置き場所の中に外を指すリンクがあると
/// `read` や `write` がその先を読み書きしていました（`files` だけはリンクを飛ばしていた）。
/// リンクを解いたパスが、リンクを解いた根の下にあるかを比べます。
///
/// - 根の実体のパスは初めて要るときに 1 回だけ求め、`Storage` どうしで共有します。
/// - Windows の `canonicalize` は `\\?\C:\...` の形を返します。両方を同じ関数で
///   解くので、比べる形はそろいます。
/// - 調べてから開くまでの間に差し替えられる競合は防げません。置き場所の中に
///   リンクを置ける人は、もともと中身を書き換えられる人だからです。
struct Jail {
    root: PathBuf,
    real_root: Arc<OnceLock<PathBuf>>,
}

impl Jail {
    /// `full` が実体でも置き場所の中にあれば `Ok`。
    ///
    /// `follow_last` が `false` なら、最後の要素のリンクはたどらず、親だけを確かめます
    /// （`remove` はリンクそのものを消すため）。
    ///
    /// まだ無いファイルは、いちばん近くにある祖先で確かめます。何も無い（根も無い）
    /// なら、これから作るのは普通のディレクトリなので通します。
    /// 先の無いリンク（壊れたリンク）は、書くとリンクの先に作られるので断ります。
    fn check(&self, full: &Path, follow_last: bool) -> Result<()> {
        let mut probe = if follow_last {
            full
        } else {
            match full.parent() {
                Some(parent) => parent,
                None => return Ok(()),
            }
        };
        loop {
            match std::fs::canonicalize(probe) {
                Ok(real) => {
                    return if real.starts_with(self.real_root()?) {
                        Ok(())
                    } else {
                        Err(self.outside(full))
                    };
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if std::fs::symlink_metadata(probe).is_ok() {
                        // 名前はあるのに先が無い。壊れたリンク。
                        return Err(self.outside(full));
                    }
                    if probe == self.root {
                        return Ok(());
                    }
                    match probe.parent() {
                        Some(parent) if parent.starts_with(&self.root) => probe = parent,
                        _ => return Ok(()),
                    }
                }
                Err(e) => return Err(Error::Io(e)),
            }
        }
    }

    /// 根の実体のパス。求められたら覚えます（失敗は覚えない）。
    fn real_root(&self) -> Result<&PathBuf> {
        if let Some(real) = self.real_root.get() {
            return Ok(real);
        }
        let real = std::fs::canonicalize(&self.root)?;
        Ok(self.real_root.get_or_init(|| real))
    }

    fn outside(&self, full: &Path) -> Error {
        let shown = full.strip_prefix(&self.root).unwrap_or(full);
        Error::msg(format!(
            "`{}` は置き場所の外を指しています（リンクの先が置き場所の外です）",
            shown.display()
        ))
    }
}

/// Windows が**どのディレクトリの下でも**装置として解決してしまう名前。
///
/// `storage/app/nul` へ書くと、`std::fs::write` は成功を返すのに中身は
/// 0 バイトになります（画面に出さずに捨てられます）。`con` は読み戻しで止まります。
///
/// 上付きの数字（`COM¹` `COM²` `COM³` と `LPT` の同じ形）と、コンソールの
/// `CONIN$` `CONOUT$` も装置です。`COM0` と `LPT0` は装置ではない版も
/// ありますが、断っても困る名前ではないので入れます。
const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6",
    "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³", "CONIN$", "CONOUT$",
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
    //
    // Windows は名前の末尾の空白を落としてから装置かを見るので、
    // `nul .txt` も `nul` です。比べる前に末尾の空白を落とします
    // （`.` より前なので、末尾の `.` はここには来ません）。
    let stem = part.split('.').next().unwrap_or(part).trim_end_matches(' ');
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

/// 末尾に書き足す（OS の追記モード）。途中のディレクトリは作ります。
fn append_to(full: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(full)?;
    file.write_all(bytes)?;
    Ok(())
}

/// `write_stream` が書いている間の名前（`.名前.<乱数>.part`）。
///
/// 置き換える先と**同じディレクトリ**に置きます。別のファイルシステムをまたぐと
/// `rename` が原子的でなくなるためです。乱数を入れるので、同じ名前へ同時に
/// 書いても互いの書きかけを壊しません（最後に置き換えたほうが残ります）。
fn part_path(full: &Path) -> Result<PathBuf> {
    let name = full
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::msg("ファイルの名前が読めません"))?;
    let random = crate::support::crypto::to_hex(&crate::support::crypto::random_bytes::<8>());
    Ok(full.with_file_name(format!(".{name}.{random}.part")))
}

/// `write_stream` の書きかけを見張る。外さないまま捨てられたら、書きかけを消す。
///
/// エラーの道は `write_stream` が裏のスレッドで消します。ここで消すのは、
/// 待つのをやめられたとき（`future` ごと捨てられたとき）だけです。
/// `Drop` は待てないので、その場で消します（ファイル 1 つの削除なので短い）。
struct PartGuard(Option<PathBuf>);

impl PartGuard {
    /// 見張りを外し、見張っていたパスを返す。
    fn disarm(&mut self) -> Option<PathBuf> {
        self.0.take()
    }
}

impl Drop for PartGuard {
    fn drop(&mut self) {
        if let Some(temp) = self.0.take() {
            remove_part(&temp);
        }
    }
}

/// 書きかけを消す。無ければ何もしない。消せなければログに出す。
fn remove_part(temp: &Path) {
    if let Err(e) = std::fs::remove_file(temp) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                "書きかけのファイル {} を消せませんでした: {e}",
                temp.display()
            );
        }
    }
}

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
        Storage::at(
            std::env::temp_dir().join(format!("bengara-storage-{}-{name}", std::process::id())),
        )
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
        let disk = Storage::at(crate::paths::storage_path("app"));
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
    }

    #[test]
    fn storageが無いときの案内はコマンドの呼び方を言う() {
        // キャッシュ（`cache/store.rs`）と同じ文。本番には cargo が無いので、
        // 実行ファイルでの呼び方も言う。
        let text = crate::paths::missing_storage_message(Path::new("/srv/app/storage"));
        assert!(text.contains("がありません"), "{text}");
        assert!(text.contains("cargo artisan storage:init"), "{text}");
        assert!(text.contains("./<実行ファイル> storage:init"), "{text}");
        assert!(text.ends_with("を実行してください"), "{text}");
    }

    #[test]
    fn storageの外の置き場所には口を出さない() {
        // テスト用の一時ディレクトリは `storage/` の外。`storage/` の有無を見ない。
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
        for ok in [
            "null", "nula", "console", "com10", "com", "lpt", "a.nul", "conin",
        ] {
            assert!(disk.absolute(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn 見落としていた装置の名前も断る() {
        // 上付きの数字・コンソール・末尾に空白のある名前は、以前は通っていた。
        for bad in [
            "COM¹",
            "com².txt",
            "LPT³",
            "lpt¹.log",
            "CONIN$",
            "conout$.txt",
            "nul .txt",
            "con  .log",
            "aux ",
            "com0",
            "LPT0.txt",
        ] {
            assert!(usable_name(bad).is_err(), "{bad}");
        }
        // 装置の名前に似ているだけなら通す。
        for ok in ["com4²", "conin", "conout", "a nul.txt", "com¹0"] {
            assert!(usable_name(ok).is_ok(), "{ok}");
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

    // ---- 書き足しと、少しずつ届く本文 ----

    fn stream_of(bytes: &[u8], limit: u64) -> crate::http::BodyStream {
        crate::http::BodyStream::new(
            crate::http::body::PendingBody::memory(bytes.to_vec()),
            limit,
        )
    }

    #[tokio::test]
    async fn appendはあれば改行を挟んで書き足す() {
        let disk = disk("append");
        let _ = std::fs::remove_dir_all(&disk.root);

        // 無ければ作る。最初は改行を挟まない（Laravel と同じ）。
        disk.append_line("logs/a.log", "1 行目".into())
            .await
            .unwrap();
        disk.append_line("logs/a.log", "2 行目".into())
            .await
            .unwrap();
        assert_eq!(
            disk.read("logs/a.log").await.unwrap().as_deref(),
            Some("1 行目\n2 行目")
        );

        let _ = std::fs::remove_dir_all(&disk.root);
    }

    #[tokio::test]
    async fn append_bytesは何も挟まずにつなげる() {
        let disk = disk("append-bytes");
        let _ = std::fs::remove_dir_all(&disk.root);

        disk.append_bytes("bin/a.dat", vec![0, 1]).await.unwrap();
        disk.append_bytes("bin/a.dat", vec![2]).await.unwrap();
        disk.append_bytes("bin/a.dat", Vec::new()).await.unwrap();
        assert_eq!(
            disk.read_bytes("bin/a.dat").await.unwrap(),
            Some(vec![0, 1, 2])
        );

        // 置き場所の外には書けない。
        assert!(disk.append_bytes("../out.dat", vec![1]).await.is_err());
        assert!(disk.append_line("../out.txt", "x".into()).await.is_err());

        let _ = std::fs::remove_dir_all(&disk.root);
    }

    #[tokio::test]
    async fn write_streamは届いた本文を書いて大きさを返す() {
        let disk = disk("stream");
        let _ = std::fs::remove_dir_all(&disk.root);

        let written = disk
            .write_stream("uploads/a.bin", stream_of(b"hello", 10))
            .await
            .unwrap();
        assert_eq!(written, 5);
        assert_eq!(
            disk.read_bytes("uploads/a.bin").await.unwrap(),
            Some(b"hello".to_vec())
        );
        assert_eq!(
            disk.files("uploads").await.unwrap(),
            vec!["uploads/a.bin".to_string()],
            "書きかけの名前は残らない"
        );

        let _ = std::fs::remove_dir_all(&disk.root);
    }

    #[tokio::test]
    async fn write_streamは失敗したら書きかけを消して前のファイルを残す() {
        let disk = disk("stream-fail");
        let _ = std::fs::remove_dir_all(&disk.root);

        disk.write("uploads/a.bin", b"old".to_vec()).await.unwrap();
        let error = disk
            .write_stream("uploads/a.bin", stream_of(b"too large body", 4))
            .await
            .unwrap_err();
        assert_eq!(error.status(), 413, "本文のエラーをそのまま返す");
        assert_eq!(
            disk.read_bytes("uploads/a.bin").await.unwrap(),
            Some(b"old".to_vec()),
            "前のファイルはそのまま"
        );
        assert_eq!(
            disk.files("uploads").await.unwrap(),
            vec!["uploads/a.bin".to_string()],
            "書きかけは消える"
        );

        let _ = std::fs::remove_dir_all(&disk.root);
    }

    #[tokio::test]
    async fn write_streamは空の本文なら空のファイルを作る() {
        let disk = disk("stream-empty");
        let _ = std::fs::remove_dir_all(&disk.root);

        assert_eq!(
            disk.write_stream("empty.bin", stream_of(b"", 4))
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            disk.read_bytes("empty.bin").await.unwrap(),
            Some(Vec::new())
        );
        assert!(disk
            .write_stream("../out.bin", stream_of(b"x", 4))
            .await
            .is_err());

        let _ = std::fs::remove_dir_all(&disk.root);
    }

    #[test]
    fn 見張りを外さずに捨てたら書きかけを消す() {
        // 待つのをやめられた（future ごと捨てられた）ときの後始末。
        let dir = std::env::temp_dir().join(format!("bengara-part-guard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join(".a.bin.0000.part");
        std::fs::write(&part, b"half").unwrap();
        drop(PartGuard(Some(part.clone())));
        assert!(!part.exists(), "捨てたら消える");

        // 外したら消さない（置き換えが済んだ後）。
        std::fs::write(&part, b"done").unwrap();
        let mut guard = PartGuard(Some(part.clone()));
        assert_eq!(guard.disarm(), Some(part.clone()));
        drop(guard);
        assert!(part.exists(), "外したら残す");

        // 無いものを消そうとしても落ちない。
        std::fs::remove_file(&part).unwrap();
        drop(PartGuard(Some(part)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 書きかけの名前は同じディレクトリの隠れた名前() {
        let full = std::env::temp_dir().join("uploads").join("movie.mp4");
        let part = part_path(&full).unwrap();
        assert_eq!(part.parent(), full.parent());
        let name = part.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with(".movie.mp4."), "{name}");
        assert!(name.ends_with(".part"), "{name}");
        assert_ne!(part_path(&full).unwrap(), part, "毎回違う名前");
    }

    // ---- 置き場所の中のリンク ----

    /// 置き場所の中の `link` が、外のディレクトリを指しているときに確かめること。
    ///
    /// 外には `secret.txt` があり、リンク越しの読み書きはどれも断る。
    async fn assert_link_is_refused(disk: &Storage, outside: &Path) {
        let error = disk.read("link/secret.txt").await.unwrap_err().to_string();
        assert!(error.contains("置き場所の外"), "{error}");
        assert!(disk.read_bytes("link/secret.txt").await.is_err());
        assert!(disk.write("link/secret.txt", b"x".to_vec()).await.is_err());
        assert!(disk.write("link/new.txt", b"x".to_vec()).await.is_err());
        assert!(disk
            .append_line("link/secret.txt", "x".into())
            .await
            .is_err());
        assert!(disk.append_bytes("link/secret.txt", vec![1]).await.is_err());
        assert!(disk
            .write_stream("link/new.txt", stream_of(b"x", 1024))
            .await
            .is_err());
        assert!(disk.has("link/secret.txt").await.is_err());
        assert!(disk.size("link/secret.txt").await.is_err());
        assert!(disk.files("link").await.is_err());
        assert!(disk.remove("link/secret.txt").await.is_err());

        // 外の中身は何も変わっていない。
        assert_eq!(
            std::fs::read(outside.join("secret.txt")).unwrap(),
            b"secret"
        );
        assert!(!outside.join("new.txt").exists());

        // 中の普通のファイルは今までどおり読み書きできる。
        disk.write("inside/a.txt", b"ok".to_vec()).await.unwrap();
        assert_eq!(
            disk.read("inside/a.txt").await.unwrap().as_deref(),
            Some("ok")
        );
        assert_eq!(disk.files("").await.unwrap(), vec!["inside/a.txt"]);
    }

    /// 外を指すリンクの用意。置き場所と外を作り直して返す。
    fn link_fixture(name: &str) -> (Storage, PathBuf) {
        let disk = disk(name);
        let outside = disk.root.with_extension("outside");
        let _ = std::fs::remove_dir_all(&disk.root);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&disk.root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
        (disk, outside)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn 外を指すシンボリックリンクはたどらない() {
        let (disk, outside) = link_fixture("symlink");
        std::os::unix::fs::symlink(&outside, disk.root.join("link")).unwrap();
        assert_link_is_refused(&disk, &outside).await;

        // ファイルを指すリンクも同じ。消すのはリンクそのものなので通る。
        std::os::unix::fs::symlink(outside.join("secret.txt"), disk.root.join("file-link"))
            .unwrap();
        assert!(disk.read("file-link").await.is_err());
        disk.remove("file-link").await.unwrap();
        assert!(outside.join("secret.txt").exists(), "リンクの先は消さない");

        // 先の無いリンク。書くと外に作られるので断る。
        std::os::unix::fs::symlink(outside.join("ghost.txt"), disk.root.join("ghost")).unwrap();
        assert!(disk.write("ghost", b"x".to_vec()).await.is_err());
        assert!(!outside.join("ghost.txt").exists());

        let _ = std::fs::remove_dir_all(&disk.root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn 根そのものがリンクでも中は読み書きできる() {
        let (real, outside) = link_fixture("root-link-real");
        let linked = Storage::at(real.root.with_extension("link"));
        let _ = std::fs::remove_file(&linked.root);
        std::os::unix::fs::symlink(&real.root, &linked.root).unwrap();

        linked.write("a/b.txt", b"ok".to_vec()).await.unwrap();
        assert_eq!(linked.read("a/b.txt").await.unwrap().as_deref(), Some("ok"));

        let _ = std::fs::remove_file(&linked.root);
        let _ = std::fs::remove_dir_all(&real.root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// Windows はジャンクションで確かめる。シンボリックリンクと違って
    /// 管理者の権限が要らない。作れない環境では飛ばす。
    #[cfg(windows)]
    #[tokio::test]
    async fn 外を指すジャンクションはたどらない() {
        let (disk, outside) = link_fixture("junction");
        let made = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(disk.root.join("link"))
            .arg(&outside)
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        if made {
            assert_link_is_refused(&disk, &outside).await;
            // ジャンクションは `remove_dir` で外す（中身は消えない）。
            let _ = std::fs::remove_dir(disk.root.join("link"));
        }
        let _ = std::fs::remove_dir_all(&disk.root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
