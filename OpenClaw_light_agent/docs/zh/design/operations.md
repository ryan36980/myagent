> [设计文档](README.md) > 运维：内存、配置、构建、测试

<a id="chapter-6"></a>
## 第六章：内存预算

### 6.1 组件级分配

| 组件 | 分配 | 说明 |
|------|------|------|
| 二进制 + 静态数据 | ~2-4MB | release + strip，含 rustls 证书包 |
| tokio 运行时 | ~1-2MB | current_thread 模式 |
| reqwest + rustls | ~1MB | 连接池 + TLS 会话缓存 |
| 音频缓冲（预分配） | ~256KB | 60 秒 Opus@32kbps 约 240KB |
| 会话上下文 | ~64KB | 20 条 ChatMessage，每条 ~3KB |
| MemoryStore + ChatContext | ~200B | PathBuf + 2×usize + Arc<Mutex<ChatContext>> 常驻 |
| ChatQueueManager | ~64B | HashMap 常驻 + 每活跃 chat ~860B |
| FailoverLlmProvider | ~430B | 2 个 fallback provider + cooldown state |
| LoopDetector | ~1.2KB/请求 | 30 条 ToolCallRecord × ~40B（hash fingerprint） |
| StreamingWriter | ~4.2KB/活跃流 | buffer + struct，非活跃时 0 |
| Serde 缓冲 | ~32KB | JSON 序列化/反序列化临时缓冲 |
| **Rust 进程合计** | **~4-8MB** | |

### 6.2 峰值场景

| 场景 | 额外内存 | 说明 |
|------|---------|------|
| 60 秒语音下载 | +256KB | 传给 STT 后 drop |
| Claude API 响应 | +32KB | 解析后释放原始 JSON |
| Edge TTS 音频接收 | +256KB | 发送后 clear |
| Groq STT 上传 | +256KB | multipart 构造时复制，上传后释放 |
| 火山引擎 STT | +256KB | gzip 压缩/解压 + WebSocket 帧，完成后释放 |
| 记忆注入 / 搜索 | +8KB | build_context ≤max_context_bytes，搜索逐文件读取 |
| **最大同时峰值** | **+512KB** | 下载/合成不会同时发生 |

### 6.3 系统总预算

| 层级 | 内存 |
|------|------|
| Linux 内核 + 基础服务 | ~15-20MB |
| Rust 网关（常驻 / 峰值） | ~4-8MB / ~5-9MB |
| **系统总计** | **~20-28MB** |
| **200MB 设备剩余** | **~171-180MB** |

### 6.4 与 Node.js 对比

| 指标 | Node.js | Rust | 节省 |
|------|---------|------|------|
| 常驻内存 | 95-125MB | 4-8MB | ~92% |
| 峰值内存 | 155-165MB | 5-9MB | ~95% |
| 二进制体积 | ~200MB | ~3MB | ~98% |

---

<a id="chapter-7"></a>
## 第七章：配置系统

### 7.1 格式与加载

使用 **JSON5** 格式（`json5` crate），支持 `//` 注释和尾随逗号。配置文件路径默认 `openclaw.json`，可通过 `--config` 指定。

加载管线：`文件文本 → ${VAR_NAME} 环境变量替换 → JSON5 解析 → Config 结构体`。

环境变量不存在时替换为空字符串，同时 `tracing::warn!` 发出警告。

### 7.2 Config 结构体定义

```rust
pub struct GatewayConfig {
    pub provider: String,                           // "anthropic" / "groq" / "deepseek" / "openai"
    pub model: String,                              // "claude-sonnet-4-5-20250929"
    pub provider_config_override: ProviderConfigOverride, // base_url / api_key_env / max_tokens 覆盖
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

pub struct ProviderConfig {                         // 运行时组装（从 provider + model + override 合成）
    pub api_key: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub base_url: Option<String>,
}

pub struct ProviderConfigOverride {                 // 用户可选覆盖
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub max_tokens: Option<u32>,
}

pub struct ChannelsConfig {
    pub telegram: TelegramConfig,
    pub feishu: FeishuConfig,                       // WebSocket 长连接
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

所有结构体使用 `#[serde(rename_all = "camelCase")]`，省略 `deny_unknown_fields` 以忽略未知字段确保向前兼容。带默认值的字段使用 `#[serde(default = "...")]` 标注。

### 7.3 多通道配置

`ChannelsConfig` 中每个通道为 `Option<T>`，启动时按存在性决定实例化：

```rust
let mut channels: Vec<Box<dyn Channel>> = Vec::new();
if let Some(tg) = &config.channels.telegram {
    channels.push(Box::new(TelegramChannel::new(tg)?));
}
```

### 7.4 示例配置

完整示例见 `config/openclaw.json.example`。关键字段：

```json5
{
  "provider": "anthropic", "model": "claude-sonnet-4-5-20250929",
  "channels": { "telegram": { "botToken": "${TELEGRAM_BOT_TOKEN}", "allowedUsers": [] } },
  "messages": {
    "tts": { "auto": "inbound", // "inbound" | "always" | "tagged" | 其他值=off
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
    "compactRatio": 0.4,     // 压缩保留比例
    "followupDebounceMs": 2000, // pending 消息 debounce 窗口（毫秒）
    "queueMode": "interrupt", // "interrupt"（新消息中断当前 turn）| "queue"（收集后合并）
    // "contextFiles": ["./SOUL.md"],  // 启动时加载到系统提示
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
## 第八章：构建与部署

### 8.1 本地开发

```bash
# 设置环境变量
export TELEGRAM_BOT_TOKEN="123456:ABCxxx"
export ANTHROPIC_API_KEY="sk-ant-xxx"
export GROQ_API_KEY="gsk_xxx"      # 可选，仅语音功能需要
export HA_TOKEN="eyJhbGci..."

# 编译运行
RUST_LOG=debug cargo run -- --config ./config/openclaw.json
```

### 8.2 交叉编译

使用 `cross` 工具，通过 Docker 提供目标编译环境：

```bash
cargo install cross --git https://github.com/cross-rs/cross

cross build --release --target aarch64-unknown-linux-musl       # 树莓派 4/5
cross build --release --target armv7-unknown-linux-musleabihf   # 树莓派 2/3
cross build --release --target x86_64-unknown-linux-musl        # x86-64 服务器
```

产物：`target/{target}/release/openclaw-light`，单个静态链接二进制。

### 8.3 Docker 可复现构建

基于 `ghcr.io/rust-cross/rust-musl-cross` 镜像的多阶段构建，支持参数化目标平台，产出 bit-for-bit 可复现的静态二进制。

**可复现性措施：**

| # | 措施 | 实现 | 消除的不确定性 |
|---|------|------|--------------|
| 1 | 工具链版本锁定 | `rust-toolchain.toml = "1.84.0"` | 编译器漂移 |
| 2 | 依赖版本锁定 | `Cargo.lock` + `--locked` | 依赖浮动 |
| 3 | 禁用增量编译 | `CARGO_INCREMENTAL=0` | 缓存时序 |
| 4 | 零化时间戳 | `SOURCE_DATE_EPOCH=0` | 时间戳嵌入 |
| 5 | 路径重映射 | `RUSTFLAGS="--remap-path-prefix=..."` | 宿主路径嵌入 |
| 6 | 单编译单元 | `codegen-units=1` + `lto=true` | 并行随机性 |

**构建命令：**

```bash
# 单目标构建
./scripts/docker-build.sh aarch64

# 全部 3 目标
./scripts/docker-build.sh

# 可复现性验证（构建两次比对 SHA-256）
./scripts/docker-build.sh --verify x86_64
```

**目标映射：**

| 短名称 | MUSL_TARGET | Rust 三元组 |
|--------|-------------|------------|
| `aarch64` | `aarch64-musl` | `aarch64-unknown-linux-musl` |
| `armv7` | `armv7-musleabihf` | `armv7-unknown-linux-musleabihf` |
| `x86_64` | `x86_64-musl` | `x86_64-unknown-linux-musl` |

产物提取到 `dist/{triple}/openclaw-light`。

**运行时镜像（Stage 3）：** 基于 `scratch` + `busybox:musl`，提供完整 shell 环境（sh, ls, cat, grep, awk, sed, wget 等 300+ 命令）。使用 `entrypoint.sh` 启动：先以 root 修复挂载卷的文件权限（`chown -R 1000:1000`），再通过 `su` 降权到非 root 用户 `openclaw` (UID 1000) 运行主进程。这解决了 Docker Desktop for Windows 下挂载卷中文件显示为 root 所有导致应用无法写入的问题。镜像总计 ~3.5MB（二进制 + CA 证书 + busybox ~1.5MB）。

详细构建规范见 `docs/zh/specs/rust-conventions.md` 第 11 节。

### 8.4 Docker Compose 生产部署

`docker-compose.yml` 提供容器级安全加固，对标 Claude Code 的 bubblewrap 沙箱模型：

```yaml
services:
  gateway:
    image: openclaw-light:latest
    read_only: true                    # FS 只读（对标 bubblewrap FS 隔离）
    cap_drop: [ALL]                    # 丢弃全部 Linux capabilities
    cap_add: [CHOWN, SETUID, SETGID]  # entrypoint 需要：chown 修复权限 + su 降权
    mem_limit: 32m                     # 内存上限
    tmpfs:
      - /tmp:size=10m                  # 临时文件可写区
    volumes:
      - ./openclaw.json:/app/openclaw.json:ro   # 配置只读
      - ./sessions:/app/sessions                # 会话数据
      - ./auth_tokens.json:/app/auth_tokens.json # OAuth token（读写）
      - ./memory:/app/memory                    # 长期记忆
      - ./skills:/app/skills                    # Agent 脚本
    env_file:
      - .env                             # 环境变量（GROQ_API_KEY 等）
    # 不设 user: — entrypoint 以 root 启动做 chown，然后 su 到 openclaw (1000)
```

**entrypoint.sh** 启动流程：
1. `chown -R 1000:1000` 修复 memory/sessions/skills 目录权限（Docker Desktop for Windows 挂载卷文件可能显示为 root 所有）
2. `exec su -s /bin/sh openclaw -c /app/openclaw-light` 降权到 UID 1000 运行主进程

**安全层次：**

| 层次 | 措施 | 对标 Claude Code |
|------|------|-----------------|
| 文件系统 | `read_only: true` + 仅挂载必要卷 | bubblewrap FS 隔离 |
| 权限 | `cap_drop: [ALL]` + 仅加回 CHOWN/SETUID/SETGID（entrypoint 用后即弃） | 最小权限 |
| 环境变量 | exec 工具 `env_clear()` 隔离（见 §12.1） | 环境隔离防凭证泄露 |
| 用户 | entrypoint 降权到 UID 1000 非 root | 非特权用户 |
| 资源 | `mem_limit: 32m` | 资源限制 |

### 8.5 systemd 服务单元

参考文件：`deploy/openclaw-light.service`。

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

# ── 启动 ──
ExecStart=/opt/openclaw/openclaw-light --config /opt/openclaw/openclaw.json
Restart=on-failure
RestartSec=10
EnvironmentFile=/opt/openclaw/.env

# ── 安全加固 ──
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true

# ── 文件系统权限 ──
ReadWritePaths=/opt/openclaw/sessions /opt/openclaw/memory /opt/openclaw/skills /opt/openclaw/backups /opt/openclaw/auth_tokens.json
ReadOnlyPaths=/opt/openclaw/openclaw-light /opt/openclaw/openclaw.json

# ── 资源限制 ──
MemoryMax=32M
MemoryHigh=24M

# ── 日志 ──
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

### 8.6 ARM 设备部署

**交叉编译：**

```bash
# 构建 aarch64 静态二进制（musl），产物输出到 dist/aarch64-unknown-linux-musl/
./scripts/docker-build.sh aarch64
```

**部署到设备：**

```bash
# 默认读取 DEPLOY_HOST 环境变量，也可以命令行指定
DEPLOY_HOST=user@orangepi ./scripts/deploy.sh

# 或直接运行（使用已配置的 DEPLOY_HOST）
./scripts/deploy.sh
```

`deploy.sh` 完成以下工作：scp 二进制 + 配置到设备 → 安装 systemd 服务 → daemon-reload。部署后自动检测远端状态：若 `openclaw.json`、`.env` 存在且服务已 active，则自动 restart 并打印最近日志；否则输出首次安装指引。

### 8.7 注意事项

**交叉编译与产物：**
- `docker-build.sh` 通过 `docker create` + `docker cp` 从构建容器中提取二进制，容器内路径为 `/openclaw-light`（不是 `/app/`）
- 产物输出到 `dist/{triple}/openclaw-light`（如 `dist/aarch64-unknown-linux-musl/openclaw-light`）
- 当前 aarch64 二进制约 3.4MB，musl 静态链接无外部依赖，直接 scp 到设备即可运行

**systemd 沙箱：**
- `ProtectSystem=strict` 将整个文件系统设为只读，所有可写路径必须在 `ReadWritePaths` 中显式声明，否则进程写文件会得到 `EROFS (Read-only file system)`
- 沙箱只在 service 进程内生效——用 SSH 登录后 `touch` 测试不会复现 EROFS，必须通过 `journalctl -u openclaw-light` 查看实际日志
- 新增可写文件/目录时务必同步更新 service 文件的 `ReadWritePaths`

### 8.8 自动备份

网关进程内置自动备份，利用现有 60s cron tick 定期检查，满足条件时自动打包数据。

**触发机制：** 每次 cron tick 读取 `backups/` 目录中最新 `.tar.gz` 的修改时间（微秒级 metadata 操作），超过 `intervalHours` 才执行备份。首次运行或目录为空时立即备份。

**备份范围：**

| 类型 | 路径 | 说明 |
|------|------|------|
| 二进制 | `openclaw-light` | ~3.4MB，方便快速回滚 |
| 配置 | `openclaw.json` | |
| 密钥 | `.env` | |
| OAuth | `auth_tokens.json` | |
| 会话 | `sessions/` | |
| 记忆 | `memory/` | |
| 技能 | `skills/` | |

只打包实际存在的文件/目录，缺失的跳过。

**配置项：**

```json5
{
  "backup": {
    "enabled": true,        // 总开关（默认 true）
    "dir": "./backups",     // 存储目录
    "intervalHours": 24,    // 最小间隔（小时）
    "retentionDays": 7,     // 保留天数
    "maxSizeMb": 200        // 备份总大小上限（MB）
  }
}
```

**Agent 工具：** 在 `tools.allow` 中加入 `"backup"` 可让 agent 通过 `backup` 工具管理备份：

| action | 说明 |
|--------|------|
| `status` | 返回当前状态（开/关、上次备份时间、备份目录大小） |
| `enable` | 开启自动备份 |
| `disable` | 关闭自动备份 |
| `run` | 立即执行一次备份 |

运行时开关状态通过 `backups/state.json` 持久化，跨重启保持。

**清理策略：** 双重清理——先按年龄删除超过 `retentionDays` 的旧备份，再按总大小检查：若剩余备份总大小超过 `maxSizeMb`，从最旧的开始删除直到低于上限。默认上限 200MB。

**实现方式：** shell out 到 `tar czf`（设备一定有 tar），不引入新 crate。清理用 `tokio::fs::read_dir` + metadata 判断文件年龄和大小。

---

<a id="chapter-9"></a>
## 第九章：测试策略

### 9.1 单元测试

每个模块使用 `#[cfg(test)] mod tests` 编写同文件单元测试。覆盖范围：

| 模块 | 测试数 | 覆盖内容 |
|------|--------|---------|
| `config.rs` | ~18 | 环境变量替换、默认值填充、JSON5 注释解析、provider 配置、TTS 配置 |
| `error.rs` | ~3 | `From` 转换正确映射到对应变体 |
| `agent/context.rs` | 3 | system prompt 记忆注入、工具列表、压缩提示 |
| `agent/react_loop.rs` | 22 | `<speak>` 标签提取、LoopDetector、response prefix、compaction、consume_stream、react_loop、truncate_tool_result |
| `memory/store.rs` | 12 | read/append/rewrite/read_log/append_log/search/build_context |
| `tools/memory.rs` | 5 | MemoryTool 6 action |
| `tools/cron.rs` | 15 | cron_matches（range/step）、schedule_at、CronTask 序列化兼容、field_matches |
| `tools/exec.rs` | 8 | echo/exit code/stderr/timeout/truncation |
| `tools/mcp.rs` | 10 | JSON-RPC 格式、proxy 命名、输出截断 |
| `tools/web_search.rs` | 6 | DDG HTML 解析、实体解码、strip tags |
| `session/store.rs` | 6 | 滑动窗口、追加、空文件、history_limit |
| `provider/llm/mod.rs` | 3 | 默认 chat_stream() 包装逻辑 |
| `provider/llm/claude.rs` | 4 | SSE 流式解析：文本/工具/错误/max_tokens |
| `provider/llm/openai_compat.rs` | ~17 | 序列化/反序列化/wiremock roundtrip/SSE 流式 |
| `provider/tts/openai.rs` | 4 | 构造/空文本/配置 |
| `provider/tts/elevenlabs.rs` | 4 | 构造/空文本/配置 |
| `channel/telegram.rs` | 12 | 音频格式检测、消息分块（chunk_text） |

### 9.2 集成测试

放在 `tests/integration/` 目录，通过 `tests/integration_tests.rs` 入口文件组织。

| 测试文件 | 测试数 | 覆盖内容 |
|---------|--------|---------|
| `mcp_client.rs` | 4 | 启动 → list_tools → call echo/add → shutdown |
| `mcp_timeout.rs` | 1 | 2s timeout vs 5s sleep server |
| `http_api.rs` | 4 | POST /chat、404、400、channel id |
| `exec_skills.rs` | 2 | skills_dir PATH 查找、description 包含路径 |
| `web_search.rs` | 2 | wiremock DDG HTML 解析、空结果 |
| `edge_tts.rs` | 2 | Edge TTS Opus/MP3 合成（真实网络请求） |
| `google_stt.rs` | 1 | Google Cloud STT 真实语音转文字 |
| `volcengine_stt.rs` | 1 | 火山引擎 STT 真实语音转文字 |
| `volcengine_tts.rs` | 2 | 火山引擎 TTS 合成 + 空文本 |

### 9.3 测试运行

```bash
docker build --target tester -t test .   # 编译（含测试二进制）
docker run --rm --env-file .env test     # 运行全部测试（0 ignored）
```

测试规范详见 `docs/zh/specs/rust-conventions.md` 第 7 节。
