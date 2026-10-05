# 多言語

画面に出す文字を、言語ごとに分けて持つ仕組みです。

## 文字を置く

`resources/lang/<言語>.toml` に置きます。言語の名前はファイル名です。

```toml
# resources/lang/ja.toml
[messages]
welcome = "ようこそ"
greeting = "こんにちは、:name さん"

[posts]
created = "記事を作りました"
not_found = "その記事はありません"
```

```toml
# resources/lang/en.toml
[messages]
welcome = "Welcome"
greeting = "Hello, :name"
```

**ビルド時に読み込んで、バイナリに入ります。** 実行時にファイルを読みません。
中身を書き換えると `cargo build` が自動で走り直します。

## 使う

```rust
use bengara::prelude::*;

__("messages.welcome");                                 // ようこそ
__with("messages.greeting", & [("name", "アリス")]);     // こんにちは、アリス さん
```

鍵は `節.鍵` の形です。`__` は Laravel と同じ名前です。

## 言語を切り替える

```
APP_LOCALE=ja
APP_FALLBACK_LOCALE=ja
```

| 設定                  | 役割                           |
|-----------------------|--------------------------------|
| `APP_LOCALE`          | 最初の言語                     |
| `APP_FALLBACK_LOCALE` | その言語に鍵が無いときに見る先 |

途中でも変えられます。切り替え方は 2 つです。

| 書き方                   | 効く範囲                     | 使う場面                       |
|--------------------------|------------------------------|--------------------------------|
| `Lang::with(言語, 処理)` | 渡した処理の中だけ           | **リクエストごとに変えるとき** |
| `Lang::set(言語)`        | **プロセス全体**（戻らない） | 起動時に 1 回だけ決めるとき    |

```rust
Lang::current();        // "ja"
Lang::available();      // ["en", "ja"]
Lang::has("fr");        // false
```

### リクエストごとに変える

`Lang::with` を使います。処理が終わると元の言語に戻ります。

```rust
pub async fn show(req: Request) -> Result<Response> {
    let locale = req.query("locale").unwrap_or_else(Lang::current);

    Lang::with(locale, async {
        json(&bengara::serde_json::json!({ "welcome": __("messages.welcome") }))
    })
        .await
}
```

**`Lang::set` をリクエストの中で呼ばないでください。** プロセス全体に効くので、
同時に来た別のリクエストの言語も変わります。

## 鍵が無いとき

**鍵の文字がそのまま返ります。** 落ちません。警告がログに出ます。

```rust
__("messages.ない鍵");   // => "messages.ない鍵"
```

画面が真っ白になるより、鍵が見えるほうが直しやすいためです。

探す順番は次のとおりです。

1. いまの言語（`Lang::current()`）
2. `APP_FALLBACK_LOCALE` の言語
3. 鍵そのもの

## 差し替えの書き方

`:name` の形です。値は前後の空白を含めてそのまま入ります。

```toml
greeting = "こんにちは、:name さん"
count = ":total 件のうち :done 件が終わりました"
name_both = ":name（:name_kanji）"
```

```rust
__with("messages.count", & [("total", "10"), ("done", "3")]);
```

**差し替えは長い名前から順に当てます。** `:name` と `:name_kanji` のように
片方がもう片方の頭と同じでも、短いほうが先に当たって壊れることはありません。

**エスケープはしません。** HTML に出すときは `escape_html` を通してください。

## 読めない TOML

必要な分だけ読む作りです。読めるのは次だけです。

| 読める                              | 読まない                        |
|-------------------------------------|---------------------------------|
| `[節]`。`[a.b]` は `a.b.鍵` になる  | 配列（`[1, 2]`）                |
| `鍵 = "値"` / `鍵 = '値'`           | 引用符なしの値（数・真偽）      |
| `"` の中の `\n` `\t` `\r` `\\` `\"` | 複数行の文字列（`"""`）         |
| `#` から行末のコメント              | インラインのテーブル（`{ … }`） |

節の外に書いた鍵は、そのままの鍵になります（`title = "x"` → `title`）。

**読めない行があるとビルドが止まります。** 行番号と理由を出すので、その行を直してください。

**同じ鍵を 2 回書いてもビルドが止まります。** どちらが勝つか分からない状態にしません。

```toml
[messages]
welcome = "ようこそ"
welcome = "いらっしゃい"   # => resources/lang/ja.toml に鍵 `messages.welcome` が2回書かれています
```

## 一覧を見る

```sh
cargo artisan lang:list
```

```
いまの言語: ja
  en
  ja
```

## 無いもの

| 項目                     | 代わりにすること                       |
|--------------------------|----------------------------------------|
| 複数形（`trans_choice`） | 鍵を分ける（`one` / `many`）           |
| JSON の言語ファイル      | TOML を使う                            |
| 実行時の読み込み         | 再ビルドする                           |
| 検査の文言の差し替え     | ありません（検査の文言は日本語で固定） |

## 関連

- [configuration.md](configuration.md) — `.env` と `config/*.rs`
- [directory-structure.md](directory-structure.md) — ファイルの自動検出の規則
