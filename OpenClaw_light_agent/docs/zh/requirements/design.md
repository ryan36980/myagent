# OpenClaw Light - 嵌入式个人 AI 助手设计文档

> **归档文档** — 本文档描述的是早期 Node.js 实现方案。项目已选定 Rust 方案并完成实现，
> 当前架构设计见 [设计文档](../design/README.md)。
> 本文档保留作为需求和 API 协议的参考。

## 1. 项目概述

### 1.1 目标

在仅有 **200MB 内存**的嵌入式设备（小盒子）上运行 OpenClaw，实现：

- 通过 Telegram 消息（文本/语音）与 AI 对话
- 通用个人助手：信息检索、日程管理、任务自动化等
- 可选集成 Home Assistant 控制智能设备
- 定时自动化（定时提醒、周期任务等）
- STT/TTS 保留自研替换能力

### 1.2 约束条件

| 约束 | 值 |
|------|-----|
| 可用内存 | 200MB |
| Node.js 版本 | >= 22.12.0 (归档，已迁移 Rust) |
| 网络 | 需要互联网（远程 AI API） |
| 可选集成 | Home Assistant（REST API） |
| 交互方式 | Telegram 消息（文本/语音） |

### 1.3 设计原则

- **极致精简**：只加载必要模块，关闭一切非必需功能
- **远程计算**：STT/TTS/LLM 全部使用远程 API，本地零模型
- **可替换**：STT 和 TTS 通过接口抽象，后续可替换为自研实现
- **稳定优先**：留足内存安全余量，避免 OOM

---

## 2. 系统架构

### 2.1 整体架构

```
                        200MB 嵌入式设备
┌─────────────────────────────────────────────────┐
│  OpenClaw Gateway (Node.js)                     │
│  ┌───────────┐  ┌──────────┐  ┌──────────────┐  │
│  │ Telegram   │  │ AI Agent │  │   工具层      │  │
│  │ Channel    │  │ (远程LLM)│  │ exec/fetch   │  │
│  │ (grammy)   │  │          │  │ cron         │  │
│  └─────┬─────┘  └────┬─────┘  └──────┬───────┘  │
│        │              │               │          │
│  ┌─────┴──────────────┴───────────────┴───────┐  │
│  │            语音处理管线                      │  │
│  │  ┌─────────────┐      ┌──────────────┐     │  │
│  │  │ STT 适配层   │      │ TTS 适配层    │     │  │
│  │  │ (可替换)     │      │ (可替换)      │     │  │
│  │  └──────┬──────┘      └──────┬───────┘     │  │
│  └─────────┼────────────────────┼─────────────┘  │
└────────────┼────────────────────┼────────────────┘
             │                    │
     ┌───────▼───────┐   ┌───────▼───────┐
     │ 远程 STT API  │   │ 远程 TTS API  │
     │ (Groq Whisper)│   │ (Edge TTS)    │
     └───────────────┘   └───────────────┘
             │
     ┌───────▼────────────────────────────┐
     │ 远程 LLM API (Claude Sonnet)       │
     │ → 决策 → 调用 exec/curl            │
     │ → 控制 Home Assistant REST API     │
     └────────────────────────────────────┘
```

### 2.2 语音交互流程

```
用户对手机说话
    │
    ▼
Telegram 录音 (.ogg/opus)
    │
    ▼
OpenClaw 收到音频附件
    │
    ▼
┌─────────────────────┐
│ STT 适配层           │  ← 可替换接口
│ 当前: Groq Whisper   │
│ 未来: 自研 STT       │
└─────────┬───────────┘
          │ 转写文本
          ▼
┌─────────────────────┐
│ AI Agent (远程 LLM)  │
│ 理解意图 → 选择工具  │
│ 例: "把客厅灯关了"   │
│ → exec: curl POST   │
│   Home Assistant API │
└─────────┬───────────┘
          │ 回复文本
          ▼
┌─────────────────────┐
│ TTS 适配层           │  ← 可替换接口
│ 当前: Edge TTS       │
│ 未来: 自研 TTS       │
└─────────┬───────────┘
          │ 音频文件 (.opus)
          ▼
Telegram 语音气泡回复
    │
    ▼
用户听到语音回答
```

---

## 3. 模块设计

### 3.1 启用模块清单

| 模块 | 用途 | 内存预估 | 状态 |
|------|------|---------|------|
| Gateway 核心 | HTTP 服务、路由、会话管理 | ~30MB | 必需 |
| Telegram 插件 | grammy SDK，消息收发 | ~20MB | 必需 |
| STT 管线 | 语音转文字（远程 API） | ~2MB | 必需 |
| TTS 管线 | 文字转语音（远程 API） | ~3MB | 必需 |
| exec 工具 | 执行 curl 控制智能设备 | ~2MB | 必需 |
| web_fetch 工具 | HTTP GET 查询设备状态 | ~2MB | 必需 |
| cron 工具 | 定时自动化任务 | ~1MB | 可选 |
| Node.js 22 运行时 | V8 引擎 + 基础库 | ~50MB | 必需 |
| **合计** | | **~110-130MB** | |
| **安全余量** | | **~70-90MB** | |

### 3.2 禁用模块清单

| 模块 | 环境变量 / 配置 | 节省内存 |
|------|----------------|---------|
| 浏览器自动化 | `OPENCLAW_SKIP_BROWSER_CONTROL_SERVER=1` | ~80-150MB |
| Canvas 渲染 | `OPENCLAW_SKIP_CANVAS_HOST=1` | ~30-60MB |
| Gmail 监听 | `OPENCLAW_SKIP_GMAIL_WATCHER=1` | ~10MB |
| 定时任务引擎 | `OPENCLAW_SKIP_CRON=1` | ~5MB |
| 向量语义搜索 | `plugins.slots.memory: "none"` | ~20-50MB |
| 其他消息频道 | `plugins.allow: ["telegram"]` | ~50MB |
| 本地 LLM | 不安装 `node-llama-cpp` | ~200MB+ |
| 本地 Canvas | 不安装 `@napi-rs/canvas` | ~30-60MB |

---

## 4. STT 适配层设计（可替换）

### 4.1 OpenClaw 现有 STT 接口

OpenClaw 的 STT 基于 `MediaUnderstandingProvider` 接口，定义在：

**`moltbot-src/src/media-understanding/types.ts`**

```typescript
// --- 核心请求/响应类型 ---

type AudioTranscriptionRequest = {
  buffer: Buffer;        // 音频二进制数据
  fileName: string;      // 文件名（如 voice.ogg）
  mime?: string;         // MIME 类型（如 audio/ogg）
  apiKey: string;        // API 密钥
  baseUrl?: string;      // 自定义 API 地址
  headers?: Record<string, string>;  // 自定义请求头
  model?: string;        // 模型名称
  language?: string;     // 语言提示
  prompt?: string;       // 上下文提示
  query?: Record<string, string | number | boolean>;
  timeoutMs: number;     // 超时时间
  fetchFn?: typeof fetch; // 可注入自定义 fetch
};

type AudioTranscriptionResult = {
  text: string;          // 转写文本
  model?: string;        // 实际使用的模型
};

// --- Provider 接口 ---

type MediaUnderstandingProvider = {
  id: string;
  capabilities?: MediaUnderstandingCapability[];  // ["audio", "image", "video"]
  transcribeAudio?: (req: AudioTranscriptionRequest) => Promise<AudioTranscriptionResult>;
  describeVideo?: (req: VideoDescriptionRequest) => Promise<VideoDescriptionResult>;
  describeImage?: (req: ImageDescriptionRequest) => Promise<ImageDescriptionResult>;
};
```

### 4.2 Provider 注册机制

**`moltbot-src/src/media-understanding/providers/index.ts`**

```typescript
// 内置 Provider 列表
const PROVIDERS: MediaUnderstandingProvider[] = [
  groqProvider,       // Groq Whisper（免费）
  openaiProvider,     // OpenAI Whisper
  googleProvider,     // Google Gemini
  anthropicProvider,  // Anthropic
  minimaxProvider,    // MiniMax
  zaiProvider,        // ZAI
  deepgramProvider,   // Deepgram Nova
];

// 支持 overrides 注入自定义 Provider
function buildMediaUnderstandingRegistry(
  overrides?: Record<string, MediaUnderstandingProvider>,
): Map<string, MediaUnderstandingProvider>;
```

### 4.3 当前方案：Groq Whisper

| 项目 | 值 |
|------|-----|
| Provider | `groq` |
| 模型 | `whisper-large-v3-turbo` |
| 费用 | 免费（有速率限制） |
| 延迟 | 1-3 秒 |
| 本地内存 | ~2MB（仅流式 HTTP 调用） |

### 4.4 自研 STT 替换路径

#### 方式一：实现 MediaUnderstandingProvider 接口（推荐）

创建自定义 Provider 插件，注册到 OpenClaw：

```typescript
// ~/.openclaw/extensions/custom-stt/index.ts
import type { OpenClawPluginApi } from "openclaw/plugin-sdk";

export default function register(api: OpenClawPluginApi) {
  api.registerProvider({
    id: "custom-stt",
    capabilities: ["audio"],

    async transcribeAudio(req) {
      // --- 在此接入自研 STT ---
      // req.buffer  : 音频 Buffer（ogg/opus 格式）
      // req.language : 语言提示
      // req.mime     : MIME 类型
      //
      // 可选实现方式:
      //   1. 调用本地 HTTP STT 服务
      //   2. 调用自研云端 STT API
      //   3. 调用本地 ONNX 模型（需额外内存）
      //   4. 通过 WebSocket 流式推送到自研服务

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

配置切换：

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

#### 方式二：独立服务 + 配置 baseUrl

不改代码，部署兼容 OpenAI Whisper API 的自研服务，改 baseUrl 即可：

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

自研服务只需实现一个接口：

```
POST /v1/audio/transcriptions
Content-Type: multipart/form-data
  - file: 音频文件
  - model: 模型名
  - language: 语言

Response: { "text": "转写结果" }
```

### 4.5 自研 STT 接口规范

无论选择哪种替换方式，自研 STT 服务需满足：

```
输入:
  - 音频格式: OGG/Opus（Telegram 原生格式），可选支持 WAV/MP3
  - 采样率: 48kHz（Telegram）或 16kHz（通用）
  - 声道: 单声道
  - 语言: zh-CN / en-US / 自动检测

输出:
  - text: string         # 转写文本（UTF-8）
  - model?: string       # 模型标识
  - language?: string    # 检测到的语言
  - confidence?: number  # 置信度 0-1（可选）

性能要求:
  - 延迟: < 3 秒（5秒语音片段）
  - 内存: 如部署在同设备，需控制在 50MB 以内
  - 并发: 支持至少 1 路同时转写
```

---

## 5. TTS 适配层设计（可替换）

### 5.1 OpenClaw 现有 TTS 架构

**`moltbot-src/src/config/types.tts.ts`**

```typescript
type TtsProvider = "elevenlabs" | "openai" | "edge";

type TtsAutoMode = "off" | "always" | "inbound" | "tagged";
  // off     : 不自动生成语音
  // always  : 所有回复都生成语音
  // inbound : 仅当收到语音消息时回复语音（推荐）
  // tagged  : 仅 [[tts:...]] 标签的内容生成语音

type TtsConfig = {
  auto?: TtsAutoMode;
  provider?: TtsProvider;
  maxTextLength?: number;       // 文本长度上限（默认 1500 字符）
  timeoutMs?: number;           // API 超时
  edge?: {                      // Edge TTS（免费，无需 API Key）
    enabled?: boolean;
    voice?: string;             // 语音角色
    lang?: string;              // 语言
    pitch?: string;             // 音高调整
    rate?: string;              // 语速调整
    volume?: string;            // 音量调整
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

### 5.2 Telegram 语音输出格式

**`moltbot-src/src/tts/tts.ts`** (第 61-68 行)

```typescript
const TELEGRAM_OUTPUT = {
  openai: "opus",                    // OpenAI 输出 Opus
  elevenlabs: "opus_48000_64",       // ElevenLabs 输出 48kHz/64kbps Opus
  extension: ".opus",                // 文件扩展名
  voiceCompatible: true,             // 作为语音气泡发送
};
```

Telegram 语音消息要求 `.ogg` / `.opus` / `.oga` 格式。OpenClaw 自动处理格式转换。

### 5.3 当前方案：Edge TTS

| 项目 | 值 |
|------|-----|
| Provider | `edge` (Microsoft Edge TTS) |
| 费用 | 免费 |
| 延迟 | < 1 秒 |
| 中文语音 | `zh-CN-XiaoxiaoNeural` / `zh-CN-YunxiNeural` |
| 输出格式 | MP3（自动转 Opus for Telegram） |
| 本地内存 | ~3MB（流式 HTTP） |
| API Key | 不需要 |

### 5.4 自研 TTS 替换路径

#### 方式一：实现为新的 TtsProvider（需修改源码）

当前 `TtsProvider` 类型是硬编码枚举 `"elevenlabs" | "openai" | "edge"`，扩展需要：

1. 在 `types.tts.ts` 中扩展类型：

```typescript
type TtsProvider = "elevenlabs" | "openai" | "edge" | "custom";
```

2. 在 `tts.ts` 中添加 `custom` provider 的合成逻辑

3. 在配置中添加 `custom` 段：

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

#### 方式二：部署为 OpenAI 兼容 TTS 服务（推荐，零改码）

自研 TTS 实现 OpenAI TTS API 协议，配置 `provider: "openai"` + 自定义 `baseUrl`：

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

自研服务需实现：

```
POST /v1/audio/speech
Content-Type: application/json
{
  "model": "my-tts-v1",
  "input": "要合成的文本",
  "voice": "xiaoming",
  "response_format": "opus"    // 或 mp3
}

Response: audio/opus 二进制流
```

#### 方式三：通过 exec 工具调用本地 TTS 程序

不改 OpenClaw 代码，AI 直接通过 exec 工具调本地程序：

```bash
# AI 调用 exec 执行:
echo "客厅灯已打开" | my-tts --voice xiaoming --output /tmp/reply.opus
# 然后通过 message 工具发送音频文件
```

适合快速验证，但体验不如原生集成。

### 5.5 自研 TTS 接口规范

```
输入:
  - text: string          # 待合成文本（UTF-8）
  - voice?: string        # 语音角色
  - speed?: number        # 语速（0.5-2.0，默认 1.0）
  - format: "opus" | "mp3" | "wav"

输出:
  - 音频二进制流
  - 格式: Opus（Telegram 兼容）或 MP3
  - 采样率: 24kHz-48kHz
  - 比特率: 64kbps（Opus）/ 128kbps（MP3）

性能要求:
  - 延迟: < 2 秒（50 字文本）
  - 首字节延迟 (TTFB): < 500ms（流式场景）
  - 内存: 如部署在同设备，需控制在 50MB 以内
  - 并发: 支持至少 1 路同时合成
```

---

## 6. Home Assistant 集成设计（可选功能）

### 6.1 控制方式

通过 `exec` 工具调用 `curl` 访问 Home Assistant REST API：

```bash
# 开灯
curl -s -X POST \
  -H "Authorization: Bearer HA_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"entity_id": "light.living_room"}' \
  http://192.168.1.100:8123/api/services/light/turn_on

# 查询状态
curl -s -H "Authorization: Bearer HA_TOKEN" \
  http://192.168.1.100:8123/api/states/light.living_room

# 设置空调温度
curl -s -X POST \
  -H "Authorization: Bearer HA_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"entity_id": "climate.ac", "temperature": 26}' \
  http://192.168.1.100:8123/api/services/climate/set_temperature
```

### 6.2 System Prompt 设计

```
你是一个个人 AI 助手（OpenClaw），运行在本地设备上。以下是可选的 Home Assistant 设备清单：

## 设备清单
- light.living_room    : 客厅灯
- light.bedroom        : 卧室灯
- light.kitchen        : 厨房灯
- climate.ac           : 空调（支持温度 16-30°C）
- cover.curtain_living : 客厅窗帘
- switch.water_heater  : 热水器
- media_player.tv      : 电视

## Home Assistant API
- 地址: http://192.168.1.100:8123
- Token: （配置在环境变量 HA_TOKEN 中）

## 操作规则
1. 用 exec 工具执行 curl 命令控制设备
2. 回复要简短，适合语音播报（不超过 30 字）
3. 控制成功后确认："好的，已帮你开灯"
4. 控制失败时说明原因："抱歉，客厅灯没有响应"
5. 不确定时先查询状态再操作
6. 支持批量操作："把所有灯关了"
7. 支持场景联动："我要睡觉了" → 关灯 + 关窗帘 + 关电视
```

### 6.3 定时自动化

通过 `cron` 工具创建定时任务：

```
用户: "每天早上 7 点把卧室灯打开"
AI → cron 工具: 添加 "0 7 * * *" 任务，执行开灯 curl 命令

用户: "取消早上开灯"
AI → cron 工具: 删除对应任务
```

---

## 7. 配置文件

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

### 7.2 环境变量

```bash
# Node.js 内存限制
NODE_OPTIONS="--max-old-space-size=100 --optimize-for-size"

# 关闭重量级模块
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

### 7.3 启动脚本

```bash
#!/bin/bash
# start-openclaw-lite.sh

set -e

# 加载环境变量
source ~/.openclaw/.env

# 内存限制
export NODE_OPTIONS="--max-old-space-size=100 --optimize-for-size"

# 关闭非必要模块
export OPENCLAW_SKIP_BROWSER_CONTROL_SERVER=1
export OPENCLAW_SKIP_CANVAS_HOST=1
export OPENCLAW_SKIP_GMAIL_WATCHER=1
export OPENCLAW_SKIP_CRON=1

# 启动
cd /opt/openclaw
exec node openclaw.mjs gateway --allow-unconfigured --port 18789
```

---

## 8. 内存预算

### 8.1 正常运行态

| 组件 | 预估 | 说明 |
|------|------|------|
| Node.js V8 引擎 | 45-55MB | `--max-old-space-size=100` + `--optimize-for-size` |
| Gateway HTTP 服务 | 15-20MB | Express 5 + 路由 + 中间件 |
| 配置 + 会话管理 | 10-15MB | JSON 配置加载、会话状态 |
| Telegram grammy | 15-20MB | WebSocket 长连接 + 消息解析 |
| STT 调用 | 1-3MB | 临时 Buffer（音频传输） |
| TTS 调用 | 1-3MB | 临时 Buffer（音频接收） |
| exec/curl 进程 | 2-5MB | 短生命周期子进程 |
| **合计** | **~95-125MB** | |
| **剩余** | **~75-105MB** | 安全余量 |

### 8.2 峰值场景

| 场景 | 额外内存 | 处理方式 |
|------|---------|---------|
| 长语音消息（60秒） | +3-5MB | 临时 Buffer，处理后释放 |
| TTS 长文本 | +2-3MB | `maxTextLength: 500` 限制 |
| 多条消息并发 | +5-10MB | Telegram 串行处理（单用户） |
| GC 压力峰值 | +10-20MB | V8 自动回收 |
| **最高峰值** | **~155-165MB** | 仍在 200MB 内 |

### 8.3 建议加 swap

```bash
# 创建 256MB swap（安全网）
dd if=/dev/zero of=/swapfile bs=1M count=256
chmod 600 /swapfile
mkswap /swapfile
swapon /swapfile
echo '/swapfile none swap sw 0 0' >> /etc/fstab
```

---

## 9. 自研扩展路线图

### 9.1 阶段一：云端 API（当前）

```
STT: Groq Whisper（免费云端）
TTS: Edge TTS（免费云端）
LLM: Claude Sonnet（付费云端）
```

- 零本地计算，200MB 完全可行
- 月成本约 $6（日均 20 次语音交互）

### 9.2 阶段二：自研 STT 替换

```
STT: 自研服务（部署在其他设备或云端）
     实现 OpenAI Whisper 兼容 API
     修改 openclaw.json 中 baseUrl 即可切换
TTS: Edge TTS（不变）
LLM: Claude Sonnet（不变）
```

替换步骤：
1. 开发自研 STT 服务，实现 `POST /v1/audio/transcriptions` 接口
2. 部署到局域网服务器（如 NAS、树莓派 4B+）
3. 修改配置 `provider: "openai"` + `baseUrl: "http://nas:9000/v1"`
4. 无需修改 OpenClaw 任何代码

### 9.3 阶段三：自研 TTS 替换

```
STT: 自研服务
TTS: 自研服务（实现 OpenAI TTS 兼容 API）
LLM: Claude Sonnet 或自研/开源 LLM
```

替换步骤：
1. 开发自研 TTS 服务，实现 `POST /v1/audio/speech` 接口
2. 输出格式支持 `opus`（Telegram 兼容）
3. 修改配置 `provider: "openai"` + `baseUrl: "http://nas:9001/v1"`
4. 无需修改 OpenClaw 任何代码

### 9.4 阶段四：全栈自研（可选）

```
STT: 自研（Whisper ONNX / Paraformer / SenseVoice）
TTS: 自研（VITS / CosyVoice / GPT-SoVITS）
LLM: 自研或开源（Qwen / DeepSeek / Llama）
```

> 注意: 全栈自研的 STT + TTS + LLM 至少需要 2-4GB 内存，
> 不适合部署在 200MB 设备上，建议部署在独立的计算节点。
> 200MB 设备仅作为 Gateway 网关 + Telegram 通道。

---

## 10. 接口兼容性矩阵

### 10.1 STT 替换兼容性

| 替换方式 | 改码 | 难度 | 推荐 |
|---------|------|------|------|
| OpenAI Whisper 兼容 API + baseUrl | 不需要 | 低 | 推荐 |
| 自定义 MediaUnderstandingProvider 插件 | 写插件 | 中 | 灵活 |
| exec 调用本地命令行工具 | 不需要 | 低 | 快速验证 |
| 修改 OpenClaw 源码新增 provider | 改源码 | 高 | 不推荐 |

### 10.2 TTS 替换兼容性

| 替换方式 | 改码 | 难度 | 推荐 |
|---------|------|------|------|
| OpenAI TTS 兼容 API + baseUrl | 不需要 | 低 | 推荐 |
| 修改 OpenClaw 源码新增 provider | 改源码 | 高 | 不推荐 |
| exec 调用本地 TTS + 发送音频 | 不需要 | 中 | 快速验证 |

### 10.3 推荐替换策略

**STT 和 TTS 统一采用 "OpenAI 兼容 API" 方式替换：**

- 自研服务实现 OpenAI 标准接口
- OpenClaw 侧仅修改 `provider` + `baseUrl` 配置
- 零代码改动，热切换
- 可随时回退到云端 API

---

## 附录 A：关键源码路径

| 模块 | 路径 |
|------|------|
| STT 类型定义 | `moltbot-src/src/media-understanding/types.ts` |
| STT Provider 注册 | `moltbot-src/src/media-understanding/providers/index.ts` |
| STT 运行器 | `moltbot-src/src/media-understanding/runner.ts` |
| Groq STT 实现 | `moltbot-src/src/media-understanding/providers/groq/index.ts` |
| OpenAI STT 实现 | `moltbot-src/src/media-understanding/providers/openai/audio.ts` |
| Deepgram STT 实现 | `moltbot-src/src/media-understanding/providers/deepgram/audio.ts` |
| TTS 核心引擎 | `moltbot-src/src/tts/tts.ts` |
| TTS 配置类型 | `moltbot-src/src/config/types.tts.ts` |
| Telegram 语音处理 | `moltbot-src/src/telegram/voice.ts` |
| Telegram 发送逻辑 | `moltbot-src/src/telegram/send.ts` |
| 插件加载器 | `moltbot-src/src/plugins/loader.ts` |
| 插件白名单机制 | `moltbot-src/src/plugins/config-state.ts` |
| Gateway 启动流程 | `moltbot-src/src/gateway/server-startup.ts` |
| 频道管理 | `moltbot-src/src/gateway/server-channels.ts` |
| exec 工具文档 | `moltbot-src/docs/tools/exec.md` |
| web_fetch 工具 | `moltbot-src/src/agents/tools/web-fetch.ts` |
| fly.toml (生产配置) | `moltbot-src/fly.toml` |

## 附录 B：费用估算

| 服务 | 单价 | 日均 20 次 | 月成本 |
|------|------|-----------|--------|
| Groq Whisper STT | 免费 | $0 | $0 |
| Edge TTS | 免费 | $0 | $0 |
| Claude Sonnet API | ~$0.01/次 | $0.20 | ~$6 |
| **合计** | | | **~$6/月** |

> 如果切换为 Gemini Flash 等更便宜的模型，月成本可降至 ~$1-2。
