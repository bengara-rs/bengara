# 入力の検査（バリデーション）

Laravel の Validation に当たります。

## 入力を読む

| メソッド | 読むところ |
|---|---|
| `req.input("title")` | 本文とクエリ。**本文が優先** |
| `req.input_all()` | 同上。全部 |
| `req.form("title")` / `form_all()` | フォームの本文だけ |
| `req.query("q")` / `query_all()` | クエリだけ |
| `req.json::<T>()` | 本文を型に読む |

本文は、`Content-Type` がフォーム（`application/x-www-form-urlencoded`）か JSON のときに読みます。
JSON は**最上位の値だけ**を読みます。入れ子は `req.json::<T>()` を使ってください。

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

### 通った値

`Validated` には **検査した項目だけ** が入ります。規則を書かなかった項目は入りません。
送られてきた値をそのまま保存してしまう事故を防ぐためです。

| メソッド | 中身 |
|---|---|
| `get(field)` | 値。無ければ空文字 |
| `try_get(field)` | 値。無ければ `None` |
| `get_as::<T>(field)` | 型を変えて取り出す |
| `all()` | 全部の組 |
| `has(field)` | あるか |

## 使える規則

| 分類 | 規則 |
|---|---|
| 必須と省略 | `required`、`nullable` |
| 型 | `integer`、`numeric`、`boolean` |
| 形 | `email`、`url`、`alpha`、`alpha_num`、`alpha_dash` |
| 大きさ | `min:n`、`max:n`、`between:a,b`、`size:n` |
| 値 | `in:a,b,c`、`starts_with:x`、`ends_with:x` |
| 項目どうし | `confirmed`、`same:other`、`different:other` |

規則は `|` でつなぎます。

### min / max の数え方

**数値として読めれば値そのもの、読めなければ文字数**で比べます（Laravel と同じ）。

```rust
("age",   "integer|min:18")   // 18 以上かどうか
("title", "min:3")            // 3 文字以上かどうか
```

文字数は見た目の文字で数えます。`あいう` は 3 文字です。

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

1つの項目に理由が複数付くことがあります。項目は名前の順に並びます。

### ブラウザには画面

`Accept` に `text/html` があれば、読める画面を返します。
どちらで返すかの決め方は [requests-and-responses.md](requests-and-responses.md) にあります。

### 前の入力を覚えている

セッションを使っていれば、落ちた入力を次のリクエストで読めます。

```rust
req.session().old("title")   // 落ちたときに送られてきた title
```

`password` / `secret` / `token` を名前に含む項目は**覚えません**。

## 自分でエラーを組み立てる

```rust
use bengara::validation::ValidationErrors;

let mut errors = ValidationErrors::default();
errors.add("title", "すでに同じ題名があります。");
if !errors.is_empty() {
    return Err(bengara::Error::Validation(Box::new(errors)));
}
```

DB を見る確認（重複など）は、いまは自分で書いてここに足してください。

## まだ無いもの

正規表現の規則、`unique` / `exists`、フォームリクエスト、配列や入れ子の項目はありません。
[backlog.md](backlog.md) を参照してください。

**知らない規則を書くと、その項目の理由として返ります。** 黙って通すことはしません。

---

関連: [requests-and-responses.md](requests-and-responses.md) / [session.md](session.md)
