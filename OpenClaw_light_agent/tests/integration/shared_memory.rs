//! Integration tests for shared memory (scope: "shared").
//!
//! Verifies end-to-end: MemoryTool with scope parameter dispatches
//! correctly between per-chat and shared storage, and build_context
//! assembles both layers.

use std::sync::Arc;

use serde_json::json;
use tokio::sync::Mutex;

use openclaw_light::memory::MemoryStore;
use openclaw_light::tools::memory::{ChatContext, MemoryTool};
use openclaw_light::tools::Tool;

fn make_tool(dir: &std::path::Path, channel: &str, chat_id: &str) -> MemoryTool {
    let store = MemoryStore::new(dir.to_str().unwrap(), 4096, 4096);
    let ctx = Arc::new(Mutex::new(ChatContext {
        channel: channel.into(),
        chat_id: chat_id.into(),
    }));
    MemoryTool::new(store, ctx)
}

/// Two different chats write to shared, both can read.
#[tokio::test]
async fn shared_memory_visible_across_chats() {
    let dir = tempfile::tempdir().unwrap();

    let tool_a = make_tool(dir.path(), "telegram", "user_a");
    tool_a.store().init().await.unwrap();

    let tool_b = make_tool(dir.path(), "feishu", "user_b");

    // User A writes shared knowledge
    let r = tool_a
        .execute(json!({
            "action": "append",
            "scope": "shared",
            "content": "TV IP: 10.0.0.116"
        }))
        .await
        .unwrap();
    assert!(r.contains("Saved"));

    // User B can read it
    let content = tool_b
        .execute(json!({"action": "read", "scope": "shared"}))
        .await
        .unwrap();
    assert!(content.contains("10.0.0.116"));
}

/// Per-chat memory is isolated between chats.
#[tokio::test]
async fn per_chat_memory_isolated() {
    let dir = tempfile::tempdir().unwrap();

    let tool_a = make_tool(dir.path(), "telegram", "user_a");
    tool_a.store().init().await.unwrap();

    let tool_b = make_tool(dir.path(), "telegram", "user_b");

    // User A writes per-chat
    tool_a
        .execute(json!({"action": "append", "content": "A's secret"}))
        .await
        .unwrap();

    // User B cannot see it
    let content = tool_b
        .execute(json!({"action": "read"}))
        .await
        .unwrap();
    assert_eq!(content, "MEMORY.md is empty.");
}

/// build_context includes both shared and per-chat memory.
#[tokio::test]
async fn build_context_merges_shared_and_chat() {
    let dir = tempfile::tempdir().unwrap();

    let tool = make_tool(dir.path(), "telegram", "42");
    tool.store().init().await.unwrap();

    // Write shared
    tool.execute(json!({
        "action": "append",
        "scope": "shared",
        "content": "Router: 10.0.0.1"
    }))
    .await
    .unwrap();

    // Write per-chat
    tool.execute(json!({
        "action": "append",
        "content": "User prefers dark mode"
    }))
    .await
    .unwrap();

    let ctx = tool.store().build_context("telegram", "42").await.unwrap();

    // Both present
    assert!(ctx.contains("### Shared MEMORY.md"));
    assert!(ctx.contains("10.0.0.1"));
    assert!(ctx.contains("### MEMORY.md"));
    assert!(ctx.contains("dark mode"));

    // Shared comes first
    let shared_pos = ctx.find("Shared MEMORY.md").unwrap();
    let chat_pos = ctx.find("### MEMORY.md").unwrap();
    assert!(shared_pos < chat_pos);
}

/// Shared rewrite replaces content, visible to all chats.
#[tokio::test]
async fn shared_rewrite_visible_to_other_chat() {
    let dir = tempfile::tempdir().unwrap();

    let tool_a = make_tool(dir.path(), "telegram", "a");
    tool_a.store().init().await.unwrap();

    let tool_b = make_tool(dir.path(), "feishu", "b");

    // A writes, then rewrites shared
    tool_a
        .execute(json!({
            "action": "append",
            "scope": "shared",
            "content": "old info"
        }))
        .await
        .unwrap();
    tool_a
        .execute(json!({
            "action": "rewrite",
            "scope": "shared",
            "content": "Router: 192.168.1.1\nTV: 192.168.1.100\n"
        }))
        .await
        .unwrap();

    // B sees the rewritten content
    let content = tool_b
        .execute(json!({"action": "read", "scope": "shared"}))
        .await
        .unwrap();
    assert!(content.contains("192.168.1.1"));
    assert!(!content.contains("old info"));
}

/// Search with scope "shared" only searches SHARED/MEMORY.md.
#[tokio::test]
async fn shared_search_does_not_leak_per_chat() {
    let dir = tempfile::tempdir().unwrap();

    let tool = make_tool(dir.path(), "telegram", "42");
    tool.store().init().await.unwrap();

    // Write per-chat only
    tool.execute(json!({"action": "append", "content": "secret password: hunter2"}))
        .await
        .unwrap();

    // Write shared
    tool.execute(json!({
        "action": "append",
        "scope": "shared",
        "content": "NAS IP: 10.0.0.50"
    }))
    .await
    .unwrap();

    // Shared search should NOT find per-chat content
    let result = tool
        .execute(json!({"action": "search", "scope": "shared", "query": "hunter2"}))
        .await
        .unwrap();
    assert!(result.contains("No results"));

    // Shared search SHOULD find shared content
    let result = tool
        .execute(json!({"action": "search", "scope": "shared", "query": "NAS"}))
        .await
        .unwrap();
    assert!(result.contains("10.0.0.50"));
}

/// Log operations always go to per-chat, even with scope "shared".
#[tokio::test]
async fn log_operations_always_per_chat() {
    let dir = tempfile::tempdir().unwrap();

    let tool_a = make_tool(dir.path(), "telegram", "a");
    tool_a.store().init().await.unwrap();

    let tool_b = make_tool(dir.path(), "telegram", "b");

    // A appends log with scope "shared" — should still go to per-chat
    tool_a
        .execute(json!({
            "action": "append_log",
            "scope": "shared",
            "content": "A's event"
        }))
        .await
        .unwrap();

    // B cannot see A's log
    let log = tool_b
        .execute(json!({"action": "read_log", "scope": "shared"}))
        .await
        .unwrap();
    assert!(!log.contains("A's event"));
}
