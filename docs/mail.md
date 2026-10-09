# メール

文面を組み立てて送る仕組みです。

送り先は 3 つです。

| `MAIL_DRIVER` | 何をするか                             | 要るもの                   |
|---------------|----------------------------------------|----------------------------|
| `log`（既定） | `storage/logs/mail.log` に書き出す     | 無し                       |
| `array`       | プロセスのメモリに溜める。**テスト用** | 無し                       |
| `smtp`        | SMTP サーバへ送る                      | Cargo の機能フラグ `mail`  |

送り先の差し替え口（`Mailer`）もあります（下の「送り先を自分で作る」）。

## 設定

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

`smtp` のときは次も見ます。`log` と `array` では使いません。

| 設定              | 既定                 | 意味                              |
|-------------------|----------------------|-----------------------------------|
| `MAIL_HOST`       | `127.0.0.1`          | SMTP サーバの名前                 |
| `MAIL_PORT`       | 0（下の表のとおり）  | ポート                            |
| `MAIL_ENCRYPTION` | `starttls`           | 暗号化の仕方（`tls` / `starttls` / `none`） |
| `MAIL_USERNAME`   | 空（認証しない）     | 利用者名                          |
| `MAIL_PASSWORD`   | 空                   | パスワード                        |
| `MAIL_EHLO_NAME`  | 空（`localhost`）    | `EHLO` で名乗る名前               |
| `MAIL_TIMEOUT`    | 10                   | 返事を待つ上限の秒数              |

`MAIL_PORT` を書かないと、暗号化に合わせて決めます。

| `MAIL_ENCRYPTION` | 既定のポート | つなぎ方                          |
|-------------------|--------------|-----------------------------------|
| `tls`（`ssl` も） | 465          | つないだ直後から暗号化する        |
| `starttls`        | 587          | 平文でつないで `STARTTLS` で切り替える |
| `none`（`null` も） | 25         | 暗号化しない                      |

`MAIL_ENCRYPTION` に知らない語を書いたときは `starttls` にします。暗号化する側に倒します。

**`MAIL_FROM_NAME` の既定は `bengara` です。** 自分のアプリ名にしたいときは、上の例のように
`.env` へ書いてください。

`config/mail.rs` を置くと `.env` より優先します。無くても動きます。

```rust
// config/mail.rs
use bengara::prelude::*;

pub fn config() -> MailConfig {
    MailConfig {
        driver: env("MAIL_DRIVER", "log"),
        from: env("MAIL_FROM", "noreply@example.com"),
        // 第2引数は「このアプリでの既定」です。bengara の既定は `bengara` です。
        from_name: env("MAIL_FROM_NAME", "myapp"),
        // 残りは既定のまま（`.env` の MAIL_HOST などを見ます）。
        ..MailConfig::default()
    }
}
```

**`..MailConfig::default()` を忘れないでください。** `MailConfig` には SMTP 用の欄が
あるので、3 つだけ書くとコンパイルが通りません。

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

送る前に中身を見られます。

```rust
let mail = Mail::to("alice@example.com").subject("ようこそ").text("本文");
let message: &Message = mail.message();
assert_eq!(message.to, ["alice@example.com"]);
mail.send().await?;
```

## 断るもの

`send()` は、送る前に 2 つだけ確かめます。

| 断るもの                   | メッセージ                 |
|----------------------------|----------------------------|
| 宛先が空（空白だけも同じ） | `メールの宛先がありません` |
| `text` も `html` も無い    | `メールの本文がありません` |

**アドレスの形は確かめません。** 入力から受け取るなら `email` の検査規則を通してください
（[validation.md](validation.md)）。

## ログに出たもの

```
---- 2026-10-05 10:20:01 ----
From: myapp <noreply@example.com>
To: alice@example.com
Subject: ようこそ

アリス さん、登録ありがとうございます。
```

`storage/logs/mail.log` に追記されます。 **自動では消えません。** 大きくなったら消してください。

## キューから送る

**メールはキューから送るのがよいです。** 送信は待ち時間が長いので、
画面の中で待つと応答が遅くなります。

```rust
// 画面の中
Queue::push("SendWelcome", &Payload { user_id: user.id }).await?;
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

`#[bengara::test]` は自動で `array` にします。溜まったメールを数えて確かめます。

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

気をつけることが 3 つあります。

- 溜まったメールは **テストの間ずっと残ります。** 数を確かめる前に `clear_sent()` を呼んでください。
- 溜まる場所はプロセス共通です。テストは並んで走るので、 **`Mail::sent()` で数を確かめるテストは
  `bengara::testing::exclusive()` で札を取ってください**（[testing.md](testing.md)）。
- `MAIL_DRIVER` を **本物の環境変数**で渡したときは、そちらを使います
  （`.env` の値はテストに効きません）。

```sh
MAIL_DRIVER=log cargo test
```

## SMTP で送る

Cargo の機能フラグ `mail` を付けます。 **既定では入りません。**

```toml
[dependencies]
bengara = { version = "0.1", features = ["sqlite", "mail"] }
```

`.env` に送り先を書きます。

```
MAIL_DRIVER=smtp
MAIL_HOST=smtp.example.com
MAIL_ENCRYPTION=starttls
MAIL_USERNAME=noreply@example.com
MAIL_PASSWORD=ここにパスワード
MAIL_FROM=noreply@example.com
MAIL_FROM_NAME=myapp
```

送り方は `log` と同じです。書き換えるのは `.env` だけです。

```rust
Mail::to("alice@example.com")
    .subject("ようこそ")
    .text("登録ありがとうございます。")
    .send()
    .await?;
```

### 知っておくこと

- **つなぐのは1通目を送るときです。** 起動時にはつなぎません。接続は溜めておくので、
  2通目から暗号化の握り直しが起きません。
- **TLS は rustls を使います。** OS の証明書置き場は見ません。自分で建てた
  証明書（自己署名）のサーバにはつながりません。
- `text` と `html` の両方を書くと `multipart/alternative` になります。読む側が選びます。
- 差出人と宛先は `alice@example.com` と `アリス <alice@example.com>` の両方の形を
  受け付けます。読めないときは、どの欄のどの文字列かを添えて断ります。
- **フラグを付けずに `MAIL_DRIVER=smtp` にすると、送るときにエラーになります。**
  黙って `log` には落ちません。送れたつもりになるのを防ぐためです。

  ```
  SMTP で送るには Cargo.toml を `bengara = { version = "0.1", features = ["mail"] }` にしてください
  ```

- `MAIL_ENCRYPTION=none` のまま `MAIL_USERNAME` を使うと、警告をログに出します。
  利用者名とパスワードが暗号化されずに流れるためです。

### ビルドに要るもの

`mail` を付けると、暗号化のために C コンパイラが要ります（rustls が使う `ring`）。

| OS      | 用意するもの                                    |
|---------|-------------------------------------------------|
| Linux   | `cc`（`build-essential` など）                  |
| macOS   | Xcode Command Line Tools                        |
| Windows | Visual Studio の C++ ビルドツール               |

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
| 添付ファイル          | ありません             |
| 自己署名の証明書      | ありません（rustls が断ります） |
| 送れなかったときの再送 | キューの再試行に任せる（[queue.md](queue.md)） |
| Markdown の文面       | `html` に自分で入れる  |
| テンプレート（Blade） | `format!` で組み立てる |
| 通知（Notification）  | `Mail` を直接使う      |

## 関連

- [queue.md](queue.md) — キュー。メールはここから送るのがよい形です
- [events.md](events.md) — イベント
- [testing.md](testing.md) — テスト
