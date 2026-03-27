> [设计文档](README.md) > 模块设计与 API 协议

# 第四章：各模块详细设计

## 4.1 config.rs — 配置加载

读取 `openclaw.json`（JSON5 格式），执行 `${VAR_NAME}` 环境变量替换后反序列化为 `Config` 结构体。处理管线：

```
配置文件文本 → 环境变量替换 → JSON5 解析 → Config 结构体
```

所有 serde 结构体不使用 `deny_unknown_fields`，未知字段静默忽略，确保向前兼容。完整 Config 定义见[运维章节](operations.md)。

## 4.2 error.rs — 统一错误类型

```rust
#[derive(thiserror::Error, Debug)]
pub enum GatewayError {
    #[error("传输层错误: {0}")]   Transport(String),
    #[error("配置错误: {0}")]     Config(String),
    #[error("Agent 错误: {0}")]   Agent(String),
    #[error("STT 错误: {0}")]     Stt(String),
    #[error("TTS 错误: {0}")]     Tts(String),
    #[error("工具错误: {0}")]     Tool(String),
    #[error("会话错误: {0}")]     Session(String),
}
```

为 `reqwest::Error`、`serde_json::Error`、`std::io::Error` 实现 `From` 转换，支持 `?` 自动传播。

## 4.3 provider/llm/claude.rs — Anthropic Claude

实现 `LlmProvider` trait，调用 Anthropic Messages API v1。

```rust
pub struct ClaudeProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,           // 默认 "https://api.anthropic.com"，可通过 ProviderConfig.base_url 覆盖
    max_tokens: u32,            // 默认 4096
    thinking_budget: Option<u32>, // Extended Thinking token 预算
}
```

请求构造：POST `{base_url}/v1/messages`，headers 含 `x-api-key` 和 `anthropic-version`。body 包含 `model`、`max_tokens`、`system`、`messages`、`tools`，可选 `thinking`。

**Extended Thinking：** 通过 `agents.thinking` 配置（"off"/"low"/"medium"/"high"），映射为 `budget_tokens`（2048/8192/32768）。开启时 API version 升级到 `2025-04-14`，请求中添加 `thinking: { type: "enabled", budget_tokens }` 字段。响应中 `thinking` content block 打 debug 日志后丢弃；SSE 流中 `thinking_delta` 事件静默跳过。

**流式模式：** `chat_stream()` 发送 `"stream": true`，解析 Anthropic SSE 事件（`content_block_delta` → TextDelta, `content_block_start/stop` → ToolUse 累积, `message_delta` → Done, `thinking_delta` → 丢弃）。行级缓冲 + `futures_util::stream::unfold` 构造 `BoxStream`。

**tool_use input 安全解析：** 当 `content_block_stop` 事件到达时，累积的 `tool_input` 字符串被解析为 `serde_json::Value`。对于无参数工具（如 `get_time`，input 为 `{}`），Anthropic SSE 可能不发送任何 `input_json_delta` 事件，导致 `tool_input` 为空字符串。空字符串时默认解析 `"{}"`，且 fallback 为 `Value::Object(Default::default())` 而非 `Value::Null`——因为 Anthropic API 要求 `tool_use.input` 必须为 dictionary，`null` 会导致 400 Bad Request（`Input should be a valid dictionary`）。

**tool_use 处理：** 当 `stop_reason == "tool_use"` 时，`content` 数组中含 `type: "tool_use"` 块，由 Agent 循环提取执行后以 `tool_result` 追加到消息列表再次调用。

## 4.4 provider/stt/groq.rs — Groq Whisper

实现 `SttProvider` trait，通过 multipart/form-data 上传音频到 Groq OpenAI 兼容 API。

```rust
pub struct GroqSttProvider { client: reqwest::Client, api_key: String, model: String }
```

构造 `multipart::Form`：`file`（音频字节）、`model`（"whisper-large-v3-turbo"）、`language`（"zh"）。POST 到 `https://api.groq.com/openai/v1/audio/transcriptions`，解析响应 JSON 中的 `text` 字段。

**延迟初始化：** `GROQ_API_KEY` 环境变量不在启动时强制要求，而是 `unwrap_or_default()` 允许为空。实际检查推迟到 `transcribe()` 调用时——若 `api_key` 为空则返回明确错误。这使得不需要语音功能的部署可以跳过配置 Groq API key。

### 4.4.2 provider/stt/volcengine.rs — 火山引擎（豆包）

实现 `SttProvider` trait，通过 WebSocket 二进制协议连接火山引擎大模型语音识别服务（v3 bigmodel）。

```rust
pub struct VolcengineSttProvider { app_id: String, access_token: String, cluster: String, ws_url: String }
```

**二进制协议（v3 bigmodel）：** 4 字节 header + 4 字节 sequence（大端 i32）+ 4 字节 payload_size（大端 u32）+ gzip(payload)。Header 编码 version、header_size、msg_type、msg_type_specific、serial_method、compression_type。

**transcribe 流程：**
1. WebSocket 连接到 `wss://openspeech.bytedance.com/api/v3/sauc/bigmodel`，携带认证头（`X-Api-App-Key`、`X-Api-Access-Key`、`X-Api-Resource-Id`、`X-Api-Connect-Id`）
2. 发送 full_client_request（gzip 压缩的 JSON 元数据：audio_params、user）序号 1
3. 发送音频数据帧（非最后帧），序号 2
4. 发送空帧（is_last=true，序号取反 → -3），通知服务端音频结束
5. 读取服务端二进制响应帧，解析 gzip 解压后的 JSON，提取 `result.text` 或 `result.utterances[].text`
6. 关闭连接

**WAV 处理：** 自动检测 WAV 文件并剥离文件头，以 raw PCM 格式发送。支持非标准 fmt chunk（如 Windows TTS 生成的 18 字节 fmt）。

**依赖：** 复用项目已有的 `tokio-tungstenite`（WebSocket）和 `flate2`（gzip 压缩/解压）。

### 4.4.3 provider/stt/google.rs — Google Cloud Speech-to-Text

实现 `SttProvider` trait，通过 Google Cloud Speech-to-Text REST API v1（同步识别）转录音频。

```rust
pub struct GoogleSttProvider { client: reqwest::Client, api_key: String, language_code: String }
```

**transcribe 流程：**
1. 根据 MIME 类型自动选择编码格式（`OGG_OPUS` / `LINEAR16` / `MP3` / `FLAC`）和采样率。
2. 音频 Base64 编码后通过 `POST speech.googleapis.com/v1/speech:recognize?key={api_key}` 发送。
3. 解析响应 JSON，提取 `results[0].alternatives[0].transcript`。

**配置：** `provider: "google"` + `apiKey`（或 `GOOGLE_STT_API_KEY` 环境变量）+ 可选 `google.languageCode`（默认 `"zh-CN"`）。

**依赖：** 复用已有 `reqwest`（HTTP）和 `base64`（编码）。

## 4.5 provider/tts/ — TTS 提供商

支持三种 TTS 提供商，通过 `messages.tts.provider` 配置选择。

### 4.5.1 Edge TTS（默认，免费）

实现 `TtsProvider` trait，通过 Microsoft Edge TTS WebSocket 协议合成语音。

```rust
pub struct EdgeTtsProvider { voice: String, rate: String, pitch: String, volume: String, chromium_version: String }
```

**DRM 认证（2024 年末起必需）：**

Microsoft 在 2024 年底对 Edge TTS WebSocket 端点添加了 DRM 验证，未携带正确 token 将返回 403。

- **Sec-MS-GEC token**：基于时间的 SHA-256 哈希。算法：当前时间 → 转 Windows FILETIME 纪元 → 向下取整到 5 分钟边界 → 转为 100 纳秒 ticks → 拼接 `TrustedClientToken` → SHA-256 → 大写十六进制。同一 5 分钟窗口内 token 相同。
- **MUID cookie**：随机 32 位大写十六进制字符串（UUID v4 去连字符）。
- **Sec-MS-GEC-Version**：格式 `1-{chromium_version}`，如 `1-143.0.3650.75`。
- **必需 HTTP headers**：Pragma、Cache-Control、Accept-Encoding、Accept-Language、Origin（chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold）、User-Agent（仅主版本号）、Cookie（muid）。
- **chromium_version 可配置**：通过 `edge.chromiumVersion` 配置项覆盖，当 Edge TTS 返回 403 时更新版本号即可，无需改代码。默认 `143.0.3650.75`。

**协议流程：**
1. 构建 WebSocket URL：`wss://speech.platform.bing.com/.../edge/v1?TrustedClientToken=...&ConnectionId={uuid}&Sec-MS-GEC={token}&Sec-MS-GEC-Version={version}`。
2. 使用 `IntoClientRequest` 构建请求并注入 DRM headers。
3. 发送 `speech.config` 文本帧（设定输出格式 `audio-24khz-48kbitrate-mono-mp3`）。
4. 发送 SSML 文本帧（含 voice name、prosody rate 和待合成文本）。
5. 接收混合帧：文本帧含 `turn.start`/`turn.end`；二进制帧含头部（以 `Path:audio\r\n` 结尾），之后为纯 MP3 音频数据。
6. 累积所有音频分块到预分配 `Vec<u8>`，收到 `turn.end` 后返回。

每次请求新建连接，合成后关闭。短句场景（10-50 字）开销可接受。

### 4.5.2 OpenAI TTS（付费，高质量）

实现 `TtsProvider` trait，通过 OpenAI Audio Speech API 合成语音。兼容所有 OpenAI-compatible TTS 端点。

```rust
pub struct OpenAiTtsProvider { client: reqwest::Client, base_url: String, api_key: String, model: String, voice: String }
```

**API 协议：**
```
POST {base_url}/audio/speech
Authorization: Bearer {api_key}
Content-Type: application/json

{ "model": "tts-1", "input": "text", "voice": "alloy", "response_format": "opus" }

← 200 OK, body = raw OGG/Opus audio bytes
```

配置 `base_url` 可指向任何 OpenAI-compatible TTS 服务。默认 `https://api.openai.com/v1`。

### 4.5.3 ElevenLabs TTS（付费，最自然）

实现 `TtsProvider` trait，通过 ElevenLabs Text-to-Speech API 合成语音。

```rust
pub struct ElevenLabsTtsProvider { client: reqwest::Client, base_url: String, api_key: String, model_id: String, voice_id: String }
```

**API 协议：**
```
POST {base_url}/text-to-speech/{voice_id}
xi-api-key: {api_key}
Content-Type: application/json

{ "text": "text", "model_id": "eleven_multilingual_v2" }

← 200 OK, body = raw MP3 audio bytes
```

### 4.5.4 火山引擎 TTS（豆包，高质量中文）

实现 `TtsProvider` trait，通过火山引擎语音合成 HTTP REST API 合成语音。原生支持 `ogg_opus` 输出。

```rust
pub struct VolcengineTtsProvider { client: reqwest::Client, base_url: String, app_id: String, access_token: String, cluster: String, voice_type: String, speed_ratio: f32, volume_ratio: f32, pitch_ratio: f32 }
```

**API 协议：**
```
POST {base_url}/api/v1/tts
Authorization: Bearer;{access_token}    ← 注意是分号，不是空格
Content-Type: application/json

{
  "app": { "appid": "xxx", "token": "access_token", "cluster": "volcano_tts" },
  "user": { "uid": "openclaw" },
  "audio": { "voice_type": "zh_female_vv_uranus_bigtts", "encoding": "ogg_opus", "speed_ratio": 1.0, ... },
  "request": { "reqid": "uuid", "text": "文本", "text_type": "plain", "operation": "query" }
}

← 200 OK, JSON: { "code": 3000, "data": "<base64 OGG/Opus>" }
```

### 4.5.5 TTS 提供商选择

`main.rs` 中按 `config.messages.tts.provider` 分发：

| provider 值 | 实现 | 原生格式 | 输出格式 | 依赖 |
|-------------|------|----------|----------|------|
| `"edge"` (默认) | `EdgeTtsProvider` | WebM Opus | OGG/Opus ★ | 无 API key |
| `"volcengine"` | `VolcengineTtsProvider` | OGG/Opus | OGG/Opus | `VOLCENGINE_ACCESS_TOKEN` |
| `"openai"` | `OpenAiTtsProvider` | OGG/Opus | OGG/Opus | `OPENAI_API_KEY` |
| `"elevenlabs"` | `ElevenLabsTtsProvider` | MP3 | MP3 | `ELEVENLABS_API_KEY` |

★ Edge TTS 内部自动调用 `webm_opus_to_ogg_opus()` 将 WebM 容器转为 OGG 容器（Opus 编码不变，仅换壳）。

**音频格式统一策略：**
- **统一输出 OGG/Opus** —— 所有通道（Telegram、飞书）和所有 TTS 提供商统一使用此格式
- Edge TTS 免费端点（Bing consumer）不支持 `ogg-*-opus` 格式，只支持 `webm-24khz-16bit-mono-opus`，因此在提供商内部通过 `webm_to_ogg.rs` 模块做容器转换（~200 行，无新依赖，EBML 解析 + OGG 封装 + CRC-32）
- Telegram `sendVoice` 接受 OGG/Opus 和 MP3；飞书 `msg_type: "audio"` 只接受 OGG/Opus（`file_type: "opus"`）
- 格式转换责任在 TTS 提供商内部，Channel 层不感知

### 4.5.6 TTS auto 模式

`messages.tts.auto` 支持四种模式：

| 模式 | 行为 |
|------|------|
| `"inbound"` (默认) | 用户发语音时回复语音 |
| `"always"` | 每次回复都附语音 |
| `"tagged"` | agent 用 `<speak>文本</speak>` 标记需要语音的内容 |
| 其他值 | 不合成语音（纯文本） |

**tagged 模式实现：** `handle()` 在获得 LLM 回复后，检查 `<speak>...</speak>` 标签。如果存在，提取标签内文本进行 TTS 合成，并从显示文本中移除标签。如果不存在标签则不合成。

## 4.6 agent/react_loop.rs — ReAct 循环

```
用户输入 → 更新 ChatContext → 加载会话 → 自动压缩(maybe_compact) → 加载记忆上下文
→ 组装 system prompt → 初始化 LoopDetector → 设置 deadline(now + agent_timeout_secs)
→ 循环（直到 deadline 到达或 LLM 完成）:
  ├─ 检查 deadline：超时 → break，返回 "处理超时，请重试"
  ├─ 调用 LLM (chat_stream → 累积事件 → LlmResponse)
  │   └─ context overflow → maybe_compact_emergency() → 重试
  ├─ EndTurn → apply_response_prefix() → 提取文本，保存会话，返回
  ├─ ToolUse → loop_detector.record() → 执行工具 → tool_result 追加 → 继续循环
  │   ├─ Warning → 追加警告到 tool_result
  │   └─ Block → 终止循环，请求最终响应
  └─ 其他 → 中断
```

**Agent 超时（对标 OpenClaw 原版）：** 无固定迭代次数限制，改用时间超时。`agent_timeout_secs`（默认 600s / 10 分钟）通过 `tokio::time::Instant` 在每次迭代开头检查。LLM 决定何时停止（model-driven loop），超时仅作为安全网。对标 OpenClaw 原版 `agents.defaults.timeoutSeconds: 600`。

**中止信号：** `react_loop()` 接受 `abort: &Arc<AtomicBool>` 参数，每次迭代开头和每次
工具执行前检查。abort 触发时保存当前会话并返回 `"Operation cancelled."`。

**工具循环检测（LoopDetector）：** VecDeque<30> 滑窗存储 `ToolCallRecord`，每条记录包含
`fingerprint`（`"tool_name:input_hash_hex"`，~30B）和 `result_hash`（`Option<u64>`，8B）。
两阶段记录：`record_input()` 在工具执行前调用，`record_outcome()` 在执行后记录结果 hash。

4 种 progress-aware 检测模式（对标 OpenClaw `tool-loop-detection.ts`）：
- **generic_repeat**：连续相同工具+输入+结果重复调用
- **ping_pong**：A→B→A→B 交替模式，含 no-progress 证据检查
- **circuit_breaker**：同一工具 no-progress streak（连续相同结果）而非单纯调用次数
- **global_circuit_breaker**：总工具调用次数达到 30 且 `global_no_progress_streak()` 无进展时终止。`total_tool_calls: usize` 字段计数所有调用。

阈值：WARNING=10，BLOCK=20，全局断路器=30，但仅在结果无变化（no-progress）时触发。
例如 `exec` 工具执行 20 次且每次结果不同（脚本在迭代）不会被阻断，只在连续 20 次
结果完全相同时才 block。指纹使用 `DefaultHasher`（非密码学，分布均匀）哈希工具输入
为 u64，避免存储大 JSON。~1.2KB/请求（30 条 × ~40B），比旧版（最差 30KB）减少 96%。

**瞬态错误重试：** `react_loop()` 中 `consume_stream_live()` 返回错误时，检查
`is_transient_error()` — 匹配 transport error、connection、timed out、overloaded、
HTTP 500/502/503/521/522/523/524/529。命中时等待 2.5s 后重试一次（仅一次），
重建 `StreamingWriter`。非瞬态错误仍立即上抛。

**工具结果裁剪（prune_tool_results）：** 每次 `react_loop` 主循环迭代开头调用。
保留最近 3 个 assistant 消息的 tool_result 不裁剪，之前的 tool_result 超过 4000 字符
时 soft-trim 为 head 1500 + tail 1500 + 中间裁剪提示。减少长会话 context 占用。

**工具输出截断：** 工具执行结果超过 400,000 字符时自动截断（优先在换行符处断开），追加截断提示后缀。防止大输出导致 context overflow。

**自动压缩（maybe_compact）：** 当 `auto_compact == true` 且 `messages.len() > sessions.history_limit() * 75%` 时触发：
1. 使用 `compact_ratio`（默认 0.4）决定保留比例，丢弃最旧的 60% 消息
2. 对丢弃部分构造摘要请求发给 LLM（"Summarize key facts, decisions, user preferences. Max 200 words."）
3. 摘要结果 append 到记忆日志（`[auto-compact] ...`）
4. 在 react_loop 开头、构建 system prompt 之前调用

**紧急压缩（maybe_compact_emergency）：** 捕获 LLM context overflow 错误后触发，渐进回退：
1. 保留 40% + 摘要
2. 保留 20% + 摘要
3. 仅保留最后 2 条消息，无摘要（最后手段）

**Response Prefix：** `messages.responsePrefix` 模板在 react_loop 返回文本前应用，支持 `{model}`、`{provider}`、`{thinkingLevel}` 变量替换。

**流式 LLM 调用与实时预览：** `react_loop()` 通过 `consume_stream_live()` 方法调用
`llm.chat_stream()`，同时将 TextDelta 推送到 `StreamingWriter` 实现打字机效果：

```rust
async fn consume_stream_live(
    &self, system, messages, tools,
    writer: &mut Option<StreamingWriter>,  // None = 不流式
    abort: &Arc<AtomicBool>,
) -> Result<LlmResponse>;
```

`StreamingWriter`（`src/channel/streaming.rs`，~110 行）负责节流消息编辑：
- 首次推送：等 buffer 达到 `min_initial_chars`（20 字符）后 `send_text` 获取 msg_id
- 后续推送：距上次 edit ≥ `throttle_ms`（1000ms）则 `edit_message`，否则仅缓冲
- `finish()`：无条件最终编辑，释放 buffer
- `stop()`（async）：ToolUse 时停止流式，**先刷新缓冲区**（发送或编辑残留文字）再置 stopped=true。注意：必须刷新，因为 ToolUse 后新建 writer，未刷新的 buffer 会丢失
- EndTurn 时调用 `finish()`，通过 `\x00STREAMED\x00` marker 机制保留文本供 TTS 使用
- ToolUse 时调用 `stop().await`，工具执行后下次 LLM 调用重新创建 writer
- `react_loop` 维护 `any_text_streamed` 跨迭代标志位，记录是否有文字通过流式发给用户

**流式输出与 TTS 共存：** 流式模式下文本已通过 `edit_message` 发给用户，但 TTS 仍需原始文本进行语音合成。`react_loop()` 在流式完成时返回 `"\x00STREAMED\x00{reply_text}"` 而非空字符串。`handle()` 解析该 marker：`already_streamed=true` 时跳过文本发送（已显示），但仍将 `reply_text` 传给 `try_synthesize()` 生成语音。

1000ms 节流对齐 Telegram editMessage API rate limit。4096 字符 cap 对齐 Telegram 消息长度限制。
对标 OpenClaw 原版 `draft-stream-loop.ts`。~4.2KB 峰值（buffer），非活跃时 0。

**记忆注入：** `handle()` 先更新 `chat_context`（channel + chat_id），然后 `react_loop()` 调用 `memory.build_context()` 获取自适应记忆上下文，注入到 system prompt 的 `## Memory` 段。

**压缩提示：** 当 `messages.len() > 15` 时，在 system prompt 末尾追加 `COMPACTION_WARNING`，提醒 agent 将重要信息保存到记忆中。

循环中 assistant 消息（含 tool_use 块）和 tool_result 交替追加到会话历史。循环结束后持久化到 SessionStore。

## 4.7 tools/ — 工具模块

**ha_control：** Home Assistant REST API 控制。支持 `call_service`（POST `/api/services/{domain}/{service}`）和 `get_state`（GET `/api/states/{entity_id}`）两种操作。30s per-request timeout。输入含 `action`、`domain`、`service`、`entity_id`、`data` 字段。

**web_fetch：** HTTP GET 指定 URL。HTML 页面自动转为纯文本（去除 script/style/标签，解码实体，压缩空白），JSON/XML/纯文本 API 响应不做转换。默认返回前 128K 字符，支持 `offset` + `max_chars` 分页读取超长页面。2MB 下载体积上限（可通过 `webFetch.maxDownloadBytes` 配置）。30s per-request timeout（覆盖 Client 级 600s）。内置 SSRF 防护：`validate_url_ssrf()` 在请求前校验 URL scheme（仅 http/https）、DNS 解析后检查所有 IP 地址是否在私有/保留范围内（127.x, 10.x, 172.16-31.x, 192.168.x, 169.254.x, ::1, fe80::/10, fc00::/7, IPv4-mapped v6）。

**get_time：** 使用 `chrono::Local::now()` 返回当前时间，格式如 `"2025-01-15 14:30:00 (Wednesday)"`。

**cron：** 管理定时任务（add/remove/list）。CronTask 结构含 `id`、`cron_expr`、`description`、`command`、`channel`、`chat_id`、`created_at`、`last_run`、`schedule_at`、`delete_after_run`、`delivery_mode`、`webhook_url`、`isolated`。`channel` 和 `chat_id` 从 `Arc<Mutex<ChatContext>>` 自动注入。持久化到 `{session_dir}/cron.json`。支持一次性调度（`schedule_at`）、自动删除（`delete_after_run`）、webhook 投递（`delivery_mode`）和隔离执行（`isolated`）。执行机制见[子系统章节](subsystems.md)。

**memory：** 长期记忆管理工具，6 个 action：`read`（读 MEMORY.md）、`append`（追加到 MEMORY.md）、`rewrite`（重写整理 MEMORY.md）、`read_log`（读某日日志，默认今天）、`append_log`（追加今日日志）、`search`（跨全部记忆文件子串搜索）。详细设计见[子系统章节](subsystems.md)。

**file（文件操作）：** 4 个零大小 struct（`FileReadTool`、`FileWriteTool`、`FileEditTool`、`FileFindTool`），无需 config，零常驻内存。`file_read` 读取文件并返回 cat -n 风格带行号文本，支持 offset/limit 分页，二进制检测，64KB 截断。`file_write` 写入文件并自动创建父目录。`file_edit` 精确字符串替换，默认单次替换，多次出现时报错含匹配数量。`file_find` BFS 递归遍历目录，支持文件名子串匹配 + 文件内容搜索（跳过二进制），最多 50 条结果。安全模型与 `exec` 一致——无路径限制，通过 `tools.allow` 控制启用。

**扩展工具：** 除上述内置工具外，`exec`（shell 命令执行）、`web_search`（DuckDuckGo 搜索）和 MCP 客户端作为可配置扩展能力实现，详见[子系统章节](subsystems.md)。

## 4.8 session/store.rs — 会话存储

每个 chat 对应一个 JSONL 文件（`{chat_id}.jsonl`），每行一条 `ChatMessage` JSON。

```rust
pub struct SessionStore { dir: PathBuf, history_limit: usize, dm_history_limit: usize }
```

- **Turn-based limiting (`dm_history_limit`)**：对标原版 OpenClaw `dmHistoryLimit`。按用户文本轮次（含 `Text` 块的 User 消息）计数，tool_result-only 的 User 消息不计入。默认 20 轮，0=不限。在 `load()` 和 `save()` 中先于 `history_limit` 执行。
- `load(chat_id)`：读取文件，逐行解析为 `Vec<ChatMessage>`。先执行 `limit_history_turns()` 按用户轮次裁剪，再执行 `history_limit` 原始消息数滑动窗口截断（默认 0=不限）。
- `save(chat_id, messages)`：同样先 `limit_history_turns()` 后滑动窗口截断，覆盖写入。
- `sanitize_after_truncation(messages)`：截断/压缩后清理孤立的 `tool_result` 消息。滑动窗口或 compaction 可能在 assistant `tool_use` 与 user `tool_result` 之间截断，导致 `tool_result` 引用不存在的 `tool_use_id`，Anthropic API 返回 400。该函数从头扫描，跳过前导的 orphaned assistant/tool_result 消息，直到找到包含 `Text` 的 User 消息作为有效起点。在 `load()`、`save()`、`do_compact()`、`maybe_compact_emergency()` 四处截断点调用。

## 4.9 HTTP 超时架构

共享单个 `reqwest::Client`（连接池复用），通过两级超时分离不同场景：

| 级别 | 超时 | 适用范围 | 机制 |
|------|------|---------|------|
| Client 级 | 600s | LLM API 调用（可能含多轮 tool use + thinking） | `Client::builder().timeout(600s)` |
| Request 级 | 30s | Web 工具（web_fetch, web_search, ha_control） | `RequestBuilder::timeout(30s)` 覆盖 Client 默认 |
| Webhook 投递 | 5s | Cron webhook | 已有 per-request timeout |

`reqwest::RequestBuilder::timeout()` 覆盖 `Client` 级超时。LLM 调用（Claude/OpenAI-compatible）继承 Client 默认 600s，足够处理 Extended Thinking + 多工具迭代。Web 工具独立 30s 超时防止单个慢请求阻塞整个 agent turn。

原版 OpenClaw agent timeout 为 600s。Rust Light 默认提高到 900s（15 分钟），
为复杂任务（如多页 PPT 生成）提供更充裕的执行窗口。Web tool timeout 仍为 30s。

## 4.10 模型 Failover

`FailoverLlmProvider`（`src/provider/llm/failover.rs`）在主模型失败时自动切换备用模型，
对标 OpenClaw 原版 `failover-error.ts` + `auth-profiles/usage.ts`。

**架构：**

```rust
pub struct FailoverLlmProvider {
    providers: Vec<Box<dyn LlmProvider>>,   // 主 + 备，按优先级排列
    cooldowns: Mutex<Vec<ProviderCooldown>>, // 每个 provider 的冷却状态
}
```

`chat()` 和 `chat_stream()` 按顺序尝试每个 provider，跳过处于 cooldown 的 provider。
成功调用重置 error_count，失败调用设置 cooldown 并继续尝试下一个。

**Failover 条件**（`is_failover_eligible()`）：

| 错误类型 | Failover? | 说明 |
|----------|-----------|------|
| 429 / rate_limit | 是 | 速率限制 |
| 402 / billing | 是 | 付费问题 |
| 401 / 403 | 是 | 认证错误（token 过期等） |
| timeout / connection | 是 | 网络问题 |
| 500 / 502 / 503 | 是 | 服务端错误 |
| context overflow | **否** | 所有 provider 都会遇到，不 failover |
| abort | **否** | 用户主动中止 |
| format error | **否** | 请求格式问题，切换 provider 无意义 |

**指数退避 Cooldown：**

| 连续失败次数 | Cooldown | 对标 |
|-------------|----------|------|
| 1 | 60s | OpenClaw `5^0` = 1 分钟 |
| 2 | 300s | OpenClaw `5^1` = 5 分钟 |
| 3 | 1500s | OpenClaw `5^2` = 25 分钟 |
| ≥4 | 3600s (cap) | 1 小时上限 |

Cooldown 期间直接跳过该 provider，不发请求。到期后自动恢复（简化版，不主动 probe）。

**配置**（`agents.fallbackModels`）：

```json5
"agents": {
    "fallbackModels": [
        { "provider": "openai", "model": "gpt-4o", "apiKeyEnv": "OPENAI_API_KEY" },
        { "provider": "groq", "model": "llama-3.3-70b-versatile",
          "apiKeyEnv": "GROQ_API_KEY", "baseUrl": "https://api.groq.com/openai/v1" }
    ]
}
```

**构建逻辑**（`src/main.rs`）：fallback provider 使用 `OpenAiCompatProvider` 构造
（所有非 anthropic provider 均走 OpenAI-compatible 协议）。如果 `fallbackModels` 非空，
将主 provider + fallback 列表包裹为 `FailoverLlmProvider`；否则直接使用主 provider。
所有 provider 共享同一个 `reqwest::Client`（连接池复用），每个 fallback ~200B 常驻。

---

# 第五章：API 协议规范

## 5.1 Telegram Bot API

**getUpdates（长轮询）：**

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
        "text": "帮我搜索一下今天的新闻"
    }}
]}
```

**sendMessage：** `POST .../sendMessage`，body `{ "chat_id": N, "text": "..." }`。

**sendVoice：** `POST .../sendVoice`，multipart: `chat_id` + `voice`（二进制音频文件）。通过音频魔术字节检测格式：OGG（`OggS`）→ `audio/ogg` + `voice.ogg`，MP3（`0xFF` 或 `ID3`）→ `audio/mpeg` + `voice.mp3`，未知 → 默认 `audio/ogg`。

**getFile：** `GET .../getFile?file_id=...` 获取 `file_path`，下载地址 `https://api.telegram.org/file/bot{token}/{file_path}`。

## 5.2 Anthropic Messages API v1

```
POST https://api.anthropic.com/v1/messages
Headers: x-api-key, anthropic-version: 2023-06-01, Content-Type: application/json
Body: { model, max_tokens, system, messages: [{ role, content: [ContentBlock] }], tools: [{ name, description, input_schema }] }
```

响应 `stop_reason: "tool_use"` 时 `content` 含 `{ type: "tool_use", id, name, input }`；`stop_reason: "end_turn"` 时含 `{ type: "text", text }`。`usage` 字段报告 token 消耗。

## 5.3 Groq Whisper API

```
POST https://api.groq.com/openai/v1/audio/transcriptions
Authorization: Bearer gsk_xxxxx
Content-Type: multipart/form-data

file: (binary, "voice.ogg", audio/ogg)
model: "whisper-large-v3-turbo"
language: "zh"
```

响应：`{ "text": "帮我搜索一下今天的新闻" }`

### 5.3.2 火山引擎 BigModel ASR WebSocket

```
wss://openspeech.bytedance.com/api/v3/sauc/bigmodel
X-Api-App-Key: {app_id}
X-Api-Access-Key: {access_token}
X-Api-Resource-Id: {cluster}
X-Api-Connect-Id: {uuid}
```

**二进制帧格式（大端序）：**
```
[4B header][4B sequence (i32)][4B payload_size (u32)][payload (gzip)]
```

Header 字节编码：`[version:4|header_size:4] [msg_type:4|msg_type_specific:4] [serial_method:4|compression:4] [reserved]`

**消息序列：**
1. Full client request (seq=1)：gzip(JSON 元数据，含 audio_params、user)
2. Audio data (seq=2, not last)：gzip(PCM/OGG 音频字节)
3. Finish frame (seq=-3, last)：gzip(空字节)，序号取反表示最后帧

**响应格式（v3 bigmodel）：**
```json
{
  "result": {
    "text": "今天天气不错",
    "utterances": [{ "text": "今天天气不错", "definite": true }]
  },
  "addition": { "duration": "3200", "logid": "..." }
}
```

## 5.4 Edge TTS WebSocket

**连接：** `wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1?TrustedClientToken=6A5AA1D4EAFF4E9FB37E23D68491D6F4&ConnectionId={uuid}`

**Headers：** `Origin: chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold`

**发送配置帧（文本）：** `Path:speech.config`，body 设定 `outputFormat: "ogg-24khz-16bit-mono-opus"`。

**发送 SSML 帧（文本）：** `Path:ssml`，body 为标准 SSML，含 `<voice name='zh-CN-XiaoxiaoNeural'>` 和 `<prosody rate='+10%'>` 包裹的待合成文本。

**接收音频帧（二进制）：** 帧内含文本头部（以 `Path:audio\r\n` 结尾），头部后为 OGG/Opus 音频字节。文本帧 `Path:turn.end` 标志合成结束。

## 5.5 OpenAI Audio Speech API

```
POST https://api.openai.com/v1/audio/speech
Authorization: Bearer {api_key}
Content-Type: application/json

{ "model": "tts-1", "input": "Hello world", "voice": "alloy", "response_format": "opus" }

← 200 OK, Content-Type: audio/ogg, body = raw OGG/Opus audio bytes
```

可通过 `base_url` 覆盖指向任何 OpenAI-compatible TTS 端点。

## 5.6 ElevenLabs Text-to-Speech API

```
POST https://api.elevenlabs.io/v1/text-to-speech/{voice_id}
xi-api-key: {api_key}
Content-Type: application/json

{ "text": "Hello world", "model_id": "eleven_multilingual_v2" }

← 200 OK, Content-Type: audio/mpeg, body = raw MP3 audio bytes
```

## 5.7 Home Assistant REST API

**调用服务：**
```
POST http://{ha_url}/api/services/{domain}/{service}
Authorization: Bearer {ha_token}
Content-Type: application/json

{ "entity_id": "light.living_room", "brightness": 255 }
```

**查询状态：**
```
GET http://{ha_url}/api/states/{entity_id}
Authorization: Bearer {ha_token}

响应: { "entity_id": "sensor.temperature", "state": "23.5", "attributes": { "unit_of_measurement": "°C", "friendly_name": "温度传感器" } }
```
