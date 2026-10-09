# 入力の検査（バリデーション）

送られてきた値が決めた形になっているかを確かめます。Laravel の Validation に当たります。

## 入力を読む

| メソッド                           | 読むところ                   |
|------------------------------------|------------------------------|
| `req.input("title")`               | 本文とクエリ。**本文が優先** |
| `req.input_all()`                  | 同上。全部                   |
| `req.form("title")` / `form_all()` | フォームの本文だけ           |
| `req.query("q")` / `query_all()`   | クエリだけ                   |
| `req.json::<T>()`                  | 本文を型に読む               |

本文を読むのは、`Content-Type` がフォーム（`application/x-www-form-urlencoded`）か
JSON のときです。JSON は**最上位の値だけ**を読みます。
入れ子は `req.json::<T>()` を使ってください。

解析するのは **1 リクエストにつき 1 回だけ**です。何度呼んでも遅くなりません。

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

`?` で返すと **422** になります。自分で `match` する必要はありません。

### 前後の空白は落とします

**`validate` が見る値は、前後の空白を落としてから検査します。**
Laravel の `TrimStrings` と同じです。

- `age=" 5 "` は `5` として検査します。`Validated` に入る値も `5` です。
- 間の空白はそのままです。`"a b"` は `"a b"` のままです。
- `req.input()` で直に読むと、**送られてきたまま**です。落とすのは検査の道だけです。

**次の 3 つだけは落としません。**

```
current_password  password  password_confirmation
```

- 前後の空白を本人が意図して入れていることがあるためです。
- Laravel が `TrimStrings` から外しているものと同じ 3 つです。
- そのため **`password` の `min:8` / `max:72` は、前後の空白も 1 文字として数えます。**

### 通った値

検査を通った値は `Validated` に入ります。**入るのは検査した項目だけ**です。
規則を書かなかった項目は入りません。
送られてきた値をそのまま保存してしまう事故を防ぐためです。

- 送られてこなかった項目は入りません。
- `nullable` の項目は、送られてこなくても**空文字**で入ります。
- 値は前後の空白を落とした形で入ります（上の 3 つの項目だけはそのままです）。

| メソッド             | 中身                |
|----------------------|---------------------|
| `get(field)`         | 値。無ければ空文字  |
| `try_get(field)`     | 値。無ければ `None` |
| `get_as::<T>(field)` | 型を変えて取り出す  |
| `all()`              | 全部の組            |
| `has(field)`         | あるか              |

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

`integer` と `boolean` は `int` / `bool` とも書けます。中身は同じです。

### alpha は日本語も通します

`alpha` / `alpha_num` / `alpha_dash` が見るのは「文字かどうか」で、
**英字だけという意味ではありません。** ひらがな・漢字も通ります。Laravel と同じです。

```rust
("name", "alpha")     // "田中" は通る
```

英数字だけに限りたいときは、いまは自分で確かめてください。

### min / max / between の数え方

**`numeric` か `integer` が付いているときだけ、値の大小で比べます。**
付いていなければ**文字数**で比べます。Laravel と同じです。

| 書き方                        | 比べるもの           |
|-------------------------------|----------------------|
| `("age", "integer\|min:18")`  | 値（18 以上）        |
| `("age", "numeric\|max:9.5")` | 値（9.5 以下）       |
| `("title", "min:3")`          | 文字数（3 文字以上） |

`size` も同じ決まりです。文字数は見た目の文字で数えます。`あいう` は 3 文字です。

- **数値として読めない値のときは、`min` / `max` / `between` / `size` は何も言いません。**
  `("n", "integer|min:5")` に `abc` を送ると、理由は「整数で入力してください」の 1 件だけです。
  読めない値を文字数で比べても意味が無く、理由が 2 件に増えて分かりにくくなるためです。
- `numeric` は `inf` / `NaN` / `1e400` を通しません。

**間違えやすい例**

```rust
("password", "required|min:8")
```

`password=9` は**落ちます**。1 文字しかないからです。
「8 以上の数」ではなく「8 文字以上」として読みます。

```rust
("title", "max:50")
```

`title=9999999` は**通ります**。7 文字しかないからです。
値として 50 を超えていても見ません。

数の大小で比べたいときは `integer` か `numeric` を足します。

```rust
("age", "nullable|integer|between:0,150")
```

### 空文字も検査する

**検査を飛ばすのは、下の 2 つのときだけです。**

| 状況                             | 動き         |
|----------------------------------|--------------|
| 項目が送られてこなかった         | 検査しない   |
| `nullable` が付いている          | 検査しない   |
| **空文字（`""`）が送られてきた** | **検査する** |

```rust
("role", "in:admin,user")
```

`role=""` は**落ちます**。空文字は `admin` でも `user` でもありません。
`email=""` も `email` の規則で落ちます。

空でよい項目には `nullable` を付けてください。

```rust
("role", "nullable|in:admin,user")
```

`required` を付けた項目は、空文字でも「必ず入力してください」で落ちます。

### confirmed

`password` に付けると、`password_confirmation` と同じかどうかを見ます。

## 落ちたとき

### JSON を求めた相手には JSON

```json
{
  "message": "入力に誤りがあります。",
  "errors": {
    "email": ["email はメールアドレスの形で入力してください。"],
    "title": ["title は必ず入力してください。"]
  }
}
```

1 つの項目に理由が複数付くことがあります。項目は名前の順に並びます。

### ブラウザには画面

`Accept` に `text/html` があれば、読める画面を返します。
どちらで返すかの決め方は [requests-and-responses.md](requests-and-responses.md) にあります。

### 前の入力を覚えている

セッションを使っていれば、落ちた入力を次のリクエストで読めます。

```rust
req.session().old("title")   // 落ちたときに送られてきた title
```

**覚えるのは、セッションが置き場所から読めたときだけです。**
Cookie を持っていない相手には覚えません。覚えると保存が必要になるので、
本文を投げるだけでセッションのファイルが 1 つ増えてしまいます。
画面を使っている人には今までどおり働きます。

次の語を名前に含む項目は**覚えません**。セッションのファイルに平文で残るのを避けるためです。

```
password  passwd  pwd  pass  secret  token  key  apikey  credential
credentials  cvv  card  ssn  otp  pin  signature
```

**語の単位で突き合わせます。** 突き合わせ方は 2 段です。

1. 名前を `_` `-` `.` 空白と大文字の境目で語に区切る。
2. 語の**末尾の数字を落として**から、上の一覧にあるかを見る。

| 名前                        | 覚えるか | 理由                            |
|-----------------------------|----------|---------------------------------|
| `new_password_confirmation` | 覚えない | `password` の語がある           |
| `password1` `password2`     | 覚えない | 末尾の数字を落とすと `password` |
| `apiKey` `apiKey2`          | 覚えない | 大文字の境目で `Key` に切れる   |
| `shipping_address`          | 覚える   | どの語も一覧に無い              |
| `keyword`                   | 覚える   | `key` を含むが語は `keyword`    |

落とすのは語の**末尾**の数字だけです。`keyword` のような普通の項目は残ります。

## 自分でエラーを組み立てる

```rust
use bengara::validation::ValidationErrors;

let mut errors = ValidationErrors::default();
errors.add("title", "すでに同じ題名があります。");
if !errors.is_empty() {
    return Err(bengara::Error::Validation(Box::new(errors)));
}
```

DB を見る確認（重複など）は、自分で書いてここに足してください。

## 規則の書き間違いは 500 になる

**書き間違えた規則名は、黙って通しません。500 になります。**
メッセージに、使える規則名の一覧が出ます。

引数も同じです。`min` / `max` / `between` / `size` に書けるのは**有限の数だけ**です。

| 書き間違い                    | どうなるか                      |
|-------------------------------|---------------------------------|
| `max:nan` `min:inf`           | **500**                         |
| `size:-3` `size:1.5`          | **500**。`size` は 0 以上の整数 |
| `between:10,1`（下限 > 上限） | **500**                         |
| `min`（引数を忘れた）         | **500**                         |

利用者の入力の問題ではなく書き間違いなので、422 ではなく開発者向けのエラーです。
黙って「何も検査しない」状態にはしません。

## まだ無いもの

正規表現の規則、`unique` / `exists`、フォームリクエスト、配列や入れ子の項目はありません。
[backlog.md](backlog.md) を参照してください。

## 関連

- [requests-and-responses.md](requests-and-responses.md)
- [session.md](session.md)
