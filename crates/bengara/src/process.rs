//! 外部のコマンドを起動して、終わりを待つ。Laravel の `Process` に当たります。
//!
//! ```ignore
//! use bengara::process::Process;
//!
//! let result = Process::timeout(30 * 60)
//!     .path(storage_path("app/videos"))
//!     .run(["ffmpeg", "-i", "in.mp4", "out.webm"])
//!     .await?
//!     .throw()?;
//! println!("{}", result.output());
//! ```
//!
//! | Laravel | bengara |
//! |---|---|
//! | `Process::run('ls -la')` | `Process::run(["ls", "-la"]).await?` |
//! | `Process::timeout(120)->run(..)` | `Process::timeout(120).run(..).await?` |
//! | `->path(..)` / `->env([..])` / `->input(..)` / `->quietly()` / `->forever()` | 同じ名前 |
//! | `$result->successful()` / `failed()` / `exitCode()` / `output()` / `errorOutput()` / `throw()` | 同じ名前（`exit_code`・`error_output`） |
//!
//! # Laravel と違うところ
//!
//! - **シェルを通しません。** コマンドは「プログラムと引数の並び」で渡します。
//!   文字列を渡すとシェルが解釈するので、外から来た値を混ぜると別のコマンドを
//!   走らせられます。OS ごとにシェルの文法も違います。
//!   パイプやリダイレクトが要るときは、`["sh", "-c", ".."]` のように自分でシェルを起動してください。
//! - **打ち切ると子プロセスを止めます。** 時間切れのときと、待っている側が
//!   待つのをやめたとき（キューの `--timeout` で打ち切られた、など）の両方です。
//!   止めるのは起動した 1 本だけで、そこから起動された孫のプロセスは残ることがあります。
//! - **終わりは「出力が閉じたとき」に分かります。** 裏で動き続ける孫のプロセス
//!   （`sh -c "daemon & echo hi"` など）が出力を握ったままだと、起動した 1 本が
//!   終わっていても待ち続け、打ち切りの時間で `Err` になります。孫を残すコマンドは
//!   `quietly()` を付けるか、孫の出力を別の場所へ向けてください。
//! - 出力は全部メモリに溜めます。出力が多いコマンドは `quietly()` で捨ててください。
//! - **時間切れのエラー文に、それまでの出力の末尾を入れます。** どこで止まったかを
//!   見られるようにするためです（標準出力とエラー出力の末尾、それぞれ 2,000 文字まで）。

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::error::{Error, Result};

/// 打ち切りの既定（秒）。Laravel と同じ 60 秒です。
const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// `throw()` のエラー文に入れるエラー出力の上限（文字数）。末尾から数えます。
///
/// エラー出力は長くなりがちで、そのままログに流すと読めません。
/// 失敗の理由はたいてい最後のほうに出るので、末尾を残します。
const ERROR_TAIL_CHARS: usize = 2000;

/// 時間切れで止めた後、子プロセスの終わりを待つ上限。
const KILL_WAIT: Duration = Duration::from_secs(5);

/// 外部のコマンドを起動する入口。
///
/// 設定を足すときは `Process::timeout(..)` のように始め、最後に `run` を呼びます。
pub struct Process;

impl Process {
    /// コマンドを起動して、終わるまで待つ。打ち切りは既定の 60 秒です。
    ///
    /// 最初の要素がプログラム、残りが引数です。
    ///
    /// | 起きたこと | 結果 |
    /// |---|---|
    /// | 終わった（終了コードが 0 でなくても） | `Ok(ProcessResult)`。`failed()` で見分けます |
    /// | 起動できなかった（プログラムが無いなど） | `Err` |
    /// | 打ち切りの時間を過ぎた | 子プロセスを止めて `Err` |
    pub async fn run<I, S>(command: I) -> Result<ProcessResult>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        PendingProcess::default().run(command).await
    }

    /// 打ち切りの時間（秒）を決める。`0` は打ち切らない（[`Process::forever`] と同じ）。
    pub fn timeout(secs: u64) -> PendingProcess {
        PendingProcess::default().timeout(secs)
    }

    /// 打ち切らない。
    pub fn forever() -> PendingProcess {
        PendingProcess::default().forever()
    }

    /// 作業ディレクトリを決める。決めなければ、このプロセスの作業ディレクトリです。
    pub fn path(dir: impl Into<PathBuf>) -> PendingProcess {
        PendingProcess::default().path(dir)
    }

    /// 環境変数を足す。このプロセスの環境変数はそのまま引き継ぎます。
    pub fn env(key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> PendingProcess {
        PendingProcess::default().env(key, value)
    }

    /// 標準入力に渡す中身を決める。決めなければ、標準入力は空です。
    pub fn input(input: impl Into<Vec<u8>>) -> PendingProcess {
        PendingProcess::default().input(input)
    }

    /// 出力（標準出力とエラー出力）を受け取らずに捨てる。
    pub fn quietly() -> PendingProcess {
        PendingProcess::default().quietly()
    }
}

/// 起動する前のコマンドの設定。Laravel の `PendingProcess` に当たります。
#[derive(Debug, Clone)]
pub struct PendingProcess {
    timeout: Option<Duration>,
    path: Option<PathBuf>,
    env: Vec<(OsString, OsString)>,
    input: Option<Vec<u8>>,
    quietly: bool,
}

impl Default for PendingProcess {
    fn default() -> Self {
        Self {
            timeout: Some(Duration::from_secs(DEFAULT_TIMEOUT_SECS)),
            path: None,
            env: Vec::new(),
            input: None,
            quietly: false,
        }
    }
}

impl PendingProcess {
    /// 打ち切りの時間（秒）を決める。`0` は打ち切らない。
    pub fn timeout(mut self, secs: u64) -> Self {
        self.timeout = (secs > 0).then(|| Duration::from_secs(secs));
        self
    }

    /// 打ち切らない。
    pub fn forever(mut self) -> Self {
        self.timeout = None;
        self
    }

    /// 作業ディレクトリを決める。
    pub fn path(mut self, dir: impl Into<PathBuf>) -> Self {
        self.path = Some(dir.into());
        self
    }

    /// 環境変数を足す。何回でも呼べます。
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env
            .push((key.as_ref().to_owned(), value.as_ref().to_owned()));
        self
    }

    /// 標準入力に渡す中身を決める。
    pub fn input(mut self, input: impl Into<Vec<u8>>) -> Self {
        self.input = Some(input.into());
        self
    }

    /// 出力を受け取らずに捨てる。
    pub fn quietly(mut self) -> Self {
        self.quietly = true;
        self
    }

    /// コマンドを起動して、終わるまで待つ。
    ///
    /// 結果の表は [`Process::run`] と同じです。
    ///
    /// エラー文に入れるのは**プログラムの名前だけ**です。引数には秘密
    /// （パスワードやトークン）が入ることがあり、エラー文はログに出るためです。
    pub async fn run<I, S>(self, command: I) -> Result<ProcessResult>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut parts = command.into_iter();
        let Some(program) = parts.next() else {
            return Err(Error::msg("起動するコマンドがありません"));
        };
        let program = program.as_ref().to_owned();
        let label = program.to_string_lossy().into_owned();

        let mut command = tokio::process::Command::new(&program);
        command.args(parts);
        if let Some(dir) = &self.path {
            command.current_dir(dir);
        }
        for (key, value) in &self.env {
            command.env(key, value);
        }
        // 標準入力を引き継がない。サーバーの端末につながったままだと、
        // 入力を待つコマンドがいつまでも終わりません。
        command.stdin(if self.input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let (stdout, stderr) = if self.quietly {
            (Stdio::null(), Stdio::null())
        } else {
            (Stdio::piped(), Stdio::piped())
        };
        command.stdout(stdout).stderr(stderr);
        // 待っている future を捨てたら子プロセスも止める。時間切れと、
        // 呼んだ側が打ち切られたとき（キューの `--timeout` など）の両方に効きます。
        command.kill_on_drop(true);

        let mut child = command.spawn().map_err(|e| {
            let hint = if e.kind() == std::io::ErrorKind::NotFound {
                "（プログラムが見つかりません。PATH にあるか、パスが正しいか確かめてください）"
            } else {
                ""
            };
            Error::msg(format!("`{label}` を起動できません: {e}{hint}"))
        })?;

        // 標準入力へは別のタスクで書く。書き終わるのを待ってから出力を読むと、
        // 出力の受け口がいっぱいになったコマンドが止まり、互いに待ち合って終わりません。
        // 書き終えたら閉じる（捨てる）ので、コマンドは入力の終わりを受け取れます。
        if let (Some(input), Some(mut stdin)) = (self.input, child.stdin.take()) {
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                // 入力を読まずに終わるコマンドもある。書けなくてもエラーにはしない。
                let _ = stdin.write_all(&input).await;
            });
        }

        // 出力は別のタスクで読み溜める。`wait_with_output` に任せていたころは、
        // 時間切れで future ごと捨てるので、それまでの出力も失っていました。
        // 読んだ分は列で受け取るので、途中で止めても手元に残ります。
        let mut stdout = Collector::spawn(child.stdout.take());
        let mut stderr = Collector::spawn(child.stderr.take());

        // 終わりは「子プロセスが終わり、出力が両方閉じたとき」です（今までと同じ）。
        let waiting = async {
            let status = child.wait().await?;
            stdout.finished().await;
            stderr.finished().await;
            Ok::<_, std::io::Error>(status)
        };
        // 先に結果だけを取り出す（`waiting` が子プロセスを借りたままにならないように）。
        let finished = match self.timeout {
            Some(limit) => tokio::time::timeout(limit, waiting).await.ok(),
            None => Some(waiting.await),
        };
        let Some(status) = finished else {
            // 時間切れ。止めて、終わりを待つ（ゾンビを残さない）。孫が出力を握っていても、
            // 読み溜めのタスクは待たずに、そこまでに届いた分だけを使う。
            let _ = child.start_kill();
            let _ = tokio::time::timeout(KILL_WAIT, child.wait()).await;
            let limit = self.timeout.unwrap_or_default();
            return Err(Error::msg(timed_out_message(
                &label,
                limit,
                &stdout.take_lossy(),
                &stderr.take_lossy(),
            )));
        };
        let status =
            status.map_err(|e| Error::msg(format!("`{label}` の終わりを待てませんでした: {e}")))?;

        Ok(ProcessResult {
            program: label,
            exit_code: status.code(),
            successful: status.success(),
            output: stdout.take_lossy(),
            error_output: stderr.take_lossy(),
        })
    }
}

/// 時間切れのエラー文。それまでの出力の末尾を入れる。
fn timed_out_message(label: &str, limit: Duration, output: &str, error_output: &str) -> String {
    let mut message = format!(
        "`{label}` が {} 秒で終わらなかったので止めました",
        limit.as_secs()
    );
    for (name, text) in [("エラー出力", error_output), ("出力", output)] {
        let tail = tail_chars(text.trim(), ERROR_TAIL_CHARS);
        if !tail.is_empty() {
            message.push_str(&format!("\n{name}の末尾: {tail}"));
        }
    }
    message
}

/// 子プロセスの出力 1 本を、別のタスクで読み溜める。
///
/// 読んだ分はすぐ列へ送ります。タスクを止めても、送った分は手元に残ります。
/// 捨てたときはタスクも止めます（呼んだ側が待つのをやめたとき、
/// 孫のプロセスが出力を握ったままでも残らないように）。
struct Collector {
    task: Option<JoinHandle<()>>,
    chunks: Option<mpsc::UnboundedReceiver<Vec<u8>>>,
    collected: Vec<u8>,
}

impl Collector {
    /// 読み始める。`quietly()` などで出力が無ければ何もしない。
    fn spawn<R>(pipe: Option<R>) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let Some(mut pipe) = pipe else {
            return Self {
                task: None,
                chunks: None,
                collected: Vec::new(),
            };
        };
        let (sender, chunks) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut buffer = vec![0u8; 8 * 1024];
            loop {
                match pipe.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if sender.send(buffer[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Self {
            task: Some(task),
            chunks: Some(chunks),
            collected: Vec::new(),
        }
    }

    /// 出力が閉じるまで待つ。
    async fn finished(&mut self) {
        if let Some(task) = &mut self.task {
            let _ = task.await;
            self.task = None;
        }
    }

    /// ここまでに届いた分を文字にする。UTF-8 として読めないバイトは置換文字にします。
    fn take_lossy(&mut self) -> String {
        if let Some(chunks) = &mut self.chunks {
            while let Ok(chunk) = chunks.try_recv() {
                self.collected.extend_from_slice(&chunk);
            }
        }
        String::from_utf8_lossy(&std::mem::take(&mut self.collected)).into_owned()
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// 終わったコマンドの結果。Laravel の `ProcessResult` に当たります。
#[derive(Debug, Clone)]
pub struct ProcessResult {
    program: String,
    exit_code: Option<i32>,
    successful: bool,
    output: String,
    error_output: String,
}

impl ProcessResult {
    /// 終了コードが 0 だったか。
    pub fn successful(&self) -> bool {
        self.successful
    }

    /// 終了コードが 0 でなかったか。
    pub fn failed(&self) -> bool {
        !self.successful
    }

    /// 終了コード。シグナルで止められたとき（Unix）は `None`。
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    /// 標準出力。UTF-8 として読めないバイトは置換文字（U+FFFD）にします。
    ///
    /// `quietly()` を付けたときは空です。
    pub fn output(&self) -> &str {
        &self.output
    }

    /// エラー出力。読み方は [`output`](Self::output) と同じです。
    pub fn error_output(&self) -> &str {
        &self.error_output
    }

    /// 失敗していたらエラーにする。成功していればそのまま返す。
    ///
    /// ```ignore
    /// let result = Process::run(["git", "pull"]).await?.throw()?;
    /// ```
    ///
    /// エラー文には、終了コードとエラー出力の**末尾**（2,000 文字まで）を入れます。
    pub fn throw(self) -> Result<Self> {
        if self.successful {
            return Ok(self);
        }
        let code = match self.exit_code {
            Some(code) => format!("終了コード {code}"),
            None => "シグナルで止められました".to_string(),
        };
        let tail = tail_chars(self.error_output.trim(), ERROR_TAIL_CHARS);
        let detail = if tail.is_empty() {
            "エラー出力はありません".to_string()
        } else {
            tail.to_string()
        };
        Err(Error::msg(format!(
            "`{}` が失敗しました（{code}）: {detail}",
            self.program
        )))
    }
}

/// 末尾の `limit` 文字。文字の途中では切りません。
fn tail_chars(text: &str, limit: usize) -> &str {
    match text.char_indices().rev().nth(limit.saturating_sub(1)) {
        Some((start, _)) if limit > 0 => &text[start..],
        _ if limit == 0 => "",
        _ => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// どの OS にもあるコマンド。テストを動かしている cargo そのものです。
    fn cargo() -> String {
        std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
    }

    /// しばらく終わらないコマンド。
    fn slow() -> Vec<&'static str> {
        if cfg!(windows) {
            vec!["ping", "-n", "30", "127.0.0.1"]
        } else {
            vec!["sleep", "30"]
        }
    }

    #[tokio::test]
    async fn 起動して出力を受け取れる() {
        let result = Process::run([cargo().as_str(), "--version"]).await.unwrap();
        assert!(result.successful());
        assert!(!result.failed());
        assert_eq!(result.exit_code(), Some(0));
        assert!(result.output().contains("cargo"), "{}", result.output());
    }

    #[tokio::test]
    async fn 終了コードが0でなくてもokで返り_throwでエラーになる() {
        let result = Process::run([cargo().as_str(), "--no-such-flag-for-bengara"])
            .await
            .unwrap();
        assert!(result.failed());
        assert_ne!(result.exit_code(), Some(0));
        assert!(!result.error_output().is_empty());

        let error = result.throw().unwrap_err().to_string();
        assert!(error.contains("失敗しました"), "{error}");
        assert!(error.contains("終了コード"), "{error}");
    }

    #[tokio::test]
    async fn 無いプログラムはエラーで名前を言う() {
        let error = Process::run(["bengara-no-such-program", "--secret=abc"])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("bengara-no-such-program"), "{error}");
        assert!(!error.contains("abc"), "引数は出さない: {error}");
    }

    #[tokio::test]
    async fn 空のコマンドはエラー() {
        let empty: [&str; 0] = [];
        assert!(Process::run(empty).await.is_err());
    }

    #[tokio::test]
    async fn 時間切れなら止めてエラーにする() {
        let started = std::time::Instant::now();
        let error = Process::timeout(1)
            .run(slow())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("1 秒で終わらなかった"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "30 秒待たずに返る: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn 時間切れのエラーにそれまでの出力の末尾を入れる() {
        // 書いてから止まるコマンド。以前は時間切れで出力ごと捨てていた。
        let command: Vec<&str> = if cfg!(windows) {
            vec![
                "cmd",
                "/C",
                "echo partial-output & echo partial-error 1>&2 & ping -n 30 127.0.0.1 >nul",
            ]
        } else {
            vec![
                "sh",
                "-c",
                "echo partial-output; echo partial-error >&2; sleep 30",
            ]
        };
        let started = std::time::Instant::now();
        let error = Process::timeout(2)
            .run(command)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("2 秒で終わらなかった"), "{error}");
        assert!(error.contains("partial-output"), "{error}");
        assert!(error.contains("partial-error"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "孫が出力を握っていても待たない: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn 時間切れの文は出力の末尾だけを入れる() {
        let limit = Duration::from_secs(3);
        assert_eq!(
            timed_out_message("x", limit, "", "  "),
            "`x` が 3 秒で終わらなかったので止めました"
        );
        let long = "a".repeat(ERROR_TAIL_CHARS + 10) + "END";
        let message = timed_out_message("x", limit, &long, "boom\n");
        assert!(message.contains("エラー出力の末尾: boom"), "{message}");
        assert!(message.contains("END"), "{message}");
        assert!(
            !message.contains(&"a".repeat(ERROR_TAIL_CHARS)),
            "末尾 {ERROR_TAIL_CHARS} 文字に切る"
        );
    }

    #[tokio::test]
    async fn 待つのをやめたら子プロセスも止まる() {
        // 呼んだ側の future を捨てる（キューの打ち切りと同じ）。
        let waited =
            tokio::time::timeout(Duration::from_millis(500), Process::forever().run(slow())).await;
        assert!(waited.is_err(), "打ち切られる");
        // 止まったことは OS に聞かないと分からないので、ここでは「戻ってくる」ことだけを見る。
        // 子プロセスが止まらないと、テストの終わりに 30 秒待たされる。
    }

    #[tokio::test]
    async fn 標準入力を渡せる() {
        // 標準入力をそのまま出すコマンド。Windows の `sort` は短い入力を
        // UTF-16 と読み違えるので使わない。
        let command: Vec<&str> = if cfg!(windows) {
            vec!["findstr", "^"]
        } else {
            vec!["cat"]
        };
        let result = Process::input("hello from stdin\n")
            .run(command)
            .await
            .unwrap();
        assert!(result.successful(), "{}", result.error_output());
        assert!(
            result.output().contains("hello from stdin"),
            "{:?}",
            result.output()
        );
    }

    #[tokio::test]
    async fn quietlyなら出力を受け取らない() {
        let result = Process::quietly()
            .run([cargo().as_str(), "--version"])
            .await
            .unwrap();
        assert!(result.successful());
        assert_eq!(result.output(), "");
    }

    #[tokio::test]
    async fn 環境変数と作業ディレクトリを渡せる() {
        let dir = std::env::temp_dir();
        let command: Vec<&str> = if cfg!(windows) {
            vec!["cmd", "/C", "echo %BENGARA_PROCESS_TEST% & cd"]
        } else {
            vec!["sh", "-c", "echo $BENGARA_PROCESS_TEST; pwd"]
        };
        // 値は ASCII にする。Windows の cmd はコードページで出力を変えるため。
        let result = Process::env("BENGARA_PROCESS_TEST", "yakisoba")
            .path(&dir)
            .run(command)
            .await
            .unwrap();
        assert!(result.successful(), "{}", result.error_output());
        assert!(result.output().contains("yakisoba"), "{}", result.output());
        let shown = result.output().to_ascii_lowercase();
        let last = dir
            .components()
            .next_back()
            .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        assert!(shown.contains(&last), "{shown} に {last} が無い");
    }

    #[test]
    fn timeoutの0は打ち切らない() {
        assert_eq!(Process::timeout(0).timeout, None);
        assert_eq!(Process::timeout(5).timeout, Some(Duration::from_secs(5)));
        assert_eq!(
            PendingProcess::default().timeout,
            Some(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        );
        assert_eq!(Process::forever().timeout, None);
    }

    #[test]
    fn エラー出力は末尾を残す() {
        assert_eq!(tail_chars("abcdef", 3), "def");
        assert_eq!(tail_chars("あいうえお", 2), "えお");
        assert_eq!(tail_chars("ab", 5), "ab");
        assert_eq!(tail_chars("ab", 0), "");
        assert_eq!(tail_chars("", 3), "");
    }
}
