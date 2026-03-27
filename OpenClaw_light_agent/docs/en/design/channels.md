> [Design Docs](README.md) > Channel Layer Design

# Chapter 3: Channel Layer Design

## 3.1 Design Principles

- A Channel is responsible only for protocol adaptation; it contains no business logic.
- `poll()` maintains internal state (offset) and uses `&mut self`; all other methods use `&self`.
- All messages are uniformly converted to `IncomingMessage`; the upper-layer Agent has no awareness of the channel protocol.

## 3.2 Telegram Implementation

Uses the Bot API long-polling mode (embedded devices typically have no public IP, making Webhook unsuitable).

```rust
pub struct TelegramChannel {
    client: reqwest::Client,
    token: String,
    offset: AtomicI64,        // interior mutability, supports poll(&self)
    allowed_users: Vec<String>,
}
```

**Long-polling flow:**
1. `GET /getUpdates?offset=N&timeout=30`, waiting up to 30 seconds.
2. Returns immediately when new messages arrive, otherwise times out and returns an empty array.
3. After processing, updates `offset = last_update_id + 1`.
4. When `allowed_users` is non-empty, only whitelisted users are processed.

**Voice download:** First calls `getFile` to obtain `file_path`, then downloads from `https://api.telegram.org/file/bot{token}/{file_path}`.

**Message sending:** Text uses `sendMessage` (JSON body). Messages exceeding 4000 characters are automatically chunked (preferring line breaks, then spaces, then forced truncation; each chunk retries Markdown/plain text independently). `send_text` returns `Result<String>` (message_id) for subsequent `edit_message` streaming previews. Voice uses `sendVoice` (multipart/form-data audio byte upload).

**Streaming preview:** During LLM generation, `StreamingWriter` edits messages in real time so users can see text being generated incrementally. A 1000ms throttle interval aligns with Telegram's `editMessage` rate limit, and the 4096-character cap aligns with Telegram's message length limit. See the streaming LLM calls section in [Advanced Features](advanced.md).

**Typing indicator:** During message processing, `sendChatAction(typing)` is sent in a loop every 6 seconds until the agent finishes replying. Telegram's typing indicator disappears after 5 seconds by default; the 6-second interval ensures it stays visible. Implementation: `chat_worker()` uses a `tokio::select!` loop to concurrently handle the agent future, typing interval, and incoming messages; stops automatically when the agent completes. Also listens for abort commands (`/stop`, etc.); see §3.6.

## 3.3 Feishu/Lark Implementation

Uses WebSocket long-connection mode (Feishu Open Platform SDK protocol), consistent with Telegram's long-polling approach — initiates an outbound connection, requiring no public URL.

```rust
pub struct FeishuChannel {
    client: reqwest::Client,
    app_id: String,
    app_secret: String,
    domain: String,                              // "https://open.feishu.cn"
    allowed_users: Vec<String>,                  // open_id whitelist
    rx: Mutex<mpsc::Receiver<IncomingMessage>>,  // receives messages from the WS background task
    token: Arc<Mutex<TokenState>>,               // tenant_access_token cache
}
```

**WebSocket protocol (reverse-engineered from the official Go/Node.js SDK):**

1. `POST {domain}/callback/ws/endpoint` (Body: `AppID` + `AppSecret`) → obtains WSS URL and client config (reconnect count/interval/ping interval).
2. Connect to the WSS endpoint (reusing `tokio-tungstenite`), receive binary frames.
3. Frames are Protobuf-encoded (`Frame` + `Header` types); a hand-written ~120-line codec with no new dependencies.
4. `headers.type == "ping"` → construct and send a pong Frame.
5. `headers.type == "event"` → parse JSON payload as a Feishu event.

**Minimal Protobuf codec** (~120 lines, only varint and length-delimited wire types):

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

**Token management** (`tenant_access_token`): lazy refresh, valid for 2 hours, auto-renewed 5 minutes before expiry.

```
POST {domain}/open-apis/auth/v3/tenant_access_token/internal
Body: { "app_id": "cli_xxx", "app_secret": "xxx" }
→ { "code": 0, "tenant_access_token": "t-xxx", "expire": 7200 }
```

**Message handling:**
- `text`: parses `{"text":"hello"}`, strips `@_user_N` mention placeholders.
- `audio`: parses `{"file_key":"xxx"}` → `MessageContent::Voice`, `file_ref` format `{message_id}:{file_key}`.
- `image`: parses `{"image_key":"img_xxx"}` → `MessageContent::Image`.

**Message sending** (REST API):
- Text: `POST /open-apis/im/v1/messages` (`msg_type: "interactive"`), using **Interactive Cards** (Interactive Card with Markdown), supports streaming edits. Automatically chunked beyond 4000 characters.
- Edit: `PATCH /open-apis/im/v1/messages/{message_id}` (updates card content), used for streaming preview. Feishu does not support editing plain-text messages, only card messages, so all text is sent as cards.
- Voice: first `POST /open-apis/im/v1/files` (multipart upload, `file_type=opus`) to obtain `file_key`, then send an audio message. The multipart form must include `duration` (milliseconds), otherwise the Feishu client shows 0s. Duration is calculated from the OGG granule position via `ogg_opus_duration_ms()`. Audio format is auto-detected (MP3 magic bytes `0xFF`/`0x49` → mp3, otherwise → opus).
- Download: `GET /open-apis/im/v1/messages/{message_id}/resources/{file_key}?type=file`.

**Event deduplication:** Feishu WebSocket may deliver the same event multiple times. Deduplicated via an `event_id` hash set (capacity capped at 1000; cleared and rebuilt when exceeded).

**Required permissions:** `im:message`, `im:message:send_as_bot`, `im:message.p2p_msg:readonly`, `im:message:update`, `im:resource`.

**Reconnection:** After disconnection, automatically reconnects with a 3s interval plus random jitter, up to 180 attempts.

**Memory overhead:** ~2KB resident (channel struct + mpsc buffer), WebSocket connection ~4KB (comparable to Telegram long-polling).

## 3.4 CLI Channel

A command-line interaction channel based on stdin/stdout, zero dependencies, used for local debugging and script integration.

```rust
pub struct CliChannel {
    reader: Mutex<tokio::io::BufReader<tokio::io::Stdin>>,  // interior mutability
}
```

- `poll()`: reads stdin line by line; each line constructs one `IncomingMessage`.
- `send_text()`: outputs directly via `println!`.
- Voice is not supported (`send_voice` / `download_voice` return empty/error).
- On EOF, returns an empty Vec; the main loop detects this and exits gracefully.

**Config:** `CliConfig { enabled: bool }`, disabled by default.

## 3.5 HTTP API Channel

A minimal HTTP/1.1 REST channel based on `tokio::net::TcpListener`, introducing no web framework. Supports oneshot request-response and SSE streaming output, with an embedded Web Chat UI.

```rust
pub struct HttpApiChannel {
    listener: TcpListener,
    pending: Mutex<HashMap<String, oneshot::Sender<(String, bool)>>>,
    sse_streams: Mutex<HashMap<String, SseState>>,
    session_dir: Option<String>,
    auth_token: String,
}
```

### Routes

| Method | Path | Behavior |
|--------|------|----------|
| `GET /` | Returns embedded HTML chat page (`include_bytes!`) |
| `GET /chat/history?chat_id=xxx` | Returns session history JSON (from JSONL session) |
| `POST /chat` | Request-response (oneshot) |
| `POST /chat/stream` | SSE streaming response |
| `OPTIONS *` | CORS preflight (204 + CORS headers) |

### SSE Streaming

**Event format** (following OpenAI convention):

```
data: {"type":"delta","text":"Hello"}      ← incremental text fragment
data: {"type":"typing"}                    ← agent is processing (keepalive)
data: {"type":"done","text":"Full reply"}  ← stream complete, contains full text
data: {"type":"error","message":"timeout"} ← error
```

**Reuses the StreamingWriter mechanism:**

1. `send_text(chat_id, text)` → checks `sse_streams[chat_id]`, sends `delta` event, returns `"sse_{chat_id}"` → StreamingWriter activates
2. `edit_message(chat_id, msg_id, text)` → when `msg_id` starts with `"sse_"`, computes incremental `text[sent_len..]`, sends `delta` event
3. `close_stream(chat_id)` → sends `done` event (with full text), removes connection

**Incremental calculation:** Each SSE connection tracks `sent_len` (bytes of text already sent). When `edit_message` receives the full accumulated text, it sends only `text[sent_len..]` as a delta.

**Connection lifecycle:**

```
Client POST /chat/stream {"text":"Hello","chat_id":"web_1"}
  ↓
poll() parses request → writes SSE response headers → creates mpsc channel → stores in sse_streams
  ↓
Returns IncomingMessage → Agent starts processing
  ↓
StreamingWriter calls send_text → delta event → returns msg_id → StreamingWriter activates
StreamingWriter calls edit_message → delta events (incremental)
StreamingWriter.finish() → final edit_message
  ↓
Agent returns → dispatch_response → close_stream(chat_id) → done event
  ↓
Background task receives Done → writes final event → closes TCP connection
```

### Bearer Token Authentication

When `HttpApiConfig.auth_token` is non-empty, all `POST` and `GET /chat/*` endpoints check the `Authorization: Bearer xxx` header. `GET /` (HTML page) does not require authentication.

### Embedded Web Chat UI

`src/channel/web_chat.html` is compiled into the binary via `include_bytes!` (~10KB .rodata). Single-file HTML/CSS/JS with zero external dependencies, uses `fetch()` + `ReadableStream` to manually parse SSE (since `EventSource` only supports GET).

### Session History Loading

`GET /chat/history?chat_id=xxx` reads `{session_dir}/http_api_{chat_id}.jsonl`, filters to keep only Text content, and returns simplified JSON `[{"role":"user","text":"..."},...]`.

### CORS

All responses include `Access-Control-Allow-Origin: *` and related headers.

### Memory Overhead

| Component | Overhead |
|-----------|----------|
| `web_chat.html` embedding | ~10KB (.rodata, not heap) |
| Per SSE connection | ~200B (mpsc channel + sent_len + HashMap entry) |
| mpsc buffer | 32 events × ~50B = ~1.6KB (peak) |

**Config:** `HttpApiConfig { enabled, listen, authToken }`, disabled by default.

## 3.6 Multi-Channel Concurrent Dispatch

The **main loop** uses `tokio::select!` to poll all channels and cron simultaneously. On receiving a message, it hands off to `ChatQueueManager` for queuing; the main loop never blocks:

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
        r = feishu.poll(), if feishu_enabled => { /* same as above */ }
        r = http_api.poll(), if http_enabled => { /* same as above */ }
        r = cli.poll(), if cli_enabled => { /* same as above */ }
        _ = cron_interval.tick() => { /* cron execution */ }
        _ = tokio::signal::ctrl_c() => { break; }
    }
}
```

**ChatQueueManager** maintains one `mpsc::channel(16)` queue and one `tokio::spawn`-ed worker task per `chat_id`. Messages from different chats are processed concurrently (interleaved at await points); messages within the same chat are processed strictly in order:

```rust
struct ChatQueueManager {
    queues: Mutex<HashMap<String, mpsc::Sender<IncomingMessage>>>,
}

impl ChatQueueManager {
    async fn enqueue(&self, msg: IncomingMessage, agent: Arc<AgentRuntime>, ...) {
        let mut queues = self.queues.lock().await;
        queues.retain(|_, tx| !tx.is_closed()); // clean up exited workers
        let tx = queues.entry(msg.chat_id.clone()).or_insert_with(|| {
            let (tx, rx) = mpsc::channel(16);
            tokio::spawn(chat_worker(agent, channel, rx, ...));
            tx
        });
        let _ = tx.send(msg).await;
    }
}
```

**chat_worker** uses `select!` to concurrently handle agent execution, the typing loop, and incoming messages (including abort commands):

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
        // process messages in pending
    }
}
```

**Abort commands** (matching the original OpenClaw): `/stop`, `stop`, `abort`, `cancel`, `esc`.
Propagated to `react_loop` via an `Arc<AtomicBool>` abort flag, checked at the start of each iteration and before each tool execution.

**Interior mutability**: The `Channel::poll` signature was changed from `&mut self` to `&self`; each channel uses interior mutability:
- TelegramChannel: `offset: AtomicI64` (lock-free, 8B)
- CliChannel: `reader: Mutex<BufReader<Stdin>>`
- HttpApiChannel: `next_req_id: Mutex<u64>`

**Memory overhead**: ChatQueueManager ~64B resident, each active chat ~860B (channel + Sender + task), idle chats are cleaned up automatically (checked via `is_closed()` after worker exits). 10 concurrent chats ≈ 8.6KB.

## 3.7 Future Extensions

| Channel  | Protocol              | Implementation approach                              |
|----------|-----------------------|------------------------------------------------------|
| WeChat   | HTTPS callback + XML  | Implement the Channel trait, parse XML messages      |
| iMessage | AppleScript           | Command-line bridge on macOS                         |
| Matrix   | HTTP API              | Similar to the Telegram implementation               |

Extensions only require adding a new file that implements the `Channel` trait, then injecting it in `main.rs` based on configuration.
