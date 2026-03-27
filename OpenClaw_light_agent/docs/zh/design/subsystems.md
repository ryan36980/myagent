> [设计文档](README.md) > 子系统：记忆、Cron、扩展、OAuth

<a id="chapter-10"></a>
## 第十章：长期记忆系统

### 10.1 设计目标

为每个聊天维护持久记忆，使 agent 能跨对话保留用户偏好、重要事实和历史上下文。

**关键决策：**
- **Agent 驱动**：LLM 通过 memory tool 自主管理记忆，不做额外 API 调用
- **磁盘是真相源，上下文是缓存**：所有状态存磁盘，按需读入
- **自适应 token 预算注入**：MEMORY.md 必注入 + 近日日志倒序填充至预算
- **子串搜索**：零依赖 grep-like 搜索，让 agent 可跨全部日志查找信息

### 10.2 存储结构

```
{memory_dir}/
  SHARED/                    ← 跨 chat 共享记忆（所有 allowedUsers 可读写）
    MEMORY.md
  {channel}_{chat_id}/       ← per-chat 隔离记忆
    MEMORY.md                ← 长期策展记忆（agent 可 rewrite 整理）
    YYYY-MM-DD.md            ← 每日追加日志（append-only）
```

**共享记忆层：** `SHARED/MEMORY.md` 存储跨 chat 通用知识（设备 IP、网络拓扑、家庭成员等）。所有 chat 的 agent 均可读写，路径固定。Per-chat 记忆隔离由 `ChatContext`（channel + chat_id）自动保证，无需额外权限控制。

### 10.3 MemoryStore 接口

```rust
pub struct MemoryStore {
    dir: PathBuf,
    max_memory_bytes: usize,   // MEMORY.md 最大字节数（默认 4096）
    max_context_bytes: usize,  // build_context 注入总预算（默认 4096）
}
```

| 方法 | 说明 |
|------|------|
| `init()` | 创建 memory_dir（如不存在） |
| `read(ch, cid)` | 读取 per-chat MEMORY.md，不存在返回空字符串 |
| `append(ch, cid, text)` | 追加到 per-chat MEMORY.md，超过 max_memory_bytes 返回警告 |
| `rewrite(ch, cid, text)` | 完全重写 per-chat MEMORY.md（agent 用于整理） |
| `read_shared()` | 读取 SHARED/MEMORY.md，不存在返回空字符串 |
| `append_shared(text)` | 追加到 SHARED/MEMORY.md，超过 max_memory_bytes 返回警告 |
| `rewrite_shared(text)` | 完全重写 SHARED/MEMORY.md |
| `read_log(ch, cid, date)` | 读取指定日期日志，不存在返回空字符串 |
| `append_log(ch, cid, text)` | 追加到当日 `YYYY-MM-DD.md` |
| `search(ch, cid, query, max)` | 跨 per-chat 全部文件子串搜索，±2 行上下文，按日期倒序 |
| `search_shared(query, max)` | 搜索 SHARED/MEMORY.md |
| `build_context(ch, cid)` | 自适应 token 预算注入（见 10.4） |

### 10.4 自适应 token 预算算法

`build_context()` 生成注入到 system prompt 的记忆上下文：

1. 先注入 SHARED/MEMORY.md（共享记忆优先，跨 chat 通用知识）
2. 再注入 per-chat MEMORY.md（优先保证长期记忆）
3. 剩余预算倒序填充近日日志
4. 每个日志文件：能放下就全放，放不下就跳过（不截断）
5. 用 `### Shared MEMORY.md` / `### MEMORY.md` / `#### YYYY-MM-DD` 作为分隔标题

共享记忆为空时不输出 `### Shared MEMORY.md` 段头。
如果 MEMORY.md 本身超预算，截断到预算（不影响磁盘原件）。

### 10.5 MemoryTool（6 action）

注入给 LLM 的工具描述：

```
Manage persistent memory across conversations.
- read: Read your long-term MEMORY.md
- append: Add new information to MEMORY.md
- rewrite: Reorganize and rewrite MEMORY.md (use when it's getting full)
- read_log: Read a daily log entry (defaults to today)
- append_log: Append to today's daily log
- search: Search across all memory files for a keyword or phrase

Use scope: "shared" for cross-conversation knowledge (device IPs, network config,
household members, universal preferences). Default scope is per-conversation.
```

Tool schema：
```json
{
  "action": "read | append | rewrite | read_log | append_log | search",
  "content": "string (for append/rewrite/append_log)",
  "date": "string YYYY-MM-DD (for read_log, optional — defaults today)",
  "query": "string (for search)",
  "scope": "string enum ['chat', 'shared'] (optional, default 'chat')"
}
```

### 10.6 System Prompt 注入

`build_system_prompt(base, tools, memory_context, compaction_warning)` 生成的结构：

```
{base_prompt}

## Current Date & Time
Thursday, February 19, 2026 — 14:30

## Memory
{memory_context — 由 MemoryStore::build_context() 生成}

## Available Tools
- ...

{compaction_warning — 仅在 messages.len() > 15 时追加}
```

**日期时间注入：** 使用 `chrono::Local::now()` 动态生成，格式 `"%A, %B %e, %Y — %H:%M"`（如 `Thursday, February 19, 2026 — 14:30`）。Agent 无需调用 get_time 工具即可获知当前时间。

**压缩提示文案：**
```
⚠ Conversation history is approaching its limit and older messages will be
lost. Store any important context, decisions, or user preferences to memory
now (action: "append" or "append_log"). Reply with the user's answer
afterward. If nothing to store, just reply normally.
```

### 10.7 内存开销

| 组件 | 常驻堆 |
|------|--------|
| `MemoryStore` | ~48B (PathBuf + 2×usize) |
| `Arc<Mutex<ChatContext>>` | ~64B |
| 读 MEMORY.md / build_context | 临时 ≤max_context_bytes，请求后释放 |
| search 操作 | 临时：逐文件读取 + 匹配行收集，请求后释放 |
| **合计** | **< 200B 常驻** |

---

<a id="chapter-11"></a>
## 第十一章：Cron 自动执行系统

### 11.1 CronTask 数据结构

```rust
pub struct CronTask {
    pub id: String,                      // UUID v4 前 8 位
    pub cron_expr: String,               // 5 字段 cron 表达式
    pub description: String,             // 人类可读描述
    pub command: String,                 // 待执行命令/指令
    pub channel: String,                 // 创建时的通道（如 "telegram"）
    pub chat_id: String,                 // 创建时的聊天 ID
    pub created_at: i64,                 // 创建时间戳
    pub last_run: Option<i64>,           // 上次执行时间戳（分钟级去重）
    pub schedule_at: Option<String>,     // ISO 8601 一次性触发时间
    pub delete_after_run: bool,          // 执行后自删
    pub delivery_mode: String,           // "announce"|"webhook"|"none"
    pub webhook_url: Option<String>,     // webhook 投递 URL
    pub isolated: bool,                  // 隔离执行（使用临时会话）
}
```

`channel` 和 `chat_id` 在 `add` 时从 `Arc<Mutex<ChatContext>>` 自动注入。所有新增字段使用 `#[serde(default)]` 确保旧数据反序列化兼容。

### 11.2 cron_matches — 5 字段匹配

```rust
pub fn cron_matches(expr: &str, now: &chrono::DateTime<chrono::Local>) -> bool
pub fn schedule_at_matches(schedule_at: &str, now: &chrono::DateTime<chrono::Local>) -> bool
```

**cron_matches**：5 字段 `min hour dom mon dow`。支持 `*`（任意）、数字、逗号分隔列表、范围（`1-5`）、步进（`*/5`）和范围+步进（`1-30/2`）。

**schedule_at_matches**：接受 RFC 3339（`2026-02-18T10:30:00+08:00`）或 NaiveDateTime（`2026-02-18 10:30:00`），比对当前分钟。

### 11.3 主循环执行流程

在 `main.rs` 的 `tokio::select!` 中增加 60 秒间隔分支：

```
每 60s tick:
  1. load_all_tasks(cron_file)
  2. for each task:
     - 分钟级去重: last_run/60 == now/60 → skip
     - 判断触发: schedule_at 优先检查 → 否则检查 cron_expr
     - 隔离执行: isolated=true → 使用临时 chat_id `_cron_isolated_{id}_{ts}`
     - 更新 chat_context → 构造 IncomingMessage → agent.handle()
     - 投递分发:
       ├─ "announce" → send_text 到原 channel
       ├─ "webhook" → SSRF 校验后 POST JSON 到 webhook_url（5s 超时）
       └─ "none" → 静默丢弃
     - task.last_run = now.timestamp()
     - delete_after_run → 标记删除
  3. 移除已标记任务
  4. if changed: save_all_tasks(cron_file)
```

**分钟级去重：** `last_run / 60 == now_timestamp / 60` 确保同一分钟内不重复执行。

**首次 tick 跳过：** `cron_interval.tick().await` 在循环前消耗第一次立即触发，避免启动时误执行。

**Webhook SSRF 防护：** 投递前调用 `validate_url_ssrf()` 检查目标地址。

### 11.4 内存开销

~50 字节/任务（新增字段）。复用主循环的 tokio timer（`tokio::time::interval`），cron.json 仅在每分钟检查时临时读入。

---

<a id="chapter-12"></a>
## 第十二章：扩展能力 — exec + web_search + MCP 客户端

### 12.1 exec 工具

Shell 命令执行工具。通过 `sh -c` 派生子进程，支持：

- 可配超时（默认 30s，上限 300s）
- stdout/stderr 分离捕获，超长输出自动截断
- skills 目录自动加入 PATH，支持保存可复用脚本
- **环境变量隔离**：`env_clear()` 清除所有继承环境变量，仅暴露 `PATH`、`HOME=/app`、`TERM=dumb`、`LANG=C.UTF-8`。防止 LLM 通过 `env`/`printenv` 读取 API key、bot token 等进程密钥

**配置：** `ExecConfig { timeout_secs, max_output_bytes, work_dir, skills_dir }`

**文件：** `src/tools/exec.rs` (~160 行)

### 12.2 web_search 工具

双搜索引擎支持的网页搜索工具：

**DuckDuckGo（默认）：**
- 请求 `https://html.duckduckgo.com/html/?q={query}`
- 纯字符串匹配解析 HTML，无 HTML parser 依赖

**Brave Search（可选）：**
- GET `https://api.search.brave.com/res/v1/web/search?q={query}`
- JSON API，`X-Subscription-Token` 认证
- 免费额度 2000 次/月
- Brave 请求失败自动回退到 DuckDuckGo

**配置：** `WebSearchConfig { provider: "duckduckgo"|"brave", api_key_env: "BRAVE_SEARCH_API_KEY", max_results: 5 }`

**文件：** `src/tools/web_search.rs` (~250 行)

### 12.3 MCP 客户端

Model Context Protocol 客户端，让用户无需修改 Rust 代码即可接入任意 MCP 服务端：

- stdio 传输：换行分隔的 JSON-RPC 2.0
- 只实现 4 个方法：`initialize`、`notifications/initialized`、`tools/list`、`tools/call`
- 每个 MCP 服务端工具以 `mcp__{server}__{tool}` 命名注册到 ToolRegistry
- 优雅关闭：close stdin → wait 2s → kill

**资源限制（对标 Claude Code）：**
- `timeout_secs`（默认 60）：单次 `tools/call` 超时，`tokio::time::timeout()` 包裹 `recv()`
- `max_output_bytes`（默认 65536）：工具输出截断到此字节数（UTF-8 安全截断），附加截断提示
- `truncate_mcp_output()` + `safe_truncate_pos()` 辅助函数确保不在多字节字符中间截断

**配置：**
```json5
{
  "mcp": {
    "servers": {
      "weather": {
        "command": "python3",
        "args": ["server.py"],
        "env": { "API_KEY": "${WEATHER_API_KEY}" },
        "timeoutSecs": 60,
        "maxOutputBytes": 65536
      }
    }
  }
}
```

**内存开销：** ~16KB/服务端 (BufReader/BufWriter) + ~200B/工具

**文件：** `src/tools/mcp.rs` (~300 行)

---

<a id="chapter-13"></a>
## 第十三章：Anthropic OAuth 2.0 认证

### 13.1 设计目标

支持通过 Anthropic OAuth（PKCE 流程）认证，使用个人账户（Claude Max/Pro）的 OAuth token 调用 API，无需单独申请 API Key。向后兼容现有 API Key 模式。

### 13.2 认证模式

```rust
pub enum AuthMode {
    ApiKey(String),                          // 传统 x-api-key header
    OAuth(Arc<Mutex<TokenStore>>),           // Bearer token + 自动刷新
}
```

配置 `auth.mode` 为 `"api_key"`（默认）或 `"oauth"` 切换模式。

### 13.3 OAuth PKCE 流程

```
用户 /auth → 生成 code_verifier + challenge(S256) → 返回授权 URL
用户浏览器授权 → 获得授权码
用户 /auth CODE → POST token_url 交换 token → 保存到文件
后续 API 调用 → Bearer token + anthropic-beta header → 过期前自动刷新
```

**关键参数：**

| 参数 | 值 |
|------|-----|
| Auth URL | `https://claude.ai/oauth/authorize`（Max/Pro 订阅）|
| Token URL | `https://console.anthropic.com/v1/oauth/token` |
| Callback | `https://console.anthropic.com/oauth/code/callback` |
| Client ID | 可配置，默认 `9d1c250a-e61b-44d9-88ed-5944d1962f5e` |
| Scopes | `org:create_api_key user:profile user:inference` |
| PKCE | S256（SHA-256 + base64url-no-pad）|
| state | 随机 32 字节 base64url 编码（CSRF 防护）|
| code | `true`（必需查询参数）|
| API Header | `Authorization: Bearer <token>` + `anthropic-beta: oauth-2025-04-20` |

### 13.4 TokenStore

```rust
pub struct TokenStore {
    client_id: String,
    token_url: String,
    file_path: PathBuf,
    tokens: Option<TokenData>,
    pkce: Option<PkceState>,      // 授权进行中的临时状态
    http_client: reqwest::Client,
}

pub struct TokenData {
    access_token: String,
    refresh_token: String,
    expires_at: i64,              // Unix timestamp (seconds)
}
```

- `get_token()` — 返回有效 token，过期前 5 分钟自动 refresh
- `load()` / `save()` — JSON 文件持久化
- Token 文件路径可配置（默认 `./auth_tokens.json`）

### 13.5 ClaudeProvider 双认证

`ClaudeProvider.auth` 字段替代原 `api_key`：

- `ApiKey` → `x-api-key: <key>` header（现有行为）
- `OAuth` → `Authorization: Bearer <token>` + `anthropic-beta: oauth-2025-04-20`

通过 `apply_auth()` 方法统一设置 header，`chat()` 和 `chat_stream()` 共用。

### 13.6 /auth 命令

Telegram 端拦截 `/auth` 命令：

| 命令 | 行为 |
|------|------|
| `/auth` | 生成 PKCE，返回授权 URL |
| `/auth CODE` | 交换授权码为 token |
| `/auth status` | 显示认证状态 |
| `/auth reset` | 清除 token |

### 13.7 配置

```json5
{
  "auth": {
    "mode": "oauth",                    // "api_key"（默认）| "oauth"
    "clientId": "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
    "tokenFile": "./auth_tokens.json"
  }
}
```

### 13.8 实战注意事项

**API Endpoint：** OAuth token 仍使用 `api.anthropic.com/v1/messages`，与 API Key 模式相同。

**`anthropic-beta` header 必需：** 不带 `anthropic-beta: oauth-2025-04-20` header 会返回
`authentication_error: OAuth authentication is currently not supported`。必须携带此 header
才能启用 OAuth 认证。

**Model ID 使用短名称：** OAuth 模式下部分带日期的 model ID（如 `claude-opus-4-6-20250610`、
`claude-sonnet-4-5-20250514`）可能返回 404 not_found。应使用不带日期的短名称：
- `claude-opus-4-6`（推荐）
- `claude-sonnet-4-5`

**授权 URL 构建：** 必须使用 `url::Url::query_pairs_mut()` 正确编码所有参数。手动
`format!` 拼接可能导致参数丢失（如 `Missing client_id parameter` 错误）。

**必需的授权 URL 参数：**
- `code=true` — 必需，缺少会导致未知错误
- `state=<random>` — 必需，缺少返回 "Missing state parameter"
- `client_id` — 必需，编码错误返回 "Missing client_id parameter"

**Token 复用：** Claude Code CLI 的 OAuth token（`~/.claude/.credentials.json`）可直接复用，
格式为 `sk-ant-oat01-...`（access）和 `sk-ant-ort01-...`（refresh）。但注意 Claude Code token
的 scope 可能包含 `user:sessions:claude_code`，与独立 OAuth 流程获取的 scope 略有不同。

**Token 文件挂载：** Docker 部署时 token 文件需要读写挂载（不能 `:ro`），因为自动刷新
会更新文件内容。

**Token Exchange 必须用 JSON：** 交换 token 的 POST 请求体必须是 `application/json`，
不能使用 `application/x-www-form-urlencoded`。表单编码会返回
`invalid_request_error: Invalid request format`。同时请求体需包含 `state` 参数。

**Telegram Markdown 转义：** 授权 URL 中 client_id 含 `_` 字符，Telegram Markdown
模式会将其解析为斜体标记，导致 URL 被截断。发送 URL 前需将 `_` 转义为 `\_`。

**Auth Code 含 URL Fragment：** 用户从回调页面复制的 auth code 可能附带 `#state_value`
后缀（URL fragment）。代码端需自动去除 `#` 及后续内容，否则返回
`invalid_grant: Invalid 'code' in request`。

### 13.9 内存开销

| 组件 | 常驻 | 瞬时 |
|------|------|------|
| AuthConfig | ~100B | 0 |
| TokenStore | ~200B | 0 |
| PKCE state（授权期间）| 0 | ~100B |
| **总计** | **~300B** | **~100B** |

### 13.10 文件清单

| 文件 | 改动 |
|------|------|
| `Cargo.toml` | +1 行 `sha2 = "0.10"` |
| `src/lib.rs` | +1 行 `pub mod auth;` |
| `src/auth/mod.rs` | **新建** ~250 行 |
| `src/config.rs` | +AuthConfig 结构体 |
| `src/provider/llm/claude.rs` | api_key→auth, apply_auth() |
| `src/main.rs` | +handle_auth_command(), TokenStore 初始化 |
