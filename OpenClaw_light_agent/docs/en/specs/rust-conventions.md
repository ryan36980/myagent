# OpenClaw Rust Gateway — Coding Conventions

This document defines the coding conventions for the OpenClaw Rust gateway project. Goal: consistent code, controlled memory usage, stable operation on 200MB embedded devices.

---

## 1. Naming Conventions

| Category | Style | Example |
|----------|-------|---------|
| Functions, variables, modules | `snake_case` | `fn poll_updates()`, `let chat_id` |
| Types, Traits, enums | `CamelCase` | `struct AgentConfig`, `enum StopReason` |
| Constants, static variables | `SCREAMING_SNAKE_CASE` | `const MAX_TOOL_ITERATIONS: usize = 10;` |
| Type parameters | Single uppercase letter or short CamelCase | `<T>`, `<S: SttProvider>` |

File names are always `snake_case.rs`. Module directories use `mod.rs` as the entry point.

---

## 2. Error Handling

### Within modules: `thiserror` for domain errors

Each module defines its own error enum, explicitly listing all possible failure reasons:

```rust
#[derive(Debug, thiserror::Error)]
pub enum TelegramError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("API returned error: {description}")]
    Api { error_code: i32, description: String },

    #[error("JSON deserialization failed: {0}")]
    Deserialize(#[from] serde_json::Error),
}
```

### Top-level entry points: `anyhow` for unified handling

`main.rs` and top-level orchestration functions use `anyhow::Result`, simplifying cross-module error propagation:

```rust
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let config = Config::load("config.json5").await?;
    // ...
    Ok(())
}
```

### Prohibited patterns

- **No `unwrap()` / `expect()`** — the only exception: during program startup (loading config, initializing logging), `expect("reason")` is permitted.
- **Use `?` operator throughout** to propagate errors; do not write manual `match` + `return Err`.
- All calls to external APIs must have a timeout (`reqwest::Client` with `timeout` set).

---

## 3. Memory Rules

This project targets 4–8 MB resident memory (Rust process). The following rules are mandatory:

### 3.1 Reuse HTTP clients

Create a single global `reqwest::Client` instance and pass it as a parameter. `Client` maintains an internal connection pool and reuses TLS sessions:

```rust
let client = reqwest::Client::builder()
    .timeout(Duration::from_secs(60))
    .pool_max_idle_per_host(2)
    .build()?;
```

**Prohibited**: calling `Client::new()` repeatedly inside loops or functions.

### 3.2 Pre-allocate audio buffers

Audio buffers use `Vec<u8>` with pre-allocation. After processing, call `clear()` to reset length while retaining capacity, avoiding repeated allocations:

```rust
let mut audio_buf: Vec<u8> = Vec::with_capacity(256 * 1024); // 256KB
// Before each use:
audio_buf.clear(); // length reset to zero, capacity preserved
```

### 3.3 Session history sliding window

Session messages retain the most recent 20 entries (10 conversation turns). Entries beyond the limit are removed from the front:

```rust
const MAX_SESSION_MESSAGES: usize = 20;

if messages.len() > MAX_SESSION_MESSAGES {
    messages.drain(..messages.len() - MAX_SESSION_MESSAGES);
}
```

### 3.4 Avoid unnecessary clones

- Prefer `&str` references over `String`.
- Use `Cow<'_, str>` for "possibly borrowed, possibly owned" scenarios:

```rust
fn build_prompt<'a>(template: &'a str, name: &'a str) -> Cow<'a, str> {
    if name.is_empty() {
        Cow::Borrowed(template)
    } else {
        Cow::Owned(template.replace("{name}", name))
    }
}
```

### 3.5 Use `Arc<Mutex<>>` sparingly

This project uses a single-threaded tokio runtime. Most state is passed via mutable references and does not need concurrency primitives.

**Only exception**: when the same state must be shared across multiple independent components (e.g., `ChatContext` shared between AgentRuntime, MemoryTool, and CronTool), use `Arc<Mutex<>>` + `tokio::sync::Mutex`. Use only when cross-component sharing is genuinely required; prefer parameter passing.

---

## 4. Module Organization

```
src/
├── main.rs                      # Entry point + multi-channel dispatch + cron execution + graceful shutdown
├── lib.rs                       # Library crate re-exports (for integration tests)
├── config.rs                    # Config struct + JSON5 loading + env var substitution
├── error.rs                     # GatewayError unified error enum
├── channel/
│   ├── mod.rs                   # Channel trait definition (including edit_message default impl)
│   ├── types.rs                 # IncomingMessage / ChatMessage / StreamEvent etc.
│   ├── telegram.rs              # Telegram Bot API long-polling implementation
│   ├── cli.rs                   # CLI channel (stdin/stdout)
│   └── http_api.rs              # HTTP REST API channel (raw TCP)
├── provider/
│   ├── mod.rs                   # LlmProvider / SttProvider / TtsProvider traits
│   ├── llm/
│   │   ├── mod.rs               # LlmProvider trait (including chat_stream default impl)
│   │   ├── claude.rs            # Anthropic Messages API v1 + SSE streaming
│   │   └── openai_compat.rs     # OpenAI Chat Completions compatible + SSE streaming
│   ├── stt/
│   │   ├── mod.rs
│   │   └── groq.rs              # Groq Whisper
│   └── tts/
│       ├── mod.rs
│       ├── edge.rs              # Edge TTS WebSocket (default, free)
│       ├── openai.rs            # OpenAI-compatible TTS
│       └── elevenlabs.rs        # ElevenLabs TTS
├── agent/
│   ├── mod.rs
│   ├── react_loop.rs            # ReAct loop + AgentRuntime + auto-compaction
│   └── context.rs               # System prompt assembly (memory + tools + compaction hints)
├── memory/
│   ├── mod.rs
│   └── store.rs                 # MemoryStore: SHARED/ + per-chat MEMORY.md + logs + search + adaptive injection
├── tools/
│   ├── mod.rs                   # Tool trait + ToolRegistry
│   ├── ha_control.rs            # Home Assistant REST API
│   ├── web_fetch.rs             # HTTP GET
│   ├── get_time.rs              # Current time
│   ├── cron.rs                  # Scheduled tasks + cron_matches
│   ├── memory.rs                # Memory management (6 actions + scope parameter) + ChatContext
│   ├── exec.rs                  # Shell command execution + skills_dir PATH
│   ├── web_search.rs            # DuckDuckGo HTML search
│   └── mcp.rs                   # MCP client (stdio JSON-RPC) + McpProxyTool
└── session/
    ├── mod.rs
    └── store.rs                 # JSONL session persistence + sliding window
```

Rules:
- **`mod.rs`** holds Trait definitions and the module's public interface.
- **Each Trait implementation occupies its own file** (e.g., `groq.rs` implements `SttProvider`).
- **`types.rs`** holds data structures for the module or shared across modules.
- Modules reference each other via traits; they do not depend directly on concrete implementations.

---

## 5. Async Rules

### Single-threaded runtime

```rust
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> { /* ... */ }
```

`current_thread` saves approximately 1 MB compared to the multi-threaded runtime. The gateway is I/O-bound; a single thread is sufficient.

### Prohibit blocking calls

The following operations are **prohibited** in async contexts:
- `std::thread::sleep()` — use `tokio::time::sleep()`
- `std::fs::read()` — use `tokio::fs::read()`
- `std::io::stdin().read_line()` — not applicable in this project

### Timeouts

Add timeouts to all external API calls:

```rust
tokio::time::timeout(Duration::from_secs(30), api_call()).await??;
```

---

## 6. Dependency Management

### Principle: minimal dependencies, hand-write everything

This project does **not** use:
- Web frameworks (no axum / actix-web / warp)
- ORMs (no database)
- Telegram frameworks (no teloxide / frankenstein)
- Additional logging libraries beyond the logging facade

All Telegram API, Claude API, and Home Assistant API calls are hand-written using `reqwest`. WebSocket uses `tokio-tungstenite`.

### Current dependency list

Non-core dependencies in use: `bytes` (byte handling for streaming responses), `futures-util` (SinkExt/StreamExt + BoxStream).

See `Cargo.toml`. New dependencies must be justified; prefer:
1. Zero or minimal transitive dependencies
2. `no_std` compatibility (where possible)
3. Widely used, actively maintained crates

---

## 7. Testing

### Unit tests

Place a `#[cfg(test)]` block at the bottom of each module file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_parse_telegram_update() {
        let json = r#"{"update_id": 1, ...}"#;
        let update: Update = serde_json::from_str(json).unwrap();
        assert_eq!(update.update_id, 1);
    }
}
```

`unwrap()` is permitted in test code.

### Integration tests

Placed in the `tests/integration/` directory, organized through the `tests/integration_tests.rs` entry file, using `wiremock` to mock external APIs:

```
tests/
├── integration_tests.rs          # Entry point (mod integration;)
├── integration/
│   ├── mod.rs                    # Module declarations
│   ├── mcp_client.rs             # MCP client e2e (mock Python server)
│   ├── mcp_timeout.rs            # MCP timeout tests
│   ├── http_api.rs               # HTTP API channel integration tests
│   ├── exec_skills.rs            # exec tool skills_dir PATH tests
│   ├── web_search.rs             # DuckDuckGo search wiremock tests
│   ├── edge_tts.rs               # Edge TTS real synthesis (Opus/MP3)
│   ├── google_stt.rs             # Google Cloud STT real speech recognition
│   ├── volcengine_stt.rs         # Volcengine STT real speech recognition
│   └── volcengine_tts.rs         # Volcengine TTS real synthesis
└── fixtures/
    ├── mock_mcp_server.py        # Mock MCP server (echo + add)
    ├── slow_mcp_server.py        # Slow MCP server (5s sleep)
    ├── test_speech.wav            # Test WAV audio file
    └── openai_compat/            # OpenAI API response fixture JSON
```

### Prohibit `#[ignore]`

**All tests must run; `#[ignore]` is prohibited.** Integration tests that require real API keys (Edge TTS, Google STT, Volcengine, etc.) receive credentials via `--env-file .env` at container runtime. When a key is missing, the test skips itself internally (`return`), but is not marked `#[ignore]`.

### Docker test workflow

```bash
# Build the test image (compile only, no run — build layer has no API keys)
docker build --target tester -t test .

# Run all tests (mount .env to inject API Keys)
docker run --rm --env-file .env test
```

The tester stage works in two steps: `docker build` compiles the test binary (cacheable); `docker run --env-file .env` runs the tests (with API keys). Expected result: **0 ignored, 0 failed**.

---

## 8. Logging

Use structured logging macros from the `tracing` crate:

```rust
use tracing::{info, warn, error, debug};

info!(chat_id = %msg.chat.id, "New message received");
warn!(api = "claude", status = %resp.status(), "API returned non-200");
error!(error = %e, "Telegram polling failed, retrying in 5s");
debug!(len = audio_buf.len(), "Audio buffer size");
```

Rules:
- `error!` — failures requiring attention (API unreachable, parse failure)
- `warn!` — recoverable anomalies (retries, fallbacks)
- `info!` — key business events (message received, reply sent, tool called)
- `debug!` — debugging details (buffer sizes, request/response bodies)
- Use structured fields (`key = value`); do not concatenate strings into message templates.
- Default level in production is `info`; adjust via the `RUST_LOG` environment variable.

---

## 9. Serde Serialization

### Config types: permissive mode + default values

All config structs in this project use `#[serde(default)]` and do **not** use `deny_unknown_fields`. Reasons:
- User config files may contain fields not yet implemented (forward compatibility)
- Default values allow users to configure only what they care about

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TtsConfig {
    pub auto: String,       // default: "inbound"
    pub provider: String,   // default: "edge"
    pub max_text_length: usize,
}
```

### External API responses: permissive mode

When parsing JSON from external APIs such as Telegram / Claude / Groq, also do **not** add `deny_unknown_fields`, allowing APIs to add new fields without causing errors. Define only the fields you need.

### JSON field naming

Structs that interact with JSON APIs uniformly use `rename_all`:

```rust
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeMessage {
    pub role: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
}
```

For `snake_case`-style APIs (e.g., Telegram), use `#[serde(rename_all = "snake_case")]` or keep field names consistent directly.

---

## 10. Binary Size Optimization

`Cargo.toml` is configured with release build optimizations:

```toml
[profile.release]
opt-level = "z"      # Optimize for size rather than speed
lto = true           # Link-time optimization, eliminates dead code
codegen-units = 1    # Single compilation unit, maximum optimization opportunity
panic = "abort"      # No unwind tables generated, reduces size
strip = true         # Strip debug symbols
```

This configuration does not need modification. Use `musl` static linking when cross-compiling for the target device:

```bash
cargo build --release --target x86_64-unknown-linux-musl
```

---

## 11. Reproducible Build Requirements

This project requires Docker build artifacts to be bit-for-bit identical given the same inputs. The following six measures must all be satisfied simultaneously:

### 11.1 Toolchain version pinning

`rust-toolchain.toml` must be pinned to a specific version number (e.g., `"1.84.0"`). Floating channels such as `"stable"` / `"nightly"` are prohibited.

### 11.2 Dependency version locking

`Cargo.lock` must be committed to the repository. Docker builds use the `--locked` flag to ensure dependency versions exactly match the lockfile:

```bash
cargo build --release --locked --target <triple>
```

After updating dependencies in local development, `Cargo.lock` changes must be committed at the same time.

### 11.3 Disable incremental compilation

Set `CARGO_INCREMENTAL=0` in the Docker build environment to eliminate timing differences from incremental compilation caches.

### 11.4 Zero-out timestamps

Set `SOURCE_DATE_EPOCH=0` to eliminate build timestamps embedded in binaries by the compiler.

### 11.5 Path remapping

Use `RUSTFLAGS` to set `--remap-path-prefix`, mapping build paths and user paths to fixed values to eliminate host path embedding:

```bash
RUSTFLAGS="--remap-path-prefix=/app=. --remap-path-prefix=/root=~"
```

### 11.6 Single compilation unit

`[profile.release]` in `Cargo.toml` already sets `codegen-units = 1` and `lto = true`, eliminating randomness from parallel compilation. This configuration must not be removed.

### 11.7 Verification method

Use `scripts/docker-build.sh --verify <target>` to build twice consecutively and compare SHA-256 hashes:

```bash
./scripts/docker-build.sh --verify x86_64
# Build 1: sha256=abc123...
# Build 2: sha256=abc123...
# PASS: Builds are bit-for-bit identical
```

---

## 12. Development Workflow

New features must follow this workflow, **executed strictly in order**:

### 12.1 Design first

Add the relevant sections to the corresponding sub-file in `docs/en/design/`:
- Architecture design (data structures, interface definitions)
- Execution flow (sequencing, state transitions)
- Memory overhead estimates
- Configuration changes

The design document is the sole basis for implementation. A design review must be completed before coding begins.

### 12.2 Code implementation

Implement according to the design document, following all coding conventions in this document (`rust-conventions.md`).

### 12.3 Test verification

Write `#[cfg(test)]` unit tests for each new module, covering:
- Happy path
- Edge cases (empty input, over-limit, non-existent resources)
- Serialization compatibility (old and new data formats)

### 12.4 Review and alignment

After implementation, verify item by item:
- Does the code fully cover all requirements in the design document?
- Does the design document accurately reflect the final implementation? (If there are discrepancies, update the document to match.)
- Does the coding conventions document need updating? (If new patterns were introduced.)
