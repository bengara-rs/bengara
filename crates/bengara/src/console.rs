//! 自作コマンド。`app/Console/Commands/` に置いた処理を `cargo artisan` から呼べます。
//!
//! ```ignore
//! // app/Console/Commands/Greet.rs
//! use bengara::prelude::*;
//!
//! pub const DESCRIPTION: &str = "名前を言って挨拶する";
//!
//! pub async fn handle(args: &[String]) -> Result<()> {
//!     let name = args.first().map(String::as_str).unwrap_or("世界");
//!     println!("こんにちは、{name} さん");
//!     Ok(())
//! }
//! ```
//!
//! ```sh
//! cargo artisan greet アリス
//! ```
//!
//! ファイル名がコマンド名になります（`Greet` → `greet`、`SendReport` → `send-report`）。
//! 一覧は `bengara-build` が作ります。登録は要りません。

use std::future::Future;
use std::pin::Pin;

use crate::error::Result;

/// コマンドが返す非同期の結果。
pub type CommandFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

/// 自作コマンド1つ。`bengara-build` が生成します。
#[derive(Clone, Copy)]
pub struct Command {
    /// 呼ぶときの名前（ファイル名を小文字とハイフンにしたもの）。
    pub name: &'static str,
    /// `cargo artisan list` に出す説明。
    pub description: &'static str,
    /// 中身。
    pub handle: fn(Vec<String>) -> CommandFuture,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Command")
            .field("name", &self.name)
            .field("description", &self.description)
            .finish()
    }
}

/// 名前でコマンドを探す。
pub(crate) fn find<'a>(commands: &'a [Command], name: &str) -> Option<&'a Command> {
    commands.iter().find(|c| c.name == name)
}

/// 自作コマンドの一覧を表で出す。
pub(crate) fn print_list(commands: &[Command]) {
    if commands.is_empty() {
        return;
    }
    println!("\nアプリの自作コマンド（app/Console/Commands/）\n");
    use crate::support::text;

    let width = commands
        .iter()
        .map(|c| text::width(c.name))
        .max()
        .unwrap_or(4)
        .max(4);
    for command in commands {
        println!(
            "  {}  {}",
            text::pad(command.name, width),
            command.description
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &[Command] = &[
        Command {
            name: "greet",
            description: "挨拶する",
            handle: |_| Box::pin(async { Ok(()) }),
        },
        Command {
            name: "send-report",
            description: "",
            handle: |_| Box::pin(async { Ok(()) }),
        },
    ];

    #[test]
    fn 名前で探せる() {
        assert_eq!(find(LIST, "greet").map(|c| c.name), Some("greet"));
        assert_eq!(
            find(LIST, "send-report").map(|c| c.name),
            Some("send-report")
        );
        assert!(find(LIST, "none").is_none());
        assert!(find(&[], "greet").is_none());
    }

    #[test]
    fn デバッグ表示に名前と説明が出る() {
        let text = format!("{:?}", LIST[0]);
        assert!(text.contains("greet"));
        assert!(text.contains("挨拶する"));
    }

    #[tokio::test]
    async fn 呼び出せる() {
        let command = find(LIST, "greet").unwrap();
        (command.handle)(vec!["アリス".to_string()]).await.unwrap();
    }
}
