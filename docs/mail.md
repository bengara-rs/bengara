# メール

文面を組み立てて送る仕組みです。
このページで、送り方・設定・テストでの確かめ方・SMTP の使い方が分かります。

送り先は `.env` の `MAIL_DRIVER` で選びます。

| `MAIL_DRIVER`          | 何をするか                             | 要るもの                  |
|------------------------|----------------------------------------|---------------------------|
| `log`（既定）          | `storage/logs/mail.log` に書き出す     | 無し                      |
| `array`（`memory` も） | プロセスのメモリに溜める。**テスト用** | 無し                      |
| `smtp`                 | SMTP サーバへ送る                      | Cargo の機能フラグ `mail` |

知らない名前を書いたときは、警告をログに出して `log` にします。

## 送る

```rust
use bengara::prelude::*;

Mail::to("alice@example.com")
    .subject("ようこそ")
    .text("登録ありがとうございます。")
    .send()
    .await?;
```

| 組み立て                 | 内容                         |
|--------------------------|------------------------------|
| `Mail::to(宛先)`         | 最初の宛先。ここから始める   |
| `also_to(宛先)`          | 宛先を足す                   |
| `cc(宛先)` / `bcc(宛先)` | 写し・隠した写し             |
| `from(差出人)`           | 書かなければ設定の値         |
| `reply_to(宛先)`         | 返信先                       |
| `subject(件名)`          | 件名                         |
| `text(本文)`             | 文字だけの本文               |
| `html(本文)`             | HTML の本文                  |
| `send()`                 | 送る                         |

- `text` と `html` は、両方書いても構いません。
- 文面は `format!` で組み立てて、`text` か `html` に渡します。
- 送る前の中身は `message()` で見られます（返るのは `&Message`）。

### 断るもの

`send()` は、送る前に 3 つを確かめます。

| 断るもの                            | エラーの文                                         |
|-------------------------------------|----------------------------------------------------|
| 宛先が 1 つも無い（空白だけも同じ） | `メールの宛先がありません`                         |
| `text` も `html` も無い             | `メールの本文がありません`                         |
| 宛先・差出人・件名に改行            | `メールの宛先・差出人・件名に改行は入れられません` |

- 改行を断るのは、**ヘッダーに偽の行を混ぜられないようにするため**です。
  写しと返信先も確かめます。本文の改行はそのまま通ります。
- **空白だけの宛先・写しは飛ばします。** `.cc("")` と書いても落ちません。
- **アドレスの形は確かめません。** 入力から受け取るなら、`email` の検査規則を通してください
  （[validation.md](validation.md)）。

### キューから送る

**メールはキューから送ってください。** 送信は待ち時間が長いので、画面の中で待つと応答が遅くなります。
画面の中では積むだけにします。

```rust
Queue::push("SendWelcome", &Payload { user_id: user.id }).await?;
```

送るのは `app/Jobs/SendWelcome.rs` の中です（[queue.md](queue.md)）。
送れなかったときのやり直しも、キューの再挑戦に任せます。

## 設定

`.env` に書きます。

```
MAIL_DRIVER=log
MAIL_FROM=noreply@example.com
MAIL_FROM_NAME=myapp
```

| 設定             | 既定                  | 意味             |
|------------------|-----------------------|------------------|
| `MAIL_DRIVER`    | `log`                 | どこへ送るか     |
| `MAIL_FROM`      | `noreply@example.com` | 差出人のアドレス |
| `MAIL_FROM_NAME` | `bengara`             | 差出人の名前     |

**`MAIL_FROM_NAME` の既定は `bengara` です。** 自分のアプリ名を書いてください。

`config/mail.rs` を置くと、そちらを使います。無くても動きます。

```rust
// config/mail.rs
use bengara::prelude::*;

pub fn config() -> MailConfig {
    MailConfig {
        driver: env("MAIL_DRIVER", "log"),
        from: env("MAIL_FROM", "noreply@example.com"),
        from_name: env("MAIL_FROM_NAME", "myapp"),
        // 残りは既定のまま（`.env` の MAIL_HOST などを見ます）。
        ..MailConfig::default()
    }
}
```

**`..MailConfig::default()` を忘れないでください。** SMTP 用の欄があるので、3 つだけ書くとコンパイルが通りません。

## ログに出たもの

`log` のときは、`storage/logs/mail.log` に追記します。

```
---- 2026-10-05 10:20:01 ----
From: "myapp" <noreply@example.com>
To: alice@example.com
Subject: ようこそ

アリス さん、登録ありがとうございます。
```

- **自動では消えません。** 大きくなったら消してください。
- `storage/logs/` が無いときは、ファイルに書かずにアプリのログへ出します。エラーにはなりません。
- 差出人の名前は、引用符で囲んで出ます。`.from(..)` で自分で書いた差出人は、書いたまま出ます。
- `html` を書いたときは、`---- HTML ----` の行のあとに続けて出ます。

## テスト

`#[bengara::test]` は、送り先を自動で `array` にします。溜まったメールを見て確かめます。

```rust
#[bengara::test]
async fn お知らせを送る() {
    let _db = refresh_database().await;
    let _lock = exclusive().await;      // Mail::sent() はプロセスで 1 つ
    Mail::clear_sent();

    // 利用者がいないと、このジョブは何もせずに終わる。先に作る
    let mut user = User::register("アリス", "alice@example.com", "password123")
        .await
        .unwrap();
    user.save().await.unwrap();

    let payload = bengara::serde_json::json!({ "user_id": user.id }).to_string();
    crate::app::jobs::send_welcome::handle(payload).await.unwrap();

    let sent = Mail::sent();            // Vec<Message>
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, ["alice@example.com"]);
    assert_eq!(sent[0].subject, "ようこそ");
}
```

`User::register` はアプリの側の関数です（[authentication.md](authentication.md)）。

| 気をつけること                 | 内容                                                               |
|--------------------------------|--------------------------------------------------------------------|
| 溜まったメールは残り続ける     | 数を確かめる前に `Mail::clear_sent()` を呼ぶ                       |
| 溜まる場所はプロセスに 1 つ    | `Mail::sent()` を見るテストは `exclusive()` の札を取る             |
| DB の札も要るとき              | `refresh_database()` → `exclusive()` の順で取る（[testing.md](testing.md)） |
| `MAIL_DRIVER` を環境変数で渡した | そちらを使う（`MAIL_DRIVER=log cargo test`）。`.env` の値は効かない |

`Message` で見られる欄は、`to`・`cc`・`bcc`・`from`・`reply_to`・`subject`・`text`・`html` です。

## SMTP で送る

Cargo の機能フラグ `mail` を足します。**既定では入りません。**

```toml
[dependencies]
bengara = { version = "0.1", features = ["sqlite", "mail"] }
```

`.env` に送り先を書きます。**送るコードは変えません。**

```
MAIL_DRIVER=smtp
MAIL_HOST=smtp.example.com
MAIL_ENCRYPTION=starttls
MAIL_USERNAME=noreply@example.com
MAIL_PASSWORD=ここにパスワード
MAIL_FROM=noreply@example.com
MAIL_FROM_NAME=myapp
```

| 設定              | 既定                      | 意味                                        |
|-------------------|---------------------------|---------------------------------------------|
| `MAIL_HOST`       | `127.0.0.1`               | SMTP サーバの名前                           |
| `MAIL_PORT`       | 暗号化に合わせる（下の表） | ポート                                     |
| `MAIL_ENCRYPTION` | `starttls`                | 暗号化の仕方                                |
| `MAIL_USERNAME`   | 空（認証しない）          | 利用者名                                    |
| `MAIL_PASSWORD`   | 空                        | パスワード                                  |
| `MAIL_EHLO_NAME`  | 空                        | サーバに名乗る名前（下の「名乗る名前」）    |
| `MAIL_TIMEOUT`    | 10                        | 返事を待つ上限の秒数                        |

### 暗号化とポート

| `MAIL_ENCRYPTION`              | 既定のポート | つなぎ方                               |
|--------------------------------|--------------|----------------------------------------|
| `tls`（`ssl` と `smtps` も）   | 465          | つないだ直後から暗号化する             |
| `starttls`                     | 587          | 平文でつないで、途中で暗号化に切り替える |
| `none`（`null` と `false` も） | 25           | 暗号化しない                           |

- **行を書かなければ `starttls` です。`MAIL_ENCRYPTION=` と空にすると、暗号化なしになります。**
  空にすると、利用者名とパスワードが平文で流れます。
- 暗号化なしのまま `MAIL_USERNAME` を使うと、警告をログに出します。
- 知らない語を書いたときは、警告をログに出して `starttls` にします。

### 名乗る名前

`MAIL_EHLO_NAME` が空のときは、`[127.0.0.1]` と名乗ります。
**これを断るサーバがあります。** 断られたら、自分のホスト名（`app.example.com` など）を書いてください。

### 知っておくこと

| こと                 | 内容                                                                     |
|----------------------|--------------------------------------------------------------------------|
| つなぐとき           | 1 通目を送るとき。接続は使い回す                                         |
| 証明書               | 自分で作った証明書（自己署名）のサーバにはつながらない。OS に足した証明書も見ない |
| `text` と `html` の両方 | 受け取った側が選べる形（`multipart/alternative`）で送る               |
| 宛先の書き方         | `alice@example.com` と `アリス <alice@example.com>` のどちらも読む       |
| 読めないアドレス     | どの欄のどの文字列かを添えたエラーになる                                 |

**フラグを足さずに `MAIL_DRIVER=smtp` にすると、送るときに次のエラーになります。**
黙って `log` には切り替えません。

```
SMTP で送るには、Cargo.toml の bengara の features に `"mail"` を足してください（例: `features = ["sqlite", "mail"]`）
```

足すだけです。`sqlite` などの DB のフラグは消さないでください。

### ビルドに要るもの

`mail` を足すと、暗号化のために C コンパイラが要ります。

| OS      | 用意するもの                      |
|---------|-----------------------------------|
| Linux   | `cc`（`build-essential` など）    |
| macOS   | Xcode Command Line Tools          |
| Windows | Visual Studio の C++ ビルドツール |

## 送り先を自分で作る

`Mailer` を実装して、起動時に入れ替えます。書くのは `send` 1 つです。

```rust
struct MyMailer;

impl bengara::mail::Mailer for MyMailer {
    fn send(&self, message: &Message) -> Result<()> { /* ... */ }
}

// bootstrap/app.rs の中で
bengara::mail::install_mailer(Box::new(MyMailer));
```

**効くのは 1 回目だけです。** 2 回目以降は何もせず、警告をログに出します。
`send` は `async` ではありません。待つ処理をそのまま書けます。

## できないこと

| できないこと                          | 代わりにすること                          |
|---------------------------------------|-------------------------------------------|
| テンプレートや Markdown で文面を書く  | `format!` で組み立てる                    |
| 通知（Notification）                  | `Mail` を直接使う                         |
| ファイルを添付する                    | ありません。リンクを本文に書く            |
| 送れなかったメールを自動で送り直す    | キューから送り、再挑戦に任せる            |

まだ無い機能の一覧は [backlog.md](backlog.md) にあります。

## 関連

- [queue.md](queue.md) — キュー。メールはここから送ります
- [events.md](events.md) — イベント
- [testing.md](testing.md) — テスト
- [configuration.md](configuration.md) — `.env` の一覧
