//! LLM provider trait and implementations.

pub mod claude;
pub mod failover;
pub mod openai_compat;

use async_trait::async_trait;
use futures_util::stream::BoxStream;

use crate::channel::types::{
    ChatMessage, ContentBlock, LlmResponse, StreamEvent, ToolDefinition,
};
use crate::error::Result;

/// Trait for LLM providers (Claude, OpenAI, Gemini, etc.).
///
/// Adding a new LLM provider requires implementing this trait in ~50 lines.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Send a chat completion request with optional tool definitions.
    async fn chat(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse>;

    /// Send a streaming chat completion request.
    ///
    /// Default implementation: calls `chat()` and wraps the result as a
    /// single-event stream.  Override for true SSE/streaming support.
    async fn chat_stream(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        let response = self.chat(system, messages, tools).await?;
        let stop_reason = response.stop_reason.clone();

        let mut events: Vec<Result<StreamEvent>> = Vec::new();

        for block in &response.content {
            match block {
                ContentBlock::Text { text } => {
                    events.push(Ok(StreamEvent::TextDelta(text.clone())));
                }
                ContentBlock::ToolUse { id, name, input } => {
                    events.push(Ok(StreamEvent::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    }));
                }
                ContentBlock::Image { .. } | ContentBlock::ToolResult { .. } => {}
            }
        }

        events.push(Ok(StreamEvent::Done { stop_reason }));

        Ok(Box::pin(futures_util::stream::iter(events)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::types::{ContentBlock, LlmResponse, Role, StopReason};
    use futures_util::StreamExt;

    /// Minimal mock provider that returns a fixed response.
    struct MockProvider {
        response: LlmResponse,
    }

    #[async_trait]
    impl LlmProvider for MockProvider {
        async fn chat(
            &self,
            _system: &str,
            _messages: &[ChatMessage],
            _tools: &[ToolDefinition],
        ) -> Result<LlmResponse> {
            Ok(self.response.clone())
        }
        // Uses the default chat_stream() implementation
    }

    #[tokio::test]
    async fn default_chat_stream_text() {
        let provider = MockProvider {
            response: LlmResponse {
                content: vec![ContentBlock::Text {
                    text: "Hello world".into(),
                }],
                stop_reason: StopReason::EndTurn,
            },
        };

        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "Hi".into() }],
        }];

        let mut stream = provider.chat_stream("sys", &messages, &[]).await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }

        assert_eq!(events.len(), 2);
        match &events[0] {
            StreamEvent::TextDelta(t) => assert_eq!(t, "Hello world"),
            other => panic!("expected TextDelta, got: {:?}", other),
        }
        match &events[1] {
            StreamEvent::Done { stop_reason } => assert_eq!(stop_reason, &StopReason::EndTurn),
            other => panic!("expected Done, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn default_chat_stream_tool_use() {
        let provider = MockProvider {
            response: LlmResponse {
                content: vec![ContentBlock::ToolUse {
                    id: "id_1".into(),
                    name: "my_tool".into(),
                    input: serde_json::json!({"key": "val"}),
                }],
                stop_reason: StopReason::ToolUse,
            },
        };

        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "Do it".into() }],
        }];

        let mut stream = provider.chat_stream("sys", &messages, &[]).await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }

        assert_eq!(events.len(), 2);
        match &events[0] {
            StreamEvent::ToolUse { id, name, input } => {
                assert_eq!(id, "id_1");
                assert_eq!(name, "my_tool");
                assert_eq!(input, &serde_json::json!({"key": "val"}));
            }
            other => panic!("expected ToolUse, got: {:?}", other),
        }
        match &events[1] {
            StreamEvent::Done { stop_reason } => assert_eq!(stop_reason, &StopReason::ToolUse),
            other => panic!("expected Done, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn default_chat_stream_skips_tool_result() {
        let provider = MockProvider {
            response: LlmResponse {
                content: vec![
                    ContentBlock::Text { text: "ok".into() },
                    ContentBlock::ToolResult {
                        tool_use_id: "id_1".into(),
                        content: "result".into(),
                    },
                ],
                stop_reason: StopReason::EndTurn,
            },
        };

        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "Hi".into() }],
        }];

        let mut stream = provider.chat_stream("sys", &messages, &[]).await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }

        // ToolResult should be skipped → TextDelta + Done only
        assert_eq!(events.len(), 2);
    }
}
