# OpenClaw Light Configuration Guide

The configuration file uses **JSON5** format (supports comments and trailing commas). The path lookup order is:
1. Path specified via startup argument
2. `./openclaw.json` (current working directory)
3. `~/.openclaw/openclaw.json` (user home directory)

All `${VAR_NAME}` placeholders in the configuration file are replaced with the corresponding environment variable values at load time.

---

## Configuration Categories

| Document | Contents |
|----------|----------|
| **[LLM Providers & Authentication](config-llm.md)** | 6 LLM providers (Anthropic / DeepSeek / Groq / GLM / OpenAI / Custom), API Key / OAuth authentication, Failover fallback models |
| **[Channel Configuration](config-channels.md)** | Telegram / Feishu / HTTP API / CLI |
| **[Voice Configuration](config-voice.md)** | TTS (Edge / OpenAI / ElevenLabs), STT (Groq / Volcengine / Google) |
| **[Agent, Tools & Subsystems](config-agent-tools.md)** | Agent behavior, tool list, sessions, memory, Home Assistant, search, Shell, MCP |
| **[Complete Examples & Deployment](config-examples.md)** | 4 complete configuration examples, environment variable reference, deployment file relationships, quick deployment steps |
| **[Build & Deployment](build-deploy.md)** | Docker deployment, Windows exe cross-compilation, Linux static binary, systemd service |

---

## Environment Variable Quick Reference

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
| `RUST_LOG` | Log level (e.g. `info`, `debug`) | Optional, defaults to `info` |
| `GOOGLE_STT_API_KEY` | Google Cloud STT API Key | Required when using Google STT |

---

Having issues? See the [Troubleshooting Guide](troubleshooting.md).
