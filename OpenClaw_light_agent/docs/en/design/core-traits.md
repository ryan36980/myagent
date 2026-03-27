> [Design Docs](README.md) > Core Specifications & Trait Definitions

# Chapter 1: Rust Project Conventions & Best Practices

## 1.1 Project Directory Structure

```
openclaw-light/
├── Cargo.toml
├── clippy.toml                        # too-many-arguments-threshold = 8
├── rustfmt.toml                       # max_width = 100, tab_spaces = 4
├── rust-toolchain.toml                # channel = "1.84.0"
├── config/
│   └── openclaw.json.example
├── scripts/
│   ├── cross-build.sh
│   └── deploy.sh
├── src/
│   ├── main.rs                        # Entry: load config, build AgentRuntime, start main loop
│   ├── config.rs                      # JSON5 parsing, environment variable substitution
│   ├── error.rs                       # GatewayError unified error enum
│   ├── lib.rs                        # Library crate re-exports
│   ├── channel/
│   │   ├── mod.rs                     # Channel trait definition
│   │   ├── types.rs                   # IncomingMessage / OutgoingMessage and other unified types
│   │   ├── telegram.rs                # Telegram Bot API long polling implementation
│   │   └── feishu.rs                  # Feishu/Lark WebSocket long connection implementation
│   ├── provider/
│   │   ├── mod.rs                     # LlmProvider / SttProvider / TtsProvider trait
│   │   ├── llm/claude.rs             # Anthropic Messages API v1
│   │   ├── stt/groq.rs               # Groq Whisper multipart upload
│   │   ├── stt/google.rs             # Google Cloud Speech-to-Text REST v1
│   │   ├── stt/volcengine.rs         # Volcengine (Doubao) STT WebSocket binary protocol
│   │   ├── tts/mod.rs                # TtsProvider trait + AudioFormat enum
│   │   ├── tts/edge.rs               # Edge TTS WebSocket (WebM→OGG auto-conversion)
│   │   ├── tts/volcengine.rs         # Volcengine TTS HTTP REST (native OGG/Opus)
│   │   ├── tts/openai.rs             # OpenAI-compatible TTS
│   │   ├── tts/elevenlabs.rs         # ElevenLabs TTS
│   │   └── tts/webm_to_ogg.rs       # WebM Opus → OGG Opus container conversion
│   ├── agent/
│   │   └── react_loop.rs             # ReAct loop (up to 10 iterations)
│   ├── memory/
│   │   ├── mod.rs                     # Module declarations
│   │   └── store.rs                   # MemoryStore: MEMORY.md + logs + search + adaptive injection
│   ├── tools/
│   │   ├── mod.rs                     # Tool trait + registry
│   │   ├── ha_control.rs             # Home Assistant REST API
│   │   ├── web_fetch.rs              # HTTP GET
│   │   ├── get_time.rs               # chrono current time
│   │   ├── cron.rs                    # Scheduled tasks + cron_matches + execution support
│   │   └── memory.rs                  # MemoryTool (6 actions) + ChatContext
│   └── session/
│       └── store.rs                   # JSONL session persistence + sliding window truncation
└── tests/
    ├── fixtures/
    └── integration/
```

## 1.2 Dependency List

| Crate | Version | Purpose |
|-------|---------|---------|
| `tokio` | 1.x | Async runtime (current_thread), features: rt, macros, time, fs, signal, sync |
| `reqwest` | 0.12 | HTTP client, features: json, rustls-tls, multipart |
| `serde` / `serde_json` | 1.x | Serialization framework + JSON parsing |
| `thiserror` | 2.x | Declarative error types (GatewayError enum) |
| `anyhow` | 1.x | Top-level function error propagation |
| `tracing` | 0.1 | Structured logging |
| `tracing-subscriber` | 0.3 | Logging backend, features: env-filter |
| `tokio-tungstenite` | 0.24 | WebSocket client (Edge TTS), features: rustls-tls-webpki-roots |
| `json5` | 0.4 | Config file parsing (supports comments) |
| `async-trait` | 0.1 | Async trait support |
| `chrono` | 0.4 | Date/time, features: clock, serde |
| `uuid` | 1.x | UUID v4 generation |
| `base64` | 0.22 | Base64 encoding/decoding |
| `url` | 2.x | URL parsing and construction |
| `sha2` | 0.10 | SHA-256 hashing (OAuth PKCE code_challenge) |

Dev dependencies: `wiremock` 0.6 (HTTP mock), `tokio-test` 0.4, `tempfile` 3 (temp directory tests).

## 1.3 Error Handling Strategy

Uses a **thiserror + anyhow** two-layer pattern:

- **Within modules**: Return `Result<T, GatewayError>`, each error variant corresponds to a failure domain.
- **Top-level dispatch**: Use `anyhow::Result`, allowing different error types to propagate freely.
- Recoverable errors (network timeout, API rate limiting) are logged via `tracing::warn!` then retried or skipped.
- Unrecoverable errors (missing config, TLS init failure) terminate the process via `anyhow::bail!`.
- **Forbidden** to use `unwrap()` / `expect()` in non-test code; always use `?` for propagation.

## 1.4 Memory Optimization Rules

1. Audio buffers use `Vec::with_capacity(256 * 1024)` for pre-allocation to avoid reallocation.
2. Large objects are immediately `drop`ped after processing, or released via `{ }` block scoping.
3. Prefer `&str` on the message path; only `to_string()` when crossing await boundaries.
4. Session history defaults to unlimited (`historyLimit: 0`), relying on auto-compaction to control context size.
5. TTS audio is received in chunks, written to the buffer chunk by chunk.
6. Use `tokio` `current_thread` mode to avoid multi-thread overhead.
7. Do not introduce stateful middleware like LRU caches.

## 1.5 Binary Size Optimization

```toml
[profile.release]
opt-level = "z"        # Optimize for minimum size
lto = true             # Link-time optimization, eliminates dead code
codegen-units = 1      # Single compilation unit, maximizes LTO
panic = "abort"        # Don't preserve unwind tables
strip = true           # Strip debug symbols
```

Result: release binary compressed from ~10-15MB to ~2-4MB.

## 1.6 Cross-Compilation Targets

| Target Triple | Applicable Devices |
|---------------|-------------------|
| `aarch64-unknown-linux-musl` | Raspberry Pi 4/5, RK3588 (64-bit ARM) |
| `armv7-unknown-linux-musleabihf` | Raspberry Pi 2/3, NanoPi (32-bit ARM) |
| `x86_64-unknown-linux-musl` | General Linux servers |

All targets use musl static linking; the output is a single binary with no external dependencies.

---

# Chapter 2: Core Trait Definitions (Five-Layer Abstraction)

The five-layer traits are decoupled through `dyn` dynamic dispatch, allowing each layer's implementation to be independently replaced.

## 2.1 Channel — Channel Layer

```rust
#[async_trait]
pub trait Channel: Send + Sync {
    fn id(&self) -> &str;

    /// Channel's preferred audio format (default OGG/Opus).
    /// Similar to C++ virtual functions — subclasses can override, agent layer reads this value before TTS synthesis.
    fn preferred_audio_format(&self) -> AudioFormat {
        AudioFormat::OggOpus
    }

    async fn poll(&self) -> Result<Vec<IncomingMessage>, GatewayError>;
    async fn send_text(&self, chat_id: &str, text: &str) -> Result<String, GatewayError>; // Returns message_id
    async fn send_voice(&self, chat_id: &str, audio: &[u8]) -> Result<(), GatewayError>;
    async fn download_voice(&self, file_ref: &str) -> Result<Vec<u8>, GatewayError>;
    async fn edit_message(&self, _chat_id: &str, _msg_id: &str, _text: &str) -> Result<(), GatewayError> {
        Ok(()) // Default no-op, channels supporting edit (e.g. Telegram, Feishu) override this method
    }
    async fn send_typing(&self, _chat_id: &str) -> Result<(), GatewayError> {
        Ok(()) // Default no-op, Telegram overrides to sendChatAction(typing)
    }
}
```

## 2.2 LlmProvider — Large Language Model Layer

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn chat(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse, GatewayError>;

    /// Streaming chat request, returns an SSE event stream.
    /// Default implementation: calls chat() then wraps as a single-event stream. Providers supporting SSE should override this method.
    async fn chat_stream(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<BoxStream<'static, Result<StreamEvent>>, GatewayError> {
        // Default: chat() → wrap as stream
    }
}
```

## 2.3 SttProvider — Speech Recognition Layer

```rust
#[async_trait]
pub trait SttProvider: Send + Sync {
    async fn transcribe(&self, audio: &[u8], mime: &str) -> Result<String, GatewayError>;
}
```

## 2.4 TtsProvider — Speech Synthesis Layer

```rust
/// Audio output format enum. Default OGG/Opus — the unified format for all channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Mp3,
    OggOpus, // Default
}

#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// Synthesize speech. `format` is the preferred format declared by the Channel.
    /// TTS providers guarantee output format matches internally:
    /// - Natively supported formats are output directly (Volcengine → OGG/Opus)
    /// - Unsupported formats are converted internally (Edge TTS → WebM Opus → OGG Opus)
    async fn synthesize(&self, text: &str, format: AudioFormat) -> Result<Vec<u8>, GatewayError>;
}
```

**Unified Audio Format Architecture (AudioFormat):**

```
Channel::preferred_audio_format()  →  AudioFormat::OggOpus (default)
        ↓
handle() extracts audio_format
        ↓
try_synthesize(text, audio_format)
        ↓
TtsProvider::synthesize(text, format)
        ↓
┌─────────────────────────────────────────────┐
│ Edge TTS:     WebM Opus → webm_to_ogg_opus()│ ← Internal auto-conversion
│ Volcengine:   Directly requests ogg_opus     │
│ OpenAI TTS:   Native OGG/Opus               │
│ ElevenLabs:   Native format (ignores format) │
└─────────────────────────────────────────────┘
        ↓
Unified OGG/Opus output → Channel::send_voice()
```

**Design Principles:**
- Format conversion responsibility lies within TTS providers, not leaked to the Channel layer
- `AudioFormat` enum preserves extensibility, but currently all channels uniformly use OggOpus
- New channels needing different formats simply override `preferred_audio_format()`
- New TTS providers must guarantee OGG Opus output (native or converted)

## 2.5 Tool — Tool Layer

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> serde_json::Value;
    async fn execute(&self, input: serde_json::Value) -> Result<String, GatewayError>;
}
```

## 2.6 Unified Message Types

Defined in `src/channel/types.rs`, serving as the cross-layer public protocol:

```rust
pub struct IncomingMessage {
    pub channel: String,           // "telegram" / "wechat"
    pub chat_id: String,
    pub sender_id: String,
    pub content: MessageContent,   // Text(String) | Voice { file_ref, mime }
    pub timestamp: i64,
}

pub struct OutgoingMessage {
    pub text: Option<String>,
    pub voice: Option<Vec<u8>>,    // Opus audio
}

pub struct ChatMessage {
    pub role: Role,                // User | Assistant
    pub content: Vec<ContentBlock>,
}

pub enum ContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String },
}

pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Other(String),
}

pub struct LlmResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
}

/// Streaming LLM event (element of the event stream returned by chat_stream)
pub enum StreamEvent {
    TextDelta(String),                                    // Incremental text
    ToolUse { id: String, name: String, input: Value },   // Complete tool call block
    Done { stop_reason: StopReason },                     // Generation complete
}

pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}
```

## 2.7 AgentRuntime Assembly

The core dispatcher, holding `Box<dyn ...>` for all trait objects, with dependency injection completed in `main`:

```rust
pub struct AgentRuntime {
    pub llm: Box<dyn LlmProvider>,
    pub stt: Box<dyn SttProvider>,
    pub tts: Box<dyn TtsProvider>,
    pub tools: ToolRegistry,
    pub sessions: SessionStore,
    pub memory: Arc<MemoryStore>,                    // Long-term memory storage
    pub chat_context: Arc<Mutex<ChatContext>>,        // Current (channel, chat_id) shared context
    pub system_prompt: String,
    pub max_iterations: usize,
    pub tts_auto_mode: String,               // "inbound"/"always"/"tagged"/other(off)
    pub auto_compact: bool,                  // Auto-compact conversation context (default true)
    pub compact_ratio: f64,                  // Compaction retention ratio (default 0.4)
    pub response_prefix: String,             // Response prefix template
    pub provider_name: String,               // For template substitution
    pub model_name: String,                  // For template substitution
    pub thinking_level: String,              // For template substitution
}
```

`ChatContext` is defined in `src/tools/memory.rs`, shared via `Arc<Mutex<>>` between AgentRuntime, MemoryTool, and CronTool:

```rust
pub struct ChatContext {
    pub channel: String,
    pub chat_id: String,
}
```

Uses `dyn` instead of generics because: the same Vec can hold different implementations; runtime selection by config; virtual function call overhead is negligible in I/O-intensive scenarios.
