> [配置指南](configuration.md) > Agent、工具与子系统

# Agent 配置

```json5
{
  "agents": {
    // 系统提示词（可自定义，默认已包含工具调用规则、安全规则、记忆使用指引）
    // "systemPrompt": "你是一个中文助手...",

    "agentTimeoutSecs": 900,       // Agent 单轮超时（秒），默认 900（15 分钟）
    "thinking": "off",             // 扩展思考："off" | "low" | "medium" | "high"
    "compactRatio": 0.4,           // 压缩比（0.0~1.0），保留 40% 消息
    "followupDebounceMs": 2000,    // 追加消息防抖窗口（毫秒）
    "queueMode": "interrupt",      // "interrupt"（新消息中断当前执行）| "queue"（排队等待）

    // 上下文文件（启动时加载到系统提示词中）
    // "contextFiles": ["./SOUL.md", "./team-rules.md"],

    // 备选模型（见 LLM 配置）
    // "fallbackModels": [...]
  }
}
```

**扩展思考（Extended Thinking）：**
- `"off"` — 关闭（默认）
- `"low"` — 2048 token 思考预算
- `"medium"` — 8192 token
- `"high"` — 32768 token

> 仅 Anthropic Claude 支持扩展思考。开启后 API 版本自动升级为 `2025-04-14`。

---

# 工具配置

通过 `tools.allow` 控制 Agent 可用的工具：

```json5
{
  "tools": {
    "allow": [
      "web_fetch",          // 网页抓取（带 SSRF 防护）
      "web_search",         // 网页搜索（DuckDuckGo / Brave）
      "get_time",           // 获取当前时间
      "cron",               // 定时任务管理
      "memory",             // 长期记忆（读写搜索）
      "exec",               // Shell 命令执行
      "ha_control",         // Home Assistant 控制
      "sessions_spawn",     // 启动子 Agent
      "sessions_list",      // 列出子 Agent
      "sessions_history",   // 获取子 Agent 历史
      "sessions_send",      // 向子 Agent 发送消息
      "backup"              // 备份管理（状态/开启/关闭/立即备份）
    ]
  }
}
```

空数组 = 没有工具可用。按需启用，安全敏感的工具（如 `exec`、`ha_control`）可以不加入列表。

## web_fetch 工具

`web_fetch` 抓取 URL 内容，自动将 HTML 页面转为纯文本（去除 script/style/标签，解码实体，压缩空白），JSON/XML/纯文本 API 响应不做转换。默认返回前 128K 字符，支持 `offset` + `max_chars` 分页读取超长页面。

```json5
{
  "webFetch": {
    "maxDownloadBytes": 2000000  // 下载体积上限（字节），默认 2MB
  }
}
```

| 参数 | 说明 |
|------|------|
| `maxDownloadBytes` | 单次下载体积上限（字节），默认 2,000,000 (2MB)。超过此限制的响应会返回错误提示而非截断内容。增大此值可抓取更大页面，但峰值内存约为此值的 2 倍。 |

工具输入参数：
- `url`（必填）：要抓取的 URL
- `headers`（可选）：自定义 HTTP 请求头
- `offset`（可选）：从第 N 个字符开始返回，默认 0
- `max_chars`（可选）：最多返回字符数，默认 128000，上限 256000

---

# 会话配置

```json5
{
  "session": {
    "dir": "./sessions",      // 会话文件存储目录
    "historyLimit": 0,        // 原始消息上限（0 = 不限）
    "dmHistoryLimit": 20      // 用户轮次上限（仅计算含文本的用户消息），默认 20
  }
}
```

- `dmHistoryLimit` 对标原版 OpenClaw 的行为：只计算包含文本内容的用户消息为一"轮"，工具调用结果不算。超过限制时裁剪最早的轮次。
- 设为 `0` 表示不限制轮次（不推荐，会导致上下文无限增长）。

---

# 记忆系统配置

```json5
{
  "memory": {
    "dir": "./memory",          // 记忆文件存储目录
    "maxMemoryBytes": 4096,     // MEMORY.md 最大字节数（超过会提示 Agent 精简）
    "maxContextBytes": 4096     // 注入系统提示词的记忆上下文上限
  }
}
```

**记忆文件结构：**
```
memory/
├── SHARED/                    # 跨聊天共享记忆（所有用户可读写）
│   └── MEMORY.md
└── telegram_123456789/        # 按 {通道}_{聊天ID} 分目录（隔离）
    ├── MEMORY.md              # 长期记忆（Agent 自行维护）
    ├── 2026-02-19.md          # 每日对话日志
    └── 2026-02-20.md
```

Agent 通过 `memory` 工具的 `scope` 参数选择记忆范围：
- `scope: "chat"`（默认）— 读写当前聊天的 per-chat 记忆
- `scope: "shared"` — 读写 `SHARED/MEMORY.md`，适合设备 IP、网络配置、家庭成员等跨聊天通用知识

系统提示词注入顺序：共享记忆 → per-chat 记忆 → 近日日志，共用 `maxContextBytes` 预算。

你也可以手动编辑 `MEMORY.md` 或 `SHARED/MEMORY.md` 注入初始知识。

---

# Home Assistant 配置

```json5
{
  "homeAssistant": {
    "url": "http://192.168.1.100:8123",
    "token": "${HA_TOKEN}"
  },
  "tools": {
    "allow": ["ha_control", ...]  // 需要在 tools.allow 中启用
  }
}
```

**.env 文件：**
```
HA_TOKEN=eyJhbGciOiJIUzI1NiIs...
```

获取 Long-Lived Access Token：Home Assistant → 用户资料 → 安全 → 创建长期访问令牌。

---

# 网页搜索配置

```json5
{
  "webSearch": {
    "provider": "duckduckgo",    // "duckduckgo"（默认，免费）| "brave"
    "maxResults": 5              // 返回结果数量上限
    // "apiKeyEnv": "BRAVE_SEARCH_API_KEY"  // 仅 Brave 需要
  }
}
```

**DuckDuckGo** — 免费，无需 API Key，默认选项。

**Brave Search** — 需要 API Key：
```json5
{
  "webSearch": {
    "provider": "brave",
    "apiKeyEnv": "BRAVE_SEARCH_API_KEY",
    "maxResults": 5
  }
}
```

**.env 文件：**
```
BRAVE_SEARCH_API_KEY=BSA...
```

获取 API Key：访问 [brave.com/search/api](https://brave.com/search/api/) → 创建应用。

> 如果配置了 Brave 但调用失败，会自动降级到 DuckDuckGo。

---

# Shell 执行配置

```json5
{
  "exec": {
    "timeoutSecs": 30,         // 单条命令超时（最大 300 秒）
    "maxOutputBytes": 8192,    // 输出截断上限
    "workDir": ".",             // 工作目录
    "skillsDir": "./skills"    // 技能脚本目录（自动加入 PATH）
  }
}
```

> 安全说明：`exec` 工具执行命令时会清除环境变量，只保留 `PATH`、`HOME=/app`、`TERM=dumb`、`LANG=C.UTF-8`，防止 Agent 通过 `env` 命令泄露 API Key 等敏感信息。

---

# 自动备份配置

```json5
{
  "backup": {
    "enabled": true,           // 总开关（默认 true）
    "dir": "./backups",        // 存储目录
    "intervalHours": 24,       // 最小间隔（小时）
    "retentionDays": 7,        // 保留天数，超过自动清理
    "maxSizeMb": 200           // 备份总大小上限（MB），超过从最旧开始删除
  }
}
```

网关进程利用现有 60s cron tick 自动检查：读取 `backups/` 中最新 `.tar.gz` 的修改时间，超过 `intervalHours` 才执行 `tar czf` 打包。备份内容包括二进制、配置、密钥、会话、记忆、技能目录（仅打包存在的文件）。清理采用双重策略：先按年龄删除超过 `retentionDays` 的旧备份，再检查总大小——若超过 `maxSizeMb` 则从最旧的开始删除直到低于上限。

在 `tools.allow` 中加入 `"backup"` 可让 Agent 通过备份工具管理：
- `status` — 查看备份状态
- `enable` / `disable` — 开关自动备份（通过 `backups/state.json` 持久化，跨重启保持）
- `run` — 立即执行一次备份

---

# 文件操作工具

在 `tools.allow` 中加入 `"file_read"`、`"file_write"`、`"file_edit"`、`"file_find"` 可让 Agent 直接操作文件，比通过 `exec` 拼 `cat`/`sed`/`grep` 更可靠。

无需额外配置，所有工具零依赖、零常驻内存。安全模型与 `exec` 一致（无路径限制，通过 `tools.allow` 控制启用）。

| 工具 | 功能 | 输入参数 |
|------|------|---------|
| `file_read` | 读取文件内容，返回带行号的文本 | `path`（必填）, `offset`（起始行号,1-based）, `limit`（行数） |
| `file_write` | 写入文件，自动创建父目录 | `path`（必填）, `content`（必填） |
| `file_edit` | 精确字符串替换 | `path`（必填）, `old_string`（必填）, `new_string`（必填）, `replace_all`（默认 false） |
| `file_find` | 按文件名/内容搜索 | `path`（搜索目录,默认"."）, `pattern`（文件名子串）, `content`（内容搜索）, `max_depth`（默认10） |

**限制：**
- `file_read`：二进制文件自动检测并拒绝，大文件截断到 64KB
- `file_edit`：默认单次替换，`old_string` 出现 0 次或 >1 次时报错（含匹配数量）
- `file_find`：最多返回 50 条结果，跳过二进制文件

**编码注意事项：**
- `file_write` 输出为 UTF-8（Rust 字符串保证）
- 写含中文/非 ASCII 文字的文件时，建议在 `content` 开头加 UTF-8 BOM（`\uFEFF`），确保 Telegram 手机端等客户端正确识别编码
- 详见[故障排除 §9.1](troubleshooting.md#91-中文文件在-telegram-手机端显示乱码)

---

# MCP 配置

[Model Context Protocol](https://modelcontextprotocol.io/) 允许连接外部工具服务器：

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

MCP 服务器通过 stdio 通信（JSON-RPC），启动时自动发现工具并注册到 Agent。
