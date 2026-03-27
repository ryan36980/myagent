> [Configuration Guide](configuration.md) > Voice Configuration

# Text-to-Speech (TTS) Configuration

## Edge TTS (Default, Free)

```json5
{
  "messages": {
    "tts": {
      "auto": "inbound",       // "inbound" | "always" | "tagged" | other value = disabled
      "provider": "edge",
      "maxTextLength": 500,
      "edge": {
        "voice": "zh-CN-XiaoxiaoNeural",  // Chinese female voice
        "rate": "+10%",                    // speech rate (optional)
        "pitch": "+0Hz",                   // pitch (optional)
        "volume": "+0%"                    // volume (optional)
        // "chromiumVersion": "143.0.3650.75"  // DRM version (update if 403 occurs)
      }
    }
  }
}
```

No API Key required, free to use. Common Chinese voices:
- `zh-CN-XiaoxiaoNeural` — female voice (default)
- `zh-CN-YunxiNeural` — male voice
- `zh-CN-XiaoyiNeural` — female voice (lively)

**Troubleshooting:** If Edge TTS returns a 403 error, it is usually because `chromiumVersion` is outdated. Check the [edge-tts Python library](https://github.com/rany2/edge-tts) for the latest version number and update the `chromiumVersion` field in the configuration — no code changes needed. For more issues see the [Troubleshooting Guide](troubleshooting.md).

**TTS auto mode description:**
- `"inbound"` — when the user sends a voice message, the reply also includes voice (default)
- `"always"` — every reply includes voice
- `"tagged"` — only converts to voice when the Agent reply contains a `<speak>` tag
- Other values — TTS disabled

## OpenAI TTS

```json5
{
  "messages": {
    "tts": {
      "provider": "openai",
      "auto": "inbound",
      "openai": {
        "apiKeyEnv": "OPENAI_API_KEY",     // environment variable name
        "model": "tts-1",                   // "tts-1" or "tts-1-hd"
        "voice": "alloy"                    // "alloy" | "echo" | "nova" | "shimmer" etc.
        // "baseUrl": "https://api.openai.com/v1"  // optional, custom endpoint
      }
    }
  }
}
```

**.env file:**
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
        "modelId": "eleven_multilingual_v2",  // supports Chinese
        "voiceId": "abc123def"                // obtain from ElevenLabs console
        // "baseUrl": "https://api.elevenlabs.io/v1"  // optional
      }
    }
  }
}
```

**.env file:**
```
ELEVENLABS_API_KEY=xxxx
```

Get Voice ID: Visit [elevenlabs.io](https://elevenlabs.io/) → Voices → Select/Create a voice → Copy Voice ID.

---

# Speech-to-Text (STT) Configuration

Three STT providers are supported: Groq Whisper, Volcengine (Doubao), and Google Cloud Speech-to-Text.

## Groq Whisper (OpenAI-Compatible Interface)

```json5
{
  "messages": {
    "mediaUnderstanding": {
      "audio": {
        "provider": "groq",
        "model": "whisper-large-v3-turbo"
        // "baseUrl": "https://my-whisper-server/v1"  // optional, self-hosted Whisper
      }
    }
  }
}
```

**.env file:**
```
GROQ_API_KEY=gsk_xxxx
```

## Volcengine / Doubao (Volcengine BigModel ASR)

Chinese speech recognition service with a free tier — no overseas account required.

```json5
{
  "messages": {
    "mediaUnderstanding": {
      "audio": {
        "provider": "doubao",    // or "volcengine", they are equivalent
        "volcengine": {
          "appId": "your-app-id",
          "accessToken": "${VOLCENGINE_ACCESS_TOKEN}",
          "cluster": "volc.bigasr.sauc.duration"
        }
      }
    }
  }
}
```

**.env file:**
```
VOLCENGINE_ACCESS_TOKEN=your-access-token
```

**Setup steps:**
1. Register at the [Volcengine Console](https://console.volcengine.com/)
2. Enable "Speech Technology" → "Speech Recognition" → "BigModel Speech Recognition" service
3. Create an application and obtain the `appId`
4. Obtain the Access Token (Access Key)
5. Select a cluster, default is `volc.bigasr.sauc.duration`

**Configuration field descriptions:**
- `appId` — Application ID (numeric string)
- `accessToken` — Access token, recommended to inject via environment variable
- `cluster` — Service cluster; different clusters correspond to different model capabilities

## Google Cloud Speech-to-Text

Google Cloud speech recognition REST API (v1 synchronous recognition), supports multiple languages.

```json5
{
  "messages": {
    "mediaUnderstanding": {
      "audio": {
        "provider": "google",
        "apiKey": "${GOOGLE_STT_API_KEY}",
        "google": {
          "languageCode": "zh-CN"  // BCP-47 language code
        }
      }
    }
  }
}
```

**.env file:**
```
GOOGLE_STT_API_KEY=AIzaSy...
```

**Setup steps:**
1. Log in to [Google Cloud Console](https://console.cloud.google.com/)
2. Create a project (or select an existing one)
3. Enable the **Cloud Speech-to-Text API**
4. Create an API Key ("APIs & Services" → "Credentials" → "Create Credentials" → "API key")
5. (Recommended) Restrict the API Key to allow only the Speech-to-Text API

**Configuration field descriptions:**
- `languageCode` — BCP-47 language code, e.g. `"zh-CN"` (Simplified Chinese), `"en-US"` (American English), `"ja-JP"` (Japanese)
- Supported audio formats: OGG/Opus, WAV, MP3, FLAC (encoding auto-detected)

> STT is optional. If no STT provider is configured, voice messages will be ignored and all other features will work normally.
