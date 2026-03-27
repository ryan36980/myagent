//! Memory management tool.
//!
//! Exposes 6 actions to the LLM: read, append, rewrite, read_log,
//! append_log, search.  The chat context (channel + chat_id) is
//! injected automatically — the agent never needs to specify it.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Mutex;

use super::Tool;
use crate::error::{GatewayError, Result};
use crate::memory::MemoryStore;

/// Per-request chat context shared between the agent runtime and tools.
pub struct ChatContext {
    pub channel: String,
    pub chat_id: String,
}

pub struct MemoryTool {
    store: MemoryStore,
    context: Arc<Mutex<ChatContext>>,
}

impl MemoryTool {
    pub fn new(store: MemoryStore, context: Arc<Mutex<ChatContext>>) -> Self {
        Self { store, context }
    }

    /// Read-only reference to the inner store (used by AgentRuntime).
    pub fn store(&self) -> &MemoryStore {
        &self.store
    }
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }

    fn description(&self) -> &str {
        "Manage persistent memory across conversations.\n\
         - read: Read your long-term MEMORY.md\n\
         - append: Add new information to MEMORY.md\n\
         - rewrite: Reorganize and rewrite MEMORY.md (use when it's getting full)\n\
         - read_log: Read a daily log entry (defaults to today)\n\
         - append_log: Append to today's daily log\n\
         - search: Search across all memory files for a keyword or phrase\n\n\
         Save user preferences, important facts, and conversation context to MEMORY.md.\n\
         Use daily logs for timestamped events and decisions.\n\
         Use search to find information across all past logs and memory.\n\n\
         Use scope: \"shared\" for cross-conversation knowledge (device IPs, network config,\n\
         household members, universal preferences). Default scope is per-conversation.\n\
         Logs (read_log/append_log) and search are always per-conversation."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["read", "append", "rewrite", "read_log", "append_log", "search"],
                    "description": "The memory operation to perform"
                },
                "content": {
                    "type": "string",
                    "description": "Text content (for append, rewrite, append_log)"
                },
                "date": {
                    "type": "string",
                    "description": "Date in YYYY-MM-DD format (for read_log; defaults to today)"
                },
                "query": {
                    "type": "string",
                    "description": "Search query string (for search)"
                },
                "scope": {
                    "type": "string",
                    "enum": ["chat", "shared"],
                    "description": "Memory scope: 'chat' (default) or 'shared' (cross-conversation knowledge)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String> {
        let action = input["action"]
            .as_str()
            .ok_or_else(|| GatewayError::Tool {
                tool: "memory".into(),
                message: "action is required".into(),
            })?;

        let scope = input["scope"].as_str().unwrap_or("chat");
        let shared = scope == "shared";

        let ctx = self.context.lock().await;
        let ch = &ctx.channel;
        let cid = &ctx.chat_id;

        match action {
            "read" if shared => self.store.read_shared().await.map(|c| {
                if c.is_empty() {
                    "Shared MEMORY.md is empty.".to_string()
                } else {
                    c
                }
            }),
            "read" => self.store.read(ch, cid).await.map(|c| {
                if c.is_empty() {
                    "MEMORY.md is empty.".to_string()
                } else {
                    c
                }
            }),
            "append" if shared => {
                let content = input["content"]
                    .as_str()
                    .ok_or_else(|| GatewayError::Tool {
                        tool: "memory".into(),
                        message: "content is required for append".into(),
                    })?;
                self.store.append_shared(content).await
            }
            "append" => {
                let content = input["content"]
                    .as_str()
                    .ok_or_else(|| GatewayError::Tool {
                        tool: "memory".into(),
                        message: "content is required for append".into(),
                    })?;
                self.store.append(ch, cid, content).await
            }
            "rewrite" if shared => {
                let content = input["content"]
                    .as_str()
                    .ok_or_else(|| GatewayError::Tool {
                        tool: "memory".into(),
                        message: "content is required for rewrite".into(),
                    })?;
                self.store.rewrite_shared(content).await
            }
            "rewrite" => {
                let content = input["content"]
                    .as_str()
                    .ok_or_else(|| GatewayError::Tool {
                        tool: "memory".into(),
                        message: "content is required for rewrite".into(),
                    })?;
                self.store.rewrite(ch, cid, content).await
            }
            "read_log" => {
                // Logs are always per-chat; scope is ignored
                let date = input["date"]
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        chrono::Local::now().format("%Y-%m-%d").to_string()
                    });
                self.store.read_log(ch, cid, &date).await.map(|c| {
                    if c.is_empty() {
                        format!("No log for {}.", date)
                    } else {
                        c
                    }
                })
            }
            "append_log" => {
                // Logs are always per-chat; scope is ignored
                let content = input["content"]
                    .as_str()
                    .ok_or_else(|| GatewayError::Tool {
                        tool: "memory".into(),
                        message: "content is required for append_log".into(),
                    })?;
                self.store.append_log(ch, cid, content).await
            }
            "search" if shared => {
                let query = input["query"]
                    .as_str()
                    .ok_or_else(|| GatewayError::Tool {
                        tool: "memory".into(),
                        message: "query is required for search".into(),
                    })?;
                self.store.search_shared(query, 20).await
            }
            "search" => {
                let query = input["query"]
                    .as_str()
                    .ok_or_else(|| GatewayError::Tool {
                        tool: "memory".into(),
                        message: "query is required for search".into(),
                    })?;
                self.store.search(ch, cid, query, 20).await
            }
            _ => Err(GatewayError::Tool {
                tool: "memory".into(),
                message: format!("unknown action: {}", action),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool() -> (tempfile::TempDir, MemoryTool) {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::new(dir.path().to_str().unwrap(), 4096, 4096);
        let ctx = Arc::new(Mutex::new(ChatContext {
            channel: "tg".into(),
            chat_id: "42".into(),
        }));
        (dir, MemoryTool::new(store, ctx))
    }

    #[tokio::test]
    async fn memory_read_empty() {
        let (_dir, tool) = make_tool();
        tool.store().init().await.unwrap();
        let result = tool.execute(json!({"action": "read"})).await.unwrap();
        assert_eq!(result, "MEMORY.md is empty.");
    }

    #[tokio::test]
    async fn memory_append_and_read() {
        let (_dir, tool) = make_tool();
        tool.store().init().await.unwrap();
        let r = tool
            .execute(json!({"action": "append", "content": "User likes cats"}))
            .await
            .unwrap();
        assert!(r.contains("Saved"));

        let content = tool.execute(json!({"action": "read"})).await.unwrap();
        assert!(content.contains("User likes cats"));
    }

    #[tokio::test]
    async fn memory_rewrite() {
        let (_dir, tool) = make_tool();
        tool.store().init().await.unwrap();
        tool.execute(json!({"action": "append", "content": "old"}))
            .await
            .unwrap();
        tool.execute(json!({"action": "rewrite", "content": "new\n"}))
            .await
            .unwrap();
        let content = tool.execute(json!({"action": "read"})).await.unwrap();
        assert!(content.contains("new"));
        assert!(!content.contains("old"));
    }

    #[tokio::test]
    async fn memory_append_log() {
        let (_dir, tool) = make_tool();
        tool.store().init().await.unwrap();
        let r = tool
            .execute(json!({"action": "append_log", "content": "deployed v1"}))
            .await
            .unwrap();
        assert!(r.contains(".md"));

        let log = tool.execute(json!({"action": "read_log"})).await.unwrap();
        assert!(log.contains("deployed v1"));
    }

    #[tokio::test]
    async fn memory_search() {
        let (_dir, tool) = make_tool();
        tool.store().init().await.unwrap();
        tool.execute(json!({"action": "append", "content": "User prefers dark mode"}))
            .await
            .unwrap();
        let result = tool
            .execute(json!({"action": "search", "query": "dark mode"}))
            .await
            .unwrap();
        assert!(result.contains("dark mode"));
    }

    // -------------------------------------------------------------------
    // Shared scope tests
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn shared_append_and_read() {
        let (_dir, tool) = make_tool();
        tool.store().init().await.unwrap();
        let r = tool
            .execute(json!({"action": "append", "scope": "shared", "content": "TV: 10.0.0.116"}))
            .await
            .unwrap();
        assert!(r.contains("Saved"));

        let content = tool
            .execute(json!({"action": "read", "scope": "shared"}))
            .await
            .unwrap();
        assert!(content.contains("10.0.0.116"));
    }

    #[tokio::test]
    async fn shared_rewrite() {
        let (_dir, tool) = make_tool();
        tool.store().init().await.unwrap();
        tool.execute(json!({"action": "append", "scope": "shared", "content": "old"}))
            .await
            .unwrap();
        tool.execute(json!({"action": "rewrite", "scope": "shared", "content": "new\n"}))
            .await
            .unwrap();
        let content = tool
            .execute(json!({"action": "read", "scope": "shared"}))
            .await
            .unwrap();
        assert!(content.contains("new"));
        assert!(!content.contains("old"));
    }

    #[tokio::test]
    async fn shared_read_empty() {
        let (_dir, tool) = make_tool();
        tool.store().init().await.unwrap();
        let result = tool
            .execute(json!({"action": "read", "scope": "shared"}))
            .await
            .unwrap();
        assert_eq!(result, "Shared MEMORY.md is empty.");
    }

    #[tokio::test]
    async fn default_scope_is_chat() {
        let (_dir, tool) = make_tool();
        tool.store().init().await.unwrap();
        // Write to chat scope (default)
        tool.execute(json!({"action": "append", "content": "chat-only fact"}))
            .await
            .unwrap();
        // Shared should be empty
        let shared = tool
            .execute(json!({"action": "read", "scope": "shared"}))
            .await
            .unwrap();
        assert_eq!(shared, "Shared MEMORY.md is empty.");
        // Chat should have it
        let chat = tool.execute(json!({"action": "read"})).await.unwrap();
        assert!(chat.contains("chat-only fact"));
    }

    #[tokio::test]
    async fn log_operations_ignore_shared_scope() {
        let (_dir, tool) = make_tool();
        tool.store().init().await.unwrap();
        // append_log with shared scope should still write to per-chat
        let r = tool
            .execute(json!({"action": "append_log", "scope": "shared", "content": "event X"}))
            .await
            .unwrap();
        assert!(r.contains(".md"));

        // read_log with shared scope should still read per-chat
        let log = tool
            .execute(json!({"action": "read_log", "scope": "shared"}))
            .await
            .unwrap();
        assert!(log.contains("event X"));
    }

    #[tokio::test]
    async fn shared_search() {
        let (_dir, tool) = make_tool();
        tool.store().init().await.unwrap();
        tool.execute(json!({"action": "append", "scope": "shared", "content": "router: 10.0.0.1"}))
            .await
            .unwrap();
        let result = tool
            .execute(json!({"action": "search", "scope": "shared", "query": "router"}))
            .await
            .unwrap();
        assert!(result.contains("10.0.0.1"));
    }
}
