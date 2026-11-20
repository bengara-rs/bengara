# 外部コマンドと重い処理

外部のコマンド（`ffmpeg` や `git` など）を起動して、終わりを待つ仕組みです。
Laravel の `Process` に当たります。
時間のかかる同期の処理を裏のスレッドへ逃がす `blocking` も、ここで説明します。

| やりたいこと                                              | 使うもの                     |
|-----------------------------------------------------------|------------------------------|
| 外部のコマンドを起動して待つ                              | `bengara::process::Process`  |
| Rust の同期の処理（計算・同期のファイル操作など）を逃がす | `bengara::support::blocking` |

どちらも `prelude` には入っていません。`use` を書いてください。

## コマンドを起動する

```rust
use bengara::prelude::*;
use bengara::process::Process;

let result = Process::run(["git", "--version"]).await?;
println!("{}", result.output());
```

- コマンドは「**プログラムと引数の並び**」で渡します。最初がプログラム、残りが引数です。
- 配列のほか、`Vec<String>` なども渡せます。並びが空ならエラーです。
- 待っている間も、ほかのリクエストは止まりません。

### 設定を足す

設定から始めて続けて書き、最後に `.run(コマンド)` を呼びます。

```rust
let result = Process::timeout(30 * 60)
    .path(storage_path("app/videos"))
    .env("LANG", "C")
    .run(["ffmpeg", "-i", "in.mp4", "out.webm"])
    .await?
    .throw()?;
```

| 書き方               | 意味                                     | 書かないとき                       |
|----------------------|------------------------------------------|------------------------------------|
| `timeout(秒)`        | この時間で打ち切る。`0` は打ち切らない   | **60 秒**（Laravel と同じ）        |
| `forever()`          | 打ち切らない                             | —                                  |
| `path(ディレクトリ)` | 作業ディレクトリ                         | このプロセスと同じ                 |
| `env(名前, 値)`      | 環境変数を足す。何回でも書ける           | このプロセスの環境変数を引き継ぐ   |
| `input(バイト列)`    | 標準入力に渡す中身                       | 標準入力は空（端末を引き継がない） |
| `quietly()`          | 標準出力とエラー出力を受け取らずに捨てる | 両方を受け取る                     |

### 結果を読む

**終了コードが 0 でなくても、`run` は `Ok` を返します。** 成否は結果のメソッドで見分けます。

| メソッド         | 返すもの                                                          |
|------------------|-------------------------------------------------------------------|
| `successful()`   | 終了コードが 0 だったか                                           |
| `failed()`       | 終了コードが 0 でなかったか                                       |
| `exit_code()`    | 終了コード（`Option<i32>`）。Unix でシグナルで止められたら `None` |
| `output()`       | 標準出力（`&str`）                                                |
| `error_output()` | エラー出力（`&str`）                                              |
| `throw()`        | 失敗ならエラーにする。成功ならそのまま返す                        |

- 出力は UTF-8 として読みます。読めないバイトは置換文字（U+FFFD）になります。
- `quietly()` を付けたときは、どちらの出力も空です。
- `throw()` のエラー文は「`` `git` が失敗しました（終了コード 1）: <エラー出力の末尾> ``」の形です。

### `run` がエラーを返すとき

| 起きたこと                               | エラーの中身                                     |
|------------------------------------------|--------------------------------------------------|
| 起動できなかった（プログラムが無いなど） | 見つからないときは PATH を確かめるよう案内する   |
| 打ち切りの時間を過ぎた                   | 子プロセスを止めて、それまでの出力の末尾を入れる |

```
`ffmpeg` が 60 秒で終わらなかったので止めました
エラー出力の末尾: <エラー出力の末尾>
出力の末尾: <標準出力の末尾>
```

- エラー文に入れる出力は、**末尾の 2,000 文字**までです。失敗の理由はたいてい最後のほうに出るためです。
- **エラー文に入れるのはプログラムの名前だけです。** 引数にはパスワードやトークンが入ることがあり、エラー文はログに出るためです。
- 待っている側が待つのをやめたとき（キューの打ち切り、リクエストの切断など）も、コマンドを止めます。

## キューのジョブから起動する

動画の変換のような長い処理は、画面の中で待たずにキューへ積みます（[queue.md](queue.md)）。

```rust
// app/Jobs/TranscodeVideo.rs（Payload は queue.md と同じ書き方）
use bengara::prelude::*;
use bengara::process::Process;

pub async fn handle(payload: String) -> Result<()> {
    let payload: Payload = bengara::serde_json::from_str(&payload)?;
    let input = format!("{}.mp4", payload.video_id);
    let output = format!("{}.webm", payload.video_id);

    // 打ち切りはジョブの側（config/queue.rs）に任せる。
    Process::forever()
        .path(storage_path("app/videos"))
        .run(["ffmpeg", "-y", "-loglevel", "error", "-i", input.as_str(), output.as_str()])
        .await?
        .throw()?;
    Ok(())
}
```

```rust
// config/queue.rs
use bengara::queue::QueueConfig;

pub fn config() -> QueueConfig {
    QueueConfig::default().timeout("TranscodeVideo", 30 * 60)
}
```

- ジョブが打ち切られると、待っていた `ffmpeg` も止まります。
- 打ち切りの時間が `--retry-after` より長くても構いません。途中で別の worker に取り直されません（[queue.md](queue.md)）。

## Laravel と違うところ

| Laravel                                 | bengara                                |
|-----------------------------------------|----------------------------------------|
| `Process::run('ls -la')`                | `Process::run(["ls", "-la"]).await?`   |
| `Process::timeout(120)->run(..)`        | `Process::timeout(120).run(..).await?` |
| `->env([..])`                           | `env` は 1 組ずつ書く                  |
| `$result->exitCode()` / `errorOutput()` | `exit_code()` / `error_output()`       |
| 時間切れは例外                          | `run` が `Err` を返す                  |

`path`・`input`・`quietly`・`forever` は Laravel と同じ名前です。

### シェルを通さない

**文字列のコマンドは受けません。** シェルを通すと、次の問題があるためです。

- 外から来た値を混ぜると、別のコマンドを走らせられる
- OS ごとにシェルの文法が違う

パイプやリダイレクトが要るときは、自分でシェルを起動します。
シェルに渡す文字列に、外から来た値を混ぜないでください。

```rust
Process::run(["sh", "-c", "ls -la | wc -l"]).await?;
```

## 同期の重い処理を逃がす（`blocking`）

`await` を挟まずに CPU やディスクを使い続ける処理を、裏のスレッドで動かして待ちます。
ハンドラやジョブの中でそのまま呼ぶと、同じスレッドのほかのリクエストが止まるためです。

```rust
use bengara::support::blocking;

let total = blocking(move || {
    // 何秒も CPU を使う同期の計算
    rows.iter().map(score).sum::<u64>()
})
.await?;
```

- 処理がパニックしたら `Err`（500）を返します。パニックはその処理に閉じ込めます。
- 処理が `Result` を返すなら、`.await??` と 2 回 `?` を付けます。
- **`blocking` は途中で止められません。** 待つのをやめても最後まで走ります。
- だから、外部のコマンドを `blocking` の中で `std::process::Command` で待たないでください。打ち切っても動き続けます。

## 気をつけること

| 起きること                                                             | どうするか                                                |
|------------------------------------------------------------------------|-----------------------------------------------------------|
| `Process` の出力は全部メモリに溜まる                                   | 出力が多いなら `quietly()` で捨てるか、ファイルへ書かせる |
| 止めるのは起動した 1 本だけ。孫のプロセスは残ることがある              | 子が自分の子を後始末する形にする                          |
| 裏で動く孫が出力を開いたままだと、打ち切りまで待って `Err` になる      | `quietly()` を付けるか、孫の出力を別の所へ向ける          |
| シェルのスクリプトは直接起動できるとは限らない                         | `["sh", "-c", ".."]` のように、シェルを自分で起動する     |
| `blocking` の裏のスレッドは最大 512 本。埋まると、ほかの読み書きも待つ | 何十分も占有する処理を大量に並べない                      |

孫が出力を開いたままになる例は `["sh", "-c", "daemon & echo hi"]` です。

## 関連

- [queue.md](queue.md) — ジョブと、ジョブごとの打ち切りの時間
- [console-commands.md](console-commands.md) — 自分のコマンドから起動する
- [laravel-differences.md](laravel-differences.md) — Laravel と違う点
