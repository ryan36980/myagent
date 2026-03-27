> [Configuration Guide](configuration.md) > Complete Examples & Deployment

# Complete Configuration Examples

## Example A: Minimal Configuration (Claude + Telegram)

```json5
{
  "provider": "anthropic",
  "channels": {
    "telegram": {
      "botToken": "${TELEGRAM_BOT_TOKEN}",
      "allowedUsers": [123456789]
    }
  },
  "tools": {
    "allow": ["web_fetch", "web_search", "get_time", "memory", "exec"]
  }
}
```

**.env:**
```
TELEGRAM_BOT_TOKEN=123456:ABCxxx
ANTHROPIC_API_KEY=sk-ant-xxx
```

## Example B: DeepSeek + Failover to Groq

```json5
{
  "provider": "deepseek",
  "model": "deepseek-chat",
  "channels": {
    "telegram": {
      "botToken": "${TELEGRAM_BOT_TOKEN}"
    }
  },
  "agents": {
    "fallbackModels": [
      {
        "provider": "groq",
        "model": "llama-3.3-70b-versatile",
        "apiKeyEnv": "GROQ_API_KEY",
        "baseUrl": "https://api.groq.com/openai/v1"
      }
    ]
  },
  "tools": {
    "allow": ["web_fetch", "web_search", "get_time", "memory", "exec"]
  }
}
```

**.env:**
```
TELEGRAM_BOT_TOKEN=123456:ABCxxx
DEEPSEEK_API_KEY=sk-xxx
GROQ_API_KEY=gsk_xxx
```

## Example C: Claude OAuth + Full Features

```json5
{
  "provider": "anthropic",
  "model": "claude-sonnet-4-5",
  "auth": {
    "mode": "oauth"
  },
  "channels": {
    "telegram": {
      "botToken": "${TELEGRAM_BOT_TOKEN}",
      "allowedUsers": [123456789]
    },
    "httpApi": {
      "enabled": true,
      "listen": "0.0.0.0:8080"
    }
  },
  "agents": {
    "thinking": "medium",
    "contextFiles": ["./SOUL.md"],
    "fallbackModels": [
      { "provider": "deepseek", "model": "deepseek-chat", "apiKeyEnv": "DEEPSEEK_API_KEY" }
    ]
  },
  "messages": {
    "tts": {
      "auto": "inbound",
      "provider": "edge",
      "edge": { "voice": "zh-CN-XiaoxiaoNeural" }
    }
  },
  "tools": {
    "allow": ["web_fetch", "web_search", "get_time", "cron", "memory", "exec",
              "ha_control", "sessions_spawn", "sessions_list", "sessions_history", "sessions_send",
              "file_read", "file_write", "file_edit", "file_find"]
  },
  "homeAssistant": {
    "url": "http://192.168.1.100:8123",
    "token": "${HA_TOKEN}"
  }
}
```

**.env:**
```
TELEGRAM_BOT_TOKEN=123456:ABCxxx
DEEPSEEK_API_KEY=sk-xxx
GROQ_API_KEY=gsk_xxx
HA_TOKEN=eyJhbGci...
```

## Example D: Local LLM (Ollama + Telegram)

```json5
{
  "provider": "ollama",
  "model": "qwen2.5:14b",
  "providerConfig": {
    "baseUrl": "http://host.docker.internal:11434/v1",
    "apiKeyEnv": "OLLAMA_API_KEY"
  },
  "channels": {
    "telegram": {
      "botToken": "${TELEGRAM_BOT_TOKEN}",
      "allowedUsers": [123456789]
    }
  },
  "tools": {
    "allow": ["web_fetch", "web_search", "get_time", "memory", "exec"]
  }
}
```

**.env:**
```
TELEGRAM_BOT_TOKEN=123456:ABCxxx
OLLAMA_API_KEY=ollama
```

> **Tip:** `host.docker.internal` is used in `baseUrl` because Ollama runs on the host machine while OpenClaw runs inside a Docker container. If both are on the host or in the same Docker network, use `localhost` directly. For more connection troubleshooting, see [Troubleshooting §10](troubleshooting.md#10-local-llm-ollama--vllm--llamacpp).

---

# Environment Variable Reference

| Environment Variable | Purpose | Required |
|---------------------|---------|----------|
| `TELEGRAM_BOT_TOKEN` | Telegram Bot Token | Required when using Telegram |
| `FEISHU_APP_ID` | Feishu App ID | Required when using Feishu |
| `FEISHU_APP_SECRET` | Feishu App Secret | Required when using Feishu |
| `ANTHROPIC_API_KEY` | Anthropic Claude API Key | Required in Claude API Key mode |
| `DEEPSEEK_API_KEY` | DeepSeek API Key | Required when using DeepSeek |
| `GROQ_API_KEY` | Groq API Key (LLM + STT) | Required when using Groq LLM or speech recognition |
| `VOLCENGINE_ACCESS_TOKEN` | Volcengine Access Token | Required when using Doubao speech recognition |
| `GLM_API_KEY` | Zhipu GLM API Key | Required when using GLM |
| `OPENAI_API_KEY` | OpenAI API Key (LLM + TTS) | Required when using OpenAI LLM or TTS |
| `ELEVENLABS_API_KEY` | ElevenLabs TTS API Key | Required when using ElevenLabs TTS |
| `BRAVE_SEARCH_API_KEY` | Brave Search API Key | Required when using Brave search |
| `HA_TOKEN` | Home Assistant Long-Lived Access Token | Required when using HA control |
| `OLLAMA_API_KEY` | Ollama API Key (any non-empty value) | Required when using Ollama |
| `VLLM_API_KEY` | vLLM API Key | Required when using vLLM |
| `RUST_LOG` | Log level (e.g. `info`, `debug`) | Optional, defaults to `info` |

---

# Deployment File Relationships

Only **2 files** need to be edited for deployment — all other files can stay at their defaults:

```
project-directory/
├── openclaw.json          ← [MUST EDIT] all application configuration goes here
├── .env                   ← [MUST EDIT] sensitive information such as API Keys
├── docker-compose.yml     ← [NO CHANGE NEEDED] Docker runtime parameters (defaults are fine)
├── sessions/              ← created automatically, session data
├── memory/                ← created automatically, memory data
├── skills/                ← created automatically, skills scripts
└── auth_tokens.json       ← created automatically, OAuth token (if used)
```

> **Important:** Directory paths in `openclaw.json` (`session.dir`, `memory.dir`, `exec.skillsDir`, `auth.tokenFile`) should keep their default values — **do not modify them**. These paths correspond one-to-one with the volume mounts in `docker-compose.yml`. If you change the paths in `openclaw.json`, you must also update `docker-compose.yml` in sync — so it is best not to change them at all.

## Quick Deployment Steps

```bash
# 1. Build the image
docker build -t openclaw-light:latest .

# 2. Create the configuration file from the template
cp config/openclaw.json.example openclaw.json
# Edit openclaw.json: select provider, model, channels, tools, etc.

# 3. Create the environment variable file
cat > .env << 'EOF'
TELEGRAM_BOT_TOKEN=your-bot-token
ANTHROPIC_API_KEY=your-api-key
EOF

# 4. Create data directories
mkdir -p sessions memory skills

# 5. Start
docker compose up -d

# 6. View logs
docker compose logs -f
```
