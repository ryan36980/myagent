# OpenClaw Light Extreme Resource Constraint Plan

> **Decision Conclusion: Plan C (Rust Gateway) has been selected** — implementation details in [Design Docs](../design/README.md)
>
> Reference project: [MimiClaw](https://github.com/memovai/mimiclaw) — runs a full AI Agent on a $5 ESP32-S3 chip

---

## Core Finding: What MimiClaw Proves

MimiClaw runs a full AI Agent on a $5 ESP32-S3 (8MB PSRAM) with a working set of only ~300KB:

| Component | Memory |
|-----------|--------|
| FreeRTOS stacks (6 tasks) | ~40KB |
| WiFi buffer | ~30KB |
| TLS (2 connections) | ~120KB |
| JSON parsing + session + LLM buffer | ~112KB |
| **Total** | **~302KB** |

**Key insight: The actual application logic for Telegram + Claude + tool calls requires only ~300KB. Over 95% of the 95–125MB in the Node.js approach is runtime overhead.**

MimiClaw's limitation: **no voice pipeline** (no STT/TTS), no Home Assistant integration. We need to add these capabilities on top.

### MimiClaw Architecture Highlights

- **Dual-core division of labor**: Core 0 handles I/O (Telegram polling, message sending); Core 1 handles the Agent loop (Claude API calls + tool execution)
- **Message bus**: FreeRTOS queue, depth 8, `mimi_msg_t` struct with ownership-transfer semantics
- **ReAct loop**: up to 10 tool iterations, non-streaming Claude API calls
- **Storage**: SPIFFS flat filesystem, SOUL.md / USER.md / MEMORY.md + per-user JSONL sessions
- **Config**: compile-time `mimi_secrets.h` + runtime NVS Flash overrides

---

## Overview of the Three Plans

| Dimension | Plan A: Node.js Deep Optimization | Plan B: Pure C Rewrite | Plan C: Rust Gateway |
|-----------|----------------------------------|----------------------|---------------------|
| Resident memory | 55–85MB | 18–20MB | 20–28MB |
| Peak memory | 75–110MB | 19–20MB | 25–30MB |
| % of 200MB | 38–55% | 9–10% | 12–15% |
| Development time | 1–2 weeks | 3–6 months | 2–4 weeks |
| Maintainability | High (JS ecosystem) | Low (manual memory management) | Medium (compiler-guaranteed safety) |
| Runs on ESP32-S3? | No | Yes | Partial (esp-rs) |
| Memory safety risk | None (GC-managed) | High (malloc/free) | None (borrow checker) |

---

## Plan A: Node.js Deep Optimization (Immediately actionable, 1–2 weeks)

Further compression on the current architecture without changing the tech stack.

### 1.1 V8 Extreme Parameters

**Current:**
```bash
NODE_OPTIONS="--max-old-space-size=100 --optimize-for-size"
```

**Extreme tuning:**
```bash
NODE_OPTIONS="--max-old-space-size=64 --max-semi-space-size=2 --optimize-for-size --jitless --lite-mode --single-threaded-gc"
```

| Parameter | Effect | Savings | Trade-off |
|-----------|--------|---------|-----------|
| `--max-old-space-size=64` | Old generation limit 100→64MB | ~36MB limit reduction | More frequent Major GC |
| `--max-semi-space-size=2` | New generation 16→2MB | ~28MB | More frequent Minor GC (scavenge) |
| `--jitless` | Disable all JIT compilation, pure interpreter | ~10–20MB (eliminate JIT code pages) | JS runs 3–10x slower, but this app is I/O-bound (waiting for API responses), so impact is minimal |
| `--lite-mode` | V8 lite mode, reduce code cache | ~5–10MB | Slightly slower startup |
| `--single-threaded-gc` | Disable parallel GC threads | ~2–4MB (GC thread stacks) | Slightly longer GC pauses |

> **Note**: The V8 blog reports only 1.7% heap reduction for `--jitless`, but the real savings come from eliminating JIT code memory pages (not counted in heap stats). For gateway apps with many modules this can save 10–20MB. `--jitless` + `--lite-mode` is the recommended combination for embedded scenarios.

**Estimated savings: 20–35MB**

### 1.2 Replace Grammy with Native Telegram API

The Grammy SDK is estimated to use 15–20MB (SDK + internal state + plugin system). Use Node.js 22's built-in `fetch` to call the Telegram Bot API directly:

```javascript
// Native long-polling, no Grammy needed
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

**Estimated savings: 8–15MB**

### 1.3 Replace Express with http.createServer

The design document §3.1 estimates "Gateway HTTP server" at 15–20MB (Express 5 + routing + middleware). The gateway only needs a few routes; use native HTTP:

```javascript
const http = require('http');
const server = http.createServer((req, res) => {
  // Simple URL routing switch/map, no middleware chain needed
});
```

**Estimated savings: 5–10MB**

### 1.4 esbuild Bundling + Tree-shaking

Bundle the entire application into a single file to eliminate dead code:

```bash
esbuild src/index.ts \
  --bundle --platform=node --target=node22 \
  --minify --tree-shaking=true \
  --outfile=openclaw-light.mjs \
  --external:better-sqlite3
```

Benefits:
- Tree-shaking eliminates disabled module code at build time (not just skipped at runtime)
- Reduces `require` cache (one module vs hundreds of node_modules files)
- Reduces file descriptor usage
- Faster startup (no module resolution overhead)

**Estimated savings: 5–15MB**

### 1.5 Further Application-Level Limits

| Optimization | Current | Change To | Savings |
|-------------|---------|-----------|---------|
| Session history | Unlimited | 10 turns (20 messages), sliding window | 2–5MB |
| maxTextLength | 500 characters | 200 characters (short reply scenario) | Minor |
| Audio processing | Full buffering | Streaming (STT upload / TTS download direct pipe) | 1–3MB peak |

### 1.6 OS-Level Hardening

#### cgroups v2 hard limit

```bash
# Create cgroup, 180MB hard limit
mkdir -p /sys/fs/cgroup/openclaw
echo 188743680 > /sys/fs/cgroup/openclaw/memory.max   # 180MB hard limit
echo 167772160 > /sys/fs/cgroup/openclaw/memory.high   # 160MB soft limit (triggers kernel memory pressure)
echo $$ > /sys/fs/cgroup/openclaw/cgroup.procs
```

> Node.js 22 supports cgroups v2 awareness and automatically adjusts its heap limit based on `memory.max`.

#### zram instead of disk swap

```bash
# zram compresses in RAM, 10-100x faster than disk swap
modprobe zram
echo lz4 > /sys/block/zram0/comp_algorithm
echo 128M > /sys/block/zram0/disksize
mkswap /dev/zram0
swapon -p 100 /dev/zram0
```

zram trades CPU for memory; typical compression ratio is 2–3x, effectively adding 64–128MB of available memory.

#### ulimit constraints

```bash
ulimit -n 256     # Limit file descriptors
ulimit -u 32      # Limit child processes
```

### 1.7 Plan A Summary

| Optimization | Resident Savings | Peak Savings | Difficulty |
|-------------|-----------------|-------------|-----------|
| V8 extreme parameters | 20–35MB | 20–35MB | Low |
| Replace Grammy | 8–15MB | 8–15MB | Medium |
| Replace Express | 5–10MB | 5–10MB | Medium |
| esbuild bundling | 5–15MB | 5–15MB | Medium |
| Session history limit | 2–5MB | 5–10MB | Low |
| Audio streaming | 1–3MB | 3–5MB | Medium |
| cgroups + zram | 0 (hard limit) | Equivalent +64–128MB | Low |
| **Total** | **~41–83MB** | **~46–90MB** | |

**Estimated post-optimization: 55–85MB resident, 75–110MB peak**

> **Bottleneck**: The Node.js V8 engine baseline of ~35–45MB is a hard floor; no amount of optimization can break through it.

---

## Plan B: Pure C Rewrite (MimiClaw approach)

Model after MimiClaw and implement the complete gateway in pure C on Linux, adding the voice pipeline + Home Assistant.

### 2.1 Architecture

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
│  │  └─ stt_tts_proxy (remote API calls) │    │
│  └──────────────────────────────────────┘    │
│                                              │
│  Storage: /data/ (flat files)                │
│  ├─ SOUL.md, USER.md, MEMORY.md             │
│  ├─ sessions/tg_<id>.jsonl                  │
│  └─ cron.json                               │
└──────────────────────────────────────────────┘
```

### 2.2 Memory Budget

| Component | Allocation | Notes |
|-----------|-----------|-------|
| C process binary | ~500KB | musl-libc static linking |
| TLS library (mbedtls) | ~200KB | 2 concurrent TLS connections |
| JSON parsing (cJSON) | ~50KB | Parse Claude responses |
| HTTP client buffer | ~64KB | Telegram + Claude API |
| Audio relay buffer | ~256KB | Sufficient for 60s Opus@32kbps |
| Session/context data | ~64KB | 20-message history |
| System prompt | ~16KB | SOUL.md + device list |
| Tool output buffer | ~32KB | Home Assistant responses |
| Thread stacks (4 threads) | ~48KB | 12KB per thread |
| **C process total** | **~1.2MB** | |
| Linux kernel + base | ~15–20MB | Alpine/BusyBox minimized |
| **System total** | **~17–22MB** | |
| **Remaining available** | **~178–183MB** | Can run other services |

### 2.3 Voice Pipeline Implementation

**STT flow (Telegram voice → text):**
1. `getUpdates` receives a `voice` message, retrieve `file_id`
2. Call `getFile` API to get download URL
3. Stream-download OGG/Opus file (5–60s voice is approximately 30–240KB)
4. Construct `multipart/form-data` POST to Groq Whisper API
5. Parse the `text` field from the JSON response
6. Feed text into the agent loop

**TTS flow (text → Telegram voice):**
1. Agent produces reply text
2. Connect to Edge TTS WebSocket endpoint
3. Send SSML synthesis request
4. Receive audio chunks, accumulate into buffer
5. Call Telegram `sendVoice` to send OGG/Opus audio

### 2.4 Differences vs MimiClaw

| Feature | MimiClaw (ESP32) | OpenClaw Micro (Linux) |
|---------|-----------------|----------------------|
| OS | FreeRTOS bare metal | Linux (Alpine/BusyBox) |
| Threads | `xTaskCreatePinnedToCore` | `pthread_create` + CPU affinity |
| Message queue | FreeRTOS `xQueueSend` | POSIX message queue |
| TLS | `esp_tls` | mbedtls or OpenSSL |
| File storage | SPIFFS flat filesystem | ext4/tmpfs flat files |
| HTTP client | `esp_http_client` | libcurl (static) or hand-written HTTP/1.1 |
| **Voice (new)** | None | STT: stream upload to Groq; TTS: Edge TTS WebSocket |
| **HA control (new)** | None | HTTP POST to HA REST API (direct, no curl subprocess) |
| **Scheduling (new)** | None | POSIX timer or sleep loop |

### 2.5 ESP32-S3 Feasibility

MimiClaw proves agent mode is feasible on 8MB PSRAM. Adding voice relay (+256KB) and HA control, total requirement is ~1.5MB, still well below the 8MB limit. **If extreme minimalism is the goal, a $5 chip can directly replace the 200MB device.**

### 2.6 Plan B Assessment

- **Advantages**: Extreme memory efficiency (1.2MB), can run on $5 hardware
- **Disadvantages**: 3–6 months of development, requires deep C expertise, manual memory management carries security risks, high maintenance cost
- **Suitable for**: Fixed functionality with rare changes, targeting lowest possible hardware cost

---

## Plan C: Rust Gateway (Recommended)

### 3.1 Core Idea

Rewrite the gateway in Rust: achieve MimiClaw-level efficiency + memory safety + modern async ecosystem. 2–3x faster to develop than C, uses 80%+ less memory than Node.js.

### 3.2 Technology Selection

| Component | Choice | Rationale |
|-----------|--------|-----------|
| Async runtime | `tokio` (single-threaded `current_thread`) | Saves memory; gateway doesn't need multi-threaded concurrency |
| HTTP client | `reqwest` + `rustls-tls` | Mature, TLS built-in |
| JSON | `serde_json` | Zero-copy deserialization |
| Telegram | Native API calls (no framework) | Minimal dependencies |
| Compilation | `musl` static linking, `-Os` size optimization | Single-file deployment |

### 3.3 Architecture

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
│  │  ├─ Claude API (non-streaming)          │ │
│  │  ├─ Tool Registry                       │ │
│  │  │   ├─ ha_control → HA REST API       │ │
│  │  │   ├─ web_search → Brave API         │ │
│  │  │   ├─ get_time                       │ │
│  │  │   └─ cron → scheduler               │ │
│  │  └─ Memory (SOUL/USER/MEMORY.md)       │ │
│  └────────────────────────────────────────┘ │
│                                             │
│  WebSocket Gateway (port 18789)             │
│  State: sessions/*.jsonl + config.json      │
└─────────────────────────────────────────────┘
```

### 3.4 Memory Budget

| Component | Allocation |
|-----------|-----------|
| Binary + static data | ~2–4MB |
| tokio runtime (single-threaded) | ~1–2MB |
| reqwest + rustls TLS | ~1MB |
| Audio relay buffer | ~256KB |
| Session/context data | ~64KB |
| JSON serde buffer | ~32KB |
| **Rust process total** | **~4–8MB** |
| Linux kernel + base | ~15–20MB |
| **System total** | **~20–28MB** |

### 3.5 Core Code Framework

```rust
#[tokio::main(flavor = "current_thread")]  // Single-threaded runtime, saves memory
async fn main() {
    let config = load_config().await;
    let mut offset = 0i64;

    loop {
        let updates = telegram_get_updates(&config, offset).await;
        for update in updates {
            offset = update.id + 1;
            match update.message {
                Message::Voice(audio) => {
                    // Voice → STT → Agent → TTS → voice reply
                    let text = stt_transcribe(&config, &audio).await;
                    let reply = claude_agent_loop(&config, &text).await;
                    let voice = tts_synthesize(&config, &reply).await;
                    telegram_send_voice(&config, update.chat_id, &voice).await;
                }
                Message::Text(text) => {
                    // Text → Agent → text reply
                    let reply = claude_agent_loop(&config, &text).await;
                    telegram_send_message(&config, update.chat_id, &reply).await;
                }
            }
        }
    }
}

/// ReAct Agent loop (modeled after MimiClaw agent_loop.c)
async fn claude_agent_loop(config: &Config, input: &str) -> String {
    let mut messages = load_session(config).await;
    messages.push(user_message(input));

    for _ in 0..10 {  // Up to 10 tool iterations
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
    "Processing timed out, please try again".to_string()
}
```

### 3.6 Rust vs C vs Node.js Comparison

| Factor | C | Rust | Node.js |
|--------|---|------|---------|
| Memory safety | Manual (CVE risk) | Compiler-guaranteed | GC-managed |
| Async I/O | Manual event loop | tokio (mature) | Built-in event loop |
| HTTP client | libcurl or hand-written | reqwest (TLS built-in) | Built-in fetch |
| JSON parsing | cJSON (manual) | serde_json (zero-copy) | Built-in JSON.parse |
| Telegram library | None, write from scratch | frankenstein crate or hand-written | Grammy |
| Binary size | ~500KB | ~2–4MB | N/A (requires full Node.js) |
| Memory usage | ~1–2MB | ~4–8MB | ~50–100MB |
| Development speed | Slow | Medium | Fast |
| Embedded support | ESP32 native | esp-rs (maturing) | Not supported |

### 3.7 Thin Proxy Variant (Fastest to implement)

If you don't want to run the Agent loop on the device, build a "dumb proxy":

```
Telegram → [Rust thin proxy ~2-4MB] → [Remote OpenClaw instance (NAS/cloud)]
                                    → [Home Assistant (LAN)]
```

The thin proxy only handles: Telegram polling, audio upload/download, WebSocket gateway. All AI logic runs on a remote machine.

**Memory: ~2–4MB + ~15–20MB Linux = ~17–24MB**

---

## Recommended Path

### Short-term (this week): Plan A — Node.js Deep Optimization
- Update V8 parameters to extreme configuration
- Add cgroups v2 + zram configuration
- Session history limit
- Takes effect immediately, no architectural changes needed

### Medium-term (1–2 months): Plan C — Rust Gateway
- Rewrite the core gateway in Rust
- Retain the same external API interfaces (Groq STT / Edge TTS / Claude / HA)
- Reduce from ~100MB to ~25MB
- Memory-safe, production quality

### Long-term (optional): Plan B — Pure C / ESP32
- If targeting $5 hardware
- Or when functionality is fully fixed and will no longer change

---

## Reference Resources

| Resource | Link / Path |
|----------|------------|
| MimiClaw repository | https://github.com/memovai/mimiclaw |
| MimiClaw architecture docs | `docs/ARCHITECTURE.md` (memory budget, task distribution, Flash partition) |
| MimiClaw Agent loop | `main/agent/agent_loop.c` (ReAct pattern, ~300 lines of C) |
| MimiClaw message bus | `main/bus/message_bus.c` (producer-consumer, depth-8 queue) |
| MimiClaw config | `main/mimi_config.h` (buffer sizes, queue depth, stack sizes) |
| Current design document | `design.md` (existing architecture, module budget, interface specs) |
