# OpenClaw Light - Embedded Personal AI Assistant Design Document

> **Archived Document** — This document describes the early Node.js implementation plan. The project has selected the Rust approach and completed its implementation;
> the current architecture design is in the [Design Docs](../design/README.md).
> This document is retained as a reference for requirements and API protocols.

## 1. Project Overview

### 1.1 Goals

Run OpenClaw on an embedded device (small box) with only **200MB of RAM**, providing:

- Conversational AI via Telegram messages (text/voice)
- General-purpose personal assistant: information retrieval, schedule management, task automation, etc.
- Optional Home Assistant integration for smart device control
- Scheduled automation (timed reminders, recurring tasks, etc.)
- STT/TTS with the ability to swap in custom implementations

### 1.2 Constraints

| Constraint | Value |
|------------|-------|
| Available memory | 200MB |
| Node.js version | >= 22.12.0 (archived, migrated to Rust) |
| Network | Internet required (remote AI APIs) |
| Optional integration | Home Assistant (REST API) |
| Interaction method | Telegram messages (text/voice) |

### 1.3 Design Principles

- **Extreme minimalism**: load only necessary modules, disable all non-essential features
- **Remote computation**: STT/TTS/LLM all use remote APIs, zero local models
- **Swappable**: STT and TTS are abstracted behind interfaces and can be replaced with custom implementations later
- **Stability first**: maintain sufficient memory safety margin to avoid OOM

---

## 2. System Architecture

### 2.1 Overall Architecture

```
                        200MB Embedded Device
┌─────────────────────────────────────────────────┐
│  OpenClaw Gateway (Node.js)                     │
│  ┌───────────┐  ┌──────────┐  ┌──────────────┐  │
│  │ Telegram   │  │ AI Agent │  │  Tool Layer  │  │
│  │ Channel    │  │(Remote   │  │ exec/fetch   │  │
│  │ (grammy)   │  │  LLM)    │  │ cron         │  │
│  └─────┬─────┘  └────┬─────┘  └──────┬───────┘  │
│        │              │               │          │
│  ┌─────┴──────────────┴───────────────┴───────┐  │
│  │            Voice Processing Pipeline        │  │
│  │  ┌─────────────┐      ┌──────────────┐     │  │
│  │  │ STT Adapter │      │ TTS Adapter  │     │  │
│  │  │ (swappable) │      │ (swappable)  │     │  │
│  │  └──────┬──────┘      └──────┬───────┘     │  │
│  └─────────┼────────────────────┼─────────────┘  │
└────────────┼────────────────────┼────────────────┘
             │                    │
     ┌───────▼───────┐   ┌───────▼───────┐
     │ Remote STT API│   │ Remote TTS API│
     │(Groq Whisper) │   │ (Edge TTS)    │
     └───────────────┘   └───────────────┘
             │
     ┌───────▼────────────────────────────┐
     │ Remote LLM API (Claude Sonnet)     │
     │ → Decide → call exec/curl          │
     │ → Control Home Assistant REST API  │
     └────────────────────────────────────┘
```

### 2.2 Voice Interaction Flow

```
User speaks into phone
    │
    ▼
Telegram voice recording (.ogg/opus)
    │
    ▼
OpenClaw receives audio attachment
    │
    ▼
┌─────────────────────┐
│ STT Adapter          │  ← Swappable interface
│ Current: Groq Whisper│
│ Future: Custom STT   │
└─────────┬───────────┘
          │ Transcribed text
          ▼
┌─────────────────────┐
│ AI Agent (Remote LLM)│
│ Understand intent    │
│ → Select tool        │
│ e.g. "Turn off the   │
│   living room light" │
│ → exec: curl POST    │
│   Home Assistant API │
└─────────┬───────────┘
          │ Reply text
          ▼
┌─────────────────────┐
│ TTS Adapter          │  ← Swappable interface
│ Current: Edge TTS    │
│ Future: Custom TTS   │
└─────────┬───────────┘
          │ Audio file (.opus)
          ▼
Telegram voice bubble reply
    │
    ▼
User hears voice response
```

---

## 3. Module Design

### 3.1 Enabled Modules

| Module | Purpose | Memory Estimate | Status |
|--------|---------|----------------|--------|
| Gateway core | HTTP server, routing, session management | ~30MB | Required |
| Telegram plugin | grammy SDK, message send/receive | ~20MB | Required |
| STT pipeline | Speech-to-text (remote API) | ~2MB | Required |
| TTS pipeline | Text-to-speech (remote API) | ~3MB | Required |
| exec tool | Execute curl to control smart devices | ~2MB | Required |
| web_fetch tool | HTTP GET to query device status | ~2MB | Required |
| cron tool | Scheduled automation tasks | ~1MB | Optional |
| Node.js 22 runtime | V8 engine + standard library | ~50MB | Required |
| **Total** | | **~110-130MB** | |
| **Safety margin** | | **~70-90MB** | |

### 3.2 Disabled Modules

| Module | Environment Variable / Config | Memory Saved |
|--------|------------------------------|-------------|
| Browser automation | `OPENCLAW_SKIP_BROWSER_CONTROL_SERVER=1` | ~80-150MB |
| Canvas rendering | `OPENCLAW_SKIP_CANVAS_HOST=1` | ~30-60MB |
| Gmail listener | `OPENCLAW_SKIP_GMAIL_WATCHER=1` | ~10MB |
| Scheduled task engine | `OPENCLAW_SKIP_CRON=1` | ~5MB |
| Vector semantic search | `plugins.slots.memory: "none"` | ~20-50MB |
| Other message channels | `plugins.allow: ["telegram"]` | ~50MB |
| Local LLM | Do not install `node-llama-cpp` | ~200MB+ |
| Local Canvas | Do not install `@napi-rs/canvas` | ~30-60MB |

---

## 4. STT Adapter Design (Swappable)

### 4.1 OpenClaw's Existing STT Interface

OpenClaw's STT is based on the `MediaUnderstandingProvider` interface, defined in:

**`moltbot-src/src/media-understanding/types.ts`**

```typescript
// --- Core request/response types ---

type AudioTranscriptionRequest = {
  buffer: Buffer;        // Audio binary data
  fileName: string;      // File name (e.g. voice.ogg)
  mime?: string;         // MIME type (e.g. audio/ogg)
  apiKey: string;        // API key
  baseUrl?: string;      // Custom API endpoint
  headers?: Record<string, string>;  // Custom request headers
  model?: string;        // Model name
  language?: string;     // Language hint
  prompt?: string;       // Context hint
  query?: Record<string, string | number | boolean>;
  timeoutMs: number;     // Timeout in milliseconds
  fetchFn?: typeof fetch; // Injectable custom fetch
};

type AudioTranscriptionResult = {
  text: string;          // Transcribed text
  model?: string;        // Actual model used
};

// --- Provider interface ---

type MediaUnderstandingProvider = {
  id: string;
  capabilities?: MediaUnderstandingCapability[];  // ["audio", "image", "video"]
  transcribeAudio?: (req: AudioTranscriptionRequest) => Promise<AudioTranscriptionResult>;
  describeVideo?: (req: VideoDescriptionRequest) => Promise<VideoDescriptionResult>;
  describeImage?: (req: ImageDescriptionRequest) => Promise<ImageDescriptionResult>;
};
```

### 4.2 Provider Registration Mechanism

**`moltbot-src/src/media-understanding/providers/index.ts`**

```typescript
// Built-in provider list
const PROVIDERS: MediaUnderstandingProvider[] = [
  groqProvider,       // Groq Whisper (free)
  openaiProvider,     // OpenAI Whisper
  googleProvider,     // Google Gemini
  anthropicProvider,  // Anthropic
  minimaxProvider,    // MiniMax
  zaiProvider,        // ZAI
  deepgramProvider,   // Deepgram Nova
];

// Supports overrides to inject custom providers
function buildMediaUnderstandingRegistry(
  overrides?: Record<string, MediaUnderstandingProvider>,
): Map<string, MediaUnderstandingProvider>;
```

### 4.3 Current Approach: Groq Whisper

| Item | Value |
|------|-------|
| Provider | `groq` |
| Model | `whisper-large-v3-turbo` |
| Cost | Free (rate limited) |
| Latency | 1–3 seconds |
| Local memory | ~2MB (streaming HTTP calls only) |

### 4.4 Custom STT Replacement Paths

#### Option 1: Implement the MediaUnderstandingProvider interface (recommended)

Create a custom provider plugin and register it with OpenClaw:

```typescript
// ~/.openclaw/extensions/custom-stt/index.ts
import type { OpenClawPluginApi } from "openclaw/plugin-sdk";

export default function register(api: OpenClawPluginApi) {
  api.registerProvider({
    id: "custom-stt",
    capabilities: ["audio"],

    async transcribeAudio(req) {
      // --- Integrate custom STT here ---
      // req.buffer  : Audio Buffer (ogg/opus format)
      // req.language : Language hint
      // req.mime     : MIME type
      //
      // Possible implementations:
      //   1. Call a local HTTP STT service
      //   2. Call a custom cloud STT API
      //   3. Call a local ONNX model (requires extra memory)
      //   4. Stream via WebSocket to a custom service

      const response = await fetch("http://localhost:9000/stt", {
        method: "POST",
        headers: { "Content-Type": req.mime ?? "audio/ogg" },
        body: req.buffer,
      });
      const result = await response.json();

      return {
        text: result.text,
        model: "custom-stt-v1",
      };
    },
  });
}
```

Configuration switch:

```json
{
  "messages": {
    "mediaUnderstanding": {
      "audio": {
        "provider": "custom-stt"
      }
    }
  },
  "plugins": {
    "load": {
      "paths": ["~/.openclaw/extensions/custom-stt"]
    }
  }
}
```

#### Option 2: Standalone service + configure baseUrl

No code changes required. Deploy a custom service compatible with the OpenAI Whisper API and change only the baseUrl:

```json
{
  "messages": {
    "mediaUnderstanding": {
      "audio": {
        "provider": "openai",
        "model": "my-custom-model",
        "baseUrl": "http://my-stt-server:9000/v1"
      }
    }
  }
}
```

The custom service only needs to implement one endpoint:

```
POST /v1/audio/transcriptions
Content-Type: multipart/form-data
  - file: audio file
  - model: model name
  - language: language

Response: { "text": "transcription result" }
```

### 4.5 Custom STT Interface Specification

Regardless of which replacement option is chosen, the custom STT service must satisfy:

```
Input:
  - Audio format: OGG/Opus (Telegram native format), optionally WAV/MP3
  - Sample rate: 48kHz (Telegram) or 16kHz (general)
  - Channels: mono
  - Language: zh-CN / en-US / auto-detect

Output:
  - text: string         # Transcribed text (UTF-8)
  - model?: string       # Model identifier
  - language?: string    # Detected language
  - confidence?: number  # Confidence 0-1 (optional)

Performance requirements:
  - Latency: < 3 seconds (5-second audio clip)
  - Memory: if deployed on the same device, keep under 50MB
  - Concurrency: support at least 1 simultaneous transcription
```

---

## 5. TTS Adapter Design (Swappable)

### 5.1 OpenClaw's Existing TTS Architecture

**`moltbot-src/src/config/types.tts.ts`**

```typescript
type TtsProvider = "elevenlabs" | "openai" | "edge";

type TtsAutoMode = "off" | "always" | "inbound" | "tagged";
  // off     : do not auto-generate voice
  // always  : generate voice for all replies
  // inbound : generate voice only when a voice message is received (recommended)
  // tagged  : generate voice only for [[tts:...]] tagged content

type TtsConfig = {
  auto?: TtsAutoMode;
  provider?: TtsProvider;
  maxTextLength?: number;       // Text length limit (default 1500 characters)
  timeoutMs?: number;           // API timeout
  edge?: {                      // Edge TTS (free, no API key required)
    enabled?: boolean;
    voice?: string;             // Voice character
    lang?: string;              // Language
    pitch?: string;             // Pitch adjustment
    rate?: string;              // Speed adjustment
    volume?: string;            // Volume adjustment
  };
  openai?: {                    // OpenAI TTS
    apiKey?: string;
    model?: string;             // gpt-4o-mini-tts / tts-1 / tts-1-hd
    voice?: string;             // alloy / nova / echo / ...
  };
  elevenlabs?: {                // ElevenLabs TTS
    apiKey?: string;
    baseUrl?: string;
    voiceId?: string;
    modelId?: string;
    voiceSettings?: { ... };
  };
};
```

### 5.2 Telegram Voice Output Format

**`moltbot-src/src/tts/tts.ts`** (lines 61–68)

```typescript
const TELEGRAM_OUTPUT = {
  openai: "opus",                    // OpenAI outputs Opus
  elevenlabs: "opus_48000_64",       // ElevenLabs outputs 48kHz/64kbps Opus
  extension: ".opus",                // File extension
  voiceCompatible: true,             // Send as voice bubble
};
```

Telegram voice messages require `.ogg` / `.opus` / `.oga` format. OpenClaw handles format conversion automatically.

### 5.3 Current Approach: Edge TTS

| Item | Value |
|------|-------|
| Provider | `edge` (Microsoft Edge TTS) |
| Cost | Free |
| Latency | < 1 second |
| Chinese voices | `zh-CN-XiaoxiaoNeural` / `zh-CN-YunxiNeural` |
| Output format | MP3 (auto-converted to Opus for Telegram) |
| Local memory | ~3MB (streaming HTTP) |
| API Key | Not required |

### 5.4 Custom TTS Replacement Paths

#### Option 1: Implement as a new TtsProvider (requires source code changes)

The current `TtsProvider` type is a hardcoded enum `"elevenlabs" | "openai" | "edge"`. Extending it requires:

1. Extend the type in `types.tts.ts`:

```typescript
type TtsProvider = "elevenlabs" | "openai" | "edge" | "custom";
```

2. Add synthesis logic for the `custom` provider in `tts.ts`

3. Add a `custom` section to the config:

```json
{
  "messages": {
    "tts": {
      "provider": "custom",
      "custom": {
        "baseUrl": "http://localhost:9001/tts",
        "voice": "speaker-01",
        "format": "opus"
      }
    }
  }
}
```

#### Option 2: Deploy as an OpenAI-compatible TTS service (recommended, zero code changes)

Implement the OpenAI TTS API protocol in the custom TTS service, configure `provider: "openai"` with a custom `baseUrl`:

```json
{
  "messages": {
    "tts": {
      "provider": "openai",
      "openai": {
        "apiKey": "any-placeholder",
        "model": "my-tts-v1",
        "voice": "xiaoming",
        "baseUrl": "http://localhost:9001/v1"
      }
    }
  }
}
```

The custom service needs to implement:

```
POST /v1/audio/speech
Content-Type: application/json
{
  "model": "my-tts-v1",
  "input": "text to synthesize",
  "voice": "xiaoming",
  "response_format": "opus"    // or mp3
}

Response: audio/opus binary stream
```

#### Option 3: Call a local TTS program via the exec tool

Without changing OpenClaw code, the AI calls a local program directly via the exec tool:

```bash
# AI calls exec to run:
echo "Living room light is now on" | my-tts --voice xiaoming --output /tmp/reply.opus
# Then send the audio file via the message tool
```

Suitable for quick validation, but the experience is not as smooth as native integration.

### 5.5 Custom TTS Interface Specification

```
Input:
  - text: string          # Text to synthesize (UTF-8)
  - voice?: string        # Voice character
  - speed?: number        # Speed (0.5-2.0, default 1.0)
  - format: "opus" | "mp3" | "wav"

Output:
  - Audio binary stream
  - Format: Opus (Telegram compatible) or MP3
  - Sample rate: 24kHz–48kHz
  - Bit rate: 64kbps (Opus) / 128kbps (MP3)

Performance requirements:
  - Latency: < 2 seconds (50-character text)
  - Time to first byte (TTFB): < 500ms (streaming scenario)
  - Memory: if deployed on the same device, keep under 50MB
  - Concurrency: support at least 1 simultaneous synthesis
```

---

## 6. Home Assistant Integration Design (Optional Feature)

### 6.1 Control Method

Use the `exec` tool to call `curl` and access the Home Assistant REST API:

```bash
# Turn on light
curl -s -X POST \
  -H "Authorization: Bearer HA_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"entity_id": "light.living_room"}' \
  http://192.168.1.100:8123/api/services/light/turn_on

# Query status
curl -s -H "Authorization: Bearer HA_TOKEN" \
  http://192.168.1.100:8123/api/states/light.living_room

# Set AC temperature
curl -s -X POST \
  -H "Authorization: Bearer HA_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"entity_id": "climate.ac", "temperature": 26}' \
  http://192.168.1.100:8123/api/services/climate/set_temperature
```

### 6.2 System Prompt Design

```
You are a personal AI assistant (OpenClaw) running on a local device. Below is the optional Home Assistant device list:

## Device List
- light.living_room    : Living room light
- light.bedroom        : Bedroom light
- light.kitchen        : Kitchen light
- climate.ac           : Air conditioner (temperature range 16–30°C)
- cover.curtain_living : Living room curtain
- switch.water_heater  : Water heater
- media_player.tv      : Television

## Home Assistant API
- Address: http://192.168.1.100:8123
- Token: (configured in environment variable HA_TOKEN)

## Operation Rules
1. Use the exec tool to run curl commands to control devices
2. Keep replies short and suitable for voice playback (no more than 30 characters)
3. Confirm after successful control: "OK, I've turned on the light"
4. Explain the reason if control fails: "Sorry, the living room light is not responding"
5. When uncertain, query the state before acting
6. Support batch operations: "Turn off all the lights"
7. Support scene automation: "I'm going to sleep" → turn off lights + close curtains + turn off TV
```

### 6.3 Scheduled Automation

Create scheduled tasks using the `cron` tool:

```
User: "Turn on the bedroom light every morning at 7am"
AI → cron tool: add "0 7 * * *" task, execute light-on curl command

User: "Cancel the morning light"
AI → cron tool: delete the corresponding task
```

---

## 7. Configuration Files

### 7.1 openclaw.json

```json
{
  "provider": "anthropic",
  "model": "claude-sonnet-4-5-20250929",

  "plugins": {
    "allow": ["telegram"],
    "slots": {
      "memory": "none"
    }
  },

  "memory": {
    "backend": "builtin"
  },

  "channels": {
    "telegram": {
      "botToken": "${TELEGRAM_BOT_TOKEN}"
    }
  },

  "messages": {
    "tts": {
      "auto": "inbound",
      "mode": "final",
      "provider": "edge",
      "maxTextLength": 500,
      "edge": {
        "enabled": true,
        "voice": "zh-CN-XiaoxiaoNeural",
        "rate": "+10%"
      }
    },
    "mediaUnderstanding": {
      "audio": {
        "enabled": true,
        "provider": "groq",
        "model": "whisper-large-v3-turbo"
      }
    }
  },

  "tools": {
    "profile": "minimal",
    "allow": ["exec", "web_fetch", "cron"]
  },

  "agents": {
    "defaults": {
      "systemPrompt": "You are a personal assistant running inside OpenClaw."
    }
  }
}
```

### 7.2 Environment Variables

```bash
# Node.js memory limit
NODE_OPTIONS="--max-old-space-size=100 --optimize-for-size"

# Disable heavyweight modules
OPENCLAW_SKIP_BROWSER_CONTROL_SERVER=1
OPENCLAW_SKIP_CANVAS_HOST=1
OPENCLAW_SKIP_GMAIL_WATCHER=1
OPENCLAW_SKIP_CRON=1

# API Keys
ANTHROPIC_API_KEY="sk-ant-xxx"
GROQ_API_KEY="gsk_xxx"
TELEGRAM_BOT_TOKEN="123456:ABCxxx"

# Home Assistant
HA_TOKEN="eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.xxx"
HA_URL="http://192.168.1.100:8123"
```

### 7.3 Startup Script

```bash
#!/bin/bash
# start-openclaw-lite.sh

set -e

# Load environment variables
source ~/.openclaw/.env

# Memory limit
export NODE_OPTIONS="--max-old-space-size=100 --optimize-for-size"

# Disable non-essential modules
export OPENCLAW_SKIP_BROWSER_CONTROL_SERVER=1
export OPENCLAW_SKIP_CANVAS_HOST=1
export OPENCLAW_SKIP_GMAIL_WATCHER=1
export OPENCLAW_SKIP_CRON=1

# Start
cd /opt/openclaw
exec node openclaw.mjs gateway --allow-unconfigured --port 18789
```

---

## 8. Memory Budget

### 8.1 Normal Running State

| Component | Estimate | Notes |
|-----------|---------|-------|
| Node.js V8 engine | 45–55MB | `--max-old-space-size=100` + `--optimize-for-size` |
| Gateway HTTP server | 15–20MB | Express 5 + routing + middleware |
| Config + session management | 10–15MB | JSON config loading, session state |
| Telegram grammy | 15–20MB | WebSocket long connection + message parsing |
| STT calls | 1–3MB | Temporary buffer (audio transfer) |
| TTS calls | 1–3MB | Temporary buffer (audio receive) |
| exec/curl process | 2–5MB | Short-lived child process |
| **Total** | **~95–125MB** | |
| **Remaining** | **~75–105MB** | Safety margin |

### 8.2 Peak Scenarios

| Scenario | Extra Memory | Handling |
|----------|-------------|---------|
| Long voice message (60s) | +3–5MB | Temporary buffer, released after processing |
| TTS long text | +2–3MB | `maxTextLength: 500` limit |
| Multiple concurrent messages | +5–10MB | Telegram serial processing (single user) |
| GC pressure peak | +10–20MB | V8 auto-reclaim |
| **Maximum peak** | **~155–165MB** | Still within 200MB |

### 8.3 Recommended: Add swap

```bash
# Create 256MB swap (safety net)
dd if=/dev/zero of=/swapfile bs=1M count=256
chmod 600 /swapfile
mkswap /swapfile
swapon /swapfile
echo '/swapfile none swap sw 0 0' >> /etc/fstab
```

---

## 9. Custom Extension Roadmap

### 9.1 Phase 1: Cloud APIs (current)

```
STT: Groq Whisper (free cloud)
TTS: Edge TTS (free cloud)
LLM: Claude Sonnet (paid cloud)
```

- Zero local computation; 200MB is entirely feasible
- Monthly cost approximately $6 (average 20 voice interactions per day)

### 9.2 Phase 2: Custom STT Replacement

```
STT: Custom service (deployed on another device or in the cloud)
     Implements OpenAI Whisper-compatible API
     Change only baseUrl in openclaw.json to switch
TTS: Edge TTS (unchanged)
LLM: Claude Sonnet (unchanged)
```

Replacement steps:
1. Develop a custom STT service implementing the `POST /v1/audio/transcriptions` endpoint
2. Deploy to a LAN server (e.g., NAS, Raspberry Pi 4B+)
3. Update config: `provider: "openai"` + `baseUrl: "http://nas:9000/v1"`
4. No OpenClaw code changes required

### 9.3 Phase 3: Custom TTS Replacement

```
STT: Custom service
TTS: Custom service (implements OpenAI TTS-compatible API)
LLM: Claude Sonnet or custom/open-source LLM
```

Replacement steps:
1. Develop a custom TTS service implementing the `POST /v1/audio/speech` endpoint
2. Output format supports `opus` (Telegram compatible)
3. Update config: `provider: "openai"` + `baseUrl: "http://nas:9001/v1"`
4. No OpenClaw code changes required

### 9.4 Phase 4: Full Custom Stack (optional)

```
STT: Custom (Whisper ONNX / Paraformer / SenseVoice)
TTS: Custom (VITS / CosyVoice / GPT-SoVITS)
LLM: Custom or open-source (Qwen / DeepSeek / Llama)
```

> Note: A full custom STT + TTS + LLM stack requires at least 2–4GB of RAM,
> making it unsuitable for deployment on a 200MB device. Deploy on a dedicated
> compute node instead. The 200MB device serves only as the Gateway + Telegram channel.

---

## 10. Interface Compatibility Matrix

### 10.1 STT Replacement Compatibility

| Replacement Method | Code Changes | Difficulty | Recommendation |
|-------------------|-------------|-----------|----------------|
| OpenAI Whisper-compatible API + baseUrl | None | Low | Recommended |
| Custom MediaUnderstandingProvider plugin | Write plugin | Medium | Flexible |
| exec to call local CLI tool | None | Low | Quick validation |
| Modify OpenClaw source to add provider | Modify source | High | Not recommended |

### 10.2 TTS Replacement Compatibility

| Replacement Method | Code Changes | Difficulty | Recommendation |
|-------------------|-------------|-----------|----------------|
| OpenAI TTS-compatible API + baseUrl | None | Low | Recommended |
| Modify OpenClaw source to add provider | Modify source | High | Not recommended |
| exec to call local TTS + send audio | None | Medium | Quick validation |

### 10.3 Recommended Replacement Strategy

**For both STT and TTS, use the "OpenAI-compatible API" replacement approach:**

- Custom service implements the OpenAI standard interface
- OpenClaw side changes only the `provider` + `baseUrl` config
- Zero code changes, hot-swappable
- Can revert to cloud API at any time

---

## Appendix A: Key Source Code Paths

| Module | Path |
|--------|------|
| STT type definitions | `moltbot-src/src/media-understanding/types.ts` |
| STT provider registration | `moltbot-src/src/media-understanding/providers/index.ts` |
| STT runner | `moltbot-src/src/media-understanding/runner.ts` |
| Groq STT implementation | `moltbot-src/src/media-understanding/providers/groq/index.ts` |
| OpenAI STT implementation | `moltbot-src/src/media-understanding/providers/openai/audio.ts` |
| Deepgram STT implementation | `moltbot-src/src/media-understanding/providers/deepgram/audio.ts` |
| TTS core engine | `moltbot-src/src/tts/tts.ts` |
| TTS config types | `moltbot-src/src/config/types.tts.ts` |
| Telegram voice handling | `moltbot-src/src/telegram/voice.ts` |
| Telegram send logic | `moltbot-src/src/telegram/send.ts` |
| Plugin loader | `moltbot-src/src/plugins/loader.ts` |
| Plugin allowlist mechanism | `moltbot-src/src/plugins/config-state.ts` |
| Gateway startup flow | `moltbot-src/src/gateway/server-startup.ts` |
| Channel management | `moltbot-src/src/gateway/server-channels.ts` |
| exec tool docs | `moltbot-src/docs/tools/exec.md` |
| web_fetch tool | `moltbot-src/src/agents/tools/web-fetch.ts` |
| fly.toml (production config) | `moltbot-src/fly.toml` |

## Appendix B: Cost Estimate

| Service | Unit Price | 20 interactions/day | Monthly Cost |
|---------|-----------|---------------------|-------------|
| Groq Whisper STT | Free | $0 | $0 |
| Edge TTS | Free | $0 | $0 |
| Claude Sonnet API | ~$0.01/call | $0.20 | ~$6 |
| **Total** | | | **~$6/month** |

> Switching to a cheaper model such as Gemini Flash could reduce monthly cost to ~$1–2.
