//! Sub-Agent session tools — matching original OpenClaw's 4-tool architecture.
//!
//! Four separate tools (aligned with original OpenClaw):
//!   - `sessions_spawn`   — spawn a sub-agent with a task
//!   - `sessions_list`    — list active sub-agents
//!   - `sessions_history` — view a sub-agent's conversation transcript
//!   - `sessions_send`    — send additional message to a running sub-agent
//!
//! Sub-agents run asynchronously via `tokio::spawn` and auto-announce
//! results to the parent chat worker.  They support multi-turn execution:
//! after the initial task, additional messages from `sessions_send` trigger
//! new react_loop turns in the same session.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::{mpsc, Mutex, Notify, OnceCell};
use tracing::info;

use super::Tool;
use crate::agent::react_loop::AgentRuntime;
use crate::channel::types::{ContentBlock, Role};
use crate::error::Result;

// ---------------------------------------------------------------------------
// Recursion guard via task-local
// ---------------------------------------------------------------------------

tokio::task_local! {
    static IN_SUBAGENT: bool;
}

fn is_in_subagent() -> bool {
    IN_SUBAGENT.try_with(|v| *v).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Sub-agent result (sent via announce channel)
// ---------------------------------------------------------------------------

/// Result of a completed sub-agent run, routed to the parent chat worker.
pub struct SubAgentResult {
    pub run_id: String,
    pub channel: String,
    pub chat_id: String,
    pub text: String,
}

// ---------------------------------------------------------------------------
// SubAgentRegistry
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum SubAgentStatus {
    Running,
    Completed,
    TimedOut,
}

struct SubAgentRun {
    task: String,
    started_at: Instant,
    status: SubAgentStatus,
    /// Channel/session identifiers for this sub-agent.
    channel: String,
    session_id: String,
    /// Pending messages from parent (via sessions_send).
    pending: Arc<Mutex<Vec<String>>>,
    /// Notification signal: wakes the sub-agent loop when a message is sent.
    notify: Arc<Notify>,
}

const MAX_CONCURRENT: usize = 8;
const STALE_SECS: u64 = 300; // 5 minutes
/// How long a sub-agent waits for additional messages after a turn completes.
const IDLE_WAIT_SECS: u64 = 30;

pub(crate) struct SubAgentRegistry {
    runs: Mutex<HashMap<String, SubAgentRun>>,
}

impl SubAgentRegistry {
    fn new() -> Self {
        Self {
            runs: Mutex::new(HashMap::new()),
        }
    }

    async fn evict_stale(&self, runtime: Option<&AgentRuntime>) {
        let mut runs = self.runs.lock().await;
        let stale_keys: Vec<String> = runs
            .iter()
            .filter(|(_, run)| match &run.status {
                SubAgentStatus::Completed
                | SubAgentStatus::TimedOut => run.started_at.elapsed().as_secs() >= STALE_SECS,
                SubAgentStatus::Running => false,
            })
            .map(|(k, _)| k.clone())
            .collect();

        for key in &stale_keys {
            if let Some(run) = runs.remove(key) {
                // Clean up session file when evicting
                if let Some(rt) = runtime {
                    let _ = rt.sessions.clear(&run.channel, &run.session_id).await;
                }
            }
        }
    }

    async fn running_count(&self) -> usize {
        let runs = self.runs.lock().await;
        runs.values()
            .filter(|r| matches!(r.status, SubAgentStatus::Running))
            .count()
    }
}

// ---------------------------------------------------------------------------
// Shared state between all session tools
// ---------------------------------------------------------------------------

/// Shared state between the 4 session tools.
pub struct SessionToolsState {
    pub runtime: Arc<OnceCell<Arc<AgentRuntime>>>,
    pub(crate) registry: Arc<SubAgentRegistry>,
    pub announce_tx: mpsc::Sender<SubAgentResult>,
}

impl SessionToolsState {
    fn get_runtime(&self) -> Result<&Arc<AgentRuntime>> {
        self.runtime
            .get()
            .ok_or_else(|| crate::error::GatewayError::Tool {
                tool: "sessions".into(),
                message: "agent runtime not initialized".into(),
            })
    }

    fn gen_run_id() -> String {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        format!("r{}", ts % 1_000_000)
    }
}

// ---------------------------------------------------------------------------
// Constructor: creates 4 tools sharing one registry + OnceCell
// ---------------------------------------------------------------------------

/// Create the 4 session tools and return them + the OnceCell for late-binding.
///
/// After constructing `AgentRuntime`, call `cell.set(agent.clone())`.
pub fn create_session_tools(
    announce_tx: mpsc::Sender<SubAgentResult>,
) -> (Vec<Box<dyn Tool>>, Arc<OnceCell<Arc<AgentRuntime>>>) {
    let cell = Arc::new(OnceCell::new());
    let state = Arc::new(SessionToolsState {
        runtime: cell.clone(),
        registry: Arc::new(SubAgentRegistry::new()),
        announce_tx,
    });

    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(SessionsSpawnTool {
            state: state.clone(),
        }),
        Box::new(SessionsListTool {
            state: state.clone(),
        }),
        Box::new(SessionsHistoryTool {
            state: state.clone(),
        }),
        Box::new(SessionsSendTool { state }),
    ];

    (tools, cell)
}

// ===========================================================================
// 1. sessions_spawn
// ===========================================================================

pub struct SessionsSpawnTool {
    state: Arc<SessionToolsState>,
}

#[async_trait]
impl Tool for SessionsSpawnTool {
    fn name(&self) -> &str {
        "sessions_spawn"
    }

    fn description(&self) -> &str {
        "Spawn a sub-agent to run a task in an isolated session.\n\
         Returns a run_id immediately. The sub-agent executes asynchronously \
         and auto-announces the result when done.\n\
         Use sessions_send to send additional instructions to a running sub-agent.\n\
         Max depth: 1 (sub-agents cannot spawn their own sub-agents)."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The task for the sub-agent to perform"
                },
                "context": {
                    "type": "string",
                    "description": "Optional additional context"
                }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String> {
        if is_in_subagent() {
            return Ok("Error: sub-agent nesting is not allowed (max depth = 1).".into());
        }

        let runtime = self.state.get_runtime()?;

        let task = input["task"]
            .as_str()
            .ok_or_else(|| crate::error::GatewayError::Tool {
                tool: "sessions_spawn".into(),
                message: "'task' field is required".into(),
            })?;

        let context = input["context"].as_str().unwrap_or("");

        // Evict stale runs and check capacity
        self.state.registry.evict_stale(Some(runtime)).await;
        if self.state.registry.running_count().await >= MAX_CONCURRENT {
            return Ok(format!(
                "Error: max concurrent sub-agents ({}) reached. \
                 Use sessions_list to check active sub-agents.",
                MAX_CONCURRENT
            ));
        }

        let run_id = SessionToolsState::gen_run_id();
        let session_id = format!("_subagent_{}", run_id);

        let (parent_channel, parent_chat_id) = {
            let ctx = runtime.chat_context.lock().await;
            (ctx.channel.clone(), ctx.chat_id.clone())
        };

        // Build task message (context + task, no parent memory injection —
        // parent can use sessions_send for additional context, matching original)
        let full_task = if context.is_empty() {
            task.to_string()
        } else {
            format!("Context: {}\n\nTask: {}", context, task)
        };

        let timeout_secs = runtime.agent_timeout_secs;

        let abort = Arc::new(AtomicBool::new(false));
        let pending = Arc::new(Mutex::new(Vec::<String>::new()));
        let notify = Arc::new(Notify::new());

        // Register run
        {
            let mut runs = self.state.registry.runs.lock().await;
            runs.insert(
                run_id.clone(),
                SubAgentRun {
                    task: task.to_string(),
                    started_at: Instant::now(),
                    status: SubAgentStatus::Running,
                    channel: parent_channel.clone(),
                    session_id: session_id.clone(),
                    pending: pending.clone(),
                    notify: notify.clone(),
                },
            );
        }

        // Spawn async multi-turn sub-agent
        let runtime_clone = runtime.clone();
        let registry_clone = self.state.registry.clone();
        let announce_tx = self.state.announce_tx.clone();
        let run_id_clone = run_id.clone();

        tokio::spawn(IN_SUBAGENT.scope(true, async move {
            let deadline =
                tokio::time::Instant::now() + Duration::from_secs(timeout_secs);

            info!(
                run_id = %run_id_clone,
                task_len = full_task.len(),
                timeout_secs,
                "sub-agent starting"
            );

            // --- Turn 1: initial task ---
            let mut last_text = match tokio::select! {
                r = runtime_clone.react_loop(
                    &parent_channel, &session_id, &full_task, &abort, None, Vec::new(),
                ) => r,
                _ = tokio::time::sleep_until(deadline) => {
                    abort.store(true, Ordering::Relaxed);
                    Ok("Sub-agent timed out.".into())
                }
            } {
                Ok(t) => t,
                Err(e) => format!("Sub-agent error: {}", e),
            };

            // --- Multi-turn loop: process pending messages from sessions_send ---
            loop {
                if abort.load(Ordering::Relaxed) {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    last_text = "Sub-agent timed out.".into();
                    break;
                }

                // Drain pending queue
                let next_msg = {
                    let mut q = pending.lock().await;
                    if q.is_empty() {
                        None
                    } else {
                        Some(q.remove(0))
                    }
                };

                match next_msg {
                    Some(msg) => {
                        info!(run_id = %run_id_clone, msg_len = msg.len(), "sub-agent processing sent message");
                        last_text = match tokio::select! {
                            r = runtime_clone.react_loop(
                                &parent_channel, &session_id, &msg, &abort, None, Vec::new(),
                            ) => r,
                            _ = tokio::time::sleep_until(deadline) => {
                                abort.store(true, Ordering::Relaxed);
                                Ok("Sub-agent timed out.".into())
                            }
                        } {
                            Ok(t) => t,
                            Err(e) => {
                                last_text = format!("Sub-agent error: {}", e);
                                break;
                            }
                        };
                    }
                    None => {
                        // No pending — wait for notification or idle timeout
                        let remaining = deadline
                            .saturating_duration_since(tokio::time::Instant::now());
                        let wait_dur =
                            Duration::from_secs(IDLE_WAIT_SECS).min(remaining);

                        tokio::select! {
                            _ = notify.notified() => continue,
                            _ = tokio::time::sleep(wait_dur) => break,
                        }
                    }
                }
            }

            // Update registry status
            let is_timeout = last_text == "Sub-agent timed out.";
            {
                let mut runs = registry_clone.runs.lock().await;
                if let Some(run) = runs.get_mut(&run_id_clone) {
                    run.status = if is_timeout {
                        SubAgentStatus::TimedOut
                    } else {
                        SubAgentStatus::Completed
                    };
                }
            }
            // NOTE: session file is NOT cleared here — kept for sessions_history.
            // Cleaned up in evict_stale() after STALE_SECS.

            info!(
                run_id = %run_id_clone,
                reply_len = last_text.len(),
                is_timeout,
                "sub-agent completed"
            );

            let _ = announce_tx
                .send(SubAgentResult {
                    run_id: run_id_clone,
                    channel: parent_channel,
                    chat_id: parent_chat_id,
                    text: last_text.chars().take(2000).collect(),
                })
                .await;
        }));

        Ok(format!(
            "Sub-agent spawned (run_id: {}). The result will be auto-announced when done. \
             Use sessions_send to send additional instructions if needed.",
            run_id
        ))
    }
}

// ===========================================================================
// 2. sessions_list
// ===========================================================================

pub struct SessionsListTool {
    state: Arc<SessionToolsState>,
}

#[async_trait]
impl Tool for SessionsListTool {
    fn name(&self) -> &str {
        "sessions_list"
    }

    fn description(&self) -> &str {
        "List all active sub-agent sessions with their status, elapsed time, and task summary."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(&self, _input: serde_json::Value) -> Result<String> {
        let runtime = self.state.get_runtime().ok();
        self.state
            .registry
            .evict_stale(runtime.map(|r| r.as_ref()))
            .await;
        let runs = self.state.registry.runs.lock().await;

        if runs.is_empty() {
            return Ok("No active sub-agents.".into());
        }

        let mut lines = Vec::new();
        for (id, run) in runs.iter() {
            let elapsed = run.started_at.elapsed().as_secs();
            let status = match &run.status {
                SubAgentStatus::Running => format!("running ({}s)", elapsed),
                SubAgentStatus::Completed => "completed".into(),
                SubAgentStatus::TimedOut => "timed out".into(),
            };
            let task_preview: String = run.task.chars().take(80).collect();
            lines.push(format!("- {} [{}]: {}", id, status, task_preview));
        }
        Ok(lines.join("\n"))
    }
}

// ===========================================================================
// 3. sessions_history
// ===========================================================================

pub struct SessionsHistoryTool {
    state: Arc<SessionToolsState>,
}

#[async_trait]
impl Tool for SessionsHistoryTool {
    fn name(&self) -> &str {
        "sessions_history"
    }

    fn description(&self) -> &str {
        "View a sub-agent's conversation transcript. Shows the messages exchanged \
         between the sub-agent and the LLM, including tool calls and results. \
         Useful for understanding what the sub-agent did."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "run_id": {
                    "type": "string",
                    "description": "The sub-agent's run_id"
                },
                "last_n": {
                    "type": "integer",
                    "description": "Only show the last N messages (default: 10)"
                }
            },
            "required": ["run_id"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String> {
        let run_id = input["run_id"]
            .as_str()
            .ok_or_else(|| crate::error::GatewayError::Tool {
                tool: "sessions_history".into(),
                message: "'run_id' is required".into(),
            })?;

        let runtime = self.state.get_runtime()?;

        // Look up the sub-agent's session info
        let (channel, session_id) = {
            let runs = self.state.registry.runs.lock().await;
            match runs.get(run_id) {
                Some(run) => (run.channel.clone(), run.session_id.clone()),
                None => {
                    return Ok(format!("No sub-agent found with run_id: {}", run_id))
                }
            }
        };

        // Load session transcript
        let messages = runtime.sessions.load(&channel, &session_id).await?;
        if messages.is_empty() {
            return Ok(format!(
                "Sub-agent {} has no conversation history.",
                run_id
            ));
        }

        let last_n = input["last_n"].as_u64().unwrap_or(10) as usize;
        let start = messages.len().saturating_sub(last_n);
        let slice = &messages[start..];

        let mut output = format!(
            "Sub-agent {} transcript ({}/{} messages):\n\n",
            run_id,
            slice.len(),
            messages.len()
        );

        for (i, msg) in slice.iter().enumerate() {
            let role = match msg.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
            };
            output.push_str(&format!("[{}] {}:\n", start + i + 1, role));

            for block in &msg.content {
                match block {
                    ContentBlock::Text { text } => {
                        let preview: String = text.chars().take(300).collect();
                        output.push_str(&format!("  {}\n", preview));
                    }
                    ContentBlock::ToolUse { name, .. } => {
                        output.push_str(&format!("  [tool_use: {}]\n", name));
                    }
                    ContentBlock::ToolResult { content, .. } => {
                        let preview: String = content.chars().take(200).collect();
                        output.push_str(&format!("  [tool_result: {}...]\n", preview));
                    }
                    ContentBlock::Image { .. } => {
                        output.push_str("  [image]\n");
                    }
                }
            }
            output.push('\n');
        }

        Ok(output)
    }
}

// ===========================================================================
// 4. sessions_send
// ===========================================================================

pub struct SessionsSendTool {
    state: Arc<SessionToolsState>,
}

#[async_trait]
impl Tool for SessionsSendTool {
    fn name(&self) -> &str {
        "sessions_send"
    }

    fn description(&self) -> &str {
        "Send an additional message to a running sub-agent. The message will be \
         processed as a new turn in the sub-agent's session after its current \
         turn completes. Use this to provide additional context, corrections, \
         or follow-up instructions."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "run_id": {
                    "type": "string",
                    "description": "The sub-agent's run_id"
                },
                "message": {
                    "type": "string",
                    "description": "The message to send to the sub-agent"
                }
            },
            "required": ["run_id", "message"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String> {
        let run_id = input["run_id"]
            .as_str()
            .ok_or_else(|| crate::error::GatewayError::Tool {
                tool: "sessions_send".into(),
                message: "'run_id' is required".into(),
            })?;

        let message = input["message"]
            .as_str()
            .ok_or_else(|| crate::error::GatewayError::Tool {
                tool: "sessions_send".into(),
                message: "'message' is required".into(),
            })?;

        let runs = self.state.registry.runs.lock().await;
        match runs.get(run_id) {
            Some(run) => {
                if !matches!(run.status, SubAgentStatus::Running) {
                    return Ok(format!(
                        "Sub-agent {} is not running (cannot send messages to completed sub-agents).",
                        run_id
                    ));
                }
                // Push message and wake the sub-agent
                run.pending.lock().await.push(message.to_string());
                run.notify.notify_one();
                Ok(format!(
                    "Message sent to sub-agent {}. It will be processed after the current turn.",
                    run_id
                ))
            }
            None => Ok(format!("No sub-agent found with run_id: {}", run_id)),
        }
    }
}
