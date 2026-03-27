[中文](README.md) | [English](README.en.md)

# OpenClaw Light

The Rust lightweight edition of OpenClaw — a universal personal AI assistant designed for resource-constrained embedded devices.

Resident memory ~1.3MB, typical load <4MB, extreme peak <8MB (~2% of the Node.js version), producing a single statically-linked binary with no external dependencies.

## Features

- **Multi-channel**: Telegram voice/text (long polling), Feishu/Lark (WebSocket), CLI, HTTP API (SSE streaming)
- **Multi-LLM backends**: Anthropic Claude / DeepSeek / Groq / OpenAI compatible, with automatic failover
- **ReAct Agent**: Automatic tool calls, time-based timeout (default 900s), three-level loop detection + global circuit breaker
- **Async Sub-Agent**: `sessions_spawn` launches asynchronously without blocking the main conversation, auto-reports results
- **SSE Streaming**: Real-time streaming output to Telegram / HTTP API / CLI
- **Embedded Web Chat UI**: `GET /` serves a built-in chat page with SSE streaming, dark/light themes, and Markdown rendering
- **HTTP API authentication**: Optional Bearer Token endpoint protection + CORS support
- **Interrupt mode**: New messages automatically interrupt current agent execution for immediate processing
- **Optional Home Assistant integration**
- **Multiple TTS providers**: Edge TTS (free default) / OpenAI TTS / ElevenLabs / Volcengine
- **TTS four modes**: inbound (voice-triggered) / always / tagged (`<speak>` tag) / off
- **Multiple STT providers**: Groq Whisper / Volcengine (Doubao) / Google Cloud STT
- **Long-term memory system**: per-chat MEMORY.md + daily logs + substring search + adaptive injection
- **Scheduled tasks**: cron expressions + one-time scheduling + multiple delivery modes (announce/webhook/silent)
- **Session management**: JSONL persistence + turn-based limiting (dmHistoryLimit) + auto-compaction
- **Context Files**: Load custom knowledge files into system prompt (SOUL.md equivalent)
- **Image/Vision**: Telegram photos/documents → multimodal LLM
- **Parallel tool execution**: `join_all` cooperative concurrency
- **File operation tools**: file_read / file_write / file_edit / file_find
- **Auto backup**: Cron-scheduled packaging + agent tool on-demand trigger, rolling cleanup (by age + total size)
- **Transient error retry**: Network/overload errors auto-retry with 2.5s backoff
- **Context Pruning**: Auto-trim old tool outputs to save context window
- **Anthropic OAuth 2.0**: PKCE authorization code flow, supports Claude Max/Pro subscriptions

## Supported Platforms

| Target | Applicable Devices |
|--------|-------------------|
| `x86_64-unknown-linux-musl` | General Linux servers |
| `aarch64-unknown-linux-musl` | Raspberry Pi 4/5, RK3588 (64-bit ARM) |
| `armv7-unknown-linux-musleabihf` | Raspberry Pi 2/3, NanoPi (32-bit ARM) |
| `x86_64-pc-windows-gnu` | Windows x86_64 |

Pre-built binaries for all platforms are available on the [Releases](https://gitcode.com/bell-innovation/OpenClaw_light/releases) page — no build required.

## Prerequisites

- **Docker** (only needed when building from source; not required if using pre-built binaries)
- **API Keys** (as needed):
  - Telegram Bot Token (create via [@BotFather](https://t.me/BotFather))
  - Feishu App ID + App Secret (create app on [Feishu Open Platform](https://open.feishu.cn/))
  - Anthropic API Key (Claude) or OAuth 2.0 authentication
  - STT: Groq API Key / Volcengine Access Token / Google STT API Key (optional)
  - Home Assistant Long-Lived Access Token (optional)

## Quick Start

### 1. Get the Binary

#### Option A: Download Pre-built Binary (Recommended)

Download the binary for your platform from the [Releases](https://gitcode.com/bell-innovation/OpenClaw_light/releases) page:

| File | Platform |
|------|----------|
| `openclaw-light-<version>-x86_64-unknown-linux-musl` | Linux x86_64 |
| `openclaw-light-<version>-aarch64-unknown-linux-musl` | Linux ARM64 (Raspberry Pi 4/5) |
| `openclaw-light-<version>-armv7-unknown-linux-musleabihf` | Linux ARMv7 (Raspberry Pi 2/3) |
| `openclaw-light-<version>-x86_64-pc-windows-gnu.exe` | Windows x86_64 |

On Linux, make the binary executable after downloading:

```bash
chmod +x openclaw-light-*
```

#### Option B: Build from Source

Requires Docker; no Rust toolchain needed on the host.

```bash
# Build for ARM64 (Raspberry Pi 4/5)
./scripts/docker-build.sh aarch64

# Build all 4 platforms (including Windows)
./scripts/docker-build.sh

# Verify build reproducibility
./scripts/docker-build.sh --verify x86_64
```

Artifacts are located at `dist/<target-triple>/openclaw-light`.

> First build requires downloading the ~1GB rust-musl-cross image and compiling dependencies. Subsequent builds leverage Docker layer caching and are much faster.

### 2. Configure

```bash
cp config/openclaw.json.example config/openclaw.json
```

Edit `config/openclaw.json` and fill in your API keys. Sensitive values are injected via environment variables:

```json5
{
  "channels": {
    "telegram": {
      "botToken": "${TELEGRAM_BOT_TOKEN}",
      "allowedUsers": ["your-telegram-user-id"]  // empty array = no restriction
    },
    "httpApi": {
      "enabled": true,
      "listen": "0.0.0.0:8080",
      "authToken": "${HTTP_AUTH_TOKEN}"  // optional, empty/unset = no auth
    }
  },
  "homeAssistant": {
    "url": "http://192.168.1.100:8123",
    "token": "${HA_TOKEN}"
  }
}
```

The config file uses JSON5 format with support for comments and trailing commas. See `config/openclaw.json.example` for a complete example.

### 3. Deploy to Target Device

#### Option A: Script Deployment (Recommended)

```bash
# Set target host (default: pi@raspberrypi.local)
export DEPLOY_HOST=pi@192.168.1.50
export DEPLOY_TARGET=aarch64-unknown-linux-musl

# Deploy binary + config template
./scripts/deploy.sh
```

After deployment, on the target device:

```bash
# Create config file
cd /opt/openclaw
cp openclaw.json.example openclaw.json
# Edit and fill in your API keys
nano openclaw.json

# Create environment variables file
cat > .env << 'EOF'
TELEGRAM_BOT_TOKEN=123456:ABCxxx
ANTHROPIC_API_KEY=sk-ant-xxx
GROQ_API_KEY=gsk_xxx              # Optional, only needed for speech recognition
HA_TOKEN=eyJhbGci...
EOF
chmod 600 .env
```

#### Option B: Docker Compose (Recommended for containerized deployment)

```bash
# 1. Build image
docker build -t openclaw-light:latest .

# 2. Prepare config files and directories
cp config/openclaw.json.example openclaw.json
# Edit openclaw.json and fill in API keys
mkdir -p sessions memory skills

# 3. Create .env file (optional, configure as needed)
cat > .env << 'EOF'
GROQ_API_KEY=gsk_xxx                    # Groq STT (optional)
VOLCENGINE_ACCESS_TOKEN=xxx             # Volcengine STT/TTS (optional)
HTTP_AUTH_TOKEN=your-secret-token       # HTTP API auth (optional)
EOF

# 4. Start
docker compose up -d

# View logs
docker compose logs -f
```

Container security features: read-only filesystem, all capabilities dropped, privilege escalation blocked, 10MB memory limit, non-root user.
The exec tool automatically isolates environment variables, preventing the LLM from reading API keys via `env` commands.

#### Option C: Manual Deployment

```bash
# 1. Copy binary to target device
scp dist/aarch64-unknown-linux-musl/openclaw-light pi@192.168.1.50:/opt/openclaw/

# 2. Copy config
scp config/openclaw.json pi@192.168.1.50:/opt/openclaw/

# 3. SSH to device and run
ssh pi@192.168.1.50
cd /opt/openclaw
export TELEGRAM_BOT_TOKEN=123456:ABCxxx
export ANTHROPIC_API_KEY=sk-ant-xxx
export GROQ_API_KEY=gsk_xxx
./openclaw-light --config openclaw.json
```

### 4. Set Up Auto-Start (systemd)

The service file is included in the repository at `deploy/openclaw-light.service`; `deploy.sh` installs it automatically.
For manual installation:

```bash
sudo cp /opt/openclaw/openclaw-light.service /etc/systemd/system/
```

```bash
# Create dedicated user
sudo useradd -r -s /usr/sbin/nologin openclaw
sudo chown -R openclaw:openclaw /opt/openclaw

# Start service
sudo systemctl daemon-reload
sudo systemctl enable --now openclaw-light

# View logs
sudo journalctl -u openclaw-light -f
```

## Project Structure

```
├── Cargo.toml                 # Dependencies and release profile
├── rust-toolchain.toml        # Rust 1.84.0 + musl targets
├── Dockerfile                 # Multi-stage reproducible build (busybox shell + non-root)
├── docker-compose.yml         # Production deployment (hardened security)
├── docker-compose.build.yml   # Compose build (optional)
├── config/
│   └── openclaw.json.example  # Config template
├── deploy/
│   └── openclaw-light.service  # systemd service unit
├── scripts/
│   ├── docker-build.sh        # Docker build/verify script
│   └── deploy.sh              # Remote deployment script
├── src/
│   ├── main.rs                # Entry + multi-channel dispatch + graceful shutdown + interrupt mode
│   ├── config.rs              # JSON5 config loading + env var substitution
│   ├── error.rs               # Unified error types
│   ├── backup.rs              # Auto-backup engine (packaging + rolling cleanup)
│   ├── channel/
│   │   ├── telegram.rs        # Telegram Bot API (long polling + streaming)
│   │   ├── feishu.rs          # Feishu/Lark (WebSocket long connection)
│   │   ├── cli.rs             # CLI interactive channel
│   │   ├── http_api.rs        # HTTP API channel (SSE streaming + Bearer Token + CORS)
│   │   ├── streaming.rs       # StreamingWriter (throttled message editing)
│   │   ├── types.rs           # Message type definitions
│   │   └── web_chat.html      # Embedded Web Chat UI (dark/light themes + Markdown rendering)
│   ├── provider/
│   │   ├── llm/
│   │   │   ├── claude.rs      # Anthropic Messages API (SSE + Extended Thinking)
│   │   │   ├── openai_compat.rs # OpenAI compatible (DeepSeek / Groq etc.)
│   │   │   └── failover.rs    # LLM failover chain (auto-switch + exponential backoff cooldown)
│   │   ├── stt/
│   │   │   ├── groq.rs        # Groq Whisper speech recognition
│   │   │   ├── volcengine.rs  # Volcengine (Doubao) speech recognition
│   │   │   └── google.rs      # Google Cloud STT
│   │   └── tts/
│   │       ├── edge.rs        # Edge TTS (free default)
│   │       ├── openai.rs      # OpenAI TTS
│   │       ├── elevenlabs.rs  # ElevenLabs TTS
│   │       ├── volcengine.rs  # Volcengine TTS
│   │       └── webm_to_ogg.rs # WebM Opus → OGG Opus remuxing
│   ├── agent/
│   │   ├── react_loop.rs      # ReAct loop + loop detection + context pruning + transient retry
│   │   └── context.rs         # System prompt assembly (memory + tools + runtime info + context files)
│   ├── auth/mod.rs            # Anthropic OAuth 2.0 (PKCE)
│   ├── memory/store.rs        # Long-term memory (MEMORY.md + logs + search)
│   ├── tools/
│   │   ├── agent_tool.rs      # Async sub-Agent (spawn/list/history/send)
│   │   ├── ha_control.rs      # Home Assistant control
│   │   ├── html_utils.rs      # HTML→plain text conversion (zero dependencies)
│   │   ├── web_fetch.rs       # Web fetching (HTML→Text + 128KB + pagination + SSRF protection)
│   │   ├── web_search.rs      # Web search (DuckDuckGo + Brave)
│   │   ├── cron.rs            # Scheduled tasks
│   │   ├── memory.rs          # Memory management (6 actions)
│   │   ├── exec.rs            # Shell command execution (environment isolation)
│   │   ├── file.rs            # File operations (read / write / edit / find)
│   │   ├── backup.rs          # Backup tool (agent on-demand trigger)
│   │   ├── get_time.rs        # Time query
│   │   └── mcp.rs             # MCP client (stdio JSON-RPC)
│   └── session/store.rs       # JSONL session persistence + turn-based limiting
└── docs/
    ├── zh/                     # Chinese docs (primary)
    │   ├── guides/             # User guides (configuration etc.)
    │   ├── design/             # Architecture design docs
    │   ├── specs/              # Coding conventions
    │   ├── reports/            # Monitoring reports
    │   └── requirements/       # Requirements & background
    └── en/                     # English docs (mirror of zh/)
```

## Logging

Control log level via the `RUST_LOG` environment variable:

```bash
RUST_LOG=info ./openclaw-light     # Production (default)
RUST_LOG=debug ./openclaw-light    # Debug
RUST_LOG=openclaw_light=debug ./openclaw-light  # Debug this project only
```

## Memory Usage

Real-world measurements (Docker container, 10 MiB limit):

| Scenario | Memory | % of Limit |
|----------|--------|-----------|
| Idle | ~1.3 MiB | 13% |
| Single tool execution | 2.0 ~ 2.6 MiB | 20~26% |
| Active ReAct loop | 2.7 ~ 3.3 MiB | 27~33% |
| Sub-Agent running | 3.0 ~ 3.6 MiB | 30~36% |
| Parallel web search + fetch | ~8 MiB (peak) | ~80% |

Detailed monitoring report: [docs/reports/](docs/en/reports/)

## Documentation

- **[Configuration Guide](docs/en/guides/configuration.md)** — Complete configuration reference: LLM providers, authentication, Failover, channels, TTS/STT, tools, full examples
- [Architecture Design](docs/en/design/README.md) — Trait definitions, module design, API protocols, memory budget, memory system, sub-Agent, Failover
- [Coding Conventions](docs/en/specs/rust-conventions.md) — Naming, error handling, memory rules, reproducible builds, development workflow
- [Monitoring Reports](docs/en/reports/) — Memory monitoring, performance analysis
- [Changelog](CHANGELOG.md) — Version change log
- [Original Design](docs/en/requirements/design.md) — Node.js initial proposal (archived, for reference only)
- [Technical Evaluation](docs/en/requirements/extreme-optimization-plan.md) — Three-plan comparison (selected Plan C: Rust)

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT), at your option.
