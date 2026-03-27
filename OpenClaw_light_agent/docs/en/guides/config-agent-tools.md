> [Configuration Guide](configuration.md) > Agent, Tools & Subsystems

# Agent Configuration

```json5
{
  "agents": {
    // System prompt (customizable; defaults already include tool invocation rules, safety rules, and memory usage guidance)
    // "systemPrompt": "You are a helpful assistant...",

    "agentTimeoutSecs": 900,       // Agent single-turn timeout (seconds), default 900 (15 minutes)
    "thinking": "off",             // Extended thinking: "off" | "low" | "medium" | "high"
    "compactRatio": 0.4,           // Compaction ratio (0.0~1.0), retain 40% of messages
    "followupDebounceMs": 2000,    // Follow-up message debounce window (milliseconds)
    "queueMode": "interrupt",      // "interrupt" (new message interrupts current execution) | "queue" (queue and wait)

    // Context files (loaded into the system prompt at startup)
    // "contextFiles": ["./SOUL.md", "./team-rules.md"],

    // Fallback models (see LLM configuration)
    // "fallbackModels": [...]
  }
}
```

**Extended Thinking:**
- `"off"` — disabled (default)
- `"low"` — 2048 token thinking budget
- `"medium"` — 8192 tokens
- `"high"` — 32768 tokens

> Only Anthropic Claude supports Extended Thinking. When enabled, the API version is automatically upgraded to `2025-04-14`.

---

# Tool Configuration

Control which tools the Agent can use via `tools.allow`:

```json5
{
  "tools": {
    "allow": [
      "web_fetch",          // web page fetching (with SSRF protection)
      "web_search",         // web search (DuckDuckGo / Brave)
      "get_time",           // get current time
      "cron",               // scheduled task management
      "memory",             // long-term memory (read/write/search)
      "exec",               // shell command execution
      "ha_control",         // Home Assistant control
      "sessions_spawn",     // spawn sub-Agent
      "sessions_list",      // list sub-Agents
      "sessions_history",   // get sub-Agent history
      "sessions_send",      // send message to sub-Agent
      "backup"              // backup management (status/enable/disable/run now)
    ]
  }
}
```

Empty array = no tools available. Enable only what you need — security-sensitive tools (e.g. `exec`, `ha_control`) can be left out.

## web_fetch Tool

`web_fetch` fetches URL content and automatically converts HTML pages to plain text (removes script/style/tags, decodes entities, collapses whitespace). JSON/XML/plain-text API responses are not converted. Returns up to the first 128K characters by default; supports `offset` + `max_chars` pagination for reading very long pages.

```json5
{
  "webFetch": {
    "maxDownloadBytes": 2000000  // download size limit (bytes), default 2MB
  }
}
```

| Parameter | Description |
|-----------|-------------|
| `maxDownloadBytes` | Per-request download size limit (bytes), default 2,000,000 (2MB). Responses exceeding this limit return an error message instead of truncated content. Increasing this value allows fetching larger pages, but peak memory usage is approximately 2x this value. |

Tool input parameters:
- `url` (required): URL to fetch
- `headers` (optional): custom HTTP request headers
- `offset` (optional): start returning from character N, default 0
- `max_chars` (optional): maximum number of characters to return, default 128000, maximum 256000

---

# Session Configuration

```json5
{
  "session": {
    "dir": "./sessions",      // session file storage directory
    "historyLimit": 0,        // raw message limit (0 = unlimited)
    "dmHistoryLimit": 20      // user turn limit (counts only user messages containing text), default 20
  }
}
```

- `dmHistoryLimit` mirrors the original OpenClaw behavior: only user messages containing text content count as a "turn" — tool call results do not. When the limit is exceeded, the oldest turns are trimmed.
- Set to `0` for unlimited turns (not recommended — context will grow indefinitely).

---

# Memory System Configuration

```json5
{
  "memory": {
    "dir": "./memory",          // memory file storage directory
    "maxMemoryBytes": 4096,     // maximum MEMORY.md size in bytes (Agent is prompted to trim when exceeded)
    "maxContextBytes": 4096     // memory context limit injected into the system prompt
  }
}
```

**Memory file structure:**
```
memory/
├── SHARED/                    # Cross-chat shared memory (readable/writable by all users)
│   └── MEMORY.md
└── telegram_123456789/        # Isolated per {channel}_{chatId} directory
    ├── MEMORY.md              # Long-term memory (maintained by the Agent)
    ├── 2026-02-19.md          # Daily conversation log
    └── 2026-02-20.md
```

The Agent selects the memory scope using the `scope` parameter of the `memory` tool:
- `scope: "chat"` (default) — reads/writes the per-chat memory for the current chat
- `scope: "shared"` — reads/writes `SHARED/MEMORY.md`, suitable for cross-chat general knowledge such as device IPs, network configuration, and household members

System prompt injection order: shared memory → per-chat memory → recent daily logs, all sharing the `maxContextBytes` budget.

You can also manually edit `MEMORY.md` or `SHARED/MEMORY.md` to inject initial knowledge.

---

# Home Assistant Configuration

```json5
{
  "homeAssistant": {
    "url": "http://192.168.1.100:8123",
    "token": "${HA_TOKEN}"
  },
  "tools": {
    "allow": ["ha_control", ...]  // must be enabled in tools.allow
  }
}
```

**.env file:**
```
HA_TOKEN=eyJhbGciOiJIUzI1NiIs...
```

Get a Long-Lived Access Token: Home Assistant → User Profile → Security → Create Long-Lived Access Token.

---

# Web Search Configuration

```json5
{
  "webSearch": {
    "provider": "duckduckgo",    // "duckduckgo" (default, free) | "brave"
    "maxResults": 5              // maximum number of results to return
    // "apiKeyEnv": "BRAVE_SEARCH_API_KEY"  // only required for Brave
  }
}
```

**DuckDuckGo** — free, no API Key required, default option.

**Brave Search** — requires an API Key:
```json5
{
  "webSearch": {
    "provider": "brave",
    "apiKeyEnv": "BRAVE_SEARCH_API_KEY",
    "maxResults": 5
  }
}
```

**.env file:**
```
BRAVE_SEARCH_API_KEY=BSA...
```

Get API Key: Visit [brave.com/search/api](https://brave.com/search/api/) → Create Application.

> If Brave is configured but the call fails, it automatically falls back to DuckDuckGo.

---

# Shell Execution Configuration

```json5
{
  "exec": {
    "timeoutSecs": 30,         // per-command timeout (maximum 300 seconds)
    "maxOutputBytes": 8192,    // output truncation limit
    "workDir": ".",             // working directory
    "skillsDir": "./skills"    // skills script directory (automatically added to PATH)
  }
}
```

> Security note: When the `exec` tool runs commands, it clears environment variables, retaining only `PATH`, `HOME=/app`, `TERM=dumb`, and `LANG=C.UTF-8`. This prevents the Agent from leaking API Keys and other sensitive information via the `env` command.

---

# Automatic Backup Configuration

```json5
{
  "backup": {
    "enabled": true,           // master switch (default true)
    "dir": "./backups",        // storage directory
    "intervalHours": 24,       // minimum interval (hours)
    "retentionDays": 7,        // retention period in days; older backups are cleaned up automatically
    "maxSizeMb": 200           // total backup size limit (MB); oldest backups are deleted when exceeded
  }
}
```

The gateway process uses the existing 60s cron tick to automatically check: it reads the modification time of the latest `.tar.gz` in `backups/` and only runs `tar czf` when `intervalHours` has been exceeded. Backup contents include the binary, configuration, keys, sessions, memory, and skills directory (only existing files are packed). Cleanup uses a dual strategy: first delete backups older than `retentionDays` by age, then check total size — if it exceeds `maxSizeMb`, delete oldest backups until below the limit.

Add `"backup"` to `tools.allow` to let the Agent manage backups via the backup tool:
- `status` — view backup status
- `enable` / `disable` — toggle automatic backup (persisted in `backups/state.json`, survives restarts)
- `run` — run a backup immediately

---

# File Operation Tools

Add `"file_read"`, `"file_write"`, `"file_edit"`, and `"file_find"` to `tools.allow` to allow the Agent to operate on files directly — more reliable than using `exec` to compose `cat`/`sed`/`grep` commands.

No additional configuration needed; all tools are zero-dependency and have zero resident memory. The security model is the same as `exec` (no path restrictions; controlled via `tools.allow`).

| Tool | Function | Input Parameters |
|------|----------|-----------------|
| `file_read` | Read file contents, returns text with line numbers | `path` (required), `offset` (starting line, 1-based), `limit` (number of lines) |
| `file_write` | Write file, automatically creates parent directories | `path` (required), `content` (required) |
| `file_edit` | Exact string replacement | `path` (required), `old_string` (required), `new_string` (required), `replace_all` (default false) |
| `file_find` | Search by filename/content | `path` (search directory, default "."), `pattern` (filename substring), `content` (content search), `max_depth` (default 10) |

**Limitations:**
- `file_read`: binary files are auto-detected and rejected; large files are truncated to 64KB
- `file_edit`: single replacement by default; errors if `old_string` appears 0 times or more than once (includes match count in error)
- `file_find`: returns at most 50 results, skips binary files

**Encoding notes:**
- `file_write` outputs UTF-8 (guaranteed by Rust strings)
- When writing files containing Chinese or non-ASCII characters, it is recommended to prepend a UTF-8 BOM (`\uFEFF`) to the `content` to ensure correct encoding detection on clients such as the Telegram mobile app
- See [Troubleshooting §9.1](troubleshooting.md#91-chinese-files-display-garbled-on-telegram-mobile)

---

# MCP Configuration

[Model Context Protocol](https://modelcontextprotocol.io/) allows connecting to external tool servers:

```json5
{
  "mcp": {
    "servers": {
      "my-tool-server": {
        "command": "npx",
        "args": ["-y", "@modelcontextprotocol/server-filesystem", "/data"],
        "env": {},
        "timeoutSecs": 60,
        "maxOutputBytes": 65536
      }
    }
  }
}
```

MCP servers communicate via stdio (JSON-RPC). Tools are automatically discovered and registered with the Agent at startup.
