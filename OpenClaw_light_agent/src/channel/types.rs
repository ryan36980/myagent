//! Unified message types shared across all channels.

use serde::{Deserialize, Serialize};

/// An incoming message from any channel (Telegram, WeChat, iMessage, etc.).
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    /// Channel identifier ("telegram", "wechat", "imessage")
    pub channel: String,
    /// Chat/conversation ID within the channel
    pub chat_id: String,
    /// Sender identifier
    pub sender_id: String,
    /// Message content (text or voice)
    pub content: MessageContent,
    /// Unix timestamp
    pub timestamp: i64,
}

/// The content of an incoming message.
#[derive(Debug, Clone)]
pub enum MessageContent {
    /// Plain text message
    Text(String),
    /// Voice message with a channel-specific file reference
    Voice {
        /// Channel-specific file reference (e.g., Telegram file_id)
        file_ref: String,
        /// MIME type (e.g., "audio/ogg")
        mime: String,
    },
    /// Image with a channel-specific file reference
    Image {
        /// Channel-specific file reference (e.g., Telegram file_id)
        file_ref: String,
        /// MIME type (e.g., "image/jpeg")
        mime: String,
        /// Optional caption text sent with the image
        caption: Option<String>,
    },
}

/// An outgoing response message.
#[derive(Debug, Clone, Default)]
pub struct OutgoingMessage {
    /// Text reply (if any)
    pub text: Option<String>,
    /// Voice audio data in Opus format (if any)
    pub voice: Option<Vec<u8>>,
}

/// A chat message for LLM context (used in session history).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

/// Message role in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// A content block within a message (text, tool use, or tool result).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        source_type: String,
        media_type: String,
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

/// Tool definition for LLM function calling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// LLM response from a provider.
#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
}

/// Why the LLM stopped generating.
#[derive(Debug, Clone, PartialEq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Other(String),
}

/// Streaming event from an LLM provider.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Incremental text content.
    TextDelta(String),
    /// A complete tool use block.
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// Generation finished.
    Done {
        stop_reason: StopReason,
    },
}

impl LlmResponse {
    /// Extract the text content from the response.
    pub fn text(&self) -> Option<&str> {
        self.content.iter().find_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
    }

    /// Extract tool use blocks from the response.
    pub fn tool_uses(&self) -> Vec<(&str, &str, &serde_json::Value)> {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => {
                    Some((id.as_str(), name.as_str(), input))
                }
                _ => None,
            })
            .collect()
    }
}
