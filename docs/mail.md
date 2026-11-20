# メール

文面を組み立てて送る仕組みです。

## 送る

```rust
use bengara::prelude::*;

Mail::to("alice@example.com")
    .subject("ようこそ")
    .text("登録ありがとうございます。")
    .send()
    .await?;
```

| 書き方                   | 意味                       |
|--------------------------|----------------------------|
| `Mail::to(宛先)`         | 最初の宛先。ここから始める |
| `also_to(宛先)`          | 宛先を足す                 |
| `cc(宛先)` / `bcc(宛先)` | 写し・隠した写し           |
| `from(差出人)`           | 書かなければ設定の値       |
| `reply_to(宛先)`         | 返信先                     |
| `subject(件名)`          | 件名                       |
| `text(本文)`             | 文字だけの本文             |
| `html(本文)`             | HTML の本文                |
| `send()`                 | 送る                       |

- `text` と `html` は両方書けます。受け取る側が選べる形で送ります。
- 文面は `format!` で組み立てます。
- 宛先は `alice@example.com` と `アリス <alice@example.com>` のどちらでも書けます。
- 送る前の中身は `message()` で見られます。

### 断るもの

`send()` は、送る前に次を確かめます。

| 断るもの                 | エラーの文                                         |
|--------------------------|----------------------------------------------------|
| 宛先が 1 つも無い        | `メールの宛先がありません`                         |
| `text` も `html` も無い  | `メールの本文がありません`                         |
| 宛先・差出人・件名に改行 | `メールの宛先・差出人・件名に改行は入れられません` |

- 改行を断るのは、ヘッダーに偽の行を混ぜられないようにするためです。本文の改行は通ります。
- 空白だけの宛先・写しは飛ばします（`.cc("")` でも落ちません）。
- **アドレスの形は確かめません。** 入力から受け取るなら、`email`
  の検査規則を通してください（[validation.md](validation.md)）。

### キューから送る

**メールはキューから送ってください。** 送信は待ち時間が長いので、画面の中では積むだけにします。

```rust
Queue::push("SendWelcome", &Payload { user_id: user.id }).await?;
```

送るのは `app/Jobs/SendWelcome.rs` の中です（[queue.md](queue.md)）。
送れなかったときは、キューの再挑戦に任せます。

## 送り先を選ぶ

`.env` の `MAIL_MAILER` で選びます（Laravel と同じ名前）。

| `MAIL_MAILER`          | すること                             | 要るもの                  |
|------------------------|--------------------------------------|---------------------------|
| `log`（既定）          | `storage/logs/mail.log` に書き出す   | 無し                      |
| `array`（`memory` も） | プロセスのメモリに溜める（テスト用） | 無し                      |
| `smtp`                 | SMTP サーバへ送る                    | Cargo の機能フラグ `mail` |

- 大文字小文字と前後の空白は見ません。
- `MAIL_MAILER` が無いか空なら、古い名前の `MAIL_DRIVER` を見ます。
- **知らない名前は、送るときにエラーになります。** 打ち間違いのまま、送れたつもりにならないためです。

### 差出人

| 設定                | 既定                  | 意味                                           |
|---------------------|-----------------------|------------------------------------------------|
| `MAIL_FROM_ADDRESS` | 空なら `MAIL_FROM`    | 差出人のアドレス（Laravel の名前。こちらが先） |
| `MAIL_FROM`         | `noreply@example.com` | 差出人のアドレス（前からの名前）               |
| `MAIL_FROM_NAME`    | `bengara`             | 差出人の名前                                   |

- **`MAIL_FROM_NAME` の既定は `bengara` です。** 自分のアプリ名を書いてください。
- アドレスの欄にはアドレスだけを書きます。`myapp <noreply@example.com>` と書くと、警告を出して `<>` の中だけを使います。
- `null`（大文字小文字は見ない）は空と同じです。Laravel の `.env` をそのまま使えます。

`config/mail.rs` を置くと、そちらを使います。無くても動きます。

```rust
// config/mail.rs
use bengara::prelude::*;

pub fn config() -> MailConfig {
    MailConfig {
        from: env("MAIL_FROM", "noreply@example.com"),
        from_name: env("MAIL_FROM_NAME", "myapp"),
        ..MailConfig::default()
    }
}
```

- **`..MailConfig::default()` を忘れないでください。** 欄が多いので、2 つだけ書くとコンパイルが通りません。
- `driver` を自分で書くと、`MAIL_MAILER` を見なくなります。既定に任せてください。

## ログに出たもの

`log` のときは、`storage/logs/mail.log` に足していきます。

```
---- 2026-10-05 10:20:01 ----
From: "myapp" <noreply@example.com>
To: alice@example.com
Subject: ようこそ

アリス さん、登録ありがとうございます。
```

- **自動では消えません。** 大きくなったら消してください。
- `storage/logs/` が無いときは、アプリのログへ出します。エラーにはなりません。
- `html` は `---- HTML ----` の行のあとに出ます。

## テスト

`#[bengara::test]` は、送り先を自動で `array` にします。溜まったメールを見て確かめます。

```rust
use bengara::queue::Json;

use crate::app::jobs::send_welcome::Payload;

#[bengara::test]
async fn お知らせを送る() {
    let _db = refresh_database().await;
    let _lock = exclusive().await;      // Mail::sent() はプロセスで 1 つ
    Mail::clear_sent();

    let mut user = User::register("アリス", "alice@example.com", "password123")
        .await
        .unwrap();
    user.save().await.unwrap();

    crate::app::jobs::send_welcome::handle(Json(Payload { user_id: user.id }))
        .await
        .unwrap();

    let sent = Mail::sent();            // Vec<Message>
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, ["alice@example.com"]);
    assert_eq!(sent[0].subject, "ようこそ");
}
```

- 溜まったメールは残り続けます。数える前に `Mail::clear_sent()` を呼びます。
- 溜まる場所はプロセスに 1 つです。`Mail::sent()` を見るテストは `exclusive()` の札を取ります。
- DB の札も取るときは、`refresh_database()` → `exclusive()` の順です（[testing.md](testing.md)）。
- `MAIL_MAILER=log cargo test` のように環境変数で渡すと、そちらを使います。
- `Message` で見られる欄は `to`・`cc`・`bcc`・`from`・`reply_to`・`subject`・`text`・`html` です。
- `User::register` はアプリの側の関数です（[authentication.md](authentication.md)）。

## SMTP で送る

Cargo の機能フラグ `mail` を足します。**既定では入りません。**
足すと、ビルドに C コンパイラが要ります（暗号化のため）。

```toml
[dependencies]
bengara = { version = "0.1", features = ["sqlite", "mail"] }
```

`.env` に送り先を書きます。送るコードは変えません。

```
MAIL_MAILER=smtp
MAIL_HOST=smtp.example.com
MAIL_ENCRYPTION=starttls
MAIL_USERNAME=noreply@example.com
MAIL_PASSWORD=ここにパスワード
MAIL_FROM_ADDRESS=noreply@example.com
MAIL_FROM_NAME=myapp
```

| 設定              | 既定                       | 意味                                      |
|-------------------|----------------------------|-------------------------------------------|
| `MAIL_HOST`       | `127.0.0.1`                | SMTP サーバの名前                         |
| `MAIL_PORT`       | 暗号化に合わせる（下の表） | ポート                                    |
| `MAIL_SCHEME`     | 空                         | Laravel 11 のつなぎ方（`smtp` / `smtps`） |
| `MAIL_ENCRYPTION` | `starttls`                 | 暗号化の仕方                              |
| `MAIL_USERNAME`   | 空（認証しない）           | 利用者名。`null` も空と同じ               |
| `MAIL_PASSWORD`   | 空                         | パスワード。`null` も空と同じ             |
| `MAIL_EHLO_NAME`  | 空（`[127.0.0.1]`）        | サーバに名乗る名前                        |
| `MAIL_TIMEOUT`    | 10                         | 返事を待つ上限の秒数                      |

フラグを足さずに `MAIL_MAILER=smtp` にすると、送るときに「features に `"mail"` を足してください」のエラーになります。

### 暗号化とポート

| `MAIL_ENCRYPTION`              | 既定のポート | つなぎ方                                              |
|--------------------------------|--------------|-------------------------------------------------------|
| `starttls`                     | 587          | 平文でつないで、途中で暗号化に切り替える              |
| `tls`（`ssl` も）              | 465          | 465 番なら最初から暗号化。ほかのポートでは `starttls` |
| `smtps`                        | 465          | ポートに関係なく、最初から暗号化する                  |
| `none`（`null` と `false` も） | 25           | 暗号化しない                                          |

- 大文字小文字は見ません。
- 空のときは `starttls` です。知らない語は、警告を出して `starttls` にします。
- 暗号化なしで `MAIL_USERNAME` を使うと、警告が出ます。パスワードが平文で流れるためです。

### `MAIL_SCHEME`（Laravel 11）

Laravel 11 の `.env` は、`MAIL_ENCRYPTION` の代わりに `MAIL_SCHEME` を書きます。 **書いてあれば `MAIL_ENCRYPTION`
より先に見ます。**

| `MAIL_SCHEME` | つなぎ方                                              |
|---------------|-------------------------------------------------------|
| 空・`null`    | `MAIL_ENCRYPTION` で決める（上の表）                  |
| `smtps`       | ポートに関係なく、最初から暗号化する                  |
| `smtp`        | 465 番なら最初から暗号化。ほかのポートでは `starttls` |
| ほかの語      | 警告を出して、`MAIL_ENCRYPTION` で決める              |

- `smtp` で `MAIL_ENCRYPTION=none`（`null`・`false` も）なら、暗号化しません。
- `MAIL_PORT` が空のとき、`smtp` は 587 番です（Laravel は 25 番）。
- **Laravel と違うところ:** Laravel の `smtp` は、サーバが `STARTTLS` を持たなければ平文で送ります。bengara
  は切り替えを必ず求めます。手元の受け口（Mailpit など）に送るときは、`MAIL_ENCRYPTION=none` を足してください。

### 知っておくこと

- `MAIL_EHLO_NAME` が空だと `[127.0.0.1]` と名乗ります。断るサーバには、自分のホスト名を書きます。
- 接続は 1 通目で張り、使い回します。
- 自己署名の証明書のサーバにはつながりません。OS に足した証明書も見ません。

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

- **効くのは 1 回目だけです。**
- `send` は `async` ではありません。待つ処理をそのまま書けます。

## 関連

- [queue.md](queue.md) — キュー。メールはここから送ります
- [events.md](events.md) — イベント
- [testing.md](testing.md) — テスト
- [configuration.md](configuration.md) — `.env` の一覧
