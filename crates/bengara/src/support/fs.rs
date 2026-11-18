//! ファイルを「別名に書いてから置き換える」。
//!
//! `std::fs::write` は切り詰めてから書くので、途中で読まれると空か途中までの
//! 中身が見えます。同じディレクトリの別名に書き、`rename` で置き換えます。
//!
//! セッション・キャッシュ・スケジュールの前回時刻・`.env` の 4 か所が
//! それぞれ同じ手順を持っていたので、ここ 1 つにまとめました。
//!
//! # 呼ぶ側が決めること
//!
//! **一時ファイルの名前は呼ぶ側が決めます。** 掃除が名前の印で一時ファイルを
//! 見分けているためです（セッションは `.tmp` で終わる名前、キャッシュは
//! `.+tmp.` を含む名前）。ここで名前を作ると、その約束が置き場所ごとに
//! 守れなくなります。
//!
//! # ここが守ること
//!
//! | 約束                                   | 中身                                         |
//! |----------------------------------------|----------------------------------------------|
//! | 権限                                   | [`Access`] のとおり                          |
//! | Windows の「使用中」                   | `rename` を少し待ってやり直す                |
//! | 失敗したら一時ファイルを残さない       | 書き込み・権限・置き換えのどこで失敗しても消す |

use std::path::Path;

/// 置き換えるファイルの権限。
pub(crate) enum Access {
    /// OS の既定のまま（Unix では umask まかせ）。
    Default,
    /// 所有者だけが読み書きできる（Unix で 0600）。
    ///
    /// Windows では何もしません（`ops.rs` の `restrict` と同じ考え方で、
    /// 既定の継承に任せます）。
    Private,
    /// 所有者だけに絞って書き、置き換える直前に渡した権限へ付け直す。
    ///
    /// 元のファイルの権限を引き継ぐときに使います。中身を書いている間は
    /// [`Access::Private`] と同じなので、途中の中身を他の利用者に読まれません。
    ///
    /// **これだけはディスクまで書き切ります（`fsync`）。** 使うのは `.env`
    /// （`key:generate`）だけです。書き切らないと、電源が落ちたときに
    /// 置き換えだけが残り、中身が空の `.env` になることがありました。
    /// セッションやキャッシュはリクエストごとに書くので、遅くなる `fsync` はしません。
    Keep(std::fs::Permissions),
}

/// `temp` に書いてから `target` へ置き換える。
///
/// `temp` は `target` と**同じディレクトリ**に置いてください。別のファイル
/// システムをまたぐと `rename` が原子的でなくなります。
///
/// 失敗したら `temp` を消してからエラーを返します。
pub(crate) fn write_atomic(
    temp: &Path,
    target: &Path,
    body: &[u8],
    access: Access,
) -> std::io::Result<()> {
    let written = (|| {
        let durable = matches!(access, Access::Keep(_));
        match access {
            Access::Default => std::fs::write(temp, body)?,
            Access::Private => write_private(temp, body)?,
            Access::Keep(permissions) => {
                write_private(temp, body)?;
                // 中身をディスクまで書き切ってから置き換える。順が逆だと、
                // 置き換えだけが先に残って空のファイルになることがある。
                // 権限を付け直す前に行う（読み取りだけの権限だと開き直せない）。
                sync_file(temp)?;
                std::fs::set_permissions(temp, permissions)?;
            }
        }
        rename_with_retry(temp, target)?;
        if durable {
            // 置き換えた（名前を付け替えた）ことも書き切る。置き換えは
            // 済んでいるので、ここで失敗してもエラーにはせず知らせるだけにする。
            if let Err(e) = sync_parent(target) {
                tracing::warn!(
                    "{} のディレクトリをディスクへ書き切れませんでした: {e}",
                    target.display()
                );
            }
        }
        Ok(())
    })();
    if written.is_err() {
        remove_temp(temp);
    }
    written
}

/// ファイルの中身をディスクまで書き切る。
///
/// 書き込み用に開き直します。Windows の `FlushFileBuffers` は、読み取りだけで
/// 開いたハンドルでは断られるためです（切り詰めはしません）。
fn sync_file(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .sync_all()
}

/// 置き換えたファイルのあるディレクトリを書き切る（Unix だけ）。
///
/// `rename` はディレクトリの中身の書き換えなので、ディレクトリを `fsync`
/// しないと、電源断の後に置き換え前へ戻ることがあります。
/// Windows はディレクトリを `fsync` する手段が無いので、何もしません。
#[cfg(unix)]
fn sync_parent(target: &Path) -> std::io::Result<()> {
    let parent = match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_target: &Path) -> std::io::Result<()> {
    Ok(())
}

/// 置き換えられなかった一時ファイルを片付ける。
///
/// 残しても、置き場所ごとの掃除が後で片付けます。ここで消せるなら消します。
/// 作る前に失敗したとき（`NotFound`）は何も言いません。
fn remove_temp(temp: &Path) {
    match std::fs::remove_file(temp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("一時ファイルを片付けられませんでした: {e}"),
    }
}

/// 所有者だけが読み書きできるファイルとして書く。
///
/// セッションのファイルには認証 ID と、検査に落ちた入力（`flash_input`）が入ります。
/// `std::fs::write` は Unix で 0644 になり、同じホストの他の利用者から読めてしまいます。
///
/// Windows では何もしません（`ops.rs` の `restrict` と同じ考え方で、既定の継承に任せます）。
#[cfg(unix)]
fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // `mode` は**作るときだけ**効きます。前のバージョンが 0644 で作ったファイルが
    // 残っている場合もあるので、開いたあとに念のため付け直します。
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(body)
}

#[cfg(not(unix))]
fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, body)
}

/// `rename` が「使用中」で失敗したときに、やり直す回数と間隔。
///
/// Windows では、別のプロセスやウイルス対策ソフトが同じファイルを開いている瞬間に
/// `rename` が失敗します。1 回で諦めるとそのリクエストが 500 になるので、
/// 少しだけ待ってやり直します。
const RENAME_RETRIES: u32 = 5;
const RENAME_RETRY_WAIT: std::time::Duration = std::time::Duration::from_millis(10);

/// 一時ファイルを本体へ置き換える。「使用中」系の失敗は少し待ってやり直す。
///
/// `write_atomic` のほか、少しずつ書いたファイルを置き換える `Storage::write_stream` も使います。
pub(crate) fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if attempt < RENAME_RETRIES && is_busy_error(&e) => {
                attempt += 1;
                std::thread::sleep(RENAME_RETRY_WAIT);
            }
            Err(e) => return Err(e),
        }
    }
}

/// やり直す価値のある失敗か（別のプロセスが使っているだけ、の失敗）。
///
/// **やり直すのは次の2つだけです。**
///
/// | 見るもの | 理由 |
/// |---|---|
/// | `PermissionDenied` | Windows は「別のプロセスが使用中」をこれで返すことがある |
/// | OS の 32・33 番 | Windows の共有違反・錠違反。標準ライブラリが種類を付けない |
///
/// 「`NotFound` 以外は全部やり直す」にしていたころは、権限不足やディスク満杯の
/// ような**待っても結果が変わらない失敗**でも 5 回 × 10ms 待ってから失敗して
/// いました。
///
/// 32・33 番を番号で見るのは、標準ライブラリの `ErrorKind::Uncategorized` が
/// まだ名前で書けないためです。
fn is_busy_error(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied || is_sharing_violation(e)
}

/// Windows の共有違反（32 番）・錠違反（33 番）か。
///
/// `ERROR_SHARING_VIOLATION` / `ERROR_LOCK_VIOLATION` です。別のプロセスが
/// 同じファイルを開いている瞬間に出ます。少し待てば消えます。
#[cfg(windows)]
fn is_sharing_violation(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(32 | 33))
}

#[cfg(not(windows))]
fn is_sharing_violation(_e: &std::io::Error) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// テスト用の空ディレクトリ。
    fn temp_dir(name: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "bengara-fs-test-{name}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// ディレクトリの中のファイル名（並べ替え済み）。
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn 別名に書いてから置き換える() {
        let dir = temp_dir("replace");
        let target = dir.join("body");
        std::fs::write(&target, "old").unwrap();

        for access in [Access::Default, Access::Private] {
            write_atomic(&dir.join("body.tmp"), &target, b"new", access).unwrap();
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
            // 一時ファイルは残らない。
            assert_eq!(names(&dir), ["body"]);
        }

        // 本体がまだ無くても書ける。
        write_atomic(&dir.join("x.tmp"), &dir.join("x"), b"1", Access::Default).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("x")).unwrap(), "1");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 置き換えに失敗したら一時ファイルを残さない() {
        let dir = temp_dir("cleanup");
        // 置き換える先がディレクトリなので、`rename` が必ず失敗する。
        let target = dir.join("target");
        std::fs::create_dir_all(target.join("inner")).unwrap();

        for access in [Access::Default, Access::Private] {
            let result = write_atomic(&dir.join("target.tmp"), &target, b"new", access);
            assert!(result.is_err(), "ディレクトリは置き換えられない");
            assert_eq!(names(&dir), ["target"], "一時ファイルが残らない");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 書けなかったときはエラーを返すだけで何も残さない() {
        let dir = temp_dir("unwritable");
        // 一時ファイルを置くディレクトリが無い。
        let temp = dir.join("無い").join("body.tmp");
        let result = write_atomic(&temp, &dir.join("body"), b"new", Access::Private);
        assert!(result.is_err());
        assert!(names(&dir).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 引き継ぐ権限は置き換えた後のファイルに付く() {
        let dir = temp_dir("keep");
        let target = dir.join("body");
        std::fs::write(&target, "old").unwrap();
        let permissions = std::fs::metadata(&target).unwrap().permissions();

        write_atomic(
            &dir.join("body.tmp"),
            &target,
            b"new",
            Access::Keep(permissions.clone()),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions(),
            permissions
        );
        assert_eq!(names(&dir), ["body"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 読み取りだけの権限でも書き切って置き換えられる() {
        // `.env` を読み取りだけにしている環境。書き切る（`fsync`）ために
        // 開き直すのは、権限を付け直す前でなければならない。
        let dir = temp_dir("keep-readonly");
        let target = dir.join("body");
        std::fs::write(&target, "old").unwrap();
        let mut permissions = std::fs::metadata(&target).unwrap().permissions();
        permissions.set_readonly(true);

        write_atomic(
            &dir.join("body.tmp"),
            &target,
            b"new",
            Access::Keep(permissions.clone()),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        assert!(std::fs::metadata(&target).unwrap().permissions().readonly());
        assert_eq!(names(&dir), ["body"]);

        // 片付けのために書けるように戻す。
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&target, permissions).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 書き切りとディレクトリの書き切りは失敗しない() {
        let dir = temp_dir("sync");
        let file = dir.join("a");
        std::fs::write(&file, "x").unwrap();
        sync_file(&file).unwrap();
        sync_parent(&file).unwrap();
        // 親の無い相対パスでも落ちない（今のディレクトリを書き切る）。
        sync_parent(Path::new("a")).unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn 権限は指定のとおりになる() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("mode");
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;

        // 所有者だけ。前に 0644 で作られた一時ファイルが残っていても付け直す。
        let temp = dir.join("private.tmp");
        std::fs::write(&temp, "残骸").unwrap();
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_atomic(&temp, &dir.join("private"), b"x", Access::Private).unwrap();
        assert_eq!(mode(&dir.join("private")), 0o600);

        // 元の権限を引き継ぐ。
        write_atomic(
            &dir.join("keep.tmp"),
            &dir.join("keep"),
            b"x",
            Access::Keep(std::fs::Permissions::from_mode(0o640)),
        )
        .unwrap();
        assert_eq!(mode(&dir.join("keep")), 0o640);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn やり直すのは使用中の失敗だけ() {
        use std::io::{Error as IoError, ErrorKind};

        // 待てば変わるかもしれない失敗。
        assert!(is_busy_error(&IoError::from(ErrorKind::PermissionDenied)));
        // 待っても変わらない失敗。やり直すと 5 回 × 10ms ぶん遅れるだけ。
        for kind in [
            ErrorKind::NotFound,
            ErrorKind::InvalidInput,
            ErrorKind::Unsupported,
            ErrorKind::AlreadyExists,
            ErrorKind::WriteZero,
        ] {
            assert!(!is_busy_error(&IoError::from(kind)), "{kind:?}");
        }
        // Windows の共有違反は番号で見分ける。
        #[cfg(windows)]
        assert!(
            is_busy_error(&IoError::from_raw_os_error(32)),
            "共有違反はやり直す"
        );
    }
}
