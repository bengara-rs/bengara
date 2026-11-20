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

### `rules!` で書くとコンパイルのときに確かめる

```rust
let input = req.validate(bengara::rules! {
    "title" => "required|max:255",
    "age"   => "nullable|integer|between:0,150",
})?;
```

- 中身は `&[("title", "required|max:255"), ..]` と同じです。空の `rules! {}` も書けます。
- 規則の書き間違い（下の「規則の書き間違いは 500 になる」）を、**実行する前にコンパイルで止めます**。
- 判定は実行時と同じです。実行時に通る書き方は止めません。
- 止まるとき、エラーの文は `検査の規則: ` で始まります（`evaluation of constant value failed`）。
- 項目名と規則は、文字列をそのまま書きます（変数は渡せません）。

## 型に読む（FormRequest）

規則と読み先の型を 1 つにまとめます。Laravel の FormRequest に当たります。
置き場所は `app/Http/Requests/` です。ほかと同じく、大文字始まりのファイルに同じ名前の型を書きます。

```rust
// app/Http/Requests/StorePost.rs
use bengara::FormRequest;

#[derive(bengara::serde::Deserialize)]
#[serde(crate = "bengara::serde")]
pub struct StorePost {
    pub title: String,
    pub age: Option<i64>,
    pub published_at: Option<bengara::Carbon>,
}

impl FormRequest for StorePost {
    const RULES: &'static [(&'static str, &'static str)] = bengara::rules! {
        "title"        => "required|max:255",
        "age"          => "nullable|integer",
        "published_at" => "nullable|date",
    };
}
```

```rust
// コントローラ
use crate::app::http::requests::StorePost;

pub async fn store(req: Request) -> Result<Response> {
    let input: StorePost = req.validate_as()?;
    // input.title, input.age ...
}
```

| 起きたこと                     | 結果                                       |
|--------------------------------|--------------------------------------------|
| 検査に落ちた                   | 422（`validate` と同じ。前の入力も覚える） |
| 通った値を型に読めない         | 500（規則と型が食い違っている）            |

- `authorize()` はありません。誰に許すかは [authorization.md](authorization.md) の書き方で確かめます。
- 通った値だけを読みます。規則に書いていない項目は、送られてきても読みません。

### 読める型

| 項目の型                                           | 読み方                                                                      | 読めないとき |
|----------------------------------------------------|-----------------------------------------------------------------------------|--------------|
| `String`                                           | そのまま                                                                    | —            |
| 整数（`i8`〜`i64`、`u8`〜`u64`、`isize`、`usize`） | 前後の空白を落として読む                                                    | 500          |
| `f32` / `f64`                                      | 同上                                                                        | 500          |
| `bool`                                             | `1` `true` `on` `yes` は真、`0` `false` `off` `no` は偽。大文字小文字は区別 | 500          |
| `Option<T>`                                        | 空文字と、無い項目は `None`。それ以外は `T` として読む                      | —            |
| `bengara::Carbon`                                  | `Carbon::parse` で読む                                                      | 500          |
| 中身の無い列挙（`enum Role { Admin, User }`）      | 値を変種の名前として読む。`#[serde(rename_all = "lowercase")]` などが効く   | 500          |
| それ以外（`Vec`・入れ子の構造体など）              | 読まない                                                                    | 500          |

- `nullable` の項目は `Option` にします。空のとき空文字が入り、`Option` でなければ読めません。
- `Option` でない項目が規則に無いと、500 になります（「値がありません」）。
- 500 の文には項目名が出ます（`検査を通った値を型に読めません（項目 `age`）: ...`）。
- `validate` の結果からも読めます。`req.validate(..)?.to_typed::<T>()?` です。

### `RULES` の書き間違いを確かめる時

| 書き方                           | `cargo build`（`serve`・`test` を含む） | `cargo check` |
|----------------------------------|-----------------------------------------|---------------|
| `RULES` を `rules!` で書く       | 止まる                                  | **止まる**    |
| `RULES` を `&[(.., ..)]` で書く  | 使われる関数の `validate_as` で止まる   | 止まらない    |

`cargo check` は型を確かめるだけで、`validate_as` の中の確かめまでは進みません。
どこからも呼ばれない関数（ルートに登録していないハンドラなど）の中の `validate_as` も確かめません。
`rules!` で書いておけば、どちらでも止まります。

## 通った値

| メソッド             | 返すもの                             |
|----------------------|--------------------------------------|
| `get(field)`         | 値。無ければ空文字                   |
| `try_get(field)`     | 値。検査した項目に無ければ `None`    |
| `get_as::<T>(field)` | 型を変えた値。変えられなければエラー |
| `all()`              | 全部の組                             |
| `has(field)`         | 検査した項目に入っているか           |

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
| 日付       | `date`                                             |
| 項目どうし | `confirmed`、`same:other`、`different:other`       |

| 規則         | 通るもの                                                                  |
|--------------|---------------------------------------------------------------------------|
| `boolean`    | `1` `0` `true` `false` `on` `off` `yes` `no`                              |
| `numeric`    | 数として読める値。`inf` `NaN` `1e400` は通さない                          |
| `email`      | `@` が 1 つあり、後ろに `.` がある。空白を含まない                        |
| `url`        | `http://` か `https://` で始まり、直後にホストがある。空白を含まない      |
| `alpha` など | 「文字」かどうかを見る。**ひらがな・漢字も通る**（Laravel と同じ）        |
| `confirmed`  | `項目名_confirmation` と同じか（`password` なら `password_confirmation`） |
| `date`       | `YYYY-MM-DD` か `YYYY-MM-DD HH:MM:SS`（`Carbon::parse` が読める形）       |

`date` は Laravel より狭く、`2026/11/20` や `+09:00` などの時差付きは落とします。
区切りの `T`・末尾の `Z`・小数の秒は通ります。

英数字だけに限りたいときは、自分で確かめます。

### min / max / between / size の数え方

**`integer` か `numeric` が付いているときだけ、値の大小で比べます。** 付いていなければ文字数です。

| 書き方                            | 送った値        | 結果                 |
|-----------------------------------|-----------------|----------------------|
| `("age", "integer\|min:18")`      | `age=20`        | 通る（値が 18 以上） |
| `("password", "required\|min:8")` | `password=9`    | **落ちる**（1 文字） |
| `("title", "max:50")`             | `title=9999999` | **通る**（7 文字）   |

- 文字数はバイト数ではありません。`あいう` は 3 文字です。
- 数として読めない値には、大きさの規則は何も言いません。理由は「整数で入力してください」などの 1 件だけです。

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

| 場合                               | 理由                                         |
|------------------------------------|----------------------------------------------|
| セッションの Cookie を持たない相手 | 壊れた入力だけでセッションを増やされないため |
| 入力の合計が 64 KiB を超える       | 大きな入力をセッションに書かないため         |
| 秘密になりうる名前の項目           | 下の「覚えない項目」                         |

## 自分でエラーを組み立てる

DB を見る確認（重複など）は、自分で書いて足します。

```rust
let mut errors = ValidationErrors::default();
errors.add("title", "すでに同じ題名があります。");
if !errors.is_empty() {
    return Err(Error::Validation(Box::new(errors)));
}
```

| メソッド     | 返すもの               |
|--------------|------------------------|
| `has(field)` | その項目に理由があるか |
| `get(field)` | その項目の理由の一覧   |
| `first()`    | 最初の理由             |
| `all()`      | 全部                   |
| `len()`      | 項目の数               |
| `total()`    | 理由の総数             |

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

規則を `rules!` で書くと、`between:10,1` 以外は**コンパイルのときに止まります**（上の「`rules!` で書くと
コンパイルのときに確かめる」）。`between:10,1` と、`max:1e400` のような桁あふれは、実行時に 500 です。

## 覚えない項目

次の語を名前に含む項目は、`old()` で覚えません。セッションのファイルに平文で残さないためです。
ログで `***` に伏せる項目も同じです。

```
password  passwd  pwd  pass  passphrase  secret  token  key  apikey
credential  credentials  cvv  cvc  csc  card  ssn  otp  totp  pin
code  seed  mnemonic  recovery  backup  answer  signature  ticket
```

- 名前を `_` `-` `.` `[` `]` 空白と、小文字から大文字へ変わる所で語に切ります。
- 語の末尾の数字を落としてから、一覧と比べます。大文字小文字は区別しません。

| 名前                                    | 覚えるか |
|-----------------------------------------|----------|
| `new_password_confirmation` `password1` | 覚えない |
| `apiKey`（`api` と `Key` に切れる）     | 覚えない |
| `zip_code` `country_code`               | 覚えない |
| `shipping_address` `keyword`            | 覚える   |

`zip_code` も覚えないので、落ちたフォームを出し直すと郵便番号の欄は空に戻ります。

まだ無い機能は [backlog.md](backlog.md) にあります。

## 関連

- [requests-and-responses.md](requests-and-responses.md)
- [session.md](session.md)
