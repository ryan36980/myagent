> [设计文档](README.md) > 高级特性

<a id="chapter-14"></a>
## 第十四章：并发、流式、Failover 与循环检测增强

本章总结对标 OpenClaw 原版后的四项核心架构升级。

### 14.1 功能总览

| 功能 | 对标原版模块 | 实现文件 | 说明 |
|------|------------|---------|------|
| Per-chat 消息队列 | `lane-manager.ts` | `src/main.rs` | 并发处理不同 chat，同 chat 顺序执行 |
| /stop 中止命令 | `AbortController` 信号链 | `src/main.rs` + `src/agent/react_loop.rs` | `Arc<AtomicBool>` 传播中止信号 |
| 流式预览 | `draft-stream-loop.ts` | `src/channel/streaming.rs` + `src/agent/react_loop.rs` | 1s 节流 edit_message |
| 模型 Failover | `failover-error.ts` | `src/provider/llm/failover.rs` | 指数退避 cooldown |
| Progress-aware 循环检测 | `tool-loop-detection.ts` | `src/agent/react_loop.rs` | hash fingerprint + no-progress streak |

### 14.2 Channel trait 变更

```rust
// Before:
async fn poll(&mut self) -> Result<Vec<IncomingMessage>>;
async fn send_text(&self, chat_id: &str, text: &str) -> Result<()>;

// After:
async fn poll(&self) -> Result<Vec<IncomingMessage>>;         // 内部可变性
async fn send_text(&self, chat_id: &str, text: &str) -> Result<String>; // 返回 message_id
```

`poll(&self)` 允许 `Arc<dyn Channel>` 跨 task 共享。`send_text` 返回 message_id
供 `StreamingWriter` 后续 `edit_message` 使用。

### 14.3 内存开销汇总

| 组件 | 常驻 | 每活跃 chat | 说明 |
|------|------|------------|------|
| ChatQueueManager | ~64B | ~860B | mpsc channel + task |
| StreamingWriter | 0 | ~4.2KB | 仅 LLM 流式期间 |
| FailoverLlmProvider | ~430B | 0 | 2 fallback + cooldown |
| LoopDetector | 0 | ~1.2KB | 30 条 hash 记录 |
| abort flag | 0 | 24B | `Arc<AtomicBool>` |
| **合计** | **~502B** | **~6.3KB/chat** | 10 chat ≈ 63KB |

全部新功能合计 <15KB 常驻+活跃，占 8MB 预算的 <0.2%。

### 14.4 新文件清单

| 文件 | 行数 | 说明 |
|------|------|------|
| `src/channel/streaming.rs` | ~110 | StreamingWriter（节流消息编辑） |
| `src/provider/llm/failover.rs` | ~170 | FailoverLlmProvider + cooldown |

### 14.5 修改文件清单

| 文件 | 改动 |
|------|------|
| `src/channel/mod.rs` | `poll(&self)`, `send_text→String`, `+pub mod streaming` |
| `src/channel/telegram.rs` | `AtomicI64 offset`, `send_text` 返回 msg_id |
| `src/channel/cli.rs` | `Mutex<BufReader>`, `poll(&self)` |
| `src/channel/http_api.rs` | `Mutex<u64>`, `poll(&self)` |
| `src/agent/react_loop.rs` | abort flag, `consume_stream_live`, LoopDetector 升级 |
| `src/config.rs` | `+FallbackModel`, `+fallback_models` |
| `src/provider/llm/mod.rs` | `+pub mod failover` |
| `src/main.rs` | ChatQueueManager, chat_worker, Arc wrappers, failover 构建 |
| `config/openclaw.json.example` | `+fallbackModels` 示例（注释） |

### 14.6 与 OpenClaw 原版对照

| OpenClaw 原版功能 | 本实现 | 状态 |
|------------------|--------|------|
| Lane-based FIFO queue | per-chat mpsc channel | 行为等价 |
| Followup queue + debounce | debounce + coalesce in chat_worker (§16) | 对齐 |
| AbortController 信号链 | `Arc<AtomicBool>` + loop check | 等价效果 |
| `/stop` 等中止命令 | 5 个关键词 | 对齐 |
| Streaming draft preview | StreamingWriter + edit_message | 对齐 |
| 1s throttle + minInitialChars | 1000ms + 20 chars | 对齐 |
| Model fallback chain | FailoverLlmProvider | 对齐 |
| Auth profile cooldown (5^n min) | 指数退避 60s→3600s | 对齐（简化） |
| Probe primary during cooldown | cooldown 到期自动恢复 | 简化版 |
| Outcome hashing (dual hash) | result_hash + no-progress streak | 对齐 |
| Ping-pong + noProgressEvidence | ping_pong_no_progress | 对齐 |
| Global circuit breaker | progress-aware (WARNING=10, BLOCK=20) | 对齐 |
| Image/Vision (photo + document) | MessageContent::Image + ContentBlock::Image | 对齐 |
| Parallel tool execution | `join_all` concurrent execution | 对齐 |
| Sub-Agent tool | AgentTool + OnceCell late-binding | 对齐 |

---

## 15. 多模态 + 子 Agent + 并行工具

本章记录三个从 OpenClaw 原版移植的特性，它们不影响已有架构，仅在类型层和执行层做增量扩展。

### 15.1 Image/Vision 类型

**MessageContent 扩展**

```rust
pub enum MessageContent {
    Text(String),
    Voice { file_ref: String, mime: String },
    Image { file_ref: String, mime: String, caption: Option<String> },
}
```

**ContentBlock 扩展**

```rust
pub enum ContentBlock {
    Text { text: String },
    Image { source_type: String, media_type: String, data: String },
    ToolUse { .. },
    ToolResult { .. },
}
```

Image 数据流：

1. Channel 层接收 photo/document → `MessageContent::Image { file_ref }`
2. `AgentRuntime::handle()` 下载文件 → base64 编码 → `ContentBlock::Image`
3. LLM 层序列化为 provider-specific 格式（Anthropic `source` 对象 / OpenAI `image_url` data URI）
4. Image 数据是 **transient**（处理完释放），不增加常驻内存

**Anthropic API 格式**

```json
{ "type": "image", "source": { "type": "base64", "media_type": "image/jpeg", "data": "..." } }
```

**OpenAI API 格式**

```json
[
  { "type": "text", "text": "What's in this image?" },
  { "type": "image_url", "image_url": { "url": "data:image/jpeg;base64,..." } }
]
```

当 user message 仅包含文本时，OpenAI content 字段保持 `Value::String(text)` 以兼容旧版 API。

### 15.2 Telegram Photo/Document 处理

**新增 API 类型**

```rust
struct TgPhotoSize { file_id: String, width: u32, height: u32 }
struct TgDocument { file_id: String, mime_type: Option<String> }
```

**解析逻辑**（在 `poll()` 中，voice/audio 之后）：

- `msg.photo`：选像素最大的 `TgPhotoSize`（`width * height`），MIME 固定 `image/jpeg`
- `msg.document`：仅接受 `mime_type.starts_with("image/")`，其他跳过
- `msg.caption`：作为 Image 的附带文本；无 caption 时默认 "What's in this image?"
- `download_voice()` 已用通用 `getFile` API，photo/document 无需额外下载逻辑

### 15.3 Sub-Agent Tool（异步架构）

**设计：SubAgentRegistry + 异步 spawn**

子 agent 不再同步阻塞父 agent turn，而是通过 `tokio::spawn` 异步执行，立即返回
`run_id`。完成后通过 announce channel 推送结果到父 chat worker。

**4 个独立工具（对标原版 OpenClaw）**

对标原版 OpenClaw 的 sessions_spawn/list/history/send 四工具架构，不做简化合并：

| 工具名 | 说明 | 必填参数 |
|--------|------|---------|
| `sessions_spawn` | 异步启动子 agent，立即返回 run_id | task |
| `sessions_list` | 列出所有活跃子 agent | — |
| `sessions_history` | 查看子 agent 的对话记录 | run_id |
| `sessions_send` | 向运行中的子 agent 发送追加消息 | run_id, message |

```
架构：
  SessionToolsState (Arc, 4 工具共享)
    ├─ runtime: Arc<OnceCell<Arc<AgentRuntime>>>   // 延迟绑定
    ├─ registry: Arc<SubAgentRegistry>              // 管理所有运行中的子 agent
    └─ announce_tx: mpsc::Sender<SubAgentResult>    // 完成通知

  SubAgentRegistry
    └─ runs: Mutex<HashMap<String, SubAgentRun>>    // max 8 concurrent

  SubAgentRun
    ├─ task, started_at, status, abort
    ├─ channel, session_id                          // 标识会话
    ├─ pending: Arc<Mutex<Vec<String>>>             // sessions_send 的消息队列
    └─ notify: Arc<Notify>                          // 唤醒子 agent 处理新消息
```

**创建顺序（OnceCell 延迟绑定）**

```
  1. let (announce_tx, announce_rx) = mpsc::channel(16)
  2. let (session_tools, cell) = create_session_tools(announce_tx)
  3. for tool in session_tools { tool_list.push(tool) }
  4. let agent = Arc::new(AgentRuntime { tools: ToolRegistry::new(tool_list), ... })
  5. cell.set(agent.clone())
```

**递归防护**

- `tokio::task_local!(static IN_SUBAGENT: bool)` — async-safe
- Spawn 时 `IN_SUBAGENT.scope(true, async { ... })` 包裹子 agent future
- 子 agent 内调用 `is_in_subagent()` → true → 拒绝嵌套（最大深度 1）
- 临时 session ID：`_subagent_{run_id}`

**多轮执行**

子 agent 支持多轮对话（通过 `sessions_send`）：
1. 初始任务完成一轮 react_loop
2. 检查 pending 队列（来自 `sessions_send`）
3. 如有待处理消息 → 执行新一轮 react_loop
4. 如无 → 等待 `IDLE_WAIT_SECS`（30s），若仍无新消息则结束
5. Session 文件在子 agent 结束后保留（供 `sessions_history` 查看），`evict_stale()` 清理

**超时**

- 默认使用全局 `agent_timeout_secs`（900s）
- LLM 可通过 `timeout` 参数逐次覆盖：`min(timeout, agent_timeout_secs)`
- `0` 或缺省 = 使用 agent_timeout_secs
- 超时后设置 abort flag，状态变为 `TimedOut`

**Auto-announce**

子 agent 完成时通过 `announce_tx` 发送 `SubAgentResult { run_id, channel, chat_id, text }`。
main.rs 的主 dispatch 循环 `select!` 监听 `announce_rx`，将结果包装为合成
`IncomingMessage`（sender_id = `_system`），路由到对应 chat worker。

**内存开销**：~64B 常驻（空 HashMap）；每个活跃 run ~500B，max 8 = ~4KB 峰值。
`evict_stale()` 清理完成超过 5 分钟的 run。

### 15.4 并行工具执行

**原实现**：`for` 循环顺序执行每个 tool call。

**新实现**：三阶段并发执行。

```
Phase 1 — Record inputs:
  for tool_use in response.tool_uses():
    loop_detector.record_input()
    if Block → break

Phase 2 — Execute in parallel:
  futures_util::future::join_all(tasks.iter().map(|t| tools.execute(...)))

Phase 3 — Process results:
  for (task, result) in zip(tasks, results):
    loop_detector.record_outcome()
    truncate + warn
```

**关键点**：

- 在 single-threaded tokio 上，`join_all` 是协作式 I/O 交错（不需要多线程）
- 多个网络 I/O tool（web_fetch、ha_control）同时等待响应，总耗时 ≈ max(各 tool 耗时)
- 内存与顺序执行相同（futures 在同一线程上依次 poll）
- Abort 检查移到 Phase 1 之前（执行开始前统一检查）

### 15.5 显式不实现的功能

| 原版功能 | 原因 | RAM 开销 |
|---------|------|---------|
| Browser automation | 需要 Chromium | 80-150MB |
| Canvas/Image generation | 需要 ML 模型 | 30-60MB |
| Gmail integration | 需要 Google OAuth | ~10MB |
| Vector search (RAG) | 需要 embedding 模型 | 20-50MB |
| Google Calendar | 需要 Google OAuth | ~10MB |

以上功能均超出 <8MB 内存预算，不在 Rust 精简版范围内。

## 16. Followup Debounce 与 System Prompt 配置对齐

### 16.1 问题背景

当前 `chat_worker` 在 agent 完成后逐条处理 pending 消息，每条消息独立运行一次
agent turn。当用户在 agent 运行中发送 "好了没" 等 follow-up 消息时，该消息会作为
新 turn 打断多步任务链。原版 OpenClaw 使用 followup debounce 机制合并 pending
消息，避免中断。

同时 `openclaw.json.example` 中 `systemPrompt` 错误放在 `messages` 段（代码实际
从 `agents` 段读取），且缺少工具使用规则（如"不限制每 turn 调用次数"），导致模型
过早 EndTurn。

### 16.2 Followup Debounce 设计

**配置**

```rust
pub struct AgentConfig {
    // ...existing fields...
    pub followup_debounce_ms: u64,  // 默认 2000（2秒）
}
```

JSON5 配置键：`agents.followupDebounceMs`。

**`AgentRuntime` 扩展**

新增字段 `pub followup_debounce_ms: u64`，从 `config.agents.followup_debounce_ms`
初始化。

**消息合并函数**

```rust
fn coalesce_pending_messages(pending: &[IncomingMessage]) -> Option<IncomingMessage> {
    // 1. 过滤 abort 命令
    // 2. 提取文本：Text→原文, Voice→"[voice message]", Image→caption 或 "[image]"
    // 3. 单条：直接返回（保留原始 IncomingMessage）
    // 4. 多条：合并文本，包裹 "[The user sent follow-up messages while you were working:]\n..."
    // 5. 返回合并后的 IncomingMessage（使用第一条的 channel/chat_id/sender_id）
}
```

**chat_worker 改造**

替换当前 pending 处理循环（逐条处理）为三阶段：

```
Phase 1 — Debounce：
  deadline = Instant::now() + debounce_ms
  loop {
    tokio::select! {
      _ = sleep_until(deadline) => break,
      Some(msg) = rx.recv() => {
        if is_abort_command(msg) → 清空 pending, break
        pending.push(msg)
        // 不重置 deadline（固定窗口，非滑动窗口）
      }
    }
  }

Phase 2 — Coalesce：
  coalesce_pending_messages(&pending) → Option<IncomingMessage>

Phase 3 — Execute：
  if let Some(coalesced) = coalesced_msg {
    agent.handle(&coalesced, ...)
    dispatch_response(...)
  }
```

固定窗口（非滑动窗口）确保 debounce 有上限，不会无限延迟。

**执行流程时序**

```
User: "写10页PPT"
  → agent 开始执行多步工具调用
  ← (agent 运行中...)

User: "好了没"      ←— 进入 pending 队列
User: "加油"        ←— 进入 pending 队列

  ← agent 完成当前 turn

  → Phase 1: 等待 2s debounce 窗口
  → Phase 2: 合并 "好了没" + "加油"
    → "[The user sent follow-up messages while you were working:]\n好了没\n加油"
  → Phase 3: 跑一次 agent turn 处理合并消息
```

### 16.3 System Prompt 配置对齐

**问题**：`openclaw.json.example` 第 24 行将 `systemPrompt` 放在 `messages` 段，
但代码从 `agents.systemPrompt` 读取（`MessagesConfig` 中无该字段）。

**修复**：
1. 删除 `messages` 段中的 `systemPrompt`
2. 在 `agents` 段添加完整的 systemPrompt，包含工具使用规则

```json
"agents": {
    "systemPrompt": "You are a personal assistant running inside OpenClaw.\n\n## Tool Usage\n- No limit on tool calls per turn — use as many as needed.\n- For multi-step tasks, complete ALL steps in one turn. Do not stop to report progress.\n- User messages during execution are queued and do not interrupt you.\n- Never fabricate system limitations.",
    ...
}
```

`src/agent/context.rs` 不需要改动——`build_system_prompt()` 已正确使用 `base_prompt` 参数。

### 16.4 Interrupt Queue Mode（对标原版 clearCommandLane）

**配置**

```rust
pub struct AgentConfig {
    // ...existing fields...
    pub queue_mode: String,  // "interrupt"（默认）| "queue"
}
```

JSON5 配置键：`agents.queueMode`。

**interrupt 模式**（默认，对标原版 OpenClaw）：

用户在 agent 运行中发送新消息时，中断当前 agent turn（设置 abort flag），
清空 pending 队列，将新消息作为下一 turn 的输入立即处理。

```
User: "写10页PPT"
  → agent 开始执行
User: "算了，帮我查天气"
  → abort 当前 turn，清空 pending
  → agent 以 "帮我查天气" 开始新 turn
```

**queue 模式**（原有行为）：

新消息入 pending 队列，agent 完成后经 debounce + coalesce 合并处理。

**chat_worker 实现**

```
loop {
    // 取消息：优先用 overflow_msg（interrupt 回环），否则从 rx.recv()
    let msg = overflow_msg.take().or(rx.recv().await);

    // agent 执行中的 select!:
    select! {
        result = agent.handle(&msg, ...) => { ... },
        Some(next_msg) = rx.recv() => {
            if is_abort_command || interrupt_mode {
                abort → clear pending → push next_msg
            } else {
                pending.push(next_msg)  // queue mode
            }
        }
    }

    // 完成后:
    if interrupt_mode {
        overflow_msg = pending.pop()  // 立即处理，不 debounce
    } else {
        debounce → coalesce → 处理合并消息
    }
}
```

### 16.5 内存开销

| 组件 | 常驻 | 说明 |
|------|------|------|
| `AgentRuntime.followup_debounce_ms` | 8B | `u64` 字段 |
| `AgentRuntime.queue_mode` | 24B | `String` 字段 |
| Debounce timer | 0B | 栈上 `Sleep` future，无堆分配 |
| Coalesced message | 瞬时 | 合并后的 `IncomingMessage`，处理完释放 |

### 16.6 改动文件

| 文件 | 改动 |
|------|------|
| `src/config.rs` | `AgentConfig` +`followup_debounce_ms`, +`context_files`, +`queue_mode` 字段 |
| `src/agent/react_loop.rs` | `AgentRuntime` +新字段, +transient retry, +prune_tool_results, +silent reply, +runtime_info |
| `src/agent/context.rs` | `build_system_prompt()` +`runtime_info`, +`context_files` 参数 |
| `src/tools/agent_tool.rs` | 重写：4 个独立工具 (sessions_spawn/list/history/send), SubAgentRegistry, task_local 递归守卫, multi-turn, auto-announce |
| `src/main.rs` | +announce channel, interrupt mode, context files 加载, coalesce |
| `config/openclaw.json.example` | +`queueMode`, +`contextFiles` 注释 |
| `docs/zh/design/advanced.md` | 本章节 |

新依赖：无。

## 17. System Prompt 增强

### 17.1 Runtime 信息注入

`build_system_prompt()` 新增 `runtime_info: &str` 参数，在 Date/Time 之后、Memory
之前注入 `## Runtime` 段：

```
## Runtime
Model: claude-sonnet-4-5 | Provider: anthropic | Channel: telegram | Thinking: off
```

`react_loop()` 在每次迭代构造 runtime_info 字符串，包含当前 model、provider、
channel、thinking level。0B 常驻（栈上临时 String）。

### 17.2 Context Files（SOUL.md 等价）

**配置**

```rust
pub context_files: Vec<String>,  // 文件路径列表，默认空
// JSON5: agents.contextFiles: ["./SOUL.md"]
```

**加载逻辑**（`src/main.rs`，启动时一次性加载）：

- 遍历 `config.agents.context_files`，`tokio::fs::read_to_string` 读取每个文件
- 每个文件截断到 20,000 字符，格式为 `### {path}\n{content}\n\n`
- 总长截断到 150,000 字符
- 结果存入 `AgentRuntime.context_files_content: String`
- `build_system_prompt()` 新增 `context_files: &str` 参数，注入到 `## Project Context` 段

**内存**：不配置时 0B；满配时 max 150KB 常驻。

### 17.3 Memory Recall 指引

默认 system_prompt 追加 `## Memory` 段：

```
## Memory
Before answering questions about prior work, decisions, dates, people, or to-do items,
search your memory first using the memory tool (action: "search" or "read").
Save important facts, preferences, and decisions to memory for future reference.
```

引导模型在回答前主动查询记忆，减少幻觉。

### 17.4 Silent Replies

默认 system_prompt 追加 `## Silent Replies` 段：

```
## Silent Replies
If you have nothing meaningful to say after completing an internal operation,
reply with exactly 🤐 (nothing else). This suppresses the message.
```

`react_loop()` 的 `handle()` 返回前检查：若 `display_text.trim() == "\u{1f910}"`，
替换为空字符串。空文本在 `OutgoingMessage` 中被过滤不发送，实现静默完成。
