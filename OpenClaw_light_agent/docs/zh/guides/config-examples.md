> [配置指南](configuration.md) > 完整示例与部署

# 完整配置示例

## 示例 A：最简配置（Claude + Telegram）

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

**.env：**
```
TELEGRAM_BOT_TOKEN=123456:ABCxxx
ANTHROPIC_API_KEY=sk-ant-xxx
```

## 示例 B：DeepSeek + Failover 到 Groq

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

**.env：**
```
TELEGRAM_BOT_TOKEN=123456:ABCxxx
DEEPSEEK_API_KEY=sk-xxx
GROQ_API_KEY=gsk_xxx
```

## 示例 C：Claude OAuth + 全功能

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

**.env：**
```
TELEGRAM_BOT_TOKEN=123456:ABCxxx
DEEPSEEK_API_KEY=sk-xxx
GROQ_API_KEY=gsk_xxx
HA_TOKEN=eyJhbGci...
```

## 示例 D：本地 LLM（Ollama + Telegram）

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

**.env：**
```
TELEGRAM_BOT_TOKEN=123456:ABCxxx
OLLAMA_API_KEY=ollama
```

> **提示：** `baseUrl` 中使用 `host.docker.internal` 是因为 Ollama 运行在宿主机而 OpenClaw 运行在 Docker 容器中。如果都在宿主机或都在同一 Docker 网络中，直接用 `localhost` 即可。更多连接问题参考 [故障排除 §10](troubleshooting.md#10-本地-llmollama--vllm--llamacpp)。

---

# 环境变量汇总

| 环境变量 | 用途 | 是否必须 |
|---------|------|---------|
| `TELEGRAM_BOT_TOKEN` | Telegram Bot Token | 使用 Telegram 时必须 |
| `FEISHU_APP_ID` | 飞书 App ID | 使用飞书时必须 |
| `FEISHU_APP_SECRET` | 飞书 App Secret | 使用飞书时必须 |
| `ANTHROPIC_API_KEY` | Anthropic Claude API Key | Claude API Key 模式必须 |
| `DEEPSEEK_API_KEY` | DeepSeek API Key | 使用 DeepSeek 时必须 |
| `GROQ_API_KEY` | Groq API Key（LLM + STT） | 使用 Groq LLM 或语音识别时必须 |
| `VOLCENGINE_ACCESS_TOKEN` | 火山引擎 Access Token | 使用豆包语音识别时必须 |
| `GLM_API_KEY` | 智谱 GLM API Key | 使用 GLM 时必须 |
| `OPENAI_API_KEY` | OpenAI API Key（LLM + TTS） | 使用 OpenAI LLM 或 TTS 时必须 |
| `ELEVENLABS_API_KEY` | ElevenLabs TTS API Key | 使用 ElevenLabs TTS 时必须 |
| `BRAVE_SEARCH_API_KEY` | Brave Search API Key | 使用 Brave 搜索时必须 |
| `HA_TOKEN` | Home Assistant 长期访问令牌 | 使用 HA 控制时必须 |
| `OLLAMA_API_KEY` | Ollama API Key（任意非空值） | 使用 Ollama 时必须 |
| `VLLM_API_KEY` | vLLM API Key | 使用 vLLM 时必须 |
| `RUST_LOG` | 日志级别（如 `info`, `debug`） | 可选，默认 `info` |

---

# 部署文件关系

部署时只需要编辑 **2 个文件**，其他文件保持默认即可：

```
项目目录/
├── openclaw.json          ← 【必编辑】所有应用配置集中在此
├── .env                   ← 【必编辑】API Key 等敏感信息
├── docker-compose.yml     ← 【不用改】Docker 运行参数（默认即可）
├── sessions/              ← 自动创建，会话数据
├── memory/                ← 自动创建，记忆数据
├── skills/                ← 自动创建，技能脚本
└── auth_tokens.json       ← 自动创建，OAuth token（如使用）
```

> **重要：** `openclaw.json` 中的目录路径（`session.dir`、`memory.dir`、`exec.skillsDir`、`auth.tokenFile`）使用默认值即可，**不要修改**。这些路径与 `docker-compose.yml` 中的 volume 挂载一一对应。如果你改了 `openclaw.json` 中的路径，`docker-compose.yml` 也必须同步修改——所以最好别改。

## 快速部署步骤

```bash
# 1. 构建镜像
docker build -t openclaw-light:latest .

# 2. 从模板创建配置文件
cp config/openclaw.json.example openclaw.json
# 编辑 openclaw.json：选择提供商、模型、通道、工具等

# 3. 创建环境变量文件
cat > .env << 'EOF'
TELEGRAM_BOT_TOKEN=你的Bot Token
ANTHROPIC_API_KEY=你的API Key
EOF

# 4. 创建数据目录
mkdir -p sessions memory skills

# 5. 启动
docker compose up -d

# 6. 查看日志
docker compose logs -f
```
