# OpenClaw Light 极致资源限制方案

> **决策结论：已选定 Plan C（Rust Gateway）** — 实现详见 [设计文档](../design/README.md)
>
> 参考项目：[MimiClaw](https://github.com/memovai/mimiclaw) — 在 $5 ESP32-S3 芯片上运行完整 AI Agent

---

## 核心发现：MimiClaw 证明了什么

MimiClaw 在 $5 的 ESP32-S3（8MB PSRAM）上跑完整 AI Agent，工作集仅 ~300KB：

| 组件 | 内存 |
|------|------|
| FreeRTOS 栈（6 任务） | ~40KB |
| WiFi 缓冲 | ~30KB |
| TLS（2 连接） | ~120KB |
| JSON 解析 + 会话 + LLM 缓冲 | ~112KB |
| **总计** | **~302KB** |

**关键洞察：Telegram + Claude + 工具调用的实际应用逻辑只需 ~300KB。当前 Node.js 方案的 95-125MB 中，95%+ 是运行时开销。**

MimiClaw 的限制：**没有语音管线**（无 STT/TTS），没有 Home Assistant 集成。我们需要在此基础上增加这些能力。

### MimiClaw 架构要点

- **双核分工**：Core 0 负责 I/O（Telegram 轮询、消息发送），Core 1 负责 Agent 循环（Claude API 调用 + 工具执行）
- **消息总线**：FreeRTOS 队列，深度 8，`mimi_msg_t` 结构体，ownership 转移语义
- **ReAct 循环**：最多 10 次工具迭代，非流式 Claude API 调用
- **存储**：SPIFFS 平面文件系统，SOUL.md / USER.md / MEMORY.md + 每用户 JSONL 会话
- **配置**：编译时 `mimi_secrets.h` + 运行时 NVS Flash 覆盖

---

## 三个方案总览

| 维度 | 方案一：Node.js 深度优化 | 方案二：纯 C 重写 | 方案三：Rust 网关 |
|------|----------------------|----------------|----------------|
| 常驻内存 | 55-85MB | 18-20MB | 20-28MB |
| 峰值内存 | 75-110MB | 19-20MB | 25-30MB |
| 占 200MB 比例 | 38-55% | 9-10% | 12-15% |
| 开发周期 | 1-2 周 | 3-6 个月 | 2-4 周 |
| 可维护性 | 高（JS 生态） | 低（手动内存管理） | 中（编译器保证安全） |
| 能跑在 ESP32-S3？ | 不能 | 能 | 部分可以（esp-rs） |
| 内存安全风险 | 无（GC 管理） | 高（malloc/free） | 无（借用检查器） |

---

## 方案一：Node.js 深度优化（立即可做，1-2 周）

在当前架构上进一步压缩，不改技术栈。

### 1.1 V8 极限参数

**当前：**
```bash
NODE_OPTIONS="--max-old-space-size=100 --optimize-for-size"
```

**极限调优：**
```bash
NODE_OPTIONS="--max-old-space-size=64 --max-semi-space-size=2 --optimize-for-size --jitless --lite-mode --single-threaded-gc"
```

| 参数 | 效果 | 节省 | 代价 |
|------|------|------|------|
| `--max-old-space-size=64` | 老生代上限 100→64MB | ~36MB 上限降低 | 更频繁 Major GC |
| `--max-semi-space-size=2` | 新生代从默认 16→2MB | ~28MB | 更频繁 Minor GC（scavenge） |
| `--jitless` | 关闭所有 JIT 编译，纯解释器执行 | ~10-20MB（消除 JIT 代码页） | JS 执行慢 3-10x，但本应用是 I/O 密集型（等 API 响应），影响很小 |
| `--lite-mode` | V8 精简模式，减少代码缓存 | ~5-10MB | 启动稍慢 |
| `--single-threaded-gc` | 关闭并行 GC 线程 | ~2-4MB（GC 线程栈） | GC 暂停稍长 |

> **注意**：`--jitless` 的 V8 博客报告仅 1.7% 堆减少，但真正的节省来自消除 JIT 代码内存页（不计入堆统计），对模块多的 gateway 应用可节省 10-20MB。`--jitless` + `--lite-mode` 是嵌入式场景的推荐组合。

**预计节省：20-35MB**

### 1.2 替换 Grammy 为原生 Telegram API

Grammy SDK 估计占 15-20MB（SDK + 内部状态 + 插件系统）。用 Node.js 22 内置 `fetch` 直接调 Telegram Bot API：

```javascript
// 原生长轮询，无需 Grammy
async function pollTelegram(token, offset) {
  const res = await fetch(
    `https://api.telegram.org/bot${token}/getUpdates` +
    `?offset=${offset}&timeout=30&allowed_updates=["message"]`
  );
  return res.json();
}

async function sendMessage(token, chatId, text) {
  await fetch(`https://api.telegram.org/bot${token}/sendMessage`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ chat_id: chatId, text })
  });
}
```

**预计节省：8-15MB**

### 1.3 替换 Express 为 http.createServer

设计文档 §3.1 估计 "Gateway HTTP 服务" 占 15-20MB（Express 5 + 路由 + 中间件）。网关只需几个路由，用原生 HTTP：

```javascript
const http = require('http');
const server = http.createServer((req, res) => {
  // 简单 URL 路由 switch/map，无需中间件链
});
```

**预计节省：5-10MB**

### 1.4 esbuild 打包 + Tree-shaking

将整个应用打包为单文件，消除死代码：

```bash
esbuild src/index.ts \
  --bundle --platform=node --target=node22 \
  --minify --tree-shaking=true \
  --outfile=openclaw-light.mjs \
  --external:better-sqlite3
```

好处：
- Tree-shaking 在构建时消除禁用模块的代码（不只是运行时跳过）
- 减少 `require` 缓存（一个模块 vs 数百个 node_modules 文件）
- 减少文件描述符占用
- 启动更快（无模块解析开销）

**预计节省：5-15MB**

### 1.5 应用层进一步限制

| 优化项 | 当前 | 改为 | 节省 |
|--------|------|------|------|
| 会话历史 | 无限制 | 10 轮（20 条消息），滑动窗口 | 2-5MB |
| maxTextLength | 500 字符 | 200 字符（简短回复场景） | 微量 |
| 音频处理 | 全量缓冲 | 流式传输（STT 上传 / TTS 下载直接 pipe） | 1-3MB 峰值 |

### 1.6 OS 层加固

#### cgroups v2 硬限制

```bash
# 创建 cgroup，180MB 硬限制
mkdir -p /sys/fs/cgroup/openclaw
echo 188743680 > /sys/fs/cgroup/openclaw/memory.max   # 180MB 硬限
echo 167772160 > /sys/fs/cgroup/openclaw/memory.high   # 160MB 软限（触发内核内存压力）
echo $$ > /sys/fs/cgroup/openclaw/cgroup.procs
```

> Node.js 22 支持 cgroups v2 感知，会根据 `memory.max` 自动调整堆上限。

#### zram 替代磁盘 swap

```bash
# zram 在 RAM 中压缩，比磁盘 swap 快 10-100x
modprobe zram
echo lz4 > /sys/block/zram0/comp_algorithm
echo 128M > /sys/block/zram0/disksize
mkswap /dev/zram0
swapon -p 100 /dev/zram0
```

zram 用 CPU 换内存，典型压缩比 2-3x，等效增加 64-128MB 可用内存。

#### ulimit 约束

```bash
ulimit -n 256     # 限制文件描述符
ulimit -u 32      # 限制子进程数
```

### 1.7 方案一汇总

| 优化项 | 常驻节省 | 峰值节省 | 难度 |
|--------|---------|---------|------|
| V8 极限参数 | 20-35MB | 20-35MB | 低 |
| 替换 Grammy | 8-15MB | 8-15MB | 中 |
| 替换 Express | 5-10MB | 5-10MB | 中 |
| esbuild 打包 | 5-15MB | 5-15MB | 中 |
| 会话历史限制 | 2-5MB | 5-10MB | 低 |
| 音频流式处理 | 1-3MB | 3-5MB | 中 |
| cgroups + zram | 0（硬限） | 等效+64-128MB | 低 |
| **总计** | **~41-83MB** | **~46-90MB** | |

**优化后预估：55-85MB 常驻，75-110MB 峰值**

> **瓶颈**：Node.js V8 引擎基线 ~35-45MB 是硬性下限，无论怎么优化都无法突破。

---

## 方案二：纯 C 重写（MimiClaw 路线）

以 MimiClaw 为蓝本，在 Linux 上用纯 C 实现完整网关，增加语音管线 + Home Assistant。

### 2.1 架构

```
┌──────────────────────────────────────────────┐
│  OpenClaw Micro (Pure C, ~1.2MB RAM)         │
│                                              │
│  Thread 0 (I/O):                             │
│  ┌──────────┐ ┌──────────┐ ┌──────────────┐  │
│  │ tg_poll  │ │ outbound │ │ ws_gateway   │  │
│  │ (12KB)   │ │ (8KB)    │ │ port 18789   │  │
│  └────┬─────┘ └────┬─────┘ └──────────────┘  │
│       │             │                        │
│  ┌────▼─────────────▼────┐                   │
│  │   Message Bus (depth 8)│                   │
│  └────┬──────────────────┘                   │
│       │                                      │
│  Thread 1 (Agent):                           │
│  ┌────▼─────────────────────────────────┐    │
│  │  agent_loop (12KB stack)             │    │
│  │  ├─ context_builder (SOUL/USER/MEM)  │    │
│  │  ├─ llm_proxy (Claude API)           │    │
│  │  ├─ tool_registry                    │    │
│  │  │   ├─ ha_control (Home Assistant)  │    │
│  │  │   ├─ web_search (Brave API)       │    │
│  │  │   ├─ get_time                     │    │
│  │  │   └─ cron_schedule                │    │
│  │  └─ stt_tts_proxy (远程 API 调用)    │    │
│  └──────────────────────────────────────┘    │
│                                              │
│  Storage: /data/ (平面文件)                   │
│  ├─ SOUL.md, USER.md, MEMORY.md             │
│  ├─ sessions/tg_<id>.jsonl                  │
│  └─ cron.json                               │
└──────────────────────────────────────────────┘
```

### 2.2 内存预算

| 组件 | 分配 | 说明 |
|------|------|------|
| C 进程二进制 | ~500KB | musl-libc 静态链接 |
| TLS 库（mbedtls） | ~200KB | 2 个并发 TLS 连接 |
| JSON 解析（cJSON） | ~50KB | 解析 Claude 响应 |
| HTTP 客户端缓冲 | ~64KB | Telegram + Claude API |
| 音频中转缓冲 | ~256KB | 60 秒 Opus@32kbps 足够 |
| 会话/上下文数据 | ~64KB | 20 条消息历史 |
| 系统提示词 | ~16KB | SOUL.md + 设备清单 |
| 工具输出缓冲 | ~32KB | Home Assistant 响应 |
| 线程栈（4 线程） | ~48KB | 每线程 12KB |
| **C 进程总计** | **~1.2MB** | |
| Linux 内核 + 基础 | ~15-20MB | Alpine/BusyBox 最小化 |
| **系统总计** | **~17-22MB** | |
| **剩余可用** | **~178-183MB** | 可跑其他服务 |

### 2.3 语音管线实现

**STT 流程（Telegram 语音 → 文本）：**
1. `getUpdates` 收到 `voice` 消息，获取 `file_id`
2. 调 `getFile` API 获取下载 URL
3. 流式下载 OGG/Opus 文件（5-60 秒语音约 30-240KB）
4. 构造 `multipart/form-data` POST 到 Groq Whisper API
5. 解析 JSON 响应中的 `text` 字段
6. 文本送入 agent 循环

**TTS 流程（文本 → Telegram 语音）：**
1. Agent 产生回复文本
2. 连接 Edge TTS WebSocket 端点
3. 发送 SSML 合成请求
4. 接收音频分块，累积到缓冲区
5. 调 Telegram `sendVoice` 发送 OGG/Opus 音频

### 2.4 vs MimiClaw 差异

| 特性 | MimiClaw (ESP32) | OpenClaw Micro (Linux) |
|------|-----------------|----------------------|
| OS | FreeRTOS 裸机 | Linux (Alpine/BusyBox) |
| 线程 | `xTaskCreatePinnedToCore` | `pthread_create` + CPU 亲和性 |
| 消息队列 | FreeRTOS `xQueueSend` | POSIX message queue |
| TLS | `esp_tls` | mbedtls 或 OpenSSL |
| 文件存储 | SPIFFS 平面文件系统 | ext4/tmpfs 平面文件 |
| HTTP 客户端 | `esp_http_client` | libcurl (静态) 或自写 HTTP/1.1 |
| **语音（新增）** | 无 | STT: 流式上传到 Groq; TTS: Edge TTS WebSocket |
| **HA 控制（新增）** | 无 | HTTP POST 到 HA REST API（直接，无 curl 子进程） |
| **定时（新增）** | 无 | POSIX timer 或 sleep 循环 |

### 2.5 ESP32-S3 可行性

MimiClaw 证明 agent 模式在 8MB PSRAM 上可行。增加语音中转（+256KB）和 HA 控制后，总需求 ~1.5MB，仍远低于 8MB 上限。**如果追求极致，可以直接用 $5 芯片替代 200MB 设备。**

### 2.6 方案二评估

- **优势**：极致内存效率（1.2MB），可下沉到 $5 硬件
- **劣势**：开发 3-6 个月，需要深厚 C 经验，手动内存管理有安全风险，维护成本高
- **适用场景**：功能固定、很少变更、追求硬件成本最低

---

## 方案三：Rust 网关（推荐方案）

### 3.1 核心思路

用 Rust 重写网关，兼得 MimiClaw 级效率 + 内存安全 + 现代异步生态。比 C 开发快 2-3x，比 Node.js 省 80%+ 内存。

### 3.2 技术选型

| 组件 | 选择 | 理由 |
|------|------|------|
| 异步运行时 | `tokio`（单线程 `current_thread`） | 省内存，gateway 不需要多线程并发 |
| HTTP 客户端 | `reqwest` + `rustls-tls` | 成熟、TLS 内建 |
| JSON | `serde_json` | 零拷贝反序列化 |
| Telegram | 原生 API 调用（不用框架） | 最小依赖 |
| 编译 | `musl` 静态链接，`-Os` 优化大小 | 单文件部署 |

### 3.3 架构

```
┌─────────────────────────────────────────────┐
│  OpenClaw Rust Gateway (~5MB)               │
│                                             │
│  ┌──────────────┐  ┌────────────────────┐   │
│  │ Telegram      │  │ Audio Relay        │   │
│  │ Long Polling  │  │ STT: Groq Whisper  │   │
│  │ (fetch API)   │  │ TTS: Edge TTS      │   │
│  └──────┬───────┘  └────────┬───────────┘   │
│         │                   │               │
│  ┌──────▼───────────────────▼─────────────┐ │
│  │  Agent Loop (ReAct)                     │ │
│  │  ├─ Claude API (非流式)                 │ │
│  │  ├─ Tool Registry                       │ │
│  │  │   ├─ ha_control → HA REST API       │ │
│  │  │   ├─ web_search → Brave API         │ │
│  │  │   ├─ get_time                       │ │
│  │  │   └─ cron → 定时器                  │ │
│  │  └─ Memory (SOUL/USER/MEMORY.md)       │ │
│  └────────────────────────────────────────┘ │
│                                             │
│  WebSocket Gateway (port 18789)             │
│  State: sessions/*.jsonl + config.json      │
└─────────────────────────────────────────────┘
```

### 3.4 内存预算

| 组件 | 分配 |
|------|------|
| 二进制 + 静态数据 | ~2-4MB |
| tokio 运行时（单线程） | ~1-2MB |
| reqwest + rustls TLS | ~1MB |
| 音频中转缓冲 | ~256KB |
| 会话/上下文数据 | ~64KB |
| JSON serde 缓冲 | ~32KB |
| **Rust 进程总计** | **~4-8MB** |
| Linux 内核 + 基础 | ~15-20MB |
| **系统总计** | **~20-28MB** |

### 3.5 核心代码框架

```rust
#[tokio::main(flavor = "current_thread")]  // 单线程运行时，省内存
async fn main() {
    let config = load_config().await;
    let mut offset = 0i64;

    loop {
        let updates = telegram_get_updates(&config, offset).await;
        for update in updates {
            offset = update.id + 1;
            match update.message {
                Message::Voice(audio) => {
                    // 语音 → STT → Agent → TTS → 语音回复
                    let text = stt_transcribe(&config, &audio).await;
                    let reply = claude_agent_loop(&config, &text).await;
                    let voice = tts_synthesize(&config, &reply).await;
                    telegram_send_voice(&config, update.chat_id, &voice).await;
                }
                Message::Text(text) => {
                    // 文本 → Agent → 文本回复
                    let reply = claude_agent_loop(&config, &text).await;
                    telegram_send_message(&config, update.chat_id, &reply).await;
                }
            }
        }
    }
}

/// ReAct Agent 循环（参考 MimiClaw agent_loop.c）
async fn claude_agent_loop(config: &Config, input: &str) -> String {
    let mut messages = load_session(config).await;
    messages.push(user_message(input));

    for _ in 0..10 {  // 最多 10 次工具迭代
        let response = call_claude_api(config, &messages).await;

        match response.stop_reason.as_str() {
            "end_turn" => {
                save_session(config, &messages).await;
                return response.text();
            }
            "tool_use" => {
                let tool_results = execute_tools(config, &response.tool_calls).await;
                messages.push(assistant_message(&response));
                messages.push(tool_result_message(&tool_results));
            }
            _ => break,
        }
    }
    "处理超时，请重试".to_string()
}
```

### 3.6 Rust vs C vs Node.js 对比

| 因素 | C | Rust | Node.js |
|------|---|------|---------|
| 内存安全 | 手动（CVE 风险） | 编译器保证 | GC 管理 |
| 异步 I/O | 手动事件循环 | tokio（成熟） | 内置 event loop |
| HTTP 客户端 | libcurl 或手写 | reqwest（TLS 内建） | 内置 fetch |
| JSON 解析 | cJSON（手动） | serde_json（零拷贝） | 内置 JSON.parse |
| Telegram 库 | 无，从零写 | 可用 frankenstein crate 或手写 | Grammy |
| 二进制大小 | ~500KB | ~2-4MB | N/A（需要整个 Node.js） |
| 内存占用 | ~1-2MB | ~4-8MB | ~50-100MB |
| 开发速度 | 慢 | 中 | 快 |
| 嵌入式支持 | ESP32 原生 | esp-rs（在成熟中） | 不支持 |

### 3.7 瘦代理变体（最快实现）

如果不想在设备上跑 Agent 循环，可以做一个"哑代理"：

```
Telegram → [Rust 瘦代理 ~2-4MB] → [远程 OpenClaw 实例（NAS/云端）]
                                → [Home Assistant（局域网）]
```

瘦代理只负责：Telegram 轮询、音频上传/下载、WebSocket 网关。所有 AI 逻辑跑在远程机器上。

**内存：~2-4MB + ~15-20MB Linux = ~17-24MB**

---

## 推荐路线

### 短期（本周）：方案一 — Node.js 深度优化
- 更新 V8 参数为极限配置
- 添加 cgroups v2 + zram 配置
- 会话历史限制
- 立即生效，无需改架构

### 中期（1-2 月）：方案三 — Rust 网关
- 用 Rust 重写核心网关
- 保留相同的外部 API 接口（Groq STT / Edge TTS / Claude / HA）
- 从 ~100MB 降至 ~25MB
- 内存安全，生产质量

### 长期（可选）：方案二 — 纯 C / ESP32
- 如果要下沉到 $5 硬件
- 或者功能已完全固化不再变更

---

## 参考资源

| 资源 | 链接/路径 |
|------|----------|
| MimiClaw 仓库 | https://github.com/memovai/mimiclaw |
| MimiClaw 架构文档 | `docs/ARCHITECTURE.md`（内存预算、任务分布、Flash 分区） |
| MimiClaw Agent 循环 | `main/agent/agent_loop.c`（ReAct 模式，~300 行 C） |
| MimiClaw 消息总线 | `main/bus/message_bus.c`（生产者-消费者，深度 8 队列） |
| MimiClaw 配置 | `main/mimi_config.h`（缓冲区大小、队列深度、栈大小） |
| 当前设计文档 | `design.md`（现有架构、模块预算、接口规范） |
