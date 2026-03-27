> [配置指南](configuration.md) > 语音配置

# 语音合成（TTS）配置

## Edge TTS（默认，免费）

```json5
{
  "messages": {
    "tts": {
      "auto": "inbound",       // "inbound" | "always" | "tagged" | 其他值 = 关闭
      "provider": "edge",
      "maxTextLength": 500,
      "edge": {
        "voice": "zh-CN-XiaoxiaoNeural",  // 中文女声
        "rate": "+10%",                    // 语速（可选）
        "pitch": "+0Hz",                   // 音高（可选）
        "volume": "+0%"                    // 音量（可选）
        // "chromiumVersion": "143.0.3650.75"  // DRM 版本号（403 时更新）
      }
    }
  }
}
```

无需 API Key，免费使用。常用中文语音：
- `zh-CN-XiaoxiaoNeural` — 女声（默认）
- `zh-CN-YunxiNeural` — 男声
- `zh-CN-XiaoyiNeural` — 女声（活泼）

**故障排除：** 如果 Edge TTS 返回 403 错误，通常是 `chromiumVersion` 过时。查看 [edge-tts Python 库](https://github.com/rany2/edge-tts) 获取最新版本号，更新配置中的 `chromiumVersion` 字段即可，无需修改代码。更多问题参见 [故障排除指南](troubleshooting.md)。

**TTS 自动模式说明：**
- `"inbound"` — 用户发语音时，回复也带语音（默认）
- `"always"` — 每条回复都带语音
- `"tagged"` — 仅当 Agent 回复中包含 `<speak>` 标签时才转语音
- 其他值 — 关闭 TTS

## OpenAI TTS

```json5
{
  "messages": {
    "tts": {
      "provider": "openai",
      "auto": "inbound",
      "openai": {
        "apiKeyEnv": "OPENAI_API_KEY",     // 环境变量名
        "model": "tts-1",                   // "tts-1" 或 "tts-1-hd"
        "voice": "alloy"                    // "alloy" | "echo" | "nova" | "shimmer" 等
        // "baseUrl": "https://api.openai.com/v1"  // 可选，自定义端点
      }
    }
  }
}
```

**.env 文件：**
```
OPENAI_API_KEY=sk-xxxx
```

## ElevenLabs TTS

```json5
{
  "messages": {
    "tts": {
      "provider": "elevenlabs",
      "auto": "inbound",
      "elevenlabs": {
        "apiKeyEnv": "ELEVENLABS_API_KEY",
        "modelId": "eleven_multilingual_v2",  // 支持中文
        "voiceId": "abc123def"                // 从 ElevenLabs 控制台获取
        // "baseUrl": "https://api.elevenlabs.io/v1"  // 可选
      }
    }
  }
}
```

**.env 文件：**
```
ELEVENLABS_API_KEY=xxxx
```

获取 Voice ID：访问 [elevenlabs.io](https://elevenlabs.io/) → Voices → 选择/创建声音 → 复制 Voice ID。

---

# 语音识别（STT）配置

支持三种 STT 提供商：Groq Whisper、火山引擎（豆包）和 Google Cloud Speech-to-Text。

## Groq Whisper（OpenAI 兼容接口）

```json5
{
  "messages": {
    "mediaUnderstanding": {
      "audio": {
        "provider": "groq",
        "model": "whisper-large-v3-turbo"
        // "baseUrl": "https://my-whisper-server/v1"  // 可选，自托管 Whisper
      }
    }
  }
}
```

**.env 文件：**
```
GROQ_API_KEY=gsk_xxxx
```

## 火山引擎 / 豆包（Volcengine BigModel ASR）

免费额度的中文语音识别服务，无需海外账号。

```json5
{
  "messages": {
    "mediaUnderstanding": {
      "audio": {
        "provider": "doubao",    // 或 "volcengine"，两者等价
        "volcengine": {
          "appId": "你的应用ID",
          "accessToken": "${VOLCENGINE_ACCESS_TOKEN}",
          "cluster": "volc.bigasr.sauc.duration"
        }
      }
    }
  }
}
```

**.env 文件：**
```
VOLCENGINE_ACCESS_TOKEN=你的Access Token
```

**设置步骤：**
1. 注册 [火山引擎控制台](https://console.volcengine.com/)
2. 开通"语音技术" → "语音识别" → "大模型语音识别"服务
3. 创建应用，获取 `appId`
4. 获取 Access Token（Access Key）
5. 选择集群（cluster），默认 `volc.bigasr.sauc.duration`

**配置字段说明：**
- `appId` — 应用 ID（数字字符串）
- `accessToken` — 访问令牌，建议通过环境变量注入
- `cluster` — 服务集群，不同集群对应不同的模型能力

## Google Cloud Speech-to-Text

Google Cloud 的语音识别 REST API（v1 同步识别），支持多语言。

```json5
{
  "messages": {
    "mediaUnderstanding": {
      "audio": {
        "provider": "google",
        "apiKey": "${GOOGLE_STT_API_KEY}",
        "google": {
          "languageCode": "zh-CN"  // BCP-47 语言代码
        }
      }
    }
  }
}
```

**.env 文件：**
```
GOOGLE_STT_API_KEY=AIzaSy...
```

**设置步骤：**
1. 登录 [Google Cloud Console](https://console.cloud.google.com/)
2. 创建项目（或选择已有项目）
3. 启用 **Cloud Speech-to-Text API**
4. 创建 API Key（"APIs & Services" → "Credentials" → "Create Credentials" → "API key"）
5. （推荐）限制 API Key 仅允许 Speech-to-Text API

**配置字段说明：**
- `languageCode` — BCP-47 语言代码，如 `"zh-CN"`（简体中文）、`"en-US"`（美式英语）、`"ja-JP"`（日语）
- 支持的音频格式：OGG/Opus、WAV、MP3、FLAC（自动检测编码）

> STT 是可选的。如果不配置 STT 提供商，语音消息会被忽略，其他功能正常运行。
