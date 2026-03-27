# OpenClaw Rust Gateway — 编码规范

本文档定义 OpenClaw Rust 网关项目的编码规范。目标：代码一致、内存可控、在 200MB 嵌入式设备上稳定运行。

---

## 1. 命名规范

| 类别 | 风格 | 示例 |
|------|------|------|
| 函数、变量、模块 | `snake_case` | `fn poll_updates()`, `let chat_id` |
| 类型、Trait、枚举 | `CamelCase` | `struct AgentConfig`, `enum StopReason` |
| 常量、静态变量 | `SCREAMING_SNAKE_CASE` | `const MAX_TOOL_ITERATIONS: usize = 10;` |
| 类型参数 | 单个大写字母或短 CamelCase | `<T>`, `<S: SttProvider>` |

文件名一律 `snake_case.rs`。模块目录使用 `mod.rs` 作为入口。

---

## 2. 错误处理

### 模块内部：`thiserror` 定义领域错误

每个模块定义自己的错误枚举，明确列出所有可能的失败原因：

```rust
#[derive(Debug, thiserror::Error)]
pub enum TelegramError {
    #[error("HTTP 请求失败: {0}")]
    Http(#[from] reqwest::Error),

    #[error("API 返回错误: {description}")]
    Api { error_code: i32, description: String },

    #[error("JSON 解析失败: {0}")]
    Deserialize(#[from] serde_json::Error),
}
```

### 顶层入口：`anyhow` 统一处理

`main.rs` 和顶层编排函数使用 `anyhow::Result`，简化跨模块错误传播：

```rust
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let config = Config::load("config.json5").await?;
    // ...
    Ok(())
}
```

### 严禁规则

- **禁止 `unwrap()` / `expect()`**，唯一例外：程序启动阶段（加载配置、初始化日志）可用 `expect("说明原因")`。
- **全面使用 `?` 操作符**传播错误，不要手写 `match` 再 `return Err`。
- 涉及外部 API 的调用必须有超时（`reqwest::Client` 设置 `timeout`）。

---

## 3. 内存规则

本项目目标常驻 4-8MB（Rust 进程），以下规则强制执行：

### 3.1 复用 HTTP 客户端

全局创建一个 `reqwest::Client` 实例，通过参数传递。`Client` 内部维护连接池，复用 TLS 会话：

```rust
let client = reqwest::Client::builder()
    .timeout(Duration::from_secs(60))
    .pool_max_idle_per_host(2)
    .build()?;
```

**禁止**在循环或函数内部反复 `Client::new()`。

### 3.2 音频缓冲区预分配

音频缓冲使用 `Vec<u8>` 预分配，处理完后 `clear()` 保留容量，避免反复分配：

```rust
let mut audio_buf: Vec<u8> = Vec::with_capacity(256 * 1024); // 256KB
// 每次使用前
audio_buf.clear(); // 长度归零，容量不变
```

### 3.3 会话历史滑动窗口

会话消息保留最近 20 条（10 轮对话）。超出时从头部移除：

```rust
const MAX_SESSION_MESSAGES: usize = 20;

if messages.len() > MAX_SESSION_MESSAGES {
    messages.drain(..messages.len() - MAX_SESSION_MESSAGES);
}
```

### 3.4 避免不必要的克隆

- 优先使用 `&str` 引用而非 `String`。
- 需要"可能借用、可能拥有"的场景用 `Cow<'_, str>`：

```rust
fn build_prompt<'a>(template: &'a str, name: &'a str) -> Cow<'a, str> {
    if name.is_empty() {
        Cow::Borrowed(template)
    } else {
        Cow::Owned(template.replace("{name}", name))
    }
}
```

### 3.5 谨慎使用 `Arc<Mutex<>>`

本项目使用单线程 tokio 运行时。大多数状态通过可变引用传递，不需要并发原语。

**唯一例外**：当同一状态需要在多个独立组件间共享时（如 `ChatContext` 在 AgentRuntime、MemoryTool、CronTool 之间共享），使用 `Arc<Mutex<>>` + `tokio::sync::Mutex`。仅在确实需要跨组件共享时才使用，优先考虑参数传递。

---

## 4. 模块组织

```
src/
├── main.rs                      # 入口 + 多通道调度 + cron 执行 + 优雅停机
├── lib.rs                       # 库 crate re-export（供集成测试使用）
├── config.rs                    # Config 结构体 + JSON5 加载 + 环境变量替换
├── error.rs                     # GatewayError 统一错误枚举
├── channel/
│   ├── mod.rs                   # Channel trait 定义（含 edit_message 默认实现）
│   ├── types.rs                 # IncomingMessage / ChatMessage / StreamEvent 等
│   ├── telegram.rs              # Telegram Bot API 长轮询实现
│   ├── cli.rs                   # CLI 通道（stdin/stdout）
│   └── http_api.rs              # HTTP REST API 通道（raw TCP）
├── provider/
│   ├── mod.rs                   # LlmProvider / SttProvider / TtsProvider trait
│   ├── llm/
│   │   ├── mod.rs               # LlmProvider trait（含 chat_stream 默认实现）
│   │   ├── claude.rs            # Anthropic Messages API v1 + SSE 流式
│   │   └── openai_compat.rs     # OpenAI Chat Completions 兼容 + SSE 流式
│   ├── stt/
│   │   ├── mod.rs
│   │   └── groq.rs              # Groq Whisper
│   └── tts/
│       ├── mod.rs
│       ├── edge.rs              # Edge TTS WebSocket（默认，免费）
│       ├── openai.rs            # OpenAI-compatible TTS
│       └── elevenlabs.rs        # ElevenLabs TTS
├── agent/
│   ├── mod.rs
│   ├── react_loop.rs            # ReAct 循环 + AgentRuntime + 自动压缩
│   └── context.rs               # System prompt 组装（记忆 + 工具 + 压缩提示）
├── memory/
│   ├── mod.rs
│   └── store.rs                 # MemoryStore：SHARED/ + per-chat MEMORY.md + 日志 + 搜索 + 自适应注入
├── tools/
│   ├── mod.rs                   # Tool trait + ToolRegistry
│   ├── ha_control.rs            # Home Assistant REST API
│   ├── web_fetch.rs             # HTTP GET
│   ├── get_time.rs              # 当前时间
│   ├── cron.rs                  # 定时任务 + cron_matches
│   ├── memory.rs                # 记忆管理（6 action + scope 参数）+ ChatContext
│   ├── exec.rs                  # Shell 命令执行 + skills_dir PATH
│   ├── web_search.rs            # DuckDuckGo HTML 搜索
│   └── mcp.rs                   # MCP 客户端 (stdio JSON-RPC) + McpProxyTool
└── session/
    ├── mod.rs
    └── store.rs                 # JSONL 会话持久化 + 滑动窗口
```

规则：
- **`mod.rs`** 放 trait 定义和模块公开接口。
- **每个 trait 实现独占一个文件**（如 `groq.rs` 实现 `SttProvider`）。
- **`types.rs`** 放该模块或跨模块的数据结构。
- 模块间通过 trait 引用，不直接依赖具体实现。

---

## 5. 异步规则

### 单线程运行时

```rust
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> { /* ... */ }
```

`current_thread` 相比多线程运行时省约 1MB 内存。网关是 I/O 密集型，单线程足够。

### 禁止阻塞调用

以下操作在 async 上下文中 **禁止**：
- `std::thread::sleep()` — 用 `tokio::time::sleep()`
- `std::fs::read()` — 用 `tokio::fs::read()`
- `std::io::stdin().read_line()` — 不适用本项目

### 超时

所有外部 API 调用加超时：

```rust
tokio::time::timeout(Duration::from_secs(30), api_call()).await??;
```

---

## 6. 依赖管理

### 原则：最小依赖，手写一切

本项目**不使用**：
- Web 框架（无 axum / actix-web / warp）
- ORM（无数据库）
- Telegram 框架（无 teloxide / frankenstein）
- 日志门面以外的额外日志库

所有 Telegram API、Claude API、Home Assistant API 调用基于 `reqwest` 手写。WebSocket 使用 `tokio-tungstenite`。

### 当前依赖清单

已有的非核心依赖：`bytes`（流式响应字节处理）、`futures-util`（SinkExt/StreamExt + BoxStream）。

参见 `Cargo.toml`。新增依赖需说明理由，优先选择：
1. 零或少传递依赖
2. `no_std` 兼容（如有可能）
3. 被广泛使用、维护活跃的 crate

---

## 7. 测试

### 单元测试

每个模块文件底部放 `#[cfg(test)]` 块：

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

测试代码中允许 `unwrap()`。

### 集成测试

放在 `tests/integration/` 目录下，通过 `tests/integration_tests.rs` 入口文件组织，使用 `wiremock` mock 外部 API：

```
tests/
├── integration_tests.rs          # 入口（mod integration;）
├── integration/
│   ├── mod.rs                    # 模块声明
│   ├── mcp_client.rs             # MCP 客户端 e2e（mock Python 服务端）
│   ├── mcp_timeout.rs            # MCP 超时测试
│   ├── http_api.rs               # HTTP API 通道集成测试
│   ├── exec_skills.rs            # exec 工具 skills_dir PATH 测试
│   ├── web_search.rs             # DuckDuckGo 搜索 wiremock 测试
│   ├── edge_tts.rs               # Edge TTS 真实合成（Opus/MP3）
│   ├── google_stt.rs             # Google Cloud STT 真实语音识别
│   ├── volcengine_stt.rs         # 火山引擎 STT 真实语音识别
│   └── volcengine_tts.rs         # 火山引擎 TTS 真实合成
└── fixtures/
    ├── mock_mcp_server.py        # Mock MCP 服务端（echo + add）
    ├── slow_mcp_server.py        # 慢 MCP 服务端（5s sleep）
    ├── test_speech.wav            # 测试用 WAV 音频
    └── openai_compat/            # OpenAI API 响应 fixture JSON
```

### 禁止 `#[ignore]`

**所有测试必须运行，禁止使用 `#[ignore]` 标记。** 需要真实 API Key 的集成测试（Edge TTS、Google STT、火山引擎等）通过容器运行时 `--env-file .env` 注入凭据。缺少 key 时测试内部自行 skip（`return`），但不标记 `#[ignore]`。

### Docker 测试流程

```bash
# 构建测试镜像（仅编译，不运行 — 构建层无 API key）
docker build --target tester -t test .

# 运行全部测试（挂载 .env 注入 API Keys）
docker run --rm --env-file .env test
```

tester 阶段分两步：`docker build` 编译测试二进制（可缓存），`docker run --env-file .env` 运行测试（有 API key）。期望结果：**0 ignored, 0 failed**。

---

## 8. 日志

使用 `tracing` crate 的结构化日志宏：

```rust
use tracing::{info, warn, error, debug};

info!(chat_id = %msg.chat.id, "收到新消息");
warn!(api = "claude", status = %resp.status(), "API 返回非 200");
error!(error = %e, "Telegram 轮询失败，5 秒后重试");
debug!(len = audio_buf.len(), "音频缓冲区大小");
```

规则：
- `error!` — 需要关注的失败（API 不可达、解析失败）
- `warn!` — 可恢复的异常（重试、降级）
- `info!` — 关键业务事件（收到消息、发送回复、工具调用）
- `debug!` — 调试细节（缓冲区大小、请求/响应体）
- 使用结构化字段（`key = value`），不要拼接字符串到消息模板里。
- 生产环境默认 `info` 级别，通过 `RUST_LOG` 环境变量调整。

---

## 9. Serde 序列化

### 配置类型：宽松模式 + 默认值

本项目所有配置结构体使用 `#[serde(default)]`，**不使用** `deny_unknown_fields`。原因：
- 用户配置文件可能包含尚未实现的字段（向前兼容）
- 默认值让用户只需配置关心的部分

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TtsConfig {
    pub auto: String,       // 默认 "inbound"
    pub provider: String,   // 默认 "edge"
    pub max_text_length: usize,
}
```

### 外部 API 响应：宽松模式

解析 Telegram / Claude / Groq 等外部 API 的 JSON 时，同样**不加** `deny_unknown_fields`，允许 API 新增字段而不报错。只定义自己需要的字段。

### JSON 字段命名

与 JSON API 交互的结构体统一使用 `rename_all`：

```rust
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeMessage {
    pub role: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
}
```

对于 `snake_case` 风格的 API（如 Telegram），使用 `#[serde(rename_all = "snake_case")]` 或直接保持字段名一致。

---

## 10. 二进制体积优化

`Cargo.toml` 中已配置 release 构建优化：

```toml
[profile.release]
opt-level = "z"      # 优化体积而非速度
lto = true           # 链接时优化，消除死代码
codegen-units = 1    # 单编译单元，最大优化机会
panic = "abort"      # 不生成 unwind 表，减少体积
strip = true         # 剥离调试符号
```

此配置无需改动。交叉编译到目标设备时使用 `musl` 静态链接：

```bash
cargo build --release --target x86_64-unknown-linux-musl
```

---

## 11. 可复现构建规范

本项目要求 Docker 构建产物在相同输入下 bit-for-bit 一致。以下六项措施必须同时满足：

### 11.1 工具链版本锁定

`rust-toolchain.toml` 必须固定到具体版本号（如 `"1.84.0"`），禁止使用 `"stable"` / `"nightly"` 等浮动通道。

### 11.2 依赖版本锁定

`Cargo.lock` 必须提交到仓库。Docker 构建使用 `--locked` 标志，确保依赖版本与 lockfile 完全一致：

```bash
cargo build --release --locked --target <triple>
```

本地开发更新依赖后，必须同时提交 `Cargo.lock` 的变更。

### 11.3 禁用增量编译

Docker 构建环境中设置 `CARGO_INCREMENTAL=0`，消除增量编译缓存带来的时序差异。

### 11.4 零化时间戳

设置 `SOURCE_DATE_EPOCH=0`，消除编译器在二进制中嵌入的构建时间戳。

### 11.5 路径重映射

通过 `RUSTFLAGS` 设置 `--remap-path-prefix`，将构建路径和用户路径映射为固定值，消除宿主路径嵌入：

```bash
RUSTFLAGS="--remap-path-prefix=/app=. --remap-path-prefix=/root=~"
```

### 11.6 单编译单元

`Cargo.toml` 的 `[profile.release]` 中已设置 `codegen-units = 1` 和 `lto = true`，消除并行编译的随机性。此配置不可移除。

### 11.7 验证方法

使用 `scripts/docker-build.sh --verify <target>` 连续构建两次并对比 SHA-256：

```bash
./scripts/docker-build.sh --verify x86_64
# Build 1: sha256=abc123...
# Build 2: sha256=abc123...
# PASS: Builds are bit-for-bit identical
```

---

## 12. 开发流程规范

新增功能必须遵循以下流程，**严格按顺序执行**：

### 12.1 设计先行

在 `docs/zh/design/` 对应子文件中补充相关章节：
- 架构设计（数据结构、接口定义）
- 执行流程（时序、状态转换）
- 内存开销估算
- 配置项变更

设计文档是实现的唯一依据。编码前必须完成设计审查。

### 12.2 编码实现

按设计文档实现，遵循本文档（`rust-conventions.md`）的所有编码规范。

### 12.3 测试验证

为每个新模块编写 `#[cfg(test)]` 单元测试，覆盖：
- 正常路径（happy path）
- 边界情况（空输入、超限、不存在的资源）
- 序列化兼容（新旧数据格式）

### 12.4 审视对齐

实现完成后，逐项检查：
- 代码是否完整覆盖设计文档中的所有要求
- 设计文档是否准确反映最终实现（如有偏差须同步更新）
- 编码规范文档是否需要更新（如引入了新模式）
