# 導入と設定

## 必要なもの

- Rust (1.85 以降、edition 2024)
- macOS または Linux
- モデムで運用する場合は USB シリアル変換ケーブルとモデム ([モデムで運用する](modem.md) を参照)

SQLite はプログラムに組み込まれているので、別に入れる必要はありません。

## ビルド

```sh
git clone https://github.com/GOROman/null-bbs.git
cd null-bbs
cargo build --release
```

実行ファイルは `target/release/null-bbs` にできます。

## 設定ファイル

`config.example.toml` を `null-bbs.toml` という名前でコピーして編集します。
別の名前にする場合は `-c` で指定します (`null-bbs -c /etc/null-bbs.toml`)。
設定ファイルが無いときは、すべて既定値で起動します。

### [bbs]

| 項目 | 既定値 | 内容 |
|---|---|---|
| `name` | `NULL-BBS` | ホスト局の名前。画面の見出しなどに出ます |
| `db` | `null-bbs.db` | SQLite のデータベースファイル |
| `text_dir` | `text` | 接続時・終了時に出す文面 (`banner.txt` / `goodbye.txt`) を置くディレクトリ |
| `files_dir` | `files` | ファイルライブラリの保存先 |
| `max_upload_kb` | `4096` | アップロードできる 1 ファイルの大きさの上限 (KB) |
| `max_lines` | `128` | 回線数の上限 (1〜128) |
| `allow_guest` | `true` | ゲストでの利用を許す |
| `chat_log` | `false` | チャットの発言をデータベースに記録する |
| `log_file` | `null-bbs.log` | システムログの出力先 |

`banner.txt` と `goodbye.txt` の中の `{name}` は、BBS 名に置き換わります。ファイルが無いときは標準の文面を出します。

### [limits]

| 項目 | 既定値 | 内容 |
|---|---|---|
| `idle_timeout_secs` | `300` | 何も入力が無いまま、この秒数が過ぎると切断します (1 分前に警告を出します) |
| `max_session_mins` | `60` | 1 回の接続で使える時間 (分) |
| `guest_session_mins` | `15` | ゲストが使える時間 (分) |
| `login_attempts` | `3` | パスワードを間違えてよい回数 |

### [tcp]

| 項目 | 既定値 | 内容 |
|---|---|---|
| `listen` | `["0.0.0.0:5656"]` | telnet で待ち受けるアドレス。複数書けます。`[]` にすると TCP では受け付けません |

既定の `0.0.0.0` は、その PC のすべてのネットワークから接続を受け付けます。手元の PC だけで使うなら `127.0.0.1:5656` にしてください。
telnet は通信が暗号化されません。インターネットに公開するときは、パスワードが平文で流れることを承知のうえで運用してください。

### [ws]

| 項目 | 既定値 | 内容 |
|---|---|---|
| `listen` | `["127.0.0.1:5657"]` | WebSocket で待ち受けるアドレス。ブラウザのソフトウェアモデム [null-modem](https://github.com/GOROman/null-modem) からの接続を受けます。`[]` にすると使いません |

WebSocket の回線は、管理コンソールで種別「WS」として表示されます。Cloudflare Tunnel 経由の接続なら、接続元には利用者の IP アドレス (`CF-Connecting-IP`) が出ます。
データはバイナリフレームでそのままやりとりします (telnet の処理はしません)。

### [[modem]]

モデム回線の設定は [モデムで運用する](modem.md) を参照してください。

## 起動と終了

```sh
null-bbs                 # 管理コンソール付きで起動 (run を省略した形)
null-bbs run --headless  # 管理コンソールを出さずに起動 (サーバーやサービスとして動かすとき)
```

- 管理コンソールでは `Q` で終了します。
- `--headless` のときは Ctrl-C か SIGTERM で終了します。
- 終了するときは、接続中の全員に「まもなくシステムを終了します」と知らせてから切断します。

## コマンドラインでの管理

起動していなくても、次のコマンドで会員やボードを操作できます。

```sh
null-bbs useradd goroman ゴロー --sysop   # 会員を追加 (パスワードは入力を求められます)
null-bbs users                           # 会員の一覧
null-bbs board add PC98 "PC-98 の部屋" -d "PC-98 の話題"   # ボードを追加
null-bbs board list                      # ボードの一覧
```

最初から「お知らせ (INFO)」と「雑談 (FREE)」の 2 つのボードがあります。お知らせは SYSOP だけが書き込めます。

## ファイルの置き場所

| ファイル | 内容 |
|---|---|
| `null-bbs.db` | 会員・掲示板・メール・足跡などすべてのデータ |
| `files/` | ファイルライブラリの実体 |
| `null-bbs.log` | システムログ |

バックアップは、この 3 つをコピーすれば十分です。データベースは WAL モードで使っているので、`null-bbs.db-wal` と `null-bbs.db-shm` も一緒にコピーするか、終了してからコピーしてください。
