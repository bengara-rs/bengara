# テンプレート

HTML の画面を、テンプレート（`resources/views/*.html`）から作ります。Laravel の `view()` に当たります。

| できること       | 中身                                                             |
|------------------|------------------------------------------------------------------|
| 画面を返す       | `req.view("posts.index", json!({ .. }))`                         |
| 文字列だけを作る | `bengara::view::render(..)`（メールの本文などに）                |
| 部品に分ける     | 継承（`{% extends %}` / `{% block %}`）・`{% include %}`・マクロ |
| 値を逃がす       | `{{ title }}` の値は自動で HTML の逃がし方をする                 |

- 書き方は **Jinja 系**です（中身は minijinja）。**Blade の構文ではありません。**
- 使う関数は `prelude` に入っていません。`bengara::view` から使います。

## 使えるようにする

Cargo の機能フラグ `view` を足します。**既定では入りません。**

```toml
[dependencies]
bengara = { version = "0.1", features = ["sqlite", "view"] }
```

- フラグが無いときも関数はあります。呼ぶと次のエラー（500）になります。
  `テンプレートを使うには、Cargo.toml の bengara の features に "view" を足してください`
- フラグが無いとき、`bengara::view::exists(..)` は常に偽です。

## 書いてみる

テンプレートは `resources/views/` に置きます。拡張子は `.html` です。

```html
<!-- resources/views/layouts/app.html -->
<!doctype html>
<title>{% block title %}{% endblock %}</title>
<main>{% block body %}{% endblock %}</main>
```

```html
<!-- resources/views/posts/index.html -->
{% extends "layouts.app" %}
{% block title %}記事{% endblock %}
{% block body %}
  {% for post in posts %}
    <a href="{{ route('posts.show', {'post': post.id}) }}">{{ post.title }}</a>
  {% else %}
    <p>{{ __('posts.empty') }}</p>
  {% endfor %}
{% endblock %}
```

```rust
// app/Http/Controllers/PostController.rs
use bengara::prelude::*;
use bengara::serde_json::json;

pub async fn index(req: Request) -> Result<Response> {
    let posts = Post::query().latest().get().await?;
    req.view("posts.index", json!({ "posts": posts }))
}
```

- 返すのは 200、`text/html; charset=utf-8` のレスポンスです。
- 2 つ目の引数は、構造体（`Serialize`）か `json!({ .. })` のオブジェクトです。何も渡さないなら `()` です。
- 配列や数を渡すと 500 です。

### 3 つの関数

| 関数                                   | 返すもの           | 使いどころ                                        |
|----------------------------------------|--------------------|---------------------------------------------------|
| `req.view(name, context)`              | `Result<Response>` | ふつうはこれ。CSRF と前の入力が使える             |
| `bengara::view(name, context)`         | `Result<Response>` | リクエストが手元に無いとき                        |
| `bengara::view::render(name, context)` | `Result<String>`   | 描いた文字列だけが欲しいとき（メールの本文など）  |
| `bengara::view::exists(name)`          | `bool`             | テンプレートがあるか（Laravel の `View::exists`） |

```rust
let body = bengara::view::render("emails.welcome", json!({ "name": user.name }))?;
```

## 名前の決まり

`view(..)` に渡す名前にも、`{% extends %}` と `{% include %}` に書く名前にも、同じ決まりを使います。

| 名前                                                 | 読むファイル（`resources/views/` から） |
|------------------------------------------------------|-----------------------------------------|
| `.html` で終わらない（`posts.index`・`posts/index`） | `posts/index.html`                      |
| `.html` で終わる（`layouts/app.html`）               | `layouts/app.html`（そのまま）          |

- `.html` で終わらない名前の区切りに使えるのは、英数字と `_` `-` だけです。
- `.html` で終わる名前の区切りに使えるのは、英数字と `_` `-` `.` だけです。`.` と `..` そのものは使えません。
- 先頭の `/` と `\` は使えません。

| 起きたこと           | エラー（500）                                                            |
|----------------------|--------------------------------------------------------------------------|
| 決まりに合わない名前 | テンプレート名 `../x` は使えません（例: posts.index）                    |
| ファイルが無い       | テンプレート `posts.show`（resources/views/posts/show.html）がありません |

## Blade との対応

| Blade                                             | bengara（minijinja）                              |
|---------------------------------------------------|---------------------------------------------------|
| `{{ $title }}`                                    | `{{ title }}`（逃がされる）                       |
| `{!! $html !!}`                                   | `{{ html\|safe }}`                                |
| `@if` / `@foreach` / `@forelse`                   | `{% if %}` / `{% for %}` / `{% for %}…{% else %}` |
| `@extends('layouts.app')` / `@section` / `@yield` | `{% extends "layouts.app" %}` / `{% block %}`     |
| `@include('partials.nav')`                        | `{% include "partials.nav" %}`                    |
| `@csrf`                                           | `{{ csrf_field() }}`                              |
| `route('posts.show', $post)`                      | `{{ route('posts.show', {'post': post.id}) }}`    |
| `__('posts.empty')`                               | `{{ __('posts.empty') }}`                         |
| `old('title')`                                    | `{{ old('title') }}`                              |

- 変数に `$` は付けません。
- **使っていない名前（未定義の変数）はエラーです。** 打ち間違いが空の画面にならないようにしています。
  あるか分からない値は `{% if title is defined %}` で確かめます。
- 値は自動で逃がされます。`<script>` は `&lt;script&gt;` になります。逃がしたくない値だけに `|safe` を付けます。

## テンプレートで使える関数

### どこでも使える

| 関数                  | 中身                                                                    |
|-----------------------|-------------------------------------------------------------------------|
| `route(name, params)` | `route_with(..)` と同じ。`params` は `{'post': 1}` のような形（省略可） |
| `url(path)`           | `url(..)` と同じ。`APP_URL` をつないだ URL                              |
| `__(key, values)`     | `__(..)` と同じ。`values` があれば `__with(..)`（省略可）               |

- `params` と `values` の値は、文字列か数です。数は文字列にして渡します。
- 名前の無いルートなど、関数が失敗したときは描画のエラー（500）になります。

### `req.view(..)` のときだけ使える

| 関数                | 中身                                                                           |
|---------------------|--------------------------------------------------------------------------------|
| `csrf_field()`      | `<input type="hidden" name="_token" value="..">`                               |
| `csrf_token()`      | CSRF のトークン（`req.csrf_token()` と同じ）                                   |
| `old(key, default)` | 前のリクエストの入力（`session.old(key)`）。無ければ `default`（省略すると空） |

```html
<form method="post" action="{{ route('posts.store') }}">
  {{ csrf_field() }}
  <input name="title" value="{{ old('title') }}">
</form>
```

- `bengara::view(..)` と `render(..)` の中で呼ぶと、描画のエラーになります。
- セッションが無い（`StartSession` を通っていない）と、`csrf_field()` と `csrf_token()` はエラーです。
  `old()` は `default` を返します。
- `context` に `csrf_token`・`csrf_field`・`old` の鍵があると 500 です。別の名前にしてください。

## デバッグとリリースの違い

| ビルド                              | どこから読むか                          | 直したとき                   |
|-------------------------------------|-----------------------------------------|------------------------------|
| デバッグ（`cargo artisan serve`）   | 描くたびにディスクの `resources/views/` | 次のリクエストから反映される |
| リリース（`cargo build --release`） | 実行ファイルに埋め込んだもの            | ビルドし直す                 |

- リリースでは `resources/views/` を本番に置く必要はありません。
- 埋め込むのは `.html` だけです。ほかの拡張子のファイルはビルドのときに警告して飛ばします。
  隠しファイル（`.` で始まる名前）は黙って飛ばします。
- 中身が UTF-8 でない `.html` があると、リリースビルドが止まります。
- リリースの `serve` は、待ち受ける前に全部のテンプレートを読み込みます。構文の誤りがあれば
  「エラー: テンプレート `posts/index.html` を読めません: ..」を出して起動しません。
- `serve` はテンプレートを見張りません。直してもアプリを作り直さず、次のリクエストで読み直します。

## エラーの見方

描けなかったときは 500 になり、エラーの文にファイルと行が入ります。

```
テンプレート `posts.index` を描けません: undefined value (in posts/index.html:5)
```

## まだ無いもの

| 項目                                     | 代わりにすること                                                   |
|------------------------------------------|--------------------------------------------------------------------|
| `$errors`（検査に落ちた理由）            | —（検査に落ちると前の画面に戻さず 422 を返します）                 |
| `@method('PUT')`（`_method` の読み替え） | POST のルートで受ける                                              |
| `errors/404.html` などのエラーページ     | —                                                                  |
| メールのテンプレート                     | `render(..)` で描いた文字列を `Mail::to(..)` の `.html(..)` に渡す |

ほかのまだ無いものは [backlog.md](backlog.md) にあります。
