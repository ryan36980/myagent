> [Design Docs](README.md) > Module Design & API Protocols

# Chapter 4: Detailed Module Design

## 4.1 config.rs — Configuration Loading

Reads `openclaw.json` (JSON5 format), performs `${VAR_NAME}` environment variable substitution, then deserializes into the `Config` struct. Processing pipeline:

```
Config file text → env var substitution → JSON5 parse → Config struct
```

All serde structs omit `deny_unknown_fields`; unknown fields are silently ignored to ensure forward compatibility. See the [operations chapter](operations.md) for the full Config definition.

## 4.2 error.rs — Unified Error Type

```rust
#[derive(thiserror::Error, Debug)]
pub enum GatewayError {
    #[error("Transport error: {0}")]   Transport(String),
    #[error("Config error: {0}")]      Config(String),
    #[error("Agent error: {0}")]       Agent(String),
    #[error("STT error: {0}")]         Stt(String),
    #[error("TTS error: {0}")]         Tts(String),
    #[error("Tool error: {0}")]        Tool(String),
    #[error("Session error: {0}")]     Session(String),
}
```

Implements `From` conversions for `reqwest::Error`, `serde_json::Error`, and `std::io::Error`, enabling automatic propagation via `?`.

## 4.3 provider/llm/claude.rs — Anthropic Claude

Implements the `LlmProvider` trait, calling the Anthropic Messages API v1.

```rust
pub struct ClaudeProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,             // defaults to "https://api.anthropic.com", overridable via ProviderConfig.base_url
    max_tokens: u32,              // defaults to 4096
    thinking_budget: Option<u32>, // Extended Thinking token budget
}
```

Request construction: POST `{base_url}/v1/messages`, headers include `x-api-key` and `anthropic-version`. Body includes `model`, `max_tokens`, `system`, `messages`, `tools`, and optionally `thinking`.

**Extended Thinking:** Configured via `agents.thinking` ("off"/"low"/"medium"/"high"), mapped to `budget_tokens` (2048/8192/32768). When enabled, API version is upgraded to `2025-04-14` and a `thinking: { type: "enabled", budget_tokens }` field is added to the request. `thinking` content blocks in responses are logged at debug level and discarded; `thinking_delta` events in the SSE stream are silently skipped.

**Streaming mode:** `chat_stream()` sends `"stream": true` and parses Anthropic SSE events (`content_block_delta` → TextDelta, `content_block_start/stop` → ToolUse accumulation, `message_delta` → Done, `thinking_delta` → discard). Line-level buffering + `futures_util::stream::unfold` constructs a `BoxStream`.

**Safe parsing of tool_use input:** When a `content_block_stop` event arrives, the accumulated `tool_input` string is parsed as `serde_json::Value`. For parameter-less tools (e.g. `get_time`, where input is `{}`), Anthropic SSE may not emit any `input_json_delta` events, leaving `tool_input` as an empty string. An empty string defaults to parsing `"{}"`, with a fallback of `Value::Object(Default::default())` rather than `Value::Null` — because the Anthropic API requires `tool_use.input` to be a dictionary; `null` causes a 400 Bad Request (`Input should be a valid dictionary`).

**tool_use handling:** When `stop_reason == "tool_use"`, the `content` array contains a `type: "tool_use"` block. The Agent loop extracts it, executes the tool, appends the result as a `tool_result`, and calls the LLM again.

## 4.4 provider/stt/groq.rs — Groq Whisper

Implements the `SttProvider` trait, uploading audio to the Groq OpenAI-compatible API via multipart/form-data.

```rust
pub struct GroqSttProvider { client: reqwest::Client, api_key: String, model: String }
```

Constructs a `multipart::Form` with: `file` (audio bytes), `model` ("whisper-large-v3-turbo"), `language` ("zh"). POSTs to `https://api.groq.com/openai/v1/audio/transcriptions` and parses the `text` field from the response JSON.

**Lazy initialization:** The `GROQ_API_KEY` environment variable is not required at startup; it uses `unwrap_or_default()` to allow an empty value. The actual check is deferred to when `transcribe()` is called — if `api_key` is empty, a clear error is returned. This allows deployments that don't need voice features to skip configuring a Groq API key.

### 4.4.2 provider/stt/volcengine.rs — Volcengine (Doubao)

Implements the `SttProvider` trait, connecting to the Volcengine BigModel speech recognition service (v3 bigmodel) via a WebSocket binary protocol.

```rust
pub struct VolcengineSttProvider { app_id: String, access_token: String, cluster: String, ws_url: String }
```

**Binary protocol (v3 bigmodel):** 4-byte header + 4-byte sequence (big-endian i32) + 4-byte payload_size (big-endian u32) + gzip(payload). The header encodes version, header_size, msg_type, msg_type_specific, serial_method, and compression_type.

**transcribe flow:**
1. Connect via WebSocket to `wss://openspeech.bytedance.com/api/v3/sauc/bigmodel` with auth headers (`X-Api-App-Key`, `X-Api-Access-Key`, `X-Api-Resource-Id`, `X-Api-Connect-Id`)
2. Send full_client_request (gzip-compressed JSON metadata: audio_params, user) with sequence 1
3. Send audio data frame (not last frame), sequence 2
4. Send empty frame (is_last=true, sequence negated → -3) to signal end of audio to server
5. Read binary response frames from server, parse gzip-decompressed JSON, extract `result.text` or `result.utterances[].text`
6. Close connection

**WAV handling:** Automatically detects WAV files and strips the file header, sending raw PCM. Supports non-standard fmt chunks (e.g. the 18-byte fmt generated by Windows TTS).

**Dependencies:** Reuses the project's existing `tokio-tungstenite` (WebSocket) and `flate2` (gzip compression/decompression).

### 4.4.3 provider/stt/google.rs — Google Cloud Speech-to-Text

Implements the `SttProvider` trait, transcribing audio via the Google Cloud Speech-to-Text REST API v1 (synchronous recognition).

```rust
pub struct GoogleSttProvider { client: reqwest::Client, api_key: String, language_code: String }
```

**transcribe flow:**
1. Automatically selects encoding format (`OGG_OPUS` / `LINEAR16` / `MP3` / `FLAC`) and sample rate based on MIME type.
2. Audio is Base64-encoded and sent via `POST speech.googleapis.com/v1/speech:recognize?key={api_key}`.
3. Parses the response JSON and extracts `results[0].alternatives[0].transcript`.

**Configuration:** `provider: "google"` + `apiKey` (or `GOOGLE_STT_API_KEY` env var) + optional `google.languageCode` (defaults to `"zh-CN"`).

**Dependencies:** Reuses existing `reqwest` (HTTP) and `base64` (encoding).

## 4.5 provider/tts/ — TTS Providers

Three TTS providers are supported, selected via `messages.tts.provider`.

### 4.5.1 Edge TTS (default, free)

Implements the `TtsProvider` trait, synthesizing speech via the Microsoft Edge TTS WebSocket protocol.

```rust
pub struct EdgeTtsProvider { voice: String, rate: String, pitch: String, volume: String, chromium_version: String }
```

**DRM authentication (required since late 2024):**

Microsoft added DRM verification to the Edge TTS WebSocket endpoint in late 2024; requests without the correct token return 403.

- **Sec-MS-GEC token**: A time-based SHA-256 hash. Algorithm: current time → convert to Windows FILETIME epoch → floor to 5-minute boundary → convert to 100-nanosecond ticks → concatenate `TrustedClientToken` → SHA-256 → uppercase hex. The token is identical within the same 5-minute window.
- **MUID cookie**: A random 32-character uppercase hex string (UUID v4 with hyphens removed).
- **Sec-MS-GEC-Version**: Format `1-{chromium_version}`, e.g. `1-143.0.3650.75`.
- **Required HTTP headers**: Pragma, Cache-Control, Accept-Encoding, Accept-Language, Origin (chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold), User-Agent (major version only), Cookie (muid).
- **chromium_version is configurable**: Override via `edge.chromiumVersion`; when Edge TTS returns 403, updating the version number requires no code changes. Default: `143.0.3650.75`.

**Protocol flow:**
1. Build WebSocket URL: `wss://speech.platform.bing.com/.../edge/v1?TrustedClientToken=...&ConnectionId={uuid}&Sec-MS-GEC={token}&Sec-MS-GEC-Version={version}`.
2. Build request using `IntoClientRequest` and inject DRM headers.
3. Send `speech.config` text frame (sets output format to `audio-24khz-48kbitrate-mono-mp3`).
4. Send SSML text frame (includes voice name, prosody rate, and the text to synthesize).
5. Receive mixed frames: text frames contain `turn.start`/`turn.end`; binary frames contain a header (ending with `Path:audio\r\n`) followed by raw MP3 audio data.
6. Accumulate all audio chunks into a pre-allocated `Vec<u8>`, return after receiving `turn.end`.

A new connection is created per request and closed after synthesis. Overhead is acceptable for short phrases (10–50 characters).

### 4.5.2 OpenAI TTS (paid, high quality)

Implements the `TtsProvider` trait, synthesizing speech via the OpenAI Audio Speech API. Compatible with all OpenAI-compatible TTS endpoints.

```rust
pub struct OpenAiTtsProvider { client: reqwest::Client, base_url: String, api_key: String, model: String, voice: String }
```

**API protocol:**
```
POST {base_url}/audio/speech
Authorization: Bearer {api_key}
Content-Type: application/json

{ "model": "tts-1", "input": "text", "voice": "alloy", "response_format": "opus" }

← 200 OK, body = raw OGG/Opus audio bytes
```

Configure `base_url` to point to any OpenAI-compatible TTS service. Default: `https://api.openai.com/v1`.

### 4.5.3 ElevenLabs TTS (paid, most natural)

Implements the `TtsProvider` trait, synthesizing speech via the ElevenLabs Text-to-Speech API.

```rust
pub struct ElevenLabsTtsProvider { client: reqwest::Client, base_url: String, api_key: String, model_id: String, voice_id: String }
```

**API protocol:**
```
POST {base_url}/text-to-speech/{voice_id}
xi-api-key: {api_key}
Content-Type: application/json

{ "text": "text", "model_id": "eleven_multilingual_v2" }

← 200 OK, body = raw MP3 audio bytes
```

### 4.5.4 Volcengine TTS (Doubao, high-quality Chinese)

Implements the `TtsProvider` trait, synthesizing speech via the Volcengine Text-to-Speech HTTP REST API. Natively supports `ogg_opus` output.

```rust
pub struct VolcengineTtsProvider { client: reqwest::Client, base_url: String, app_id: String, access_token: String, cluster: String, voice_type: String, speed_ratio: f32, volume_ratio: f32, pitch_ratio: f32 }
```

**API protocol:**
```
POST {base_url}/api/v1/tts
Authorization: Bearer;{access_token}    ← note semicolon, not space
Content-Type: application/json

{
  "app": { "appid": "xxx", "token": "access_token", "cluster": "volcano_tts" },
  "user": { "uid": "openclaw" },
  "audio": { "voice_type": "zh_female_vv_uranus_bigtts", "encoding": "ogg_opus", "speed_ratio": 1.0, ... },
  "request": { "reqid": "uuid", "text": "text content", "text_type": "plain", "operation": "query" }
}

← 200 OK, JSON: { "code": 3000, "data": "<base64 OGG/Opus>" }
```

### 4.5.5 TTS Provider Selection

Dispatched in `main.rs` based on `config.messages.tts.provider`:

| provider value | Implementation | Native format | Output format | Dependency |
|----------------|----------------|---------------|---------------|------------|
| `"edge"` (default) | `EdgeTtsProvider` | WebM Opus | OGG/Opus ★ | No API key |
| `"volcengine"` | `VolcengineTtsProvider` | OGG/Opus | OGG/Opus | `VOLCENGINE_ACCESS_TOKEN` |
| `"openai"` | `OpenAiTtsProvider` | OGG/Opus | OGG/Opus | `OPENAI_API_KEY` |
| `"elevenlabs"` | `ElevenLabsTtsProvider` | MP3 | MP3 | `ELEVENLABS_API_KEY` |

★ Edge TTS internally calls `webm_opus_to_ogg_opus()` to convert the WebM container to an OGG container (Opus encoding is unchanged; only the container is swapped).

**Audio format unification strategy:**
- **Unified OGG/Opus output** — all channels (Telegram, Feishu) and all TTS providers use this format
- The free Edge TTS endpoint (Bing consumer) does not support `ogg-*-opus` formats; it only supports `webm-24khz-16bit-mono-opus`. Container conversion is handled inside the provider via the `webm_to_ogg.rs` module (~200 lines, no new dependencies: EBML parsing + OGG encapsulation + CRC-32)
- Telegram `sendVoice` accepts OGG/Opus and MP3; Feishu `msg_type: "audio"` only accepts OGG/Opus (`file_type: "opus"`)
- Responsibility for format conversion lies within the TTS provider; the Channel layer is unaware of it

### 4.5.6 TTS auto Mode

`messages.tts.auto` supports four modes:

| Mode | Behavior |
|------|----------|
| `"inbound"` (default) | Reply with voice when the user sends voice |
| `"always"` | Attach voice to every reply |
| `"tagged"` | Agent marks content for voice using `<speak>text</speak>` |
| Any other value | No voice synthesis (text only) |

**tagged mode implementation:** After receiving the LLM reply, `handle()` checks for `<speak>...</speak>` tags. If present, the text inside the tags is extracted for TTS synthesis and the tags are removed from the displayed text. If no tags are present, no synthesis occurs.

## 4.6 agent/react_loop.rs — ReAct Loop

```
User input → update ChatContext → load session → auto-compact (maybe_compact) → load memory context
→ assemble system prompt → init LoopDetector → set deadline (now + agent_timeout_secs)
→ loop (until deadline or LLM completion):
  ├─ check deadline: timeout → break, return "Processing timed out, please retry"
  ├─ call LLM (chat_stream → accumulate events → LlmResponse)
  │   └─ context overflow → maybe_compact_emergency() → retry
  ├─ EndTurn → apply_response_prefix() → extract text, save session, return
  ├─ ToolUse → loop_detector.record() → execute tool → append tool_result → continue loop
  │   ├─ Warning → append warning to tool_result
  │   └─ Block → terminate loop, request final response
  └─ Other → break
```

**Agent timeout (aligned with original OpenClaw):** No fixed iteration count limit; uses a time-based timeout instead. `agent_timeout_secs` (default 600s / 10 minutes) is checked at the start of each iteration via `tokio::time::Instant`. The LLM decides when to stop (model-driven loop); the timeout serves only as a safety net. Aligned with the original OpenClaw `agents.defaults.timeoutSeconds: 600`.

**Abort signal:** `react_loop()` accepts an `abort: &Arc<AtomicBool>` parameter, checked at the start of each iteration and before each tool execution. When abort is triggered, the current session is saved and `"Operation cancelled."` is returned.

**Tool loop detection (LoopDetector):** A VecDeque<30> sliding window stores `ToolCallRecord` entries, each containing a `fingerprint` (`"tool_name:input_hash_hex"`, ~30B) and `result_hash` (`Option<u64>`, 8B). Two-phase recording: `record_input()` is called before tool execution; `record_outcome()` records the result hash after execution.

4 progress-aware detection modes (aligned with OpenClaw `tool-loop-detection.ts`):
- **generic_repeat**: consecutive calls to the same tool with identical input and result
- **ping_pong**: A→B→A→B alternating pattern, with no-progress evidence check
- **circuit_breaker**: same tool no-progress streak (consecutive identical results), not just call count
- **global_circuit_breaker**: total tool call count reaches 30 and `global_no_progress_streak()` shows no progress. The `total_tool_calls: usize` field counts all calls.

Thresholds: WARNING=10, BLOCK=20, global circuit breaker=30, but triggered only when results show no change (no-progress). For example, an `exec` tool called 20 times with different results each time (script iterating) will not be blocked; it is only blocked when 20 consecutive calls return exactly the same result. Fingerprints use `DefaultHasher` (non-cryptographic, well-distributed) to hash tool inputs as u64, avoiding storing large JSON blobs. ~1.2KB/request (30 records × ~40B), a 96% reduction from the old version (up to 30KB).

**Transient error retry:** When `consume_stream_live()` in `react_loop()` returns an error, `is_transient_error()` is checked — it matches transport errors, connection errors, timed out, overloaded, and HTTP 500/502/503/521/522/523/524/529. On a match, it waits 2.5s and retries once (one retry only), rebuilding the `StreamingWriter`. Non-transient errors are still propagated immediately.

**Tool result pruning (prune_tool_results):** Called at the start of each `react_loop` main loop iteration. The most recent 3 assistant messages' tool_results are retained without pruning; earlier tool_results exceeding 4000 characters are soft-trimmed to head 1500 + tail 1500 + a truncation notice in between. Reduces context usage in long sessions.

**Tool output truncation:** Tool execution results exceeding 400,000 characters are automatically truncated (preferring a newline break point), with a truncation notice suffix appended. Prevents large outputs from causing context overflow.

**Auto-compaction (maybe_compact):** Triggered when `auto_compact == true` and `messages.len() > sessions.history_limit() * 75%`:
1. Uses `compact_ratio` (default 0.4) to determine the retention fraction, discarding the oldest 60% of messages
2. Constructs a summary request for the discarded portion and sends it to the LLM ("Summarize key facts, decisions, user preferences. Max 200 words.")
3. Appends the summary result to the memory log (`[auto-compact] ...`)
4. Called at the start of react_loop, before building the system prompt

**Emergency compaction (maybe_compact_emergency):** Triggered after catching a context overflow error from the LLM, with progressive fallback:
1. Retain 40% + summary
2. Retain 20% + summary
3. Retain only the last 2 messages, no summary (last resort)

**Response Prefix:** The `messages.responsePrefix` template is applied before react_loop returns text, supporting `{model}`, `{provider}`, and `{thinkingLevel}` variable substitution.

**Streaming LLM calls and live preview:** `react_loop()` calls `llm.chat_stream()` via `consume_stream_live()`, simultaneously pushing TextDeltas to a `StreamingWriter` for a typewriter effect:

```rust
async fn consume_stream_live(
    &self, system, messages, tools,
    writer: &mut Option<StreamingWriter>,  // None = no streaming
    abort: &Arc<AtomicBool>,
) -> Result<LlmResponse>;
```

`StreamingWriter` (`src/channel/streaming.rs`, ~110 lines) handles throttled message editing:
- First push: waits until buffer reaches `min_initial_chars` (20 characters), then `send_text` obtains a msg_id
- Subsequent pushes: if ≥ `throttle_ms` (1000ms) have elapsed since the last edit, calls `edit_message`; otherwise only buffers
- `finish()`: unconditional final edit, flushes buffer
- `stop()` (async): stops streaming on ToolUse, **flushes the buffer first** (sends or edits any remaining text) then sets stopped=true. Note: flushing is required because a new writer is created after ToolUse; unflushed buffer content would be lost
- On EndTurn: calls `finish()`, using the `\x00STREAMED\x00` marker mechanism to preserve text for TTS
- On ToolUse: calls `stop().await`; a new writer is created for the next LLM call after tool execution
- `react_loop` maintains the `any_text_streamed` cross-iteration flag to track whether any text was streamed to the user

**Coexistence of streaming output and TTS:** In streaming mode, text has already been sent to the user via `edit_message`, but TTS still requires the original text for speech synthesis. `react_loop()` returns `"\x00STREAMED\x00{reply_text}"` rather than an empty string when streaming completes. `handle()` parses this marker: when `already_streamed=true`, text sending is skipped (already displayed), but `reply_text` is still passed to `try_synthesize()` for voice generation.

The 1000ms throttle aligns with the Telegram editMessage API rate limit. The 4096-character cap aligns with Telegram's message length limit. Aligned with the original OpenClaw `draft-stream-loop.ts`. ~4.2KB peak (buffer), 0 when inactive.

**Memory injection:** `handle()` first updates `chat_context` (channel + chat_id), then `react_loop()` calls `memory.build_context()` to obtain the adaptive memory context, which is injected into the `## Memory` section of the system prompt.

**Compaction hint:** When `messages.len() > 15`, a `COMPACTION_WARNING` is appended to the end of the system prompt, reminding the agent to save important information to memory.

During the loop, assistant messages (including tool_use blocks) and tool_results are appended to the session history in alternation. After the loop ends, the session is persisted to SessionStore.

## 4.7 tools/ — Tool Modules

**ha_control:** Home Assistant REST API control. Supports two operations: `call_service` (POST `/api/services/{domain}/{service}`) and `get_state` (GET `/api/states/{entity_id}`). 30s per-request timeout. Input includes `action`, `domain`, `service`, `entity_id`, and `data` fields.

**web_fetch:** HTTP GET for a specified URL. HTML pages are automatically converted to plain text (script/style/tags stripped, entities decoded, whitespace collapsed); JSON/XML/plain text API responses are not transformed. Returns the first 128K characters by default; supports `offset` + `max_chars` pagination for reading very long pages. 2MB download size limit (configurable via `webFetch.maxDownloadBytes`). 30s per-request timeout (overrides the Client-level 600s). Built-in SSRF protection: `validate_url_ssrf()` validates the URL scheme (http/https only) before the request, then resolves DNS and checks all IP addresses against private/reserved ranges (127.x, 10.x, 172.16–31.x, 192.168.x, 169.254.x, ::1, fe80::/10, fc00::/7, IPv4-mapped v6).

**get_time:** Returns the current time using `chrono::Local::now()`, formatted as `"2025-01-15 14:30:00 (Wednesday)"`.

**cron:** Manages scheduled tasks (add/remove/list). A CronTask struct contains `id`, `cron_expr`, `description`, `command`, `channel`, `chat_id`, `created_at`, `last_run`, `schedule_at`, `delete_after_run`, `delivery_mode`, `webhook_url`, and `isolated`. `channel` and `chat_id` are automatically injected from `Arc<Mutex<ChatContext>>`. Persisted to `{session_dir}/cron.json`. Supports one-shot scheduling (`schedule_at`), auto-deletion (`delete_after_run`), webhook delivery (`delivery_mode`), and isolated execution (`isolated`). See the [subsystems chapter](subsystems.md) for execution mechanics.

**memory:** Long-term memory management tool with 6 actions: `read` (read MEMORY.md), `append` (append to MEMORY.md), `rewrite` (rewrite and reorganize MEMORY.md), `read_log` (read a daily log, defaults to today), `append_log` (append to today's log), `search` (substring search across all memory files). See the [subsystems chapter](subsystems.md) for detailed design.

**file (file operations):** 4 zero-size structs (`FileReadTool`, `FileWriteTool`, `FileEditTool`, `FileFindTool`), requiring no config and zero resident memory. `file_read` reads a file and returns cat -n style line-numbered text, with offset/limit pagination, binary detection, and a 64KB truncation limit. `file_write` writes a file and automatically creates parent directories. `file_edit` performs exact string replacement, single replacement by default, and reports an error with the match count if multiple occurrences exist. `file_find` BFS-traverses a directory recursively, supporting filename substring matching and file content search (skipping binaries), with a maximum of 50 results. The security model is identical to `exec` — no path restrictions; enabled via `tools.allow`.

**Extension tools:** Beyond the built-in tools above, `exec` (shell command execution), `web_search` (DuckDuckGo search), and the MCP client are implemented as configurable extension capabilities. See the [subsystems chapter](subsystems.md) for details.

## 4.8 session/store.rs — Session Storage

Each chat corresponds to a JSONL file (`{chat_id}.jsonl`), with one `ChatMessage` JSON object per line.

```rust
pub struct SessionStore { dir: PathBuf, history_limit: usize, dm_history_limit: usize }
```

- **Turn-based limiting (`dm_history_limit`)**: Aligned with the original OpenClaw `dmHistoryLimit`. Counts by user text turns (User messages containing a `Text` block); User messages with only `tool_result` are not counted. Default: 20 turns, 0 = unlimited. Applied in both `load()` and `save()`, before `history_limit`.
- `load(chat_id)`: Reads the file and parses it line by line into a `Vec<ChatMessage>`. First applies `limit_history_turns()` to trim by user turn count, then applies the `history_limit` raw message count sliding window truncation (default 0 = unlimited).
- `save(chat_id, messages)`: Likewise applies `limit_history_turns()` first, then sliding window truncation, then overwrites the file.
- `sanitize_after_truncation(messages)`: Cleans up orphaned `tool_result` messages after truncation/compaction. The sliding window or compaction may truncate between an assistant `tool_use` and its corresponding user `tool_result`, leaving a `tool_result` that references a nonexistent `tool_use_id`, which causes the Anthropic API to return 400. This function scans from the beginning and skips any leading orphaned assistant/tool_result messages until it finds a User message containing `Text` as a valid starting point. Called at all four truncation points: `load()`, `save()`, `do_compact()`, and `maybe_compact_emergency()`.

## 4.9 HTTP Timeout Architecture

Shares a single `reqwest::Client` (connection pool reuse), with two-level timeouts separating different scenarios:

| Level | Timeout | Scope | Mechanism |
|-------|---------|-------|-----------|
| Client-level | 600s | LLM API calls (may involve multiple tool use rounds + thinking) | `Client::builder().timeout(600s)` |
| Request-level | 30s | Web tools (web_fetch, web_search, ha_control) | `RequestBuilder::timeout(30s)` overrides Client default |
| Webhook delivery | 5s | Cron webhooks | Existing per-request timeout |

`reqwest::RequestBuilder::timeout()` overrides the `Client`-level timeout. LLM calls (Claude/OpenAI-compatible) inherit the Client default of 600s, sufficient for Extended Thinking + multi-tool iterations. Web tools have an independent 30s timeout to prevent a single slow request from blocking the entire agent turn.

The original OpenClaw agent timeout is 600s. Rust Light defaults to 900s (15 minutes), providing a more generous execution window for complex tasks (e.g. multi-page PPT generation). Web tool timeout remains 30s.

## 4.10 Model Failover

`FailoverLlmProvider` (`src/provider/llm/failover.rs`) automatically switches to a backup model when the primary model fails, aligned with the original OpenClaw `failover-error.ts` + `auth-profiles/usage.ts`.

**Architecture:**

```rust
pub struct FailoverLlmProvider {
    providers: Vec<Box<dyn LlmProvider>>,   // primary + fallbacks, ordered by priority
    cooldowns: Mutex<Vec<ProviderCooldown>>, // cooldown state per provider
}
```

`chat()` and `chat_stream()` try each provider in order, skipping providers that are in cooldown. A successful call resets the error_count; a failed call sets a cooldown and continues to the next provider.

**Failover conditions** (`is_failover_eligible()`):

| Error type | Failover? | Notes |
|------------|-----------|-------|
| 429 / rate_limit | Yes | Rate limited |
| 402 / billing | Yes | Billing issue |
| 401 / 403 | Yes | Auth error (expired token, etc.) |
| timeout / connection | Yes | Network issue |
| 500 / 502 / 503 | Yes | Server-side error |
| context overflow | **No** | All providers would encounter this; no failover |
| abort | **No** | User-initiated abort |
| format error | **No** | Request format issue; switching provider is pointless |

**Exponential backoff cooldown:**

| Consecutive failures | Cooldown | Reference |
|----------------------|----------|-----------|
| 1 | 60s | OpenClaw `5^0` = 1 minute |
| 2 | 300s | OpenClaw `5^1` = 5 minutes |
| 3 | 1500s | OpenClaw `5^2` = 25 minutes |
| ≥4 | 3600s (cap) | 1-hour ceiling |

During cooldown, the provider is skipped without sending any requests. Automatically recovers after expiry (simplified: no active probing).

**Configuration** (`agents.fallbackModels`):

```json5
"agents": {
    "fallbackModels": [
        { "provider": "openai", "model": "gpt-4o", "apiKeyEnv": "OPENAI_API_KEY" },
        { "provider": "groq", "model": "llama-3.3-70b-versatile",
          "apiKeyEnv": "GROQ_API_KEY", "baseUrl": "https://api.groq.com/openai/v1" }
    ]
}
```

**Build logic** (`src/main.rs`): Fallback providers are constructed using `OpenAiCompatProvider` (all non-Anthropic providers use the OpenAI-compatible protocol). If `fallbackModels` is non-empty, the primary provider + fallback list are wrapped in a `FailoverLlmProvider`; otherwise the primary provider is used directly. All providers share the same `reqwest::Client` (connection pool reuse); each fallback uses ~200B resident memory.

---

# Chapter 5: API Protocol Specifications

## 5.1 Telegram Bot API

**getUpdates (long polling):**

```
GET https://api.telegram.org/bot{token}/getUpdates?offset={N}&timeout=30&allowed_updates=["message"]
```

```json
{ "ok": true, "result": [
    { "update_id": 123456789, "message": {
        "message_id": 100,
        "from": { "id": 111222333 },
        "chat": { "id": 111222333, "type": "private" },
        "date": 1705300000,
        "text": "Search today's news for me"
    }}
]}
```

**sendMessage:** `POST .../sendMessage`, body `{ "chat_id": N, "text": "..." }`.

**sendVoice:** `POST .../sendVoice`, multipart: `chat_id` + `voice` (binary audio file). Format is detected via audio magic bytes: OGG (`OggS`) → `audio/ogg` + `voice.ogg`, MP3 (`0xFF` or `ID3`) → `audio/mpeg` + `voice.mp3`, unknown → default `audio/ogg`.

**getFile:** `GET .../getFile?file_id=...` retrieves the `file_path`; download URL is `https://api.telegram.org/file/bot{token}/{file_path}`.

## 5.2 Anthropic Messages API v1

```
POST https://api.anthropic.com/v1/messages
Headers: x-api-key, anthropic-version: 2023-06-01, Content-Type: application/json
Body: { model, max_tokens, system, messages: [{ role, content: [ContentBlock] }], tools: [{ name, description, input_schema }] }
```

When the response has `stop_reason: "tool_use"`, `content` contains `{ type: "tool_use", id, name, input }`; when `stop_reason: "end_turn"`, it contains `{ type: "text", text }`. The `usage` field reports token consumption.

## 5.3 Groq Whisper API

```
POST https://api.groq.com/openai/v1/audio/transcriptions
Authorization: Bearer gsk_xxxxx
Content-Type: multipart/form-data

file: (binary, "voice.ogg", audio/ogg)
model: "whisper-large-v3-turbo"
language: "zh"
```

Response: `{ "text": "Search today's news for me" }`

### 5.3.2 Volcengine BigModel ASR WebSocket

```
wss://openspeech.bytedance.com/api/v3/sauc/bigmodel
X-Api-App-Key: {app_id}
X-Api-Access-Key: {access_token}
X-Api-Resource-Id: {cluster}
X-Api-Connect-Id: {uuid}
```

**Binary frame format (big-endian):**
```
[4B header][4B sequence (i32)][4B payload_size (u32)][payload (gzip)]
```

Header byte encoding: `[version:4|header_size:4] [msg_type:4|msg_type_specific:4] [serial_method:4|compression:4] [reserved]`

**Message sequence:**
1. Full client request (seq=1): gzip(JSON metadata, including audio_params, user)
2. Audio data (seq=2, not last): gzip(PCM/OGG audio bytes)
3. Finish frame (seq=-3, last): gzip(empty bytes); negated sequence number indicates the last frame

**Response format (v3 bigmodel):**
```json
{
  "result": {
    "text": "The weather is nice today",
    "utterances": [{ "text": "The weather is nice today", "definite": true }]
  },
  "addition": { "duration": "3200", "logid": "..." }
}
```

## 5.4 Edge TTS WebSocket

**Connection:** `wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1?TrustedClientToken=6A5AA1D4EAFF4E9FB37E23D68491D6F4&ConnectionId={uuid}`

**Headers:** `Origin: chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold`

**Send config frame (text):** `Path:speech.config`, body sets `outputFormat: "ogg-24khz-16bit-mono-opus"`.

**Send SSML frame (text):** `Path:ssml`, body is standard SSML, containing `<voice name='zh-CN-XiaoxiaoNeural'>` and `<prosody rate='+10%'>` wrapping the text to synthesize.

**Receive audio frames (binary):** Frames contain a text header (ending with `Path:audio\r\n`), followed by OGG/Opus audio bytes. The text frame `Path:turn.end` signals synthesis completion.

## 5.5 OpenAI Audio Speech API

```
POST https://api.openai.com/v1/audio/speech
Authorization: Bearer {api_key}
Content-Type: application/json

{ "model": "tts-1", "input": "Hello world", "voice": "alloy", "response_format": "opus" }

← 200 OK, Content-Type: audio/ogg, body = raw OGG/Opus audio bytes
```

Configure `base_url` to point to any OpenAI-compatible TTS endpoint.

## 5.6 ElevenLabs Text-to-Speech API

```
POST https://api.elevenlabs.io/v1/text-to-speech/{voice_id}
xi-api-key: {api_key}
Content-Type: application/json

{ "text": "Hello world", "model_id": "eleven_multilingual_v2" }

← 200 OK, Content-Type: audio/mpeg, body = raw MP3 audio bytes
```

## 5.7 Home Assistant REST API

**Call service:**
```
POST http://{ha_url}/api/services/{domain}/{service}
Authorization: Bearer {ha_token}
Content-Type: application/json

{ "entity_id": "light.living_room", "brightness": 255 }
```

**Query state:**
```
GET http://{ha_url}/api/states/{entity_id}
Authorization: Bearer {ha_token}

Response: { "entity_id": "sensor.temperature", "state": "23.5", "attributes": { "unit_of_measurement": "°C", "friendly_name": "Temperature sensor" } }
```
