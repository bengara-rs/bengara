# bengara ドキュメント

bengara を使う人のための資料です。どのページを読めばよいかをここで探します。
フレームワークの紹介は [../README.md](../README.md) にあります。

初めてなら、[getting-started.md](getting-started.md) →
[directory-structure.md](directory-structure.md) → [routing.md](routing.md) の順に読んでください。

## やりたいこと → 読むページ

| やりたいこと               | ページ                                                 |
|----------------------------|--------------------------------------------------------|
| まず動かす                 | [getting-started.md](getting-started.md)               |
| どこに何を置くか知る       | [directory-structure.md](directory-structure.md)       |
| コマンドの名前を知る       | [artisan.md](artisan.md)                               |
| URL を足す                 | [routing.md](routing.md)                               |
| 入力を読む・応答を返す     | [requests-and-responses.md](requests-and-responses.md) |
| リクエストの前後に挟む     | [middleware.md](middleware.md)                         |
| 入力を確かめる             | [validation.md](validation.md)                         |
| 値を覚える・CSRF を防ぐ    | [session.md](session.md)                               |
| ログインを作る             | [authentication.md](authentication.md)                 |
| 誰に何を許すか決める       | [authorization.md](authorization.md)                   |
| 表を作る                   | [migrations.md](migrations.md)                         |
| データを読み書きする       | [database.md](database.md) → [models.md](models.md)    |
| `.env` と設定を知る        | [configuration.md](configuration.md)                   |
| テストを書く               | [testing.md](testing.md)                               |
| 本番に置く                 | [deployment.md](deployment.md)                         |

## 周辺の機能

| ページ                                     | 内容                                 |
|--------------------------------------------|--------------------------------------|
| [cache.md](cache.md)                       | 結果を取っておく                     |
| [storage.md](storage.md)                   | ファイルの置き場所                   |
| [queue.md](queue.md)                       | キューとジョブ（データベースが必要） |
| [scheduling.md](scheduling.md)             | 定期処理                             |
| [events.md](events.md)                     | イベントと、聞く側                   |
| [mail.md](mail.md)                         | メール（`log` / `array` / SMTP）     |
| [localization.md](localization.md)         | 多言語                               |
| [console-commands.md](console-commands.md) | 自分のコマンド                       |

## 知っておく

| ページ                                           | 内容                            |
|--------------------------------------------------|---------------------------------|
| [laravel-differences.md](laravel-differences.md) | Laravel と違う点                |
| [backlog.md](backlog.md)                         | まだ無いものと、分かっている制限 |
| [decisions.md](decisions.md)                     | 設計判断の記録                  |
