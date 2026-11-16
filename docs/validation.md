# 入力の検査（バリデーション）

送られてきた値が、決めた形になっているかを確かめます。Laravel の Validation に当たります。

## 入力を読む

| メソッド                           | 読むところ                   |
|------------------------------------|------------------------------|
| `req.input("title")`               | 本文とクエリ。**本文が優先** |
| `req.input_all()`                  | 同上。全部                   |
| `req.form("title")` / `form_all()` | フォームの本文だけ           |
| `req.query("q")` / `query_all()`   | クエリだけ                   |
| `req.json::<T>()`                  | 本文を型に読む               |

- 本文を読むのは、`Content-Type` がフォーム（`application/x-www-form-urlencoded`）か JSON のときです。
- JSON は最上位の値だけを読みます。数値や真偽値は、見たままの文字になります。入れ子は `req.json::<T>()` で読みます。
- 同じ名前が何個も届いたときは、最初の 1 個を使います。

## 検査する

```rust
pub async fn store(req: Request) -> Result<Response> {
    let input = req.validate(&[
        ("title", "required|max:255"),
        ("email", "required|email"),
        ("age",   "nullable|integer|between:0,150"),
    ])?;

    let title = input.get("title");
    let age: i64 = input.get_as("age").unwrap_or(0);
    // ...
}
```

落ちたら `?` で返すだけで **422** になります。

## 通った値

| メソッド             | 返すもの                                   |
|----------------------|--------------------------------------------|
| `get(field)`         | 値。無ければ空文字                         |
| `try_get(field)`     | 値。検査した項目に無ければ `None`          |
| `get_as::<T>(field)` | 型を変えた値。変えられなければエラー       |
| `all()`              | 全部の組                                   |
| `has(field)`         | 検査した項目に入っているか                 |

**入るのは、規則を書いた項目だけです。**

| 送ったもの       | 規則              | 入るもの     |
|------------------|-------------------|--------------|
| 何も送らない     | `max:10`          | **入らない** |
| 何も送らない     | `nullable`        | 空文字       |
| `age=""`         | `nullable`        | 空文字       |
| `password="   "` | `nullable\|min:8` | 空文字       |

`nullable` の項目は、送られてこなくても `has` が真になります。「送られてきたか」は分かりません。

### 前後の空白は落とす

値の前後の空白を落としてから検査します。Laravel の `TrimStrings` と同じです。

- `" 5 "` は `5` として検査し、`5` が入ります。間の空白は残ります。
- `req.input()` で直に読むと、送られてきたままです。
- `current_password` `password` `password_confirmation` の 3 つだけは落としません。
  そのため `password` の `min:8` は、前後の空白も 1 文字に数えます。

## 使える規則

規則は `|` でつなぎます。

| 分類       | 規則                                               |
|------------|----------------------------------------------------|
| 必須と省略 | `required`、`nullable`                             |
| 型         | `integer`（`int`）、`numeric`、`boolean`（`bool`） |
| 形         | `email`、`url`、`alpha`、`alpha_num`、`alpha_dash` |
| 大きさ     | `min:n`、`max:n`、`between:a,b`、`size:n`          |
| 値         | `in:a,b,c`、`starts_with:x`、`ends_with:x`         |
| 項目どうし | `confirmed`、`same:other`、`different:other`       |

| 規則         | 通るもの                                                       |
|--------------|----------------------------------------------------------------|
| `boolean`    | `1` `0` `true` `false` `on` `off` `yes` `no`                   |
| `numeric`    | 数として読める値。`inf` `NaN` `1e400` は通さない               |
| `email`      | `@` が 1 つあり、後ろに `.` がある。空白を含まない             |
| `url`        | `http://` か `https://` で始まる                               |
| `alpha` など | 「文字」かどうかを見る。**ひらがな・漢字も通る**（Laravel と同じ） |
| `confirmed`  | `password` に付けると、`password_confirmation` と同じかを見る  |

英数字だけに限りたいときは、自分で確かめます。

### min / max / between / size の数え方

**`integer` か `numeric` が付いているときだけ、値の大小で比べます。** 付いていなければ文字数です。

| 書き方                            | 送った値        | 結果                         |
|-----------------------------------|-----------------|------------------------------|
| `("age", "integer\|min:18")`      | `age=20`        | 通る（値が 18 以上）         |
| `("password", "required\|min:8")` | `password=9`    | **落ちる**（1 文字）         |
| `("title", "max:50")`             | `title=9999999` | **通る**（7 文字）           |

- 文字数はバイト数ではありません。`あいう` は 3 文字です。
- 数として読めない値には、大きさの規則は何も言いません。理由は「整数で入力してください」の 1 件だけです。

### 空文字も検査する

検査を飛ばすのは、次の 2 つのときだけです。

- 項目が送られてこなかった
- 空で、`nullable` が付いている

**空文字（`""`）が送られてきたら検査します。** `("role", "in:admin,user")` に `role=""` を送ると落ちます。
空でよい項目には `nullable` を付けます。

```rust
("role", "nullable|in:admin,user")
```

`required` の項目は、空文字でも空白だけでも「必ず入力してください」で落ちます。

## 落ちたとき

JSON を求める相手には、次の形で返します。

```json
{
  "message": "入力に誤りがあります。",
  "errors": {
    "email": ["email はメールアドレスの形で入力してください。"],
    "title": ["title は必ず入力してください。"]
  }
}
```

- 1 つの項目に理由が何件も付くことがあります。項目は名前の順に並びます。
- ほかの相手には、同じ理由を並べた画面を返します。決め方は [requests-and-responses.md](requests-and-responses.md) にあります。
- **メッセージは日本語で固定です。** 多言語の仕組み（`resources/lang/`）とはつながっていません。
  言い回しを変えたいときは、下の「自分でエラーを組み立てる」を使います。

### 前の入力を覚えている

セッションを使っていれば、落ちた入力を次のリクエストで読めます。

```rust
req.session().old("title")   // 落ちたときに送られてきた title
```

次のときは覚えません。

| 場合                                 | 理由                                         |
|--------------------------------------|----------------------------------------------|
| セッションの Cookie を持たない相手   | 壊れた入力だけでセッションを増やされないため |
| 入力の合計が 64 KiB を超える         | 大きな入力をセッションに書かないため         |
| 秘密になりうる名前の項目             | 下の「覚えない項目」                         |

## 自分でエラーを組み立てる

DB を見る確認（重複など）は、自分で書いて足します。

```rust
let mut errors = ValidationErrors::default();
errors.add("title", "すでに同じ題名があります。");
if !errors.is_empty() {
    return Err(Error::Validation(Box::new(errors)));
}
```

ほかに `has(field)` `get(field)` `first()` `all()` と、`len()`（項目の数）・`total()`（理由の総数）があります。

## 規則の書き間違いは 500 になる

**書き間違えた規則は、黙って通しません。** 知らない規則名なら、使える名前の一覧がメッセージに出ます。

| 書き間違い           | 理由                   |
|----------------------|------------------------|
| `maxx:10`            | 知らない規則名         |
| `min`                | 引数を忘れた           |
| `max:nan` `min:inf`  | 数として使えない       |
| `size:-3` `size:1.5` | `size` は 0 以上の整数 |
| `between:10,1`       | 下限が上限より大きい   |
| `in:` `in:,,`        | 許す値が 1 つも無い    |

`in:` の空の値は捨てます。`in:admin,user,` でも、`role=`（空）は通りません。

## 覚えない項目

次の語を名前に含む項目は、`old()` で覚えません。セッションのファイルに平文で残さないためです。
ログで `***` に伏せる項目も同じです。

```
password  passwd  pwd  pass  passphrase  secret  token  key  apikey
credential  credentials  cvv  cvc  csc  card  ssn  otp  totp  pin
code  seed  mnemonic  recovery  backup  answer  signature
```

- 名前を `_` `-` `.` `[` `]` 空白と、小文字から大文字へ変わる所で語に切ります。
- 語の末尾の数字を落としてから、一覧と比べます。大文字小文字は区別しません。

| 名前                                     | 覚えるか |
|------------------------------------------|----------|
| `new_password_confirmation` `password1`  | 覚えない |
| `apiKey`（`api` と `Key` に切れる）      | 覚えない |
| `zip_code` `country_code`                | 覚えない |
| `shipping_address` `keyword`             | 覚える   |

`zip_code` も覚えないので、落ちたフォームを出し直すと郵便番号の欄は空に戻ります。

まだ無い機能は [backlog.md](backlog.md) にまとめています。

## 関連

- [requests-and-responses.md](requests-and-responses.md)
- [session.md](session.md)
