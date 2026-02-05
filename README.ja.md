# diggy-gizzy

[English](README.md) | [日本語](README.ja.md)

Twilightを使用したRust製のDiscordボット。ボイスチャンネルの会話を録音し、Whisperで文字起こし、議事録を生成します。

## 機能

- 🎙️ **音声録音**: ボイスチャンネルに参加して全参加者の音声を録音
- 📝 **文字起こし**: OpenAIのWhisperモデルを使用して音声をテキスト化
- 📄 **自動議事録**: 会議の議事録やサマリーを自動生成
- 🎮 **簡単操作**: リアクションで録音開始/停止
- 🔒 **プライバシー重視**: サマリー生成以外はすべてローカル処理

## 前提条件

- Rust 1.75+
- Discord Bot Token
- Whisperモデル（GGML形式）
- （オプション）Z.AI API Key（議事録生成用）

## セットアップ

### 1. クローンとビルド

```bash
git clone https://github.com/sitne/diggy-gizzy.git
cd diggy-gizzy
cargo build --release
```

### 2. 環境設定

`.env.example`を`.env`にコピーして必要な値を入力：

```bash
cp .env.example .env
```

必要な環境変数：
- `DISCORD_TOKEN`: Discordボットトークン
- `DISCORD_APPLICATION_ID`: DiscordアプリケーションID
- `WHISPER_MODEL_PATH`: Whisperモデルファイルのパス
- `ZAI_API_KEY`: （オプション）AI議事録生成用

### 3. Whisperモデルのダウンロード

Whisperモデル（GGML形式）をダウンロードして`models/`ディレクトリに配置：

```bash
# 例：baseモデルをダウンロード
mkdir -p models
cd models
wget https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin
```

詳細は[models/README.md](models/README.md)を参照。

### 4. Discordボットの設定

1. [Discord Developer Portal](https://discord.com/developers/applications)にアクセス
2. 新しいアプリケーションを作成
3. 「Bot」セクションで以下のPrivileged Intentsを有効化：
   - Server Members Intent
   - Message Content Intent
4. ボットトークンを`.env`ファイルにコピー
5. 「OAuth2」→「URL Generator」で以下を選択：
   - Scopes: `bot`, `applications.commands`
   - Bot Permissions:
     - View Channels
     - Send Messages
     - Connect
     - Speak
     - Use Voice Activity
6. 生成されたURLを使用してボットを招待

## 使い方

### 録音開始

テキストチャンネルで以下を入力：
```
/record
```

ボットは以下を実行：
1. 現在のボイスチャンネルに参加
2. 全参加者の録音を開始
3. 🛑（停止）リアクション付きのコントロールメッセージを送信

### 録音停止

コントロールメッセージの🛑リアクションをクリック、または全員が退室すると自動停止。

### 文字起こしと議事録取得

停止後、ボットは以下を実行：
1. Whisperで音声を文字起こし
2. （Z.AI API key設定時）議事録を生成
3. テキストチャンネルに結果を送信

## プロジェクト構成

```
.
├── src/
│   ├── main.rs              # ボットのエントリーポイント
│   ├── voice_recorder.rs    # 録音ロジック
│   ├── transcriber.rs       # Whisper文字起こし
│   ├── summarizer.rs        # AI議事録生成
│   └── commands.rs          # コマンドハンドラ
├── models/                  # Whisperモデル
├── recordings/             # 一時音声ファイル
├── Cargo.toml
└── .env
```

## 設定

### 環境変数

| 変数 | 必須 | 説明 |
|------|------|-------------|
| `DISCORD_TOKEN` | はい | Discordボットトークン |
| `DISCORD_APPLICATION_ID` | はい | DiscordアプリケーションID |
| `WHISPER_MODEL_PATH` | はい | Whisperモデルのパス |
| `ZAI_API_KEY` | いいえ | Z.AI API key（議事録生成用） |

### Systemdサービス

`/etc/systemd/system/discord-bot.service`の例：

```ini
[Unit]
Description=Discord Voice Recording Bot
After=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/root/twilight
EnvironmentFile=/root/twilight/.env
ExecStart=/root/twilight/target/release/discord-recording-bot
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

有効化と起動：
```bash
sudo systemctl enable discord-bot
sudo systemctl start discord-bot
```

## トラブルシューティング

### ボットがコマンドに応答しない
- ボットに適切な権限があるか確認
- `DISCORD_TOKEN`が正しいか確認
- ログを確認：`journalctl -u discord-bot -f`

### 録音品質の問題
- ボットに「Use Voice Activity」権限があるか確認
- ボイスチャンネルのビットレートが十分か確認
- 別のWhisperモデルサイズを試す

### 文字起こしが失敗
- Whisperモデルファイルが存在し、有効か確認
- 使用可能なRAMを確認（大きなモデルはより多くのメモリが必要）
- recordingsディレクトリに書き込み権限があるか確認

## ライセンス

MIT License - 詳細は[LICENSE](LICENSE)ファイルを参照。

## 貢献

貢献は歓迎します！気軽にPull Requestを送信してください。

## 謝辞

- [twilight-rs](https://twilight.rs/) - Rust用Discordライブラリ
- [whisper.cpp](https://github.com/ggerganov/whisper.cpp) - 音声認識
- [Songbird](https://github.com/serenity-rs/songbird) - 音声処理
