# メール

文面を組み立てて送ります。

> **SMTP はまだありません。** いま送れるのは「ログに書き出す」と「メモリに溜める」の
> 2 つだけです。TLS を自前で書けないためです。差し替えられる形（`Mailer`）は
> 用意してあるので、決まれば足せます。

## 送り先

`.env` に書きます。下は「自分のアプリ名を入れた」例です。

```
MAIL_DRIVER=log
MAIL_FROM=noreply@example.com
MAIL_FROM_NAME=myapp
```

書かなかったときに使われる値は次のとおりです。

| 設定             | 既定                  | 意味             |
|------------------|-----------------------|------------------|
| `MAIL_DRIVER`    | `log`                 | どこへ送るか     |
| `MAIL_FROM`      | `noreply@example.com` | 差出人のアドレス |
| `MAIL_FROM_NAME` | `bengara`             | 差出人の名前     |

**`MAIL_FROM_NAME` の既定は `bengara` です。** 自分のアプリ名にしたいときは、上の例のように
`.env` へ書いてください。

| `MAIL_DRIVER` | 何をするか                             |
|---------------|----------------------------------------|
| `log`（既定） | `storage/logs/mail.log` に書き出す     |
| `array`       | プロセスのメモリに溜める。**テスト用** |

`config/mail.rs` を置けば `.env` より優先します。無くても動きます。

```rust
// config/mail.rs
use bengara::prelude::*;

pub fn config() -> MailConfig {
    MailConfig {
        driver: env("MAIL_DRIVER", "log"),
        from: env("MAIL_FROM", "noreply@example.com"),
        // 第2引数は「このアプリでの既定」です。bengara の既定は `bengara` です。
        from_name: env("MAIL_FROM_NAME", "myapp"),
    }
}
```

## 送る

```rust
use bengara::prelude::*;

Mail::to("alice@example.com")
.subject("ようこそ")
.text("登録ありがとうございます。")
.send()
.await?;
```

| 組み立て                 | 内容                       |
|--------------------------|----------------------------|
| `to(宛先)`               | 最初の宛先。ここから始める |
| `also_to(宛先)`          | 宛先を足す                 |
| `cc(宛先)` / `bcc(宛先)` | 写し・隠した写し           |
| `from(差出人)`           | 書かなければ設定の値       |
| `reply_to(宛先)`         | 返信先                     |
| `subject(件名)`          | 件名                       |
| `text(本文)`             | 文字だけの本文             |
| `html(本文)`             | HTML の本文                |
| `send()`                 | 送る                       |

`text` と `html` の両方を書いても構いません。`log` はどちらも書き出します。

## ログに出たもの

```
---- 2026-10-05 10:20:01 ----
From: myapp <noreply@example.com>
To: alice@example.com
Subject: ようこそ

アリス さん、登録ありがとうございます。
```

`storage/logs/mail.log` に追記されます。 **自動では消えません。** 大きくなったら消してください。

## 断るもの

`send()` は、送る前に 2 つだけ確かめます。

| 断るもの                   | メッセージ                 |
|----------------------------|----------------------------|
| 宛先が空（空白だけも同じ） | `メールの宛先がありません` |
| `text` も `html` も無い    | `メールの本文がありません` |

**アドレスの形は確かめません。** 入力から受け取るなら `email` の検査規則を通してください
（[validation.md](validation.md)）。

## 送る前に中身を見る

```rust
let mail = Mail::to("alice@example.com").subject("ようこそ").text("本文");
let message: & Message = mail.message();
assert_eq!(message.to, ["alice@example.com"]);
mail.send().await?;
```

## キューから送る

**メールはキューから送るのがよいです。** 送信は待ち時間が長いので、
画面の中で待つと応答が遅くなります。

```rust
// 画面の中
Queue::push("SendWelcome", & Payload { user_id: user.id }).await?;
```

```rust
// app/Jobs/SendWelcome.rs
pub async fn handle(payload: String) -> Result<()> {
    let payload: Payload = bengara::serde_json::from_str(&payload)?;
    let Some(user) = User::find(payload.user_id).await? else {
        return Ok(());
    };
    Mail::to(&user.email).subject("ようこそ").text("...").send().await
}
```

## テスト

`#[bengara::test]` は自動で `array` にします。 **テストでは本当には送りません。**

```rust
#[bengara::test]
async fn お知らせを送る() {
    Mail::clear_sent();

    crate::app::jobs::send_welcome::handle(r#"{"user_id":1}"#.to_string())
        .await
        .unwrap();

    let sent = Mail::sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, ["alice@example.com"]);
    assert_eq!(sent[0].subject, "ようこそ");
}
```

| メソッド             | 返るもの       |
|----------------------|----------------|
| `Mail::sent()`       | `Vec<Message>` |
| `Mail::clear_sent()` | –              |

溜まったメールは **テストの間ずっと残ります。** 数を確かめる前に `clear_sent()` を呼んでください。

溜まる場所はプロセス共通です。テストは並んで走るので、 **`Mail::sent()` で数を確かめるテストは
`bengara::testing::exclusive()` で札を取ってください**（[testing.md](testing.md)）。

`MAIL_DRIVER` を **本物の環境変数**で渡したときは、そちらを使います
（`.env` の値はテストに効きません）。

```sh
MAIL_DRIVER=log cargo test
```

## 送り先を自分で作る

`Mailer` を実装して、起動時に入れ替えます。

```rust
bengara::mail::install_mailer(Box::new(MyMailer));
```

**効くのは 1 回目だけです。** 2 回目以降は何もせず、警告をログに出します。
`bootstrap/app.rs` の中など、1 か所から 1 回だけ呼んでください。

## 無いもの

| 項目                  | 代わりにすること       |
|-----------------------|------------------------|
| SMTP での送信         | ありません             |
| 添付ファイル          | ありません             |
| Markdown の文面       | `html` に自分で入れる  |
| テンプレート（Blade） | `format!` で組み立てる |
| 通知（Notification）  | `Mail` を直接使う      |

## 関連

- [queue.md](queue.md) — キュー。メールはここから送るのがよい形です
- [events.md](events.md) — イベント
- [testing.md](testing.md) — テスト
