> [Design Docs](README.md) > Operations: Memory, Config, Build, Test

<a id="chapter-6"></a>
## Chapter 6: Memory Budget

### 6.1 Component-Level Allocation

| Component | Allocation | Notes |
|-----------|------------|-------|
| Binary + static data | ~2-4MB | release + strip, includes rustls certificate bundle |
| tokio runtime | ~1-2MB | current_thread mode |
| reqwest + rustls | ~1MB | connection pool + TLS session cache |
| Audio buffer (pre-allocated) | ~256KB | 60s Opus@32kbps ≈ 240KB |
| Session context | ~64KB | 20 ChatMessages, ~3KB each |
| MemoryStore + ChatContext | ~200B | PathBuf + 2×usize + Arc<Mutex<ChatContext>> resident |
| ChatQueueManager | ~64B | HashMap resident + ~860B per active chat |
| FailoverLlmProvider | ~430B | 2 fallback providers + cooldown state |
| LoopDetector | ~1.2KB/request | 30 ToolCallRecords × ~40B (hash fingerprint) |
| StreamingWriter | ~4.2KB/active stream | buffer + struct, 0 when inactive |
| Serde buffer | ~32KB | JSON serialization/deserialization temporary buffer |
| **Rust process total** | **~4-8MB** | |

### 6.2 Peak Scenarios

| Scenario | Extra Memory | Notes |
|----------|-------------|-------|
| 60s audio download | +256KB | Dropped after passing to STT |
| Claude API response | +32KB | Raw JSON freed after parsing |
| Edge TTS audio receive | +256KB | Cleared after sending |
| Groq STT upload | +256KB | Copied during multipart construction, freed after upload |
| Volcengine STT | +256KB | gzip compress/decompress + WebSocket frames, freed on completion |
| Memory injection / search | +8KB | build_context ≤max_context_bytes, search reads files one at a time |
| **Maximum simultaneous peak** | **+512KB** | Download and synthesis do not occur simultaneously |

### 6.3 System Total Budget

| Layer | Memory |
|-------|--------|
| Linux kernel + base services | ~15-20MB |
| Rust gateway (resident / peak) | ~4-8MB / ~5-9MB |
| **System total** | **~20-28MB** |
| **Remaining on 200MB device** | **~171-180MB** |

### 6.4 Comparison with Node.js

| Metric | Node.js | Rust | Savings |
|--------|---------|------|---------|
| Resident memory | 95-125MB | 4-8MB | ~92% |
| Peak memory | 155-165MB | 5-9MB | ~95% |
| Binary size | ~200MB | ~3MB | ~98% |

---

<a id="chapter-7"></a>
## Chapter 7: Configuration System

### 7.1 Format and Loading

Uses **JSON5** format (`json5` crate), supporting `//` comments and trailing commas. The config file path defaults to `openclaw.json` and can be overridden with `--config`.

Loading pipeline: `file text → ${VAR_NAME} environment variable substitution → JSON5 parse → Config struct`.

If an environment variable does not exist, it is substituted with an empty string and `tracing::warn!` emits a warning.

### 7.2 Config Struct Definitions

```rust
pub struct GatewayConfig {
    pub provider: String,                           // "anthropic" / "groq" / "deepseek" / "openai"
    pub model: String,                              // "claude-sonnet-4-5-20250929"
    pub provider_config_override: ProviderConfigOverride, // base_url / api_key_env / max_tokens overrides
    pub channels: ChannelsConfig,
    pub messages: MessagesConfig,                   // → tts, media_understanding
    pub tools: ToolsConfig,                         // → allow: Vec<String>
    pub home_assistant: HomeAssistantConfig,         // → url, token
    pub agents: AgentConfig,                        // → system_prompt, agent_timeout_secs(900), auto_compact(true), thinking("off"), compact_ratio(0.4), followup_debounce_ms(2000)
    pub session: SessionConfig,                     // → dir("./sessions"), history_limit(0=unlimited), dm_history_limit(20)
    pub memory: MemoryConfig,                       // → dir, max_memory_bytes(4096), max_context_bytes(4096)
    pub exec: ExecConfig,                           // → timeout_secs(30), skills_dir("./skills")
    pub web_search: WebSearchConfig,                // → provider("duckduckgo"), api_key_env, max_results(5)
    pub auth: AuthConfig,                            // → mode("api_key"), client_id, token_file
    pub backup: BackupConfig,                        // → enabled(true), dir("./backups"), interval_hours(24), retention_days(7), max_size_mb(200)
    pub mcp: McpConfig,                             // → servers: HashMap<String, McpServerConfig>
}

pub struct ProviderConfig {                         // Assembled at runtime (derived from provider + model + override)
    pub api_key: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub base_url: Option<String>,
}

pub struct ProviderConfigOverride {                 // Optional user overrides
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub max_tokens: Option<u32>,
}

pub struct ChannelsConfig {
    pub telegram: TelegramConfig,
    pub feishu: FeishuConfig,                       // WebSocket long connection
    pub http_api: HttpApiConfig,                    // enabled: bool, listen: "127.0.0.1:8080"
    pub cli: CliConfig,                             // enabled: bool
}

pub struct TelegramConfig { pub bot_token: String, pub allowed_users: Vec<i64> }
pub struct FeishuConfig { pub app_id: String, pub app_secret: String, pub domain: String, pub allowed_users: Vec<String> }
pub struct HttpApiConfig { pub enabled: bool, pub listen: String }
pub struct CliConfig { pub enabled: bool }
pub struct TtsConfig { pub auto: String, pub provider: String, pub max_text_length: usize, pub edge: Option<EdgeTtsConfig>, pub openai: Option<OpenAiTtsConfig>, pub elevenlabs: Option<ElevenLabsTtsConfig> }
pub struct EdgeTtsConfig { pub voice: String, pub rate: Option<String>, pub pitch: Option<String>, pub volume: Option<String> }
pub struct AudioConfig { pub provider: String, pub model: String }
pub struct OpenAiTtsConfig { pub base_url: Option<String>, pub api_key_env: String, pub model: String, pub voice: String }
pub struct ElevenLabsTtsConfig { pub base_url: Option<String>, pub api_key_env: String, pub model_id: String, pub voice_id: String }
pub struct MemoryConfig { pub dir: String, pub max_memory_bytes: usize, pub max_context_bytes: usize }
pub struct AgentConfig { pub system_prompt: String, pub agent_timeout_secs: u64, pub auto_compact: bool, pub thinking: String, pub compact_ratio: f64, pub fallback_models: Vec<FallbackModel>, pub followup_debounce_ms: u64, pub context_files: Vec<String>, pub queue_mode: String }
pub struct FallbackModel { pub provider: String, pub model: String, pub api_key_env: Option<String>, pub base_url: Option<String> }
pub struct WebSearchConfig { pub provider: String, pub api_key_env: String, pub max_results: usize }
pub struct ExecConfig { pub timeout_secs: u64, pub max_output_bytes: usize, pub work_dir: String, pub skills_dir: String }
pub struct McpConfig { pub servers: HashMap<String, McpServerConfig> }
pub struct AuthConfig { pub mode: String, pub client_id: String, pub token_file: String }
pub struct BackupConfig { pub enabled: bool, pub dir: String, pub interval_hours: u64, pub retention_days: u64, pub max_size_mb: u64 }
pub struct McpServerConfig { pub command: String, pub args: Vec<String>, pub env: HashMap<String, String>, pub timeout_secs: u64, pub max_output_bytes: usize }
```

All structs use `#[serde(rename_all = "camelCase")]`. `deny_unknown_fields` is omitted to silently ignore unknown fields and ensure forward compatibility. Fields with default values are annotated with `#[serde(default = "...")]`.

### 7.3 Multi-Channel Configuration

Each channel in `ChannelsConfig` is `Option<T>`; at startup, instantiation is decided by presence:

```rust
let mut channels: Vec<Box<dyn Channel>> = Vec::new();
if let Some(tg) = &config.channels.telegram {
    channels.push(Box::new(TelegramChannel::new(tg)?));
}
```

### 7.4 Example Configuration

See `config/openclaw.json.example` for a complete example. Key fields:

```json5
{
  "provider": "anthropic", "model": "claude-sonnet-4-5-20250929",
  "channels": { "telegram": { "botToken": "${TELEGRAM_BOT_TOKEN}", "allowedUsers": [] } },
  "messages": {
    "tts": { "auto": "inbound", // "inbound" | "always" | "tagged" | any other value = off
             "provider": "edge", // "edge" | "openai" | "elevenlabs"
             "maxTextLength": 500,
             "edge": { "voice": "zh-CN-XiaoxiaoNeural", "rate": "+10%" } },
    "mediaUnderstanding": { "audio": { "provider": "groq", "model": "whisper-large-v3-turbo" } }
  },
  "tools": { "allow": ["ha_control", "web_fetch", "get_time", "cron", "memory", "web_search", "exec"] },
  "homeAssistant": { "url": "http://192.168.1.100:8123", "token": "${HA_TOKEN}" },
  "agents": {
    "systemPrompt": "You are a personal assistant running inside OpenClaw.",
    "agentTimeoutSecs": 900,
    "thinking": "off",       // "off"|"low"|"medium"|"high"
    "compactRatio": 0.4,     // fraction of context to retain after compaction
    "followupDebounceMs": 2000, // debounce window for pending messages (milliseconds)
    "queueMode": "interrupt", // "interrupt" (new message interrupts current turn) | "queue" (collect and merge)
    // "contextFiles": ["./SOUL.md"],  // loaded into system prompt at startup
    // "fallbackModels": [
    //   { "provider": "openai", "model": "gpt-4o", "apiKeyEnv": "OPENAI_API_KEY" },
    //   { "provider": "groq", "model": "llama-3.3-70b-versatile",
    //     "apiKeyEnv": "GROQ_API_KEY", "baseUrl": "https://api.groq.com/openai/v1" }
    // ]
  },
  "session": { "dir": "./sessions", "historyLimit": 0 },
  "memory": { "dir": "./memory", "maxMemoryBytes": 4096, "maxContextBytes": 4096 },
  "webSearch": {
    "provider": "duckduckgo", // "brave"|"duckduckgo"
    "apiKeyEnv": "BRAVE_SEARCH_API_KEY",
    "maxResults": 5
  }
}
```

---

<a id="chapter-8"></a>
## Chapter 8: Build and Deployment

### 8.1 Local Development

```bash
# Set environment variables
export TELEGRAM_BOT_TOKEN="123456:ABCxxx"
export ANTHROPIC_API_KEY="sk-ant-xxx"
export GROQ_API_KEY="gsk_xxx"      # optional, required only for voice features
export HA_TOKEN="eyJhbGci..."

# Compile and run
RUST_LOG=debug cargo run -- --config ./config/openclaw.json
```

### 8.2 Cross-Compilation

Uses the `cross` tool, which provides the target compilation environment via Docker:

```bash
cargo install cross --git https://github.com/cross-rs/cross

cross build --release --target aarch64-unknown-linux-musl       # Raspberry Pi 4/5
cross build --release --target armv7-unknown-linux-musleabihf   # Raspberry Pi 2/3
cross build --release --target x86_64-unknown-linux-musl        # x86-64 server
```

Output: `target/{target}/release/openclaw-light`, a single statically-linked binary.

### 8.3 Reproducible Docker Build

Multi-stage build based on `ghcr.io/rust-cross/rust-musl-cross` image, supporting parameterized target platforms and producing bit-for-bit reproducible static binaries.

**Reproducibility measures:**

| # | Measure | Implementation | Non-determinism eliminated |
|---|---------|----------------|---------------------------|
| 1 | Toolchain version pinning | `rust-toolchain.toml = "1.84.0"` | Compiler drift |
| 2 | Dependency version pinning | `Cargo.lock` + `--locked` | Dependency floating |
| 3 | Disable incremental compilation | `CARGO_INCREMENTAL=0` | Cache timing |
| 4 | Zero timestamps | `SOURCE_DATE_EPOCH=0` | Timestamp embedding |
| 5 | Path remapping | `RUSTFLAGS="--remap-path-prefix=..."` | Host path embedding |
| 6 | Single codegen unit | `codegen-units=1` + `lto=true` | Parallel randomness |

**Build commands:**

```bash
# Single target build
./scripts/docker-build.sh aarch64

# All 3 targets
./scripts/docker-build.sh

# Reproducibility verification (build twice and compare SHA-256)
./scripts/docker-build.sh --verify x86_64
```

**Target mapping:**

| Short name | MUSL_TARGET | Rust triple |
|------------|-------------|-------------|
| `aarch64` | `aarch64-musl` | `aarch64-unknown-linux-musl` |
| `armv7` | `armv7-musleabihf` | `armv7-unknown-linux-musleabihf` |
| `x86_64` | `x86_64-musl` | `x86_64-unknown-linux-musl` |

Artifacts are extracted to `dist/{triple}/openclaw-light`.

**Runtime image (Stage 3):** Based on `scratch` + `busybox:musl`, providing a complete shell environment (sh, ls, cat, grep, awk, sed, wget and 300+ other commands). Started via `entrypoint.sh`: first fixes mounted volume file permissions as root (`chown -R 1000:1000`), then drops privileges via `su` to the non-root user `openclaw` (UID 1000) to run the main process. This resolves the issue where files in Docker Desktop for Windows mounted volumes appear as root-owned, preventing the application from writing to them. Total image size ~3.5MB (binary + CA certificates + busybox ~1.5MB).

See `docs/en/specs/rust-conventions.md` section 11 for detailed build specifications.

### 8.4 Docker Compose Production Deployment

`docker-compose.yml` provides container-level security hardening, modeled after Claude Code's bubblewrap sandbox:

```yaml
services:
  gateway:
    image: openclaw-light:latest
    read_only: true                    # Read-only filesystem (mirrors bubblewrap FS isolation)
    cap_drop: [ALL]                    # Drop all Linux capabilities
    cap_add: [CHOWN, SETUID, SETGID]  # Required by entrypoint: chown fixes permissions + su drops privileges
    mem_limit: 32m                     # Memory limit
    tmpfs:
      - /tmp:size=10m                  # Writable area for temporary files
    volumes:
      - ./openclaw.json:/app/openclaw.json:ro   # Config read-only
      - ./sessions:/app/sessions                # Session data
      - ./auth_tokens.json:/app/auth_tokens.json # OAuth token (read-write)
      - ./memory:/app/memory                    # Long-term memory
      - ./skills:/app/skills                    # Agent scripts
    env_file:
      - .env                             # Environment variables (GROQ_API_KEY, etc.)
    # No user: setting — entrypoint starts as root to chown, then su to openclaw (1000)
```

**entrypoint.sh** startup flow:
1. `chown -R 1000:1000` fixes permissions on memory/sessions/skills directories (files in Docker Desktop for Windows mounted volumes may appear as root-owned)
2. `exec su -s /bin/sh openclaw -c /app/openclaw-light` drops privileges to UID 1000 to run the main process

**Security layers:**

| Layer | Measure | Mirrors Claude Code |
|-------|---------|---------------------|
| Filesystem | `read_only: true` + only necessary volumes mounted | bubblewrap FS isolation |
| Privileges | `cap_drop: [ALL]` + only CHOWN/SETUID/SETGID added back (discarded after entrypoint use) | Least privilege |
| Environment variables | exec tool `env_clear()` isolation (see §12.1) | Environment isolation to prevent credential leakage |
| User | entrypoint drops to UID 1000 non-root | Unprivileged user |
| Resources | `mem_limit: 32m` | Resource limits |

### 8.5 systemd Service Unit

Reference file: `deploy/openclaw-light.service`.

```ini
[Unit]
Description=OpenClaw Rust Gateway
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=openclaw
Group=openclaw
WorkingDirectory=/opt/openclaw

# ── Startup ──
ExecStart=/opt/openclaw/openclaw-light --config /opt/openclaw/openclaw.json
Restart=on-failure
RestartSec=10
EnvironmentFile=/opt/openclaw/.env

# ── Security hardening ──
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true

# ── Filesystem permissions ──
ReadWritePaths=/opt/openclaw/sessions /opt/openclaw/memory /opt/openclaw/skills /opt/openclaw/backups /opt/openclaw/auth_tokens.json
ReadOnlyPaths=/opt/openclaw/openclaw-light /opt/openclaw/openclaw.json

# ── Resource limits ──
MemoryMax=32M
MemoryHigh=24M

# ── Logging ──
StandardOutput=journal
StandardError=journal
SyslogIdentifier=openclaw

[Install]
WantedBy=multi-user.target
```

```bash
sudo cp deploy/openclaw-light.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now openclaw-light
```

### 8.6 ARM Device Deployment

**Cross-compilation:**

```bash
# Build aarch64 static binary (musl), output to dist/aarch64-unknown-linux-musl/
./scripts/docker-build.sh aarch64
```

**Deploy to device:**

```bash
# Reads DEPLOY_HOST environment variable by default, or specify on the command line
DEPLOY_HOST=user@orangepi ./scripts/deploy.sh

# Or run directly (using a pre-configured DEPLOY_HOST)
./scripts/deploy.sh
```

`deploy.sh` performs: scp binary + config to device → install systemd service → daemon-reload. After deployment, it automatically checks the remote state: if `openclaw.json`, `.env` exist and the service is already active, it automatically restarts and prints recent logs; otherwise it prints first-time installation instructions.

### 8.7 Notes

**Cross-compilation and artifacts:**
- `docker-build.sh` extracts the binary from the build container via `docker create` + `docker cp`; the in-container path is `/openclaw-light` (not `/app/`)
- Artifacts are output to `dist/{triple}/openclaw-light` (e.g. `dist/aarch64-unknown-linux-musl/openclaw-light`)
- The current aarch64 binary is ~3.4MB; musl static linking has no external dependencies and can be scp'd directly to a device and run

**systemd sandbox:**
- `ProtectSystem=strict` makes the entire filesystem read-only; all writable paths must be explicitly declared in `ReadWritePaths`, otherwise the process will get `EROFS (Read-only file system)` when writing files
- The sandbox only takes effect within the service process — testing with `touch` after SSH login will not reproduce EROFS; use `journalctl -u openclaw-light` to see actual logs
- When adding new writable files/directories, always update `ReadWritePaths` in the service file accordingly

### 8.8 Automatic Backup

The gateway process has built-in automatic backup, leveraging the existing 60s cron tick to check periodically and automatically package data when conditions are met.

**Trigger mechanism:** Each cron tick reads the modification time of the most recent `.tar.gz` in the `backups/` directory (a microsecond-level metadata operation); a backup is only performed when the interval exceeds `intervalHours`. On first run or when the directory is empty, a backup is taken immediately.

**Backup scope:**

| Type | Path | Notes |
|------|------|-------|
| Binary | `openclaw-light` | ~3.4MB, convenient for quick rollback |
| Config | `openclaw.json` | |
| Secrets | `.env` | |
| OAuth | `auth_tokens.json` | |
| Sessions | `sessions/` | |
| Memory | `memory/` | |
| Skills | `skills/` | |

Only files/directories that actually exist are packaged; missing ones are skipped.

**Configuration:**

```json5
{
  "backup": {
    "enabled": true,        // Master switch (default true)
    "dir": "./backups",     // Storage directory
    "intervalHours": 24,    // Minimum interval (hours)
    "retentionDays": 7,     // Retention period (days)
    "maxSizeMb": 200        // Total backup size limit (MB)
  }
}
```

**Agent tool:** Add `"backup"` to `tools.allow` to let the agent manage backups via the `backup` tool:

| action | Description |
|--------|-------------|
| `status` | Returns current status (enabled/disabled, last backup time, backup directory size) |
| `enable` | Enable automatic backup |
| `disable` | Disable automatic backup |
| `run` | Execute a backup immediately |

The runtime enable/disable state is persisted via `backups/state.json` and survives restarts.

**Cleanup strategy:** Dual cleanup — first delete backups older than `retentionDays`, then check total size: if the remaining total backup size exceeds `maxSizeMb`, delete from the oldest until under the limit. Default limit is 200MB.

**Implementation:** Shells out to `tar czf` (tar is always available on devices); no new crates introduced. Cleanup uses `tokio::fs::read_dir` + metadata to check file age and size.

---

<a id="chapter-9"></a>
## Chapter 9: Testing Strategy

### 9.1 Unit Tests

Each module has co-located unit tests under `#[cfg(test)] mod tests`. Coverage:

| Module | Tests | Coverage |
|--------|-------|---------|
| `config.rs` | ~18 | Env var substitution, default value population, JSON5 comment parsing, provider config, TTS config |
| `error.rs` | ~3 | `From` conversions map correctly to corresponding variants |
| `agent/context.rs` | 3 | System prompt memory injection, tool list, compaction prompt |
| `agent/react_loop.rs` | 22 | `<speak>` tag extraction, LoopDetector, response prefix, compaction, consume_stream, react_loop, truncate_tool_result |
| `memory/store.rs` | 12 | read/append/rewrite/read_log/append_log/search/build_context |
| `tools/memory.rs` | 5 | MemoryTool 6 actions |
| `tools/cron.rs` | 15 | cron_matches (range/step), schedule_at, CronTask serialization compatibility, field_matches |
| `tools/exec.rs` | 8 | echo/exit code/stderr/timeout/truncation |
| `tools/mcp.rs` | 10 | JSON-RPC format, proxy naming, output truncation |
| `tools/web_search.rs` | 6 | DDG HTML parsing, entity decoding, strip tags |
| `session/store.rs` | 6 | Sliding window, append, empty file, history_limit |
| `provider/llm/mod.rs` | 3 | Default chat_stream() wrapper logic |
| `provider/llm/claude.rs` | 4 | SSE streaming parse: text/tool/error/max_tokens |
| `provider/llm/openai_compat.rs` | ~17 | Serialization/deserialization/wiremock roundtrip/SSE streaming |
| `provider/tts/openai.rs` | 4 | Construction/empty text/config |
| `provider/tts/elevenlabs.rs` | 4 | Construction/empty text/config |
| `channel/telegram.rs` | 12 | Audio format detection, message chunking (chunk_text) |

### 9.2 Integration Tests

Located in `tests/integration/`, organized through the `tests/integration_tests.rs` entry file.

| Test file | Tests | Coverage |
|-----------|-------|---------|
| `mcp_client.rs` | 4 | Start → list_tools → call echo/add → shutdown |
| `mcp_timeout.rs` | 1 | 2s timeout vs 5s sleep server |
| `http_api.rs` | 4 | POST /chat, 404, 400, channel id |
| `exec_skills.rs` | 2 | skills_dir PATH lookup, description contains path |
| `web_search.rs` | 2 | wiremock DDG HTML parsing, empty results |
| `edge_tts.rs` | 2 | Edge TTS Opus/MP3 synthesis (real network requests) |
| `google_stt.rs` | 1 | Google Cloud STT real speech-to-text |
| `volcengine_stt.rs` | 1 | Volcengine STT real speech-to-text |
| `volcengine_tts.rs` | 2 | Volcengine TTS synthesis + empty text |

### 9.3 Running Tests

```bash
docker build --target tester -t test .   # Compile (including test binaries)
docker run --rm --env-file .env test     # Run all tests (0 ignored)
```

See `docs/en/specs/rust-conventions.md` section 7 for testing conventions.
