# OpenClaw Light 配置指南

配置文件使用 **JSON5** 格式（支持注释和尾随逗号），路径查找顺序：
1. 启动参数指定的路径
2. `./openclaw.json`（当前工作目录）
3. `~/.openclaw/openclaw.json`（用户主目录）

配置文件中所有 `${VAR_NAME}` 占位符会在加载时被替换为对应的环境变量值。

---

## 配置分类导航

| 文档 | 内容 |
|------|------|
| **[LLM 提供商与认证](config-llm.md)** | 6 个 LLM 提供商（Anthropic / DeepSeek / Groq / GLM / OpenAI / 自定义）、API Key / OAuth 认证、Failover 备选模型 |
| **[通道配置](config-channels.md)** | Telegram / 飞书 / HTTP API / CLI |
| **[语音配置](config-voice.md)** | TTS（Edge / OpenAI / ElevenLabs）、STT（Groq / 火山引擎 / Google） |
| **[Agent、工具与子系统](config-agent-tools.md)** | Agent 行为、工具列表、会话、记忆、Home Assistant、搜索、Shell、MCP |
| **[完整示例与部署](config-examples.md)** | 4 套完整配置示例、环境变量汇总、部署文件关系、快速部署步骤 |
| **[构建与部署](build-deploy.md)** | Docker 部署、Windows exe 交叉编译、Linux 静态二进制、systemd 服务 |

---

## 环境变量速查

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
| `RUST_LOG` | 日志级别（如 `info`, `debug`） | 可选，默认 `info` |
| `GOOGLE_STT_API_KEY` | Google Cloud STT API Key | 使用 Google STT 时必须 |

---

遇到问题？请查看 [故障排除指南](troubleshooting.md)。
