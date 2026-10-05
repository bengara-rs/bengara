# bengara ドキュメント

bengara を使う人のための資料です。フレームワークの紹介は [../README.md](../README.md) にあります。

## 使い始める

| ページ                                           | 内容                                         |
|--------------------------------------------------|----------------------------------------------|
| [getting-started.md](getting-started.md)         | 導入手順。つまずきやすい点つき               |
| [directory-structure.md](directory-structure.md) | ディレクトリ構成と、ファイルの自動検出の規則 |
| [artisan.md](artisan.md)                         | `cargo artisan` の仕組みとコマンド           |

## 書く

| ページ                                                 | 内容                                                                     |
|--------------------------------------------------------|--------------------------------------------------------------------------|
| [routing.md](routing.md)                               | ルート定義、パス引数、名前付きルート、グループ、署名付き URL、404 と 405 |
| [middleware.md](middleware.md)                         | リクエストの前後に挟む処理、回数の制限                                   |
| [validation.md](validation.md)                         | 入力の検査                                                               |
| [session.md](session.md)                               | セッション、Cookie、CSRF                                                 |
| [authentication.md](authentication.md)                 | ログイン、パスワード、再設定、暗号化                                     |
| [authorization.md](authorization.md)                   | 誰に何を許すか（ポリシー）                                               |
| [database.md](database.md)                             | 接続、クエリビルダ、ページ分け、トランザクション                         |
| [migrations.md](migrations.md)                         | 表を作る。`migrate` 系のコマンド                                         |
| [models.md](models.md)                                 | `#[derive(Model)]`、リレーション、シーダー、ファクトリ                   |
| [requests-and-responses.md](requests-and-responses.md) | Request / Response / リダイレクト / エラー / 静的ファイル                |
| [configuration.md](configuration.md)                   | `config/*.rs` と `.env`                                                  |
| [testing.md](testing.md)                               | `#[bengara::test]` でのテスト                                            |

## 周辺の機能

| ページ                                       | 内容                                       |
|----------------------------------------------|--------------------------------------------|
| [cache.md](cache.md)                         | 結果を取っておく                           |
| [storage.md](storage.md)                     | ファイルの置き場所                         |
| [queue.md](queue.md)                         | キューとジョブ（`sqlite` が必要）          |
| [scheduling.md](scheduling.md)               | 定期処理                                   |
| [events.md](events.md)                       | イベントと、聞く側                         |
| [mail.md](mail.md)                           | メール（**SMTP はまだありません**）        |
| [localization.md](localization.md)           | 多言語                                     |
| [console-commands.md](console-commands.md)   | 自分のコマンド                             |

## 知っておく

| ページ                                           | 内容             |
|--------------------------------------------------|------------------|
| [laravel-differences.md](laravel-differences.md) | Laravel と違う点 |
| [backlog.md](backlog.md)                         | まだ無いもの     |
| [decisions.md](decisions.md)                     | 設計判断の記録   |
