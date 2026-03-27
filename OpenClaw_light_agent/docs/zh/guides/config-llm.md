> [配置指南](configuration.md) > LLM 提供商与认证

# LLM 提供商配置

通过顶层 `provider` 和 `model` 字段选择 LLM 提供商。内置 5 个已知提供商，自动填充默认值：

| 提供商 | `provider` 值 | 默认模型 | API Key 环境变量 | 默认 Base URL |
|--------|--------------|---------|-----------------|--------------|
| Anthropic Claude | `"anthropic"` | `claude-sonnet-4-5-20250929` | `ANTHROPIC_API_KEY` | `api.anthropic.com`（内置） |
| DeepSeek | `"deepseek"` | `deepseek-chat` | `DEEPSEEK_API_KEY` | `https://api.deepseek.com/v1` |
| Groq | `"groq"` | `llama-3.3-70b-versatile` | `GROQ_API_KEY` | `https://api.groq.com/openai/v1` |
| 智谱 GLM | `"glm"` | `glm-5` | `GLM_API_KEY` | `https://api.z.ai/api/paas/v4` |
| OpenAI | `"openai"` | `gpt-4o` | `OPENAI_API_KEY` | `https://api.openai.com/v1` |

---

## 1. Anthropic Claude

最简配置（API Key 模式）：

```json5
{
  "provider": "anthropic",
  "model": "claude-sonnet-4-5-20250929"
}
```

**.env 文件：**
```
ANTHROPIC_API_KEY=sk-ant-api03-xxxx
```

获取 API Key：访问 [console.anthropic.com](https://console.anthropic.com/) → API Keys → 创建密钥。

**可选模型：**
- `claude-opus-4-6` — 最强，成本最高
- `claude-sonnet-4-5-20250929` — 默认，性价比好
- `claude-haiku-4-5-20251001` — 最快最便宜

> 注意：使用短名称（如 `claude-sonnet-4-5`）或带日期的完整名称均可。OAuth 模式下建议使用短名称，带日期的 ID 可能返回 404。

## 2. DeepSeek

```json5
{
  "provider": "deepseek",
  "model": "deepseek-chat"  // 可省略，自动使用默认模型
}
```

**.env 文件：**
```
DEEPSEEK_API_KEY=sk-xxxx
```

获取 API Key：访问 [platform.deepseek.com](https://platform.deepseek.com/) → API Keys。

**可选模型：**
- `deepseek-chat` — 通用对话（默认）
- `deepseek-reasoner` — 推理增强

## 3. Groq（LLM）

```json5
{
  "provider": "groq",
  "model": "llama-3.3-70b-versatile"  // 可省略
}
```

**.env 文件：**
```
GROQ_API_KEY=gsk_xxxx
```

获取 API Key：访问 [console.groq.com](https://console.groq.com/) → API Keys。

> 注意：Groq 同时用于 LLM 和 STT（Whisper），共用同一个 `GROQ_API_KEY`。

**可选模型：**
- `llama-3.3-70b-versatile` — 默认，平衡
- `llama-3.1-8b-instant` — 更快，更便宜
- `mixtral-8x7b-32768` — 长上下文

## 4. 智谱 GLM

```json5
{
  "provider": "glm",
  "model": "glm-5"  // 可省略
}
```

**.env 文件：**
```
GLM_API_KEY=xxxx.xxxx
```

获取 API Key：访问 [open.bigmodel.cn](https://open.bigmodel.cn/) → API Keys。

## 5. OpenAI

```json5
{
  "provider": "openai",
  "model": "gpt-4o"  // 可省略
}
```

**.env 文件：**
```
OPENAI_API_KEY=sk-xxxx
```

获取 API Key：访问 [platform.openai.com](https://platform.openai.com/) → API Keys。

**可选模型：**
- `gpt-4o` — 默认，多模态
- `gpt-4o-mini` — 更快更便宜
- `o3-mini` — 推理增强

## 6. 自定义 / 本地 LLM（OpenAI 兼容）

任何兼容 OpenAI Chat Completions API（`POST /chat/completions`）的服务都可以接入，包括本地部署的 LLM。
将 `provider` 设为任意非内置名称（即 `anthropic`/`deepseek`/`groq`/`glm`/`openai` 以外的值），配合 `providerConfig` 指定连接信息：

```json5
{
  "provider": "local",          // 任意名称，非内置即走 OpenAI 兼容路径
  "model": "your-model-name",   // 模型名称（必须与本地 LLM 一致）
  "providerConfig": {
    "baseUrl": "http://192.168.1.100:8000/v1",  // 本地 LLM 的地址
    "apiKeyEnv": "LOCAL_LLM_API_KEY",            // 环境变量名（非 Key 本身）
    "maxTokens": 4096                            // 可选
  }
}
```

**.env 文件：**
```
LOCAL_LLM_API_KEY=your-secret-key
```

> `baseUrl` 应包含 `/v1`，代码会自动拼接 `/chat/completions`，最终请求 `http://192.168.1.100:8000/v1/chat/completions`。

`providerConfig` 字段：
- `baseUrl` — API 基础 URL（必填，含 `/v1`）
- `apiKeyEnv` — API Key 的环境变量名（必填，即使本地 LLM 不需要认证也要设置，值可以是任意非空字符串）
- `maxTokens` — 最大输出 token 数（可选）

### 常见本地 LLM 示例

**Ollama：**
```json5
{
  "provider": "ollama",
  "model": "qwen2.5:14b",
  "providerConfig": {
    "baseUrl": "http://localhost:11434/v1",
    "apiKeyEnv": "OLLAMA_API_KEY"
  }
}
```
```
OLLAMA_API_KEY=ollama       # Ollama 不校验 Key，任意非空值即可
```

**vLLM：**
```json5
{
  "provider": "vllm",
  "model": "Qwen/Qwen2.5-72B-Instruct",
  "providerConfig": {
    "baseUrl": "http://192.168.1.100:8000/v1",
    "apiKeyEnv": "VLLM_API_KEY",
    "maxTokens": 4096
  }
}
```
```
VLLM_API_KEY=your-vllm-api-key     # 启动 vLLM 时用 --api-key 设置
```

**llama.cpp server：**
```json5
{
  "provider": "llamacpp",
  "model": "local",
  "providerConfig": {
    "baseUrl": "http://localhost:8080/v1",
    "apiKeyEnv": "LLAMACPP_API_KEY"
  }
}
```
```
LLAMACPP_API_KEY=any        # llama.cpp 默认不校验，任意非空值
```

> 也可以用于任何 OpenAI API 兼容的中转代理、API 网关等场景。将 `baseUrl` 指向代理地址即可。

---

# 认证配置

## API Key 模式（默认）

无需配置 `auth` 节，直接设置对应提供商的环境变量即可。

## Anthropic OAuth 模式

使用 Claude Max/Pro 订阅账号（而非 API 付费），通过 OAuth 2.0 PKCE 授权：

```json5
{
  "provider": "anthropic",
  "auth": {
    "mode": "oauth",
    "clientId": "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
    "tokenFile": "./auth_tokens.json"
  }
}
```

**设置步骤：**
1. 在 Telegram 中发送 `/auth start`，机器人会返回一个授权链接
2. 在浏览器中打开链接，用 Claude Max/Pro 账号登录并授权
3. 页面会显示一个授权码，复制它
4. 在 Telegram 中发送 `/auth <授权码>`（去掉尖括号）
5. 发送 `/auth status` 确认授权状态

**配置字段说明：**
- `mode` — `"api_key"`（默认）或 `"oauth"`
- `clientId` — OAuth 客户端 ID（默认值即可，除非你有自己的应用）
- `tokenFile` — Token 持久化文件路径（默认 `./auth_tokens.json`）

> 注意：OAuth 模式下不需要设置 `ANTHROPIC_API_KEY` 环境变量。Token 会自动刷新（过期前 5 分钟）。

> 技巧：如果你本地装了 Claude Code（Anthropic 的 CLI 工具），可以复用它的 token。文件位于 `~/.claude/.credentials.json`，格式为 `sk-ant-oat01-...`，直接设为 `ANTHROPIC_API_KEY` 即可当 API Key 使用。

---

# Failover 备选模型

当主模型失败（限流、超时、服务不可用）时，自动按顺序尝试备选模型：

```json5
{
  "provider": "anthropic",
  "model": "claude-sonnet-4-5-20250929",
  "agents": {
    "fallbackModels": [
      {
        "provider": "deepseek",
        "model": "deepseek-chat",
        "apiKeyEnv": "DEEPSEEK_API_KEY"
      },
      {
        "provider": "groq",
        "model": "llama-3.3-70b-versatile",
        "apiKeyEnv": "GROQ_API_KEY",
        "baseUrl": "https://api.groq.com/openai/v1"
      }
    ]
  }
}
```

**.env 文件需要包含所有提供商的 Key：**
```
ANTHROPIC_API_KEY=sk-ant-xxx
DEEPSEEK_API_KEY=sk-xxx
GROQ_API_KEY=gsk_xxx
```

**行为：**
- 主模型失败 → 尝试第一个备选 → 失败 → 尝试第二个 → ...
- 失败的提供商进入冷却期（指数退避），不会反复重试
- 冷却恢复后自动回到优先级最高的可用提供商
