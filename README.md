# diggy-gizzy

[English](README.md) | [日本語](README.ja.md)

A Rust-based Discord bot built with Twilight that records voice channel conversations, transcribes them using Whisper, and generates meeting minutes.

## Features

- 🎙️ **Voice Recording**: Joins voice channels and records all participants
- 📝 **Speech-to-Text**: Transcribes recordings using OpenAI's Whisper model
- 📄 **Automatic Summarization**: Generates meeting minutes and summaries
- 🎮 **Simple Controls**: Start/stop recording with reactions
- 🔒 **Privacy First**: All processing done locally (except optional summarization)

## Prerequisites

- Rust 1.75+ 
- Discord Bot Token
- Whisper Model (GGML format)
- (Optional) Z.AI API Key for summarization

## Setup

### 1. Clone and Build

```bash
git clone https://github.com/sitne/diggy-gizzy.git
cd diggy-gizzy
cargo build --release
```

### 2. Configure Environment

Copy `.env.example` to `.env` and fill in your values:

```bash
cp .env.example .env
```

Required environment variables:
- `DISCORD_TOKEN`: Your Discord bot token
- `DISCORD_APPLICATION_ID`: Your Discord application ID
- `WHISPER_MODEL_PATH`: Path to Whisper model file
- `ZAI_API_KEY`: (Optional) For AI summarization

### 3. Download Whisper Model

Download a Whisper model in GGML format and place it in the `models/` directory:

```bash
# Example: Download base model
mkdir -p models
cd models
wget https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin
```

See [models/README.md](models/README.md) for more options.

### 4. Discord Bot Setup

1. Go to [Discord Developer Portal](https://discord.com/developers/applications)
2. Create a new application
3. Go to "Bot" section and enable these Privileged Intents:
   - Server Members Intent
   - Message Content Intent
4. Copy the bot token to your `.env` file
5. Go to "OAuth2" → "URL Generator" and select:
   - Scopes: `bot`, `applications.commands`
   - Bot Permissions:
     - View Channels
     - Send Messages
     - Connect
     - Speak
     - Use Voice Activity
6. Use the generated URL to invite the bot

## Usage

### Start Recording

In any text channel, type:
```
/record
```

The bot will:
1. Join your current voice channel
2. Start recording all participants
3. Send a control message with 🛑 (stop) reaction

### Stop Recording

Click the 🛑 reaction on the control message, or the bot will automatically stop when everyone leaves.

### Get Transcription & Summary

After stopping, the bot will:
1. Transcribe the audio using Whisper
2. Generate a summary (if Z.AI API key is configured)
3. Send results to the text channel

## Project Structure

```
.
├── src/
│   ├── main.rs              # Bot entry point
│   ├── voice_recorder.rs    # Voice recording logic
│   ├── transcriber.rs       # Whisper transcription
│   ├── summarizer.rs        # AI summarization
│   └── commands.rs          # Command handlers
├── models/                  # Whisper models
├── recordings/             # Temporary audio files
├── Cargo.toml
└── .env
```

## Configuration

### Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `DISCORD_TOKEN` | Yes | Discord bot token |
| `DISCORD_APPLICATION_ID` | Yes | Discord application ID |
| `WHISPER_MODEL_PATH` | Yes | Path to Whisper model |
| `ZAI_API_KEY` | No | Z.AI API key for summarization |

### Systemd Service

Example service file at `/etc/systemd/system/discord-bot.service`:

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

Enable and start:
```bash
sudo systemctl enable discord-bot
sudo systemctl start discord-bot
```

## Troubleshooting

### Bot doesn't respond to commands
- Check if the bot has proper permissions
- Verify `DISCORD_TOKEN` is correct
- Check logs: `journalctl -u discord-bot -f`

### Recording quality issues
- Ensure the bot has "Use Voice Activity" permission
- Check if the voice channel bitrate is sufficient
- Try a different Whisper model size

### Transcription fails
- Verify Whisper model file exists and is valid
- Check available RAM (larger models need more memory)
- Ensure recordings directory has write permissions

## License

MIT License - see [LICENSE](LICENSE) file for details.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## Acknowledgments

- [twilight-rs](https://twilight.rs/) - Discord library for Rust
- [whisper.cpp](https://github.com/ggerganov/whisper.cpp) - Speech recognition
- [Songbird](https://github.com/serenity-rs/songbird) - Voice handling
