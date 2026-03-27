> [Design Docs](README.md) > Advanced Features

<a id="chapter-14"></a>
## Chapter 14: Concurrency, Streaming, Failover, and Enhanced Loop Detection

This chapter summarizes four core architectural upgrades benchmarked against the original OpenClaw.

### 14.1 Feature Overview

| Feature | Original Module | Implementation File | Notes |
|---------|----------------|--------------------|----|
| Per-chat message queue | `lane-manager.ts` | `src/main.rs` | Concurrent handling across chats, sequential within same chat |
| /stop abort command | `AbortController` signal chain | `src/main.rs` + `src/agent/react_loop.rs` | `Arc<AtomicBool>` propagates abort signal |
| Streaming preview | `draft-stream-loop.ts` | `src/channel/streaming.rs` + `src/agent/react_loop.rs` | 1s throttle edit_message |
| Model Failover | `failover-error.ts` | `src/provider/llm/failover.rs` | Exponential backoff cooldown |
| Progress-aware loop detection | `tool-loop-detection.ts` | `src/agent/react_loop.rs` | hash fingerprint + no-progress streak |

### 14.2 Channel Trait Changes

```rust
// Before:
async fn poll(&mut self) -> Result<Vec<IncomingMessage>>;
async fn send_text(&self, chat_id: &str, text: &str) -> Result<()>;

// After:
async fn poll(&self) -> Result<Vec<IncomingMessage>>;         // interior mutability
async fn send_text(&self, chat_id: &str, text: &str) -> Result<String>; // returns message_id
```

`poll(&self)` allows `Arc<dyn Channel>` to be shared across tasks. `send_text` returns a
message_id for subsequent `StreamingWriter` `edit_message` calls.

### 14.3 Memory Overhead Summary

| Component | Resident | Per Active Chat | Notes |
|-----------|----------|----------------|-------|
| ChatQueueManager | ~64B | ~860B | mpsc channel + task |
| StreamingWriter | 0 | ~4.2KB | Only during LLM streaming |
| FailoverLlmProvider | ~430B | 0 | 2 fallbacks + cooldown |
| LoopDetector | 0 | ~1.2KB | 30 hash records |
| abort flag | 0 | 24B | `Arc<AtomicBool>` |
| **Total** | **~502B** | **~6.3KB/chat** | 10 chats ≈ 63KB |

All new features combined: <15KB resident+active, <0.2% of the 8MB budget.

### 14.4 New Files

| File | Lines | Notes |
|------|-------|-------|
| `src/channel/streaming.rs` | ~110 | StreamingWriter (throttled message editing) |
| `src/provider/llm/failover.rs` | ~170 | FailoverLlmProvider + cooldown |

### 14.5 Modified Files

| File | Changes |
|------|---------|
| `src/channel/mod.rs` | `poll(&self)`, `send_text→String`, `+pub mod streaming` |
| `src/channel/telegram.rs` | `AtomicI64 offset`, `send_text` returns msg_id |
| `src/channel/cli.rs` | `Mutex<BufReader>`, `poll(&self)` |
| `src/channel/http_api.rs` | `Mutex<u64>`, `poll(&self)` |
| `src/agent/react_loop.rs` | abort flag, `consume_stream_live`, LoopDetector upgrade |
| `src/config.rs` | `+FallbackModel`, `+fallback_models` |
| `src/provider/llm/mod.rs` | `+pub mod failover` |
| `src/main.rs` | ChatQueueManager, chat_worker, Arc wrappers, failover construction |
| `config/openclaw.json.example` | `+fallbackModels` example (commented) |

### 14.6 Comparison with Original OpenClaw

| Original OpenClaw Feature | This Implementation | Status |
|--------------------------|---------------------|--------|
| Lane-based FIFO queue | per-chat mpsc channel | Behavior equivalent |
| Followup queue + debounce | debounce + coalesce in chat_worker (§16) | Aligned |
| AbortController signal chain | `Arc<AtomicBool>` + loop check | Equivalent effect |
| `/stop` and other abort commands | 5 keywords | Aligned |
| Streaming draft preview | StreamingWriter + edit_message | Aligned |
| 1s throttle + minInitialChars | 1000ms + 20 chars | Aligned |
| Model fallback chain | FailoverLlmProvider | Aligned |
| Auth profile cooldown (5^n min) | Exponential backoff 60s→3600s | Aligned (simplified) |
| Probe primary during cooldown | Auto-recovery after cooldown expires | Simplified |
| Outcome hashing (dual hash) | result_hash + no-progress streak | Aligned |
| Ping-pong + noProgressEvidence | ping_pong_no_progress | Aligned |
| Global circuit breaker | progress-aware (WARNING=10, BLOCK=20) | Aligned |
| Image/Vision (photo + document) | MessageContent::Image + ContentBlock::Image | Aligned |
| Parallel tool execution | `join_all` concurrent execution | Aligned |
| Sub-Agent tool | AgentTool + OnceCell late-binding | Aligned |

---

## 15. Multimodal + Sub-Agent + Parallel Tools

This chapter documents three features ported from the original OpenClaw. They do not affect the existing architecture and only add incremental extensions at the type and execution layers.

### 15.1 Image/Vision Types

**MessageContent Extension**

```rust
pub enum MessageContent {
    Text(String),
    Voice { file_ref: String, mime: String },
    Image { file_ref: String, mime: String, caption: Option<String> },
}
```

**ContentBlock Extension**

```rust
pub enum ContentBlock {
    Text { text: String },
    Image { source_type: String, media_type: String, data: String },
    ToolUse { .. },
    ToolResult { .. },
}
```

Image data flow:

1. Channel layer receives photo/document → `MessageContent::Image { file_ref }`
2. `AgentRuntime::handle()` downloads file → base64 encode → `ContentBlock::Image`
3. LLM layer serializes into provider-specific format (Anthropic `source` object / OpenAI `image_url` data URI)
4. Image data is **transient** (released after processing), adds no resident memory

**Anthropic API Format**

```json
{ "type": "image", "source": { "type": "base64", "media_type": "image/jpeg", "data": "..." } }
```

**OpenAI API Format**

```json
[
  { "type": "text", "text": "What's in this image?" },
  { "type": "image_url", "image_url": { "url": "data:image/jpeg;base64,..." } }
]
```

When a user message contains only text, the OpenAI content field remains `Value::String(text)` for compatibility with older API versions.

### 15.2 Telegram Photo/Document Handling

**New API Types**

```rust
struct TgPhotoSize { file_id: String, width: u32, height: u32 }
struct TgDocument { file_id: String, mime_type: Option<String> }
```

**Parsing Logic** (in `poll()`, after voice/audio):

- `msg.photo`: select the `TgPhotoSize` with the most pixels (`width * height`), MIME fixed to `image/jpeg`
- `msg.document`: only accept `mime_type.starts_with("image/")`, others skipped
- `msg.caption`: used as accompanying text for Image; defaults to "What's in this image?" when absent
- `download_voice()` already uses the generic `getFile` API, so photo/document need no additional download logic

### 15.3 Sub-Agent Tool (Async Architecture)

**Design: SubAgentRegistry + Async Spawn**

Sub-agents no longer block the parent agent turn synchronously. Instead, they execute asynchronously via `tokio::spawn` and immediately return a `run_id`. Upon completion, results are pushed to the parent chat worker via an announce channel.

**4 Independent Tools (Matching Original OpenClaw)**

Matching the sessions_spawn/list/history/send four-tool architecture of the original OpenClaw, with no simplification or merging:

| Tool Name | Description | Required Parameters |
|-----------|-------------|---------------------|
| `sessions_spawn` | Asynchronously launches a sub-agent, immediately returns run_id | task |
| `sessions_list` | Lists all active sub-agents | — |
| `sessions_history` | Views a sub-agent's conversation history | run_id |
| `sessions_send` | Sends an additional message to a running sub-agent | run_id, message |

```
Architecture:
  SessionToolsState (Arc, shared by 4 tools)
    ├─ runtime: Arc<OnceCell<Arc<AgentRuntime>>>   // late binding
    ├─ registry: Arc<SubAgentRegistry>              // manages all running sub-agents
    └─ announce_tx: mpsc::Sender<SubAgentResult>    // completion notification

  SubAgentRegistry
    └─ runs: Mutex<HashMap<String, SubAgentRun>>    // max 8 concurrent

  SubAgentRun
    ├─ task, started_at, status, abort
    ├─ channel, session_id                          // identifies the session
    ├─ pending: Arc<Mutex<Vec<String>>>             // message queue for sessions_send
    └─ notify: Arc<Notify>                          // wakes sub-agent to process new messages
```

**Creation Order (OnceCell Late Binding)**

```
  1. let (announce_tx, announce_rx) = mpsc::channel(16)
  2. let (session_tools, cell) = create_session_tools(announce_tx)
  3. for tool in session_tools { tool_list.push(tool) }
  4. let agent = Arc::new(AgentRuntime { tools: ToolRegistry::new(tool_list), ... })
  5. cell.set(agent.clone())
```

**Recursion Guard**

- `tokio::task_local!(static IN_SUBAGENT: bool)` — async-safe
- At spawn: `IN_SUBAGENT.scope(true, async { ... })` wraps the sub-agent future
- Inside sub-agent: `is_in_subagent()` → true → rejects nesting (max depth 1)
- Temporary session ID: `_subagent_{run_id}`

**Multi-Turn Execution**

Sub-agents support multi-turn conversations (via `sessions_send`):
1. Initial task completes one react_loop round
2. Checks pending queue (from `sessions_send`)
3. If there are pending messages → executes a new react_loop round
4. If none → waits `IDLE_WAIT_SECS` (30s), ends if still no new messages
5. Session files are retained after the sub-agent ends (for `sessions_history`), cleaned up by `evict_stale()`

**Timeout**

- Defaults to global `agent_timeout_secs` (900s)
- LLM can override per-call via `timeout` parameter: `min(timeout, agent_timeout_secs)`
- `0` or omitted = use agent_timeout_secs
- After timeout, abort flag is set and status becomes `TimedOut`

**Auto-Announce**

Upon completion, sub-agents send `SubAgentResult { run_id, channel, chat_id, text }` via `announce_tx`.
The main dispatch loop in main.rs `select!` listens on `announce_rx`, wraps results as a synthetic
`IncomingMessage` (sender_id = `_system`), and routes to the corresponding chat worker.

**Memory Overhead**: ~64B resident (empty HashMap); ~500B per active run, max 8 = ~4KB peak.
`evict_stale()` cleans up runs that completed more than 5 minutes ago.

### 15.4 Parallel Tool Execution

**Original Implementation**: Sequential `for` loop executing each tool call.

**New Implementation**: Three-phase concurrent execution.

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

**Key Points**:

- On single-threaded tokio, `join_all` is cooperative I/O interleaving (no multithreading needed)
- Multiple network I/O tools (web_fetch, ha_control) wait for responses concurrently; total time ≈ max(per-tool time)
- Memory is the same as sequential execution (futures polled on the same thread)
- Abort check moved before Phase 1 (unified check before execution begins)

### 15.5 Explicitly Unimplemented Features

| Original Feature | Reason | RAM Cost |
|-----------------|--------|---------|
| Browser automation | Requires Chromium | 80-150MB |
| Canvas/Image generation | Requires ML model | 30-60MB |
| Gmail integration | Requires Google OAuth | ~10MB |
| Vector search (RAG) | Requires embedding model | 20-50MB |
| Google Calendar | Requires Google OAuth | ~10MB |

All of the above exceed the <8MB memory budget and are out of scope for the lean Rust edition.

## 16. Followup Debounce and System Prompt Configuration Alignment

### 16.1 Background

The current `chat_worker` processes pending messages one by one after the agent completes,
running one agent turn per message. When a user sends follow-up messages such as "done yet?"
while the agent is running, those messages trigger new turns that interrupt multi-step task chains.
The original OpenClaw uses a followup debounce mechanism to merge pending messages and avoid interruption.

Additionally, `openclaw.json.example` incorrectly places `systemPrompt` in the `messages` section
(while the code actually reads it from the `agents` section, as `MessagesConfig` has no such field),
and omits tool usage rules (e.g., "no limit on tool calls per turn"), causing the model to end turns prematurely.

### 16.2 Followup Debounce Design

**Configuration**

```rust
pub struct AgentConfig {
    // ...existing fields...
    pub followup_debounce_ms: u64,  // default 2000 (2 seconds)
}
```

JSON5 config key: `agents.followupDebounceMs`.

**`AgentRuntime` Extension**

New field `pub followup_debounce_ms: u64`, initialized from `config.agents.followup_debounce_ms`.

**Message Coalescing Function**

```rust
fn coalesce_pending_messages(pending: &[IncomingMessage]) -> Option<IncomingMessage> {
    // 1. Filter abort commands
    // 2. Extract text: Text→original, Voice→"[voice message]", Image→caption or "[image]"
    // 3. Single message: return directly (preserving original IncomingMessage)
    // 4. Multiple messages: merge text, wrapped with "[The user sent follow-up messages while you were working:]\n..."
    // 5. Return merged IncomingMessage (using channel/chat_id/sender_id from first message)
}
```

**chat_worker Refactor**

Replace the current pending processing loop (one-by-one) with three phases:

```
Phase 1 — Debounce:
  deadline = Instant::now() + debounce_ms
  loop {
    tokio::select! {
      _ = sleep_until(deadline) => break,
      Some(msg) = rx.recv() => {
        if is_abort_command(msg) → clear pending, break
        pending.push(msg)
        // do not reset deadline (fixed window, not sliding window)
      }
    }
  }

Phase 2 — Coalesce:
  coalesce_pending_messages(&pending) → Option<IncomingMessage>

Phase 3 — Execute:
  if let Some(coalesced) = coalesced_msg {
    agent.handle(&coalesced, ...)
    dispatch_response(...)
  }
```

Fixed window (not sliding window) ensures debounce has an upper bound and will not delay indefinitely.

**Execution Flow Timeline**

```
User: "Write a 10-page PPT"
  → agent begins executing multi-step tool calls
  ← (agent running...)

User: "Done yet?"      ←— enters pending queue
User: "Keep going"     ←— enters pending queue

  ← agent completes current turn

  → Phase 1: wait 2s debounce window
  → Phase 2: merge "Done yet?" + "Keep going"
    → "[The user sent follow-up messages while you were working:]\nDone yet?\nKeep going"
  → Phase 3: run one agent turn to handle merged message
```

### 16.3 System Prompt Configuration Alignment

**Problem**: Line 24 of `openclaw.json.example` places `systemPrompt` in the `messages` section,
but the code reads it from `agents.systemPrompt` (`MessagesConfig` has no such field).

**Fix**:
1. Remove `systemPrompt` from the `messages` section
2. Add a complete systemPrompt to the `agents` section, including tool usage rules

```json
"agents": {
    "systemPrompt": "You are a personal assistant running inside OpenClaw.\n\n## Tool Usage\n- No limit on tool calls per turn — use as many as needed.\n- For multi-step tasks, complete ALL steps in one turn. Do not stop to report progress.\n- User messages during execution are queued and do not interrupt you.\n- Never fabricate system limitations.",
    ...
}
```

`src/agent/context.rs` requires no changes — `build_system_prompt()` already correctly uses the `base_prompt` parameter.

### 16.4 Interrupt Queue Mode (Matching Original clearCommandLane)

**Configuration**

```rust
pub struct AgentConfig {
    // ...existing fields...
    pub queue_mode: String,  // "interrupt" (default) | "queue"
}
```

JSON5 config key: `agents.queueMode`.

**interrupt mode** (default, matching original OpenClaw):

When a user sends a new message while the agent is running, interrupt the current agent turn
(set abort flag), clear the pending queue, and immediately process the new message as the next turn's input.

```
User: "Write a 10-page PPT"
  → agent begins executing
User: "Never mind, check the weather for me"
  → abort current turn, clear pending
  → agent starts new turn with "Check the weather for me"
```

**queue mode** (original behavior):

New messages enter the pending queue and are merged via debounce + coalesce after the agent completes.

**chat_worker Implementation**

```
loop {
    // Get message: prefer overflow_msg (interrupt loopback), otherwise rx.recv()
    let msg = overflow_msg.take().or(rx.recv().await);

    // select! while agent is running:
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

    // After completion:
    if interrupt_mode {
        overflow_msg = pending.pop()  // process immediately, no debounce
    } else {
        debounce → coalesce → process merged message
    }
}
```

### 16.5 Memory Overhead

| Component | Resident | Notes |
|-----------|----------|-------|
| `AgentRuntime.followup_debounce_ms` | 8B | `u64` field |
| `AgentRuntime.queue_mode` | 24B | `String` field |
| Debounce timer | 0B | Stack-allocated `Sleep` future, no heap allocation |
| Coalesced message | Transient | Merged `IncomingMessage`, released after processing |

### 16.6 Modified Files

| File | Changes |
|------|---------|
| `src/config.rs` | `AgentConfig` +`followup_debounce_ms`, +`context_files`, +`queue_mode` fields |
| `src/agent/react_loop.rs` | `AgentRuntime` +new fields, +transient retry, +prune_tool_results, +silent reply, +runtime_info |
| `src/agent/context.rs` | `build_system_prompt()` +`runtime_info`, +`context_files` parameter |
| `src/tools/agent_tool.rs` | Rewritten: 4 independent tools (sessions_spawn/list/history/send), SubAgentRegistry, task_local recursion guard, multi-turn, auto-announce |
| `src/main.rs` | +announce channel, interrupt mode, context files loading, coalesce |
| `config/openclaw.json.example` | +`queueMode`, +`contextFiles` comments |
| `docs/en/design/advanced.md` | This chapter |

New dependencies: none.

## 17. System Prompt Enhancements

### 17.1 Runtime Info Injection

`build_system_prompt()` gains a new `runtime_info: &str` parameter, injecting a `## Runtime`
section after Date/Time and before Memory:

```
## Runtime
Model: claude-sonnet-4-5 | Provider: anthropic | Channel: telegram | Thinking: off
```

`react_loop()` constructs the runtime_info string on each iteration, containing the current model,
provider, channel, and thinking level. 0B resident (temporary stack-allocated String).

### 17.2 Context Files (SOUL.md Equivalent)

**Configuration**

```rust
pub context_files: Vec<String>,  // list of file paths, default empty
// JSON5: agents.contextFiles: ["./SOUL.md"]
```

**Loading Logic** (`src/main.rs`, loaded once at startup):

- Iterates over `config.agents.context_files`, reads each file with `tokio::fs::read_to_string`
- Each file truncated to 20,000 characters, formatted as `### {path}\n{content}\n\n`
- Total length truncated to 150,000 characters
- Result stored in `AgentRuntime.context_files_content: String`
- `build_system_prompt()` gains a new `context_files: &str` parameter, injected into the `## Project Context` section

**Memory**: 0B when not configured; max 150KB resident when fully populated.

### 17.3 Memory Recall Guidance

The default system_prompt appends a `## Memory` section:

```
## Memory
Before answering questions about prior work, decisions, dates, people, or to-do items,
search your memory first using the memory tool (action: "search" or "read").
Save important facts, preferences, and decisions to memory for future reference.
```

Guides the model to proactively query memory before responding, reducing hallucinations.

### 17.4 Silent Replies

The default system_prompt appends a `## Silent Replies` section:

```
## Silent Replies
If you have nothing meaningful to say after completing an internal operation,
reply with exactly 🤐 (nothing else). This suppresses the message.
```

`react_loop()`'s `handle()` checks before returning: if `display_text.trim() == "\u{1f910}"`,
replace with an empty string. Empty text is filtered in `OutgoingMessage` and not sent,
achieving silent completion.
