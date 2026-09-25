# NULL BBS

パソコン通信のホスト局プログラムです。モデム (電話回線) と TCP (telnet) の両方から接続でき、最大 128 回線を同時に受け付けます。
利用者側の端末には、姉妹プロジェクトの [null-term](https://github.com/GOROman/null-term) などが使えます。

## できること

| 機能 | 内容 |
|---|---|
| 掲示板 (BBS) | 複数のボード、記事の一覧・読む・書く・返信、ボードごとの未読管理 |
| メール (MAIL) | 会員どうしのメール。相手が接続中なら着信を知らせる |
| チャット (CHAT) | 部屋ごとのリアルタイムチャット、入退室の通知 |
| 電報 (TEL) | 接続中の相手に 1 行メッセージを送る |
| 在室者 (WHO) | 接続中の回線・会員・接続速度・いる場所 |
| 足跡 (LOG) | 最近のログイン記録 |
| ファイル (FILE) | ファイルライブラリ。XMODEM / XMODEM-1K / YMODEM でアップロード・ダウンロード |
| 管理コンソール | 回線一覧、システムログ、回線のモニタ、強制切断、全体放送、会員・ボードの管理 |

- 利用者の画面は番号メニューとコマンドの併用 (「1」でも「BBS」でも掲示板に入れます)
- 文字コードは UTF-8
- 会員登録はオンラインで行い、最初に登録した人が SYSOP になります。ゲストでの利用もできます
- データは SQLite 1 ファイルに保存します

## すぐに試す

```sh
cargo build --release
cp config.example.toml null-bbs.toml
./target/release/null-bbs            # 管理コンソール付きで起動
```

別の端末から接続します。

```sh
telnet localhost 5656
```

最初に `NEW` で会員登録した人が SYSOP になります。

## ドキュメント

- [導入と設定](docs/setup.md)
- [利用者の操作](docs/user-guide.md)
- [管理者 (SYSOP) の操作](docs/sysop-guide.md)
- [モデムで運用する](docs/modem.md)
- [設計メモ](docs/design.md)

