> [Configuration Guide](configuration.md) > LLM Providers & Authentication

# LLM Provider Configuration

Select the LLM provider via the top-level `provider` and `model` fields. 5 known providers are built in with default values pre-filled:

| Provider | `provider` value | Default model | API Key env var | Default Base URL |
|----------|-----------------|---------------|-----------------|-----------------|
| Anthropic Claude | `"anthropic"` | `claude-sonnet-4-5-20250929` | `ANTHROPIC_API_KEY` | `api.anthropic.com` (built-in) |
| DeepSeek | `"deepseek"` | `deepseek-chat` | `DEEPSEEK_API_KEY` | `https://api.deepseek.com/v1` |
| Groq | `"groq"` | `llama-3.3-70b-versatile` | `GROQ_API_KEY` | `https://api.groq.com/openai/v1` |
| Zhipu GLM | `"glm"` | `glm-5` | `GLM_API_KEY` | `https://api.z.ai/api/paas/v4` |
| OpenAI | `"openai"` | `gpt-4o` | `OPENAI_API_KEY` | `https://api.openai.com/v1` |

---

## 1. Anthropic Claude

Minimal configuration (API Key mode):

```json5
{
  "provider": "anthropic",
  "model": "claude-sonnet-4-5-20250929"
}
```

**.env file:**
```
ANTHROPIC_API_KEY=sk-ant-api03-xxxx
```

Get API Key: Visit [console.anthropic.com](https://console.anthropic.com/) → API Keys → Create Key.

**Available models:**
- `claude-opus-4-6` — most capable, highest cost
- `claude-sonnet-4-5-20250929` — default, good balance
- `claude-haiku-4-5-20251001` — fastest and cheapest

> Note: Both short names (e.g. `claude-sonnet-4-5`) and full names with dates are accepted. For OAuth mode, short names are recommended — dated IDs may return 404.

## 2. DeepSeek

```json5
{
  "provider": "deepseek",
  "model": "deepseek-chat"  // can be omitted, uses default model automatically
}
```

**.env file:**
```
DEEPSEEK_API_KEY=sk-xxxx
```

Get API Key: Visit [platform.deepseek.com](https://platform.deepseek.com/) → API Keys.

**Available models:**
- `deepseek-chat` — general conversation (default)
- `deepseek-reasoner` — enhanced reasoning

## 3. Groq (LLM)

```json5
{
  "provider": "groq",
  "model": "llama-3.3-70b-versatile"  // can be omitted
}
```

**.env file:**
```
GROQ_API_KEY=gsk_xxxx
```

Get API Key: Visit [console.groq.com](https://console.groq.com/) → API Keys.

> Note: Groq is used for both LLM and STT (Whisper), sharing the same `GROQ_API_KEY`.

**Available models:**
- `llama-3.3-70b-versatile` — default, balanced
- `llama-3.1-8b-instant` — faster, cheaper
- `mixtral-8x7b-32768` — long context

## 4. Zhipu GLM

```json5
{
  "provider": "glm",
  "model": "glm-5"  // can be omitted
}
```

**.env file:**
```
GLM_API_KEY=xxxx.xxxx
```

Get API Key: Visit [open.bigmodel.cn](https://open.bigmodel.cn/) → API Keys.

## 5. OpenAI

```json5
{
  "provider": "openai",
  "model": "gpt-4o"  // can be omitted
}
```

**.env file:**
```
OPENAI_API_KEY=sk-xxxx
```

Get API Key: Visit [platform.openai.com](https://platform.openai.com/) → API Keys.

**Available models:**
- `gpt-4o` — default, multimodal
- `gpt-4o-mini` — faster and cheaper
- `o3-mini` — enhanced reasoning

## 6. Custom / Local LLM (OpenAI-Compatible)

Any service compatible with the OpenAI Chat Completions API (`POST /chat/completions`) can be used, including locally deployed LLMs.
Set `provider` to any non-built-in name (anything other than `anthropic`/`deepseek`/`groq`/`glm`/`openai`) and use `providerConfig` to specify connection details:

```json5
{
  "provider": "local",          // any name; non-built-in triggers OpenAI-compatible path
  "model": "your-model-name",   // must match the model name in your local LLM
  "providerConfig": {
    "baseUrl": "http://192.168.1.100:8000/v1",  // your local LLM address
    "apiKeyEnv": "LOCAL_LLM_API_KEY",            // env var name (not the key itself)
    "maxTokens": 4096                            // optional
  }
}
```

**.env file:**
```
LOCAL_LLM_API_KEY=your-secret-key
```

> `baseUrl` should include `/v1` — the code appends `/chat/completions` automatically, resulting in `http://192.168.1.100:8000/v1/chat/completions`.

`providerConfig` fields:
- `baseUrl` — API base URL (required, include `/v1`)
- `apiKeyEnv` — environment variable name for the API Key (required; even if your local LLM doesn't need auth, set it to any non-empty string)
- `maxTokens` — maximum output token count (optional)

### Common Local LLM Examples

**Ollama:**
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
OLLAMA_API_KEY=ollama       # Ollama doesn't validate keys; any non-empty value works
```

**vLLM:**
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
VLLM_API_KEY=your-vllm-api-key     # set via --api-key when starting vLLM
```

**llama.cpp server:**
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
LLAMACPP_API_KEY=any        # llama.cpp doesn't validate by default; any non-empty value
```

> This also works for any OpenAI API-compatible proxy, API gateway, or relay service. Just point `baseUrl` to the proxy address.

---

# Authentication Configuration

## API Key Mode (Default)

No `auth` section needed — just set the environment variable for the corresponding provider.

## Anthropic OAuth Mode

Use a Claude Max/Pro subscription account (instead of API billing) with OAuth 2.0 PKCE authorization:

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

**Setup steps:**
1. Send `/auth start` in Telegram — the bot will return an authorization link
2. Open the link in a browser, log in with your Claude Max/Pro account and authorize
3. The page will display an authorization code — copy it
4. Send `/auth <authorization-code>` in Telegram (without the angle brackets)
5. Send `/auth status` to confirm authorization

**Configuration field descriptions:**
- `mode` — `"api_key"` (default) or `"oauth"`
- `clientId` — OAuth client ID (the default value is fine unless you have your own application)
- `tokenFile` — token persistence file path (default `./auth_tokens.json`)

> Note: OAuth mode does not require the `ANTHROPIC_API_KEY` environment variable. Tokens are automatically refreshed (5 minutes before expiry).

> Tip: If you have Claude Code (Anthropic's CLI tool) installed locally, you can reuse its token. The file is at `~/.claude/.credentials.json` in `sk-ant-oat01-...` format — set it directly as `ANTHROPIC_API_KEY` to use it as an API Key.

---

# Failover Fallback Models

When the primary model fails (rate limited, timed out, service unavailable), fallback models are tried in order automatically:

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

**.env file must include keys for all providers:**
```
ANTHROPIC_API_KEY=sk-ant-xxx
DEEPSEEK_API_KEY=sk-xxx
GROQ_API_KEY=gsk_xxx
```

**Behavior:**
- Primary model fails → try first fallback → fails → try second → ...
- Failed providers enter a cooldown period (exponential backoff) and are not retried immediately
- After cooldown, automatically returns to the highest-priority available provider
