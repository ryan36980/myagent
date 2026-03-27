> [设计文档](README.md) > 核心规范与 Trait 定义

# 第一章：Rust 项目规范与最佳实践

## 1.1 项目目录结构

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
│   ├── main.rs                        # 入口：加载配置、构建 AgentRuntime、启动主循环
│   ├── config.rs                      # JSON5 解析、环境变量替换
│   ├── error.rs                       # GatewayError 统一错误枚举
│   ├── lib.rs                        # Library crate re-exports
│   ├── channel/
│   │   ├── mod.rs                     # Channel trait 定义
│   │   ├── types.rs                   # IncomingMessage / OutgoingMessage 等统一类型
│   │   ├── telegram.rs                # Telegram Bot API 长轮询实现
│   │   └── feishu.rs                  # 飞书/Lark WebSocket 长连接实现
│   ├── provider/
│   │   ├── mod.rs                     # LlmProvider / SttProvider / TtsProvider trait
│   │   ├── llm/claude.rs             # Anthropic Messages API v1
│   │   ├── stt/groq.rs               # Groq Whisper multipart 上传
│   │   ├── stt/google.rs             # Google Cloud Speech-to-Text REST v1
│   │   ├── stt/volcengine.rs         # 火山引擎（豆包）STT WebSocket 二进制协议
│   │   ├── tts/mod.rs                # TtsProvider trait + AudioFormat 枚举
│   │   ├── tts/edge.rs               # Edge TTS WebSocket（WebM→OGG 自动转换）
│   │   ├── tts/volcengine.rs         # 火山引擎 TTS HTTP REST（原生 OGG/Opus）
│   │   ├── tts/openai.rs             # OpenAI-compatible TTS
│   │   ├── tts/elevenlabs.rs         # ElevenLabs TTS
│   │   └── tts/webm_to_ogg.rs       # WebM Opus → OGG Opus 容器转换
│   ├── agent/
│   │   └── react_loop.rs             # ReAct 循环（最多 10 次迭代）
│   ├── memory/
│   │   ├── mod.rs                     # 模块声明
│   │   └── store.rs                   # MemoryStore：MEMORY.md + 日志 + 搜索 + 自适应注入
│   ├── tools/
│   │   ├── mod.rs                     # Tool trait + 注册表
│   │   ├── ha_control.rs             # Home Assistant REST API
│   │   ├── web_fetch.rs              # HTTP GET
│   │   ├── get_time.rs               # chrono 当前时间
│   │   ├── cron.rs                    # 定时任务 + cron_matches + 执行支持
│   │   └── memory.rs                  # MemoryTool（6 action）+ ChatContext
│   └── session/
│       └── store.rs                   # JSONL 会话持久化 + 滑动窗口截断
└── tests/
    ├── fixtures/
    └── integration/
```

## 1.2 依赖清单

| Crate | 版本 | 用途 |
|-------|------|------|
| `tokio` | 1.x | 异步运行时（current_thread），features: rt, macros, time, fs, signal, sync |
| `reqwest` | 0.12 | HTTP 客户端，features: json, rustls-tls, multipart |
| `serde` / `serde_json` | 1.x | 序列化框架 + JSON 解析 |
| `thiserror` | 2.x | 声明式错误类型（GatewayError 枚举） |
| `anyhow` | 1.x | 顶层函数错误传播 |
| `tracing` | 0.1 | 结构化日志 |
| `tracing-subscriber` | 0.3 | 日志后端，features: env-filter |
| `tokio-tungstenite` | 0.24 | WebSocket 客户端（Edge TTS），features: rustls-tls-webpki-roots |
| `json5` | 0.4 | 配置文件解析（支持注释） |
| `async-trait` | 0.1 | 异步 trait 支持 |
| `chrono` | 0.4 | 日期时间，features: clock, serde |
| `uuid` | 1.x | UUID v4 生成 |
| `base64` | 0.22 | Base64 编解码 |
| `url` | 2.x | URL 解析构建 |
| `sha2` | 0.10 | SHA-256 哈希（OAuth PKCE code_challenge）|

开发依赖：`wiremock` 0.6（HTTP mock）、`tokio-test` 0.4、`tempfile` 3（临时目录测试）。

## 1.3 错误处理策略

采用 **thiserror + anyhow** 双层模式：

- **模块内部**：返回 `Result<T, GatewayError>`，每个错误变体对应一个故障域。
- **顶层调度**：使用 `anyhow::Result`，允许不同错误类型自由传播。
- 可恢复错误（网络超时、API 限流）通过 `tracing::warn!` 记录后重试或跳过。
- 不可恢复错误（配置缺失、TLS 初始化失败）通过 `anyhow::bail!` 终止进程。
- **禁止**在非测试代码中使用 `unwrap()` / `expect()`，一律使用 `?` 传播。

## 1.4 内存优化规则

1. 音频缓冲使用 `Vec::with_capacity(256 * 1024)` 预分配，避免扩容。
2. 大对象处理完毕后立即 `drop`，或通过 `{ }` 块作用域释放。
3. 消息路径上优先使用 `&str`，仅在跨 await 边界时 `to_string()`。
4. 会话历史默认无限（`historyLimit: 0`），依靠自动压缩控制上下文大小。
5. TTS 音频分块接收，逐块写入缓冲区。
6. 使用 `tokio` `current_thread` 模式，避免多线程开销。
7. 不引入 LRU 缓存等有状态中间层。

## 1.5 二进制体积优化

```toml
[profile.release]
opt-level = "z"        # 最小体积优化
lto = true             # 链接时优化，消除死代码
codegen-units = 1      # 单编译单元，最大化 LTO
panic = "abort"        # 不保留 unwind 表
strip = true           # 剥离调试符号
```

效果：release 二进制从 ~10-15MB 压缩到 ~2-4MB。

## 1.6 交叉编译目标

| 目标三元组 | 适用设备 |
|-----------|---------|
| `aarch64-unknown-linux-musl` | 树莓派 4/5、RK3588（64 位 ARM） |
| `armv7-unknown-linux-musleabihf` | 树莓派 2/3、NanoPi（32 位 ARM） |
| `x86_64-unknown-linux-musl` | 通用 Linux 服务器 |

所有目标使用 musl 静态链接，产物为单个二进制，无外部依赖。

---

# 第二章：核心 Trait 定义（五层抽象）

五层 trait 通过 `dyn` 动态分发解耦，每层可独立替换实现。

## 2.1 Channel — 通道层

```rust
#[async_trait]
pub trait Channel: Send + Sync {
    fn id(&self) -> &str;

    /// 通道偏好的音频格式（默认 OGG/Opus）。
    /// 类似 C++ 虚函数 —— 子类可覆盖，agent 层在 TTS 合成前读取此值。
    fn preferred_audio_format(&self) -> AudioFormat {
        AudioFormat::OggOpus
    }

    async fn poll(&self) -> Result<Vec<IncomingMessage>, GatewayError>;
    async fn send_text(&self, chat_id: &str, text: &str) -> Result<String, GatewayError>; // 返回 message_id
    async fn send_voice(&self, chat_id: &str, audio: &[u8]) -> Result<(), GatewayError>;
    async fn download_voice(&self, file_ref: &str) -> Result<Vec<u8>, GatewayError>;
    async fn edit_message(&self, _chat_id: &str, _msg_id: &str, _text: &str) -> Result<(), GatewayError> {
        Ok(()) // 默认 no-op，支持编辑的通道（如 Telegram、飞书）覆盖此方法
    }
    async fn send_typing(&self, _chat_id: &str) -> Result<(), GatewayError> {
        Ok(()) // 默认 no-op，Telegram 覆盖为 sendChatAction(typing)
    }
}
```

## 2.2 LlmProvider — 大语言模型层

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn chat(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse, GatewayError>;

    /// 流式聊天请求，返回 SSE 事件流。
    /// 默认实现：调用 chat() 后包装为单事件流。支持 SSE 的 provider 应覆盖此方法。
    async fn chat_stream(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<BoxStream<'static, Result<StreamEvent>>, GatewayError> {
        // 默认：chat() → 包装为 stream
    }
}
```

## 2.3 SttProvider — 语音识别层

```rust
#[async_trait]
pub trait SttProvider: Send + Sync {
    async fn transcribe(&self, audio: &[u8], mime: &str) -> Result<String, GatewayError>;
}
```

## 2.4 TtsProvider — 语音合成层

```rust
/// 音频输出格式枚举。默认 OGG/Opus —— 所有通道的统一格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Mp3,
    OggOpus, // 默认
}

#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// 合成语音。`format` 为 Channel 声明的偏好格式。
    /// TTS 提供商内部保证输出格式匹配：
    /// - 原生支持该格式的直接输出（火山引擎 → OGG/Opus）
    /// - 不支持的内部转换（Edge TTS → WebM Opus → OGG Opus）
    async fn synthesize(&self, text: &str, format: AudioFormat) -> Result<Vec<u8>, GatewayError>;
}
```

**音频格式统一架构（AudioFormat）：**

```
Channel::preferred_audio_format()  →  AudioFormat::OggOpus (默认)
        ↓
handle() 提取 audio_format
        ↓
try_synthesize(text, audio_format)
        ↓
TtsProvider::synthesize(text, format)
        ↓
┌─────────────────────────────────────────────┐
│ Edge TTS:     WebM Opus → webm_to_ogg_opus()│ ← 内部自动转换
│ 火山引擎:    直接请求 ogg_opus              │
│ OpenAI TTS:  原生 OGG/Opus                  │
│ ElevenLabs:  原生格式（忽略 format hint）    │
└─────────────────────────────────────────────┘
        ↓
统一 OGG/Opus 输出 → Channel::send_voice()
```

**设计原则：**
- 格式转换责任在 TTS 提供商内部，不泄漏到 Channel 层
- `AudioFormat` 枚举保留扩展性，但当前所有通道统一使用 OggOpus
- 新增通道如需不同格式，覆盖 `preferred_audio_format()` 即可
- 新增 TTS 提供商需保证能输出 OGG Opus（原生或转换）

## 2.5 Tool — 工具层

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> serde_json::Value;
    async fn execute(&self, input: serde_json::Value) -> Result<String, GatewayError>;
}
```

## 2.6 统一消息类型

定义在 `src/channel/types.rs`，作为跨层公共协议：

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
    pub voice: Option<Vec<u8>>,    // Opus 音频
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

/// 流式 LLM 事件（chat_stream 返回的事件流元素）
pub enum StreamEvent {
    TextDelta(String),                                    // 增量文本
    ToolUse { id: String, name: String, input: Value },   // 完整工具调用块
    Done { stop_reason: StopReason },                     // 生成结束
}

pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}
```

## 2.7 AgentRuntime 组装

核心调度器，持有所有 trait 对象的 `Box<dyn ...>`，在 `main` 中完成依赖注入：

```rust
pub struct AgentRuntime {
    pub llm: Box<dyn LlmProvider>,
    pub stt: Box<dyn SttProvider>,
    pub tts: Box<dyn TtsProvider>,
    pub tools: ToolRegistry,
    pub sessions: SessionStore,
    pub memory: Arc<MemoryStore>,                    // 长期记忆存储
    pub chat_context: Arc<Mutex<ChatContext>>,        // 当前 (channel, chat_id) 共享上下文
    pub system_prompt: String,
    pub max_iterations: usize,
    pub tts_auto_mode: String,               // "inbound"/"always"/"tagged"/其他(off)
    pub auto_compact: bool,                  // 自动压缩对话上下文（默认 true）
    pub compact_ratio: f64,                  // 压缩保留比例（默认 0.4）
    pub response_prefix: String,             // 回复前缀模板
    pub provider_name: String,               // 用于模板替换
    pub model_name: String,                  // 用于模板替换
    pub thinking_level: String,              // 用于模板替换
}
```

`ChatContext` 在 `src/tools/memory.rs` 中定义，通过 `Arc<Mutex<>>` 在 AgentRuntime、MemoryTool、CronTool 之间共享：

```rust
pub struct ChatContext {
    pub channel: String,
    pub chat_id: String,
}
```

使用 `dyn` 而非泛型的原因：同一 Vec 可放不同实现；运行时按配置选择；I/O 密集型场景下虚函数调用开销可忽略。
