# bengara ドキュメント

bengara を使う人のための資料です。フレームワークの紹介は [../README.md](../README.md) にあります。

## 知りたいこと → 読む場所

| 知りたいこと                       | 読む場所                                                             |
|------------------------------------|----------------------------------------------------------------------|
| まず動かしたい                     | [getting-started.md](getting-started.md)                             |
| どこに何を置くのか                 | [directory-structure.md](directory-structure.md)                     |
| URL を足したい                     | [routing.md](routing.md)                                             |
| フォームの値を受け取りたい         | [requests-and-responses.md](requests-and-responses.md)               |
| 入力を確かめたい                   | [validation.md](validation.md)                                       |
| ログインを作りたい                 | [authentication.md](authentication.md)                               |
| 表とデータを扱いたい               | [migrations.md](migrations.md) → [models.md](models.md)              |
| コマンドの名前を知りたい           | [artisan.md](artisan.md)                                             |
| `.env` の項目を知りたい            | [configuration.md](configuration.md)                                 |
| テストを書きたい                   | [testing.md](testing.md)                                             |
| 本番に置きたい                     | [deployment.md](deployment.md)                                       |
| Laravel と何が違うのか             | [laravel-differences.md](laravel-differences.md)                     |
| 動かない・困っている               | [getting-started.md](getting-started.md) の「つまずきやすい点」      |

初めてなら、上から 3 つを順に読むのがおすすめです。

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
| [requests-and-responses.md](requests-and-responses.md) | 入力の読み方、応答の作り方、リダイレクト、エラー、静的ファイル           |
| [middleware.md](middleware.md)                         | リクエストの前後に挟む処理、回数の制限                                   |
| [validation.md](validation.md)                         | 入力の検査                                                               |
| [session.md](session.md)                               | セッション、Cookie、CSRF                                                 |
| [authentication.md](authentication.md)                 | ログイン、パスワード、再設定、暗号化                                     |
| [authorization.md](authorization.md)                   | 誰に何を許すか（ポリシー）                                               |
| [database.md](database.md)                             | 接続、クエリビルダ、ページ分け、トランザクション                         |
| [migrations.md](migrations.md)                         | 表を作る。`migrate` 系のコマンド                                         |
| [models.md](models.md)                                 | `#[derive(Model)]`、リレーション、シーダー、ファクトリ                   |
| [configuration.md](configuration.md)                   | `config/*.rs` と `.env`                                                  |
| [testing.md](testing.md)                               | `#[bengara::test]` でのテスト                                            |

## 周辺の機能

| ページ                                     | 内容                                |
|--------------------------------------------|-------------------------------------|
| [cache.md](cache.md)                       | 結果を取っておく                    |
| [storage.md](storage.md)                   | ファイルの置き場所                  |
| [queue.md](queue.md)                       | キューとジョブ（`sqlite` が必要）   |
| [scheduling.md](scheduling.md)             | 定期処理                            |
| [events.md](events.md)                     | イベントと、聞く側                  |
| [mail.md](mail.md)                         | メール（**SMTP はありません**）     |
| [localization.md](localization.md)         | 多言語                              |
| [console-commands.md](console-commands.md) | 自分のコマンド                      |

## 本番で動かす

| ページ                         | 内容                                                      |
|--------------------------------|-----------------------------------------------------------|
| [deployment.md](deployment.md) | 置き方、`storage:init`、止め方、プロセス 2 つ、更新の手順 |

## 知っておく

| ページ                                           | 内容             |
|--------------------------------------------------|------------------|
| [laravel-differences.md](laravel-differences.md) | Laravel と違う点 |
| [backlog.md](backlog.md)                         | まだ無いもの     |
| [decisions.md](decisions.md)                     | 設計判断の記録   |
