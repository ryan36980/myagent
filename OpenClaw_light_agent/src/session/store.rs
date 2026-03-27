//! JSONL file-based session store.
//!
//! Each chat has its own JSONL file: `{session_dir}/{channel}_{chat_id}.jsonl`.
//! Messages are appended, and a sliding window keeps only the last N messages.

use std::path::PathBuf;

use tracing::{debug, warn};

use crate::channel::types::{ChatMessage, ContentBlock, Role};
use crate::error::Result;

/// Repair trailing orphaned `tool_use` blocks at the end of the history.
///
/// When the agent is interrupted mid-tool-execution (timeout, crash, restart),
/// the session may end with an Assistant `tool_use` message that has no
/// corresponding User `tool_result`.  The Claude API rejects this with:
///   "tool_use ids were found without tool_result blocks immediately after"
///
/// This function appends a synthetic `tool_result` for each orphaned `tool_use`,
/// allowing the conversation to resume cleanly.
pub(crate) fn repair_trailing_tool_use(messages: &mut Vec<ChatMessage>) {
    // Find the last Assistant message; if it contains tool_use blocks,
    // check that a matching User tool_result follows.
    let last_assistant_idx = messages
        .iter()
        .rposition(|m| m.role == Role::Assistant);
    let Some(idx) = last_assistant_idx else {
        return;
    };

    // Collect tool_use IDs from the last Assistant message
    let tool_use_ids: Vec<String> = messages[idx]
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();

    if tool_use_ids.is_empty() {
        return;
    }

    // Check if there's a following User message with tool_results for ALL ids
    let mut answered: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for msg in &messages[idx + 1..] {
        for block in &msg.content {
            if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                answered.insert(tool_use_id.as_str());
            }
        }
    }

    let orphaned: Vec<&String> = tool_use_ids
        .iter()
        .filter(|id| !answered.contains(id.as_str()))
        .collect();

    if orphaned.is_empty() {
        return;
    }

    warn!(
        orphaned_count = orphaned.len(),
        "repairing trailing tool_use without tool_result (agent was likely interrupted)"
    );

    // Append a synthetic User message with error tool_results
    let tool_results: Vec<ContentBlock> = orphaned
        .into_iter()
        .map(|id| ContentBlock::ToolResult {
            tool_use_id: id.clone(),
            content: "[interrupted] Tool execution was interrupted by a restart or timeout. \
                      The result is unavailable. Please retry if needed."
                .into(),
        })
        .collect();

    messages.push(ChatMessage {
        role: Role::User,
        content: tool_results,
    });
}

/// Remove orphaned messages after truncation to maintain API validity.
///
/// After sliding window or compaction truncation, the first remaining message
/// may be a User `tool_result` whose corresponding Assistant `tool_use` was
/// truncated. The Anthropic API rejects this with "unexpected tool_use_id".
///
/// This function strips leading messages until a User message with at least
/// one `Text` content block is found — that's a valid conversation start.
pub(crate) fn sanitize_after_truncation(messages: &mut Vec<ChatMessage>) {
    let valid_start = messages.iter().position(|m| {
        m.role == Role::User
            && m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { .. }))
    });
    match valid_start {
        Some(0) => {} // already valid
        Some(pos) => {
            let kept = messages.split_off(pos);
            *messages = kept;
        }
        None => {} // no valid user text message found, leave as-is
    }
}

/// Limit conversation history to the last N **user turns**.
///
/// A "user turn" is a User message that contains at least one `Text` content
/// block (tool_result-only messages are not counted as turns).  This matches
/// the original OpenClaw `limitHistoryTurns()` algorithm.
///
/// When the number of user turns exceeds `limit`, older messages (including
/// their associated assistant responses) are dropped from the front.
fn limit_history_turns(messages: &mut Vec<ChatMessage>, limit: usize) {
    if limit == 0 || messages.is_empty() {
        return;
    }

    // A "user turn" is a User message with at least one Text block.
    // tool_result-only User messages are NOT counted as turns (matching
    // original OpenClaw's limitHistoryTurns algorithm).
    let is_user_text_turn = |m: &ChatMessage| -> bool {
        m.role == Role::User
            && m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { .. }))
    };

    let mut user_count: usize = 0;
    let mut last_user_index = messages.len(); // "just past the end"

    for i in (0..messages.len()).rev() {
        if is_user_text_turn(&messages[i]) {
            user_count += 1;
            if user_count > limit {
                // Keep from last_user_index (the start of the Nth-from-end
                // user turn) onwards.
                let kept = messages.split_off(last_user_index);
                *messages = kept;
                return;
            }
            last_user_index = i;
        }
    }
    // user_count <= limit — keep everything
}

/// JSONL-based session store with sliding window + user-turn limiting.
pub struct SessionStore {
    dir: PathBuf,
    history_limit: usize,
    dm_history_limit: usize,
}

impl SessionStore {
    pub fn new(dir: &str, history_limit: usize, dm_history_limit: usize) -> Self {
        Self {
            dir: PathBuf::from(dir),
            history_limit,
            dm_history_limit,
        }
    }

    /// Return the configured history limit.
    pub fn history_limit(&self) -> usize {
        self.history_limit
    }

    /// Ensure the session directory exists.
    pub async fn init(&self) -> Result<()> {
        tokio::fs::create_dir_all(&self.dir).await?;
        Ok(())
    }

    fn session_path(&self, channel: &str, chat_id: &str) -> PathBuf {
        self.dir.join(format!("{}_{}.jsonl", channel, chat_id))
    }

    /// Load chat history for a session.
    pub async fn load(&self, channel: &str, chat_id: &str) -> Result<Vec<ChatMessage>> {
        let path = self.session_path(channel, chat_id);

        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(e) => return Err(e.into()),
        };

        let mut messages = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<ChatMessage>(line) {
                Ok(msg) => messages.push(msg),
                Err(e) => {
                    warn!(error = %e, "skipping malformed session line");
                }
            }
        }

        // Apply turn-based limit first (matching OpenClaw dmHistoryLimit)
        limit_history_turns(&mut messages, self.dm_history_limit);

        // Then apply raw message-count sliding window as backstop
        if self.history_limit > 0 && messages.len() > self.history_limit {
            messages = messages.split_off(messages.len() - self.history_limit);
        }

        sanitize_after_truncation(&mut messages);
        repair_trailing_tool_use(&mut messages);

        debug!(
            channel,
            chat_id,
            count = messages.len(),
            "loaded session history"
        );

        Ok(messages)
    }

    /// Save (replace) the full chat history for a session.
    pub async fn save(&self, channel: &str, chat_id: &str, messages: &[ChatMessage]) -> Result<()> {
        let path = self.session_path(channel, chat_id);

        // Apply turn-based limit + sliding window before saving
        let mut to_save_vec = messages.to_vec();
        limit_history_turns(&mut to_save_vec, self.dm_history_limit);
        if self.history_limit > 0 && to_save_vec.len() > self.history_limit {
            let start = to_save_vec.len() - self.history_limit;
            to_save_vec = to_save_vec.split_off(start);
        }
        sanitize_after_truncation(&mut to_save_vec);
        let to_save = to_save_vec.as_slice();

        let mut content = String::new();
        for msg in to_save {
            let line = serde_json::to_string(msg)?;
            content.push_str(&line);
            content.push('\n');
        }

        tokio::fs::write(&path, content).await?;

        debug!(
            channel,
            chat_id,
            count = to_save.len(),
            "saved session history"
        );

        Ok(())
    }

    /// Append a single message to a session file.
    pub async fn append(
        &self,
        channel: &str,
        chat_id: &str,
        message: &ChatMessage,
    ) -> Result<()> {
        let path = self.session_path(channel, chat_id);
        let line = serde_json::to_string(message)?;

        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;

        file.write_all(line.as_bytes()).await?;
        file.write_all(b"\n").await?;

        Ok(())
    }

    /// Clear a session.
    pub async fn clear(&self, channel: &str, chat_id: &str) -> Result<()> {
        let path = self.session_path(channel, chat_id);
        if path.exists() {
            tokio::fs::remove_file(&path).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_limit_returns_configured_value() {
        let store = SessionStore::new("/tmp/test", 42, 0);
        assert_eq!(store.history_limit(), 42);
    }

    #[test]
    fn history_limit_default_case() {
        let store = SessionStore::new("/tmp/test", 20, 0);
        assert_eq!(store.history_limit(), 20);
    }

    #[tokio::test]
    async fn save_and_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(tmp.path().to_str().unwrap(), 10, 0);
        store.init().await.unwrap();

        let messages = vec![
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "Hello".into() }],
            },
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::Text { text: "Hi there".into() }],
            },
        ];

        store.save("test", "chat1", &messages).await.unwrap();
        let loaded = store.load("test", "chat1").await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].role, Role::User);
        assert_eq!(loaded[1].role, Role::Assistant);
    }

    #[tokio::test]
    async fn load_applies_sliding_window() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(tmp.path().to_str().unwrap(), 3, 0);
        store.init().await.unwrap();

        // Save 5 messages but limit is 3
        let messages: Vec<ChatMessage> = (0..5)
            .map(|i| ChatMessage {
                role: if i % 2 == 0 { Role::User } else { Role::Assistant },
                content: vec![ContentBlock::Text { text: format!("msg_{i}") }],
            })
            .collect();

        store.save("test", "chat1", &messages).await.unwrap();
        let loaded = store.load("test", "chat1").await.unwrap();
        assert_eq!(loaded.len(), 3);
        // Should be the last 3 messages
        match &loaded[0].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "msg_2"),
            _ => panic!("expected text"),
        }
    }

    #[tokio::test]
    async fn load_nonexistent_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(tmp.path().to_str().unwrap(), 10, 0);
        store.init().await.unwrap();

        let loaded = store.load("test", "nonexistent").await.unwrap();
        assert!(loaded.is_empty());
    }

    #[tokio::test]
    async fn append_and_load() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(tmp.path().to_str().unwrap(), 10, 0);
        store.init().await.unwrap();

        let msg = ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "appended".into() }],
        };
        store.append("test", "chat1", &msg).await.unwrap();

        let loaded = store.load("test", "chat1").await.unwrap();
        assert_eq!(loaded.len(), 1);
    }

    // -- limit_history_turns tests --

    #[test]
    fn limit_turns_keeps_last_n_user_turns() {
        // 3 user turns with assistant replies: U A U A U A
        let mut messages = vec![
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "turn1".into() }],
            },
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::Text { text: "reply1".into() }],
            },
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "turn2".into() }],
            },
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::Text { text: "reply2".into() }],
            },
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "turn3".into() }],
            },
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::Text { text: "reply3".into() }],
            },
        ];

        // Keep last 2 user turns → should drop turn1 + reply1
        limit_history_turns(&mut messages, 2);
        assert_eq!(messages.len(), 4);
        match &messages[0].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "turn2"),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn limit_turns_zero_means_unlimited() {
        let mut messages = vec![
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "a".into() }],
            },
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::Text { text: "b".into() }],
            },
        ];
        limit_history_turns(&mut messages, 0);
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn limit_turns_tool_result_not_counted() {
        // tool_result-only User messages should NOT count as turns
        let mut messages = vec![
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "turn1".into() }],
            },
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "test".into(),
                    input: serde_json::json!({}),
                }],
            },
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: "result".into(),
                }],
            },
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::Text { text: "reply1".into() }],
            },
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "turn2".into() }],
            },
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::Text { text: "reply2".into() }],
            },
        ];

        // Only 2 user TEXT turns (turn1, turn2). tool_result is not counted.
        // limit=1 → keep only the last user text turn (turn2 + reply2)
        limit_history_turns(&mut messages, 1);
        assert_eq!(messages.len(), 2);
        match &messages[0].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "turn2"),
            _ => panic!("expected text"),
        }
    }

    // -- sanitize_after_truncation tests --

    #[test]
    fn sanitize_strips_orphaned_tool_result() {
        let mut messages = vec![
            // Orphaned tool_result (its tool_use was truncated)
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: "result".into(),
                }],
            },
            // Valid conversation start
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "hello".into() }],
            },
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::Text { text: "hi".into() }],
            },
        ];

        sanitize_after_truncation(&mut messages);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::User);
        match &messages[0].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hello"),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn sanitize_strips_orphaned_assistant_tool_use_then_tool_result() {
        let mut messages = vec![
            // Orphaned assistant with tool_use
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "test".into(),
                    input: serde_json::json!({}),
                }],
            },
            // Orphaned tool_result
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: "result".into(),
                }],
            },
            // Valid start
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "hi".into() }],
            },
        ];

        sanitize_after_truncation(&mut messages);
        assert_eq!(messages.len(), 1);
        match &messages[0].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hi"),
            _ => panic!("expected text"),
        }
    }

    // -- repair_trailing_tool_use tests --

    #[test]
    fn repair_trailing_tool_use_appends_error_result() {
        let mut messages = vec![
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "do something".into() }],
            },
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "web_search".into(),
                    input: serde_json::json!({"query": "test"}),
                }],
            },
            // No tool_result — agent was interrupted
        ];

        repair_trailing_tool_use(&mut messages);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2].role, Role::User);
        match &messages[2].content[0] {
            ContentBlock::ToolResult { tool_use_id, content } => {
                assert_eq!(tool_use_id, "t1");
                assert!(content.contains("interrupted"));
            }
            _ => panic!("expected tool_result"),
        }
    }

    #[test]
    fn repair_trailing_tool_use_multiple_ids() {
        let mut messages = vec![
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "search".into() }],
            },
            ChatMessage {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::ToolUse {
                        id: "t1".into(),
                        name: "web_search".into(),
                        input: serde_json::json!({}),
                    },
                    ContentBlock::ToolUse {
                        id: "t2".into(),
                        name: "get_time".into(),
                        input: serde_json::json!({}),
                    },
                ],
            },
        ];

        repair_trailing_tool_use(&mut messages);
        assert_eq!(messages.len(), 3);
        // Should have 2 tool_result blocks
        assert_eq!(messages[2].content.len(), 2);
    }

    #[test]
    fn repair_trailing_tool_use_noop_when_complete() {
        let mut messages = vec![
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "search".into() }],
            },
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "web_search".into(),
                    input: serde_json::json!({}),
                }],
            },
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: "ok".into(),
                }],
            },
        ];

        repair_trailing_tool_use(&mut messages);
        assert_eq!(messages.len(), 3); // unchanged
    }

    #[test]
    fn repair_trailing_tool_use_noop_when_no_tool_use() {
        let mut messages = vec![
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "hi".into() }],
            },
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::Text { text: "hello".into() }],
            },
        ];

        repair_trailing_tool_use(&mut messages);
        assert_eq!(messages.len(), 2); // unchanged
    }

    #[test]
    fn sanitize_noop_when_valid() {
        let mut messages = vec![
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "hello".into() }],
            },
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::Text { text: "hi".into() }],
            },
        ];

        sanitize_after_truncation(&mut messages);
        assert_eq!(messages.len(), 2);
    }
}
