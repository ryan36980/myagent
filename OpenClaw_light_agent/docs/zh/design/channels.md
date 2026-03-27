> [设计文档](README.md) > 通道层设计

# 第三章：Channel 通道层设计

## 3.1 设计原则

- Channel 只负责协议适配，不包含业务逻辑。
- `poll()` 维护内部状态（offset），使用 `&mut self`；其余方法使用 `&self`。
- 所有消息统一转换为 `IncomingMessage`，上层 Agent 完全不感知通道协议。

## 3.2 Telegram 实现

采用 Bot API 长轮询模式（嵌入式设备通常无公网 IP，不适合 Webhook）。

```rust
pub struct TelegramChannel {
    client: reqwest::Client,
    token: String,
    offset: AtomicI64,        // 内部可变性，支持 poll(&self)
    allowed_users: Vec<String>,
}
```

**长轮询流程：**
1. `GET /getUpdates?offset=N&timeout=30`，最多等 30 秒。
2. 有新消息立即返回，否则超时返回空数组。
3. 处理后更新 `offset = last_update_id + 1`。
4. `allowed_users` 非空时只处理白名单用户。

**语音下载：** 先调 `getFile` 获取 `file_path`，再从 `https://api.telegram.org/file/bot{token}/{file_path}` 下载。

**消息发送：** 文本用 `sendMessage`（JSON body），超过 4000 字符时自动分块发送（优先在换行符处断开，其次空格，最后强制截断，每块独立重试 Markdown/plain text）。`send_text` 返回 `Result<String>`（message_id），用于后续 `edit_message` 流式预览。语音用 `sendVoice`（multipart/form-data 上传音频字节）。

**流式预览：** LLM 生成过程中通过 `StreamingWriter` 实时编辑消息，用户可看到逐步生成的文字。1000ms 节流间隔对齐 Telegram editMessage rate limit，4096 字符 cap 对齐 Telegram 消息长度限制。详见 [高级特性](advanced.md) 流式 LLM 调用章节。

**Typing 指示器：** 处理消息期间每 6 秒循环发送 `sendChatAction(typing)`，直到 agent 回复完成。Telegram typing 指示器默认 5 秒消失，6 秒间隔确保持续显示。实现方式：`chat_worker()` 中使用 `tokio::select!` 循环，agent future、typing interval 和新消息接收并发，agent 完成时自动停止。同时监听中止命令（`/stop` 等），详见 §3.6。

## 3.3 飞书/Lark 实现

采用 WebSocket 长连接模式（飞书 Open Platform SDK 协议），与 Telegram 长轮询模式一致——主动向外建连，无需公网 URL。

```rust
pub struct FeishuChannel {
    client: reqwest::Client,
    app_id: String,
    app_secret: String,
    domain: String,                              // "https://open.feishu.cn"
    allowed_users: Vec<String>,                  // open_id 白名单
    rx: Mutex<mpsc::Receiver<IncomingMessage>>,  // 从 WS 后台任务接收消息
    token: Arc<Mutex<TokenState>>,               // tenant_access_token 缓存
}
```

**WebSocket 协议（从官方 Go/Node.js SDK 逆向）：**

1. `POST {domain}/callback/ws/endpoint`（Body: `AppID` + `AppSecret`）→ 获取 WSS URL 和客户端配置（重连次数/间隔/ping 间隔）。
2. 连接 WSS 端点（复用 `tokio-tungstenite`），接收二进制帧。
3. 帧采用 Protobuf 编码（`Frame` + `Header` 两个类型），手写 ~120 行编解码器，无新依赖。
4. `headers.type == "ping"` → 构造 pong Frame 回复。
5. `headers.type == "event"` → 解析 JSON payload 为飞书事件。

**最小 Protobuf 编解码器**（~120 行，仅 varint + length-delimited 两种 wire type）：

```protobuf
message Frame {
  uint64 SeqID = 1;
  uint64 LogID = 2;
  int32  service = 3;   // 0=control, 1=data
  int32  method = 4;
  repeated Header headers = 5;
  string payloadEncoding = 6;
  string payloadType = 7;
  bytes  payload = 8;
  string LogIDNew = 9;
}
message Header { string key = 1; string value = 2; }
```

**Token 管理**（`tenant_access_token`）：惰性刷新，2 小时有效，过期前 5 分钟自动续期。

```
POST {domain}/open-apis/auth/v3/tenant_access_token/internal
Body: { "app_id": "cli_xxx", "app_secret": "xxx" }
→ { "code": 0, "tenant_access_token": "t-xxx", "expire": 7200 }
```

**消息处理：**
- `text`: 解析 `{"text":"hello"}`，去除 `@_user_N` mention 占位符。
- `audio`: 解析 `{"file_key":"xxx"}` → `MessageContent::Voice`，`file_ref` 格式 `{message_id}:{file_key}`。
- `image`: 解析 `{"image_key":"img_xxx"}` → `MessageContent::Image`。

**消息发送**（REST API）：
- 文本：`POST /open-apis/im/v1/messages`（`msg_type: "interactive"`），使用**交互式卡片**（Interactive Card with Markdown），支持流式编辑。超 4000 字符自动分块。
- 编辑：`PATCH /open-apis/im/v1/messages/{message_id}`（更新卡片内容），用于 streaming preview。飞书不支持编辑纯文本消息，只能编辑卡片消息，因此所有文本统一使用卡片发送。
- 语音：先 `POST /open-apis/im/v1/files`（multipart 上传，`file_type=opus`）获取 `file_key`，再发送 audio 消息。上传时必须在 multipart form 中包含 `duration`（毫秒），否则飞书客户端显示 0s。时长通过 `ogg_opus_duration_ms()` 从 OGG granule position 计算。自动检测音频格式（MP3 magic bytes `0xFF`/`0x49` → mp3，否则 → opus）。
- 下载：`GET /open-apis/im/v1/messages/{message_id}/resources/{file_key}?type=file`。

**事件去重：** 飞书 WebSocket 可能重复投递同一事件。通过 `event_id` 哈希集去重（容量上限 1000，超限清空重建）。

**所需权限：** `im:message`、`im:message:send_as_bot`、`im:message.p2p_msg:readonly`、`im:message:update`、`im:resource`。

**重连机制：** 断线后间隔 3s + 随机抖动自动重连，最多 180 次。

**内存开销：** ~2KB 常驻（channel 结构体 + mpsc buffer），WebSocket 连接 ~4KB（与 Telegram 长轮询相当）。

## 3.4 CLI 通道

基于 stdin/stdout 的命令行交互通道，零依赖，用于本地调试和脚本集成。

```rust
pub struct CliChannel {
    reader: Mutex<tokio::io::BufReader<tokio::io::Stdin>>,  // 内部可变性
}
```

- `poll()`: 逐行读取 stdin，每行构造一条 `IncomingMessage`。
- `send_text()`: 直接 `println!` 输出。
- 语音不支持（`send_voice` / `download_voice` 返回空/错误）。
- EOF 时返回空 Vec，主循环检测到后优雅退出。

**配置：** `CliConfig { enabled: bool }`，默认关闭。

## 3.5 HTTP API 通道

基于 `tokio::net::TcpListener` 的最小 HTTP/1.1 REST 通道，不引入 web 框架。支持 oneshot 请求-响应和 SSE 流式输出，内嵌 Web Chat UI。

```rust
pub struct HttpApiChannel {
    listener: TcpListener,
    pending: Mutex<HashMap<String, oneshot::Sender<(String, bool)>>>,
    sse_streams: Mutex<HashMap<String, SseState>>,
    session_dir: Option<String>,
    auth_token: String,
}
```

### 路由

| 方法 | 路径 | 行为 |
|------|------|------|
| `GET /` | 返回嵌入式 HTML 聊天页面（`include_bytes!`） |
| `GET /chat/history?chat_id=xxx` | 返回会话历史 JSON（从 JSONL session 读取） |
| `POST /chat` | 请求-响应（oneshot） |
| `POST /chat/stream` | SSE 流式响应 |
| `OPTIONS *` | CORS 预检（204 + CORS 头） |

### SSE 流式输出

**事件格式**（参照 OpenAI convention）：

```
data: {"type":"delta","text":"你好"}      ← 增量文本片段
data: {"type":"typing"}                   ← Agent 正在处理（keepalive）
data: {"type":"done","text":"完整回复"}    ← 流结束，包含完整文本
data: {"type":"error","message":"timeout"} ← 错误
```

**复用 StreamingWriter 机制：**

1. `send_text(chat_id, text)` → 查 `sse_streams[chat_id]`，发送 `delta` 事件，返回 `"sse_{chat_id}"` → StreamingWriter 激活
2. `edit_message(chat_id, msg_id, text)` → `msg_id` 以 `"sse_"` 开头时，计算增量 `text[sent_len..]`，发送 `delta` 事件
3. `close_stream(chat_id)` → 发送 `done` 事件（含完整文本），移除连接

**增量计算：** 每个 SSE 连接跟踪 `sent_len`（已发送文本字节数）。`edit_message` 收到全量累积文本时，只发送 `text[sent_len..]` 作为 delta。

**连接生命周期：**

```
客户端 POST /chat/stream {"text":"你好","chat_id":"web_1"}
  ↓
poll() 解析请求 → 写 SSE 响应头 → 创建 mpsc channel → 存入 sse_streams
  ↓
返回 IncomingMessage → Agent 开始处理
  ↓
StreamingWriter 调用 send_text → delta 事件 → 返回 msg_id → StreamingWriter 激活
StreamingWriter 调用 edit_message → delta 事件（增量）
StreamingWriter.finish() → 最终 edit_message
  ↓
Agent 返回 → dispatch_response → close_stream(chat_id) → done 事件
  ↓
后台任务收到 Done → 写最后事件 → 关闭 TCP 连接
```

### Bearer Token 认证

`HttpApiConfig.auth_token` 非空时，所有 `POST` 和 `GET /chat/*` 端点检查 `Authorization: Bearer xxx` 头。`GET /`（HTML 页面）不需要认证。

### 内嵌 Web Chat UI

`src/channel/web_chat.html` 通过 `include_bytes!` 编译进二进制（~10KB .rodata）。单文件 HTML/CSS/JS，零外部依赖，使用 `fetch()` + `ReadableStream` 手动解析 SSE（因 `EventSource` 只支持 GET）。

### 会话历史加载

`GET /chat/history?chat_id=xxx` 读取 `{session_dir}/http_api_{chat_id}.jsonl`，过滤只保留 Text 内容，返回简化 JSON `[{"role":"user","text":"..."},...]`。

### CORS

所有响应添加 `Access-Control-Allow-Origin: *` 等头。

### 内存开销

| 组件 | 开销 |
|------|------|
| `web_chat.html` 嵌入 | ~10KB（.rodata，非堆） |
| 每个 SSE 连接 | ~200B（mpsc channel + sent_len + HashMap entry） |
| mpsc 缓冲区 | 32 条事件 × ~50B = ~1.6KB（峰值） |

**配置：** `HttpApiConfig { enabled, listen, authToken }`，默认关闭。

## 3.6 多通道并发调度

**主循环**使用 `tokio::select!` 同时轮询所有通道和 cron，收到消息后交给
`ChatQueueManager` 入队，主循环永不阻塞：

```rust
let agent = Arc::new(agent);
let telegram = Arc::new(TelegramChannel::new(...));
let queue_manager = ChatQueueManager::new();

loop {
    tokio::select! {
        r = telegram.poll(), if telegram_enabled => {
            for msg in r? {
                queue_manager.enqueue(msg, agent.clone(), telegram.clone(), ...).await;
            }
        }
        r = feishu.poll(), if feishu_enabled => { /* 同上 */ }
        r = http_api.poll(), if http_enabled => { /* 同上 */ }
        r = cli.poll(), if cli_enabled => { /* 同上 */ }
        _ = cron_interval.tick() => { /* cron 执行 */ }
        _ = tokio::signal::ctrl_c() => { break; }
    }
}
```

**ChatQueueManager** 为每个 `chat_id` 维护一个 `mpsc::channel(16)` 队列和一个
`tokio::spawn` 的 worker task。不同 chat 的消息并发处理（在 await 间隙交替），
同一 chat 的消息严格顺序执行：

```rust
struct ChatQueueManager {
    queues: Mutex<HashMap<String, mpsc::Sender<IncomingMessage>>>,
}

impl ChatQueueManager {
    async fn enqueue(&self, msg: IncomingMessage, agent: Arc<AgentRuntime>, ...) {
        let mut queues = self.queues.lock().await;
        queues.retain(|_, tx| !tx.is_closed()); // 清理已退出的 worker
        let tx = queues.entry(msg.chat_id.clone()).or_insert_with(|| {
            let (tx, rx) = mpsc::channel(16);
            tokio::spawn(chat_worker(agent, channel, rx, ...));
            tx
        });
        let _ = tx.send(msg).await;
    }
}
```

**chat_worker** 使用 `select!` 同时处理 agent 执行、typing 循环和新消息接收（含中止命令）：

```rust
async fn chat_worker(agent, channel, mut rx, ...) {
    while let Some(msg) = rx.recv().await {
        let abort = Arc::new(AtomicBool::new(false));
        let agent_fut = agent.handle(&msg, ..., abort.clone(), Some(channel.clone()));
        tokio::pin!(agent_fut);
        let mut typing = tokio::time::interval(Duration::from_secs(6));
        let mut pending = Vec::new();
        loop {
            tokio::select! {
                result = &mut agent_fut => { dispatch(result); break; }
                _ = typing.tick() => { channel.send_typing(&chat_id).await; }
                Some(next_msg) = rx.recv() => {
                    if is_abort_command(&next_msg) {
                        abort.store(true, Ordering::Relaxed);
                        channel.send_text(&chat_id, "Stopping...").await;
                    } else {
                        pending.push(next_msg);
                    }
                }
            }
        }
        // 处理 pending 中的消息
    }
}
```

**中止命令**（对标 OpenClaw 原版）：`/stop`、`stop`、`abort`、`cancel`、`esc`。
通过 `Arc<AtomicBool>` abort 标志传播到 `react_loop`，在每次迭代开头和每次工具执行前检查。

**内部可变性**：`Channel::poll` 签名从 `&mut self` 改为 `&self`，各通道使用内部可变性：
- TelegramChannel: `offset: AtomicI64`（lock-free，8B）
- CliChannel: `reader: Mutex<BufReader<Stdin>>`
- HttpApiChannel: `next_req_id: Mutex<u64>`

**内存开销**：ChatQueueManager ~64B 常驻，每个活跃 chat ~860B（channel + Sender + task），
空闲 chat 自动清理（worker 退出后 `is_closed()` 检查）。10 个并发 chat ≈ 8.6KB。

## 3.7 未来扩展

| 通道 | 协议 | 实现思路 |
|------|------|---------|
| WeChat | HTTPS 回调 + XML | 实现 Channel trait，解析 XML 消息 |
| iMessage | AppleScript | macOS 下命令行桥接 |
| Matrix | HTTP API | 类似 Telegram 实现 |

扩展只需新增文件实现 `Channel` trait，在 `main.rs` 按配置注入。
