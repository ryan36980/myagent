//! Channel abstraction layer for multi-platform message routing.
//!
//! Each messaging platform (Telegram, WeChat, iMessage) implements the
//! `Channel` trait. The agent core is completely channel-agnostic.

pub mod cli;
pub mod feishu;
pub mod http_api;
pub mod streaming;
pub mod telegram;
pub mod types;

use async_trait::async_trait;
use types::IncomingMessage;

use crate::error::Result;
use crate::provider::tts::AudioFormat;

/// Trait for a messaging channel (Telegram, WeChat, iMessage, etc.).
///
/// Adding a new channel requires implementing this trait in a single file.
/// No changes to the agent core, session, or tools are needed.
#[async_trait]
pub trait Channel: Send + Sync {
    /// Channel identifier (e.g., "telegram", "wechat", "imessage").
    fn id(&self) -> &str;

    /// Preferred audio format for TTS output.
    ///
    /// Default: OGG/Opus. Channels override this to request their preferred
    /// format (e.g., Telegram prefers MP3).
    fn preferred_audio_format(&self) -> AudioFormat {
        AudioFormat::OggOpus
    }

    /// Poll for new incoming messages (long-polling or WebSocket receive).
    ///
    /// Uses `&self` (interior mutability) so the channel can be shared via Arc.
    async fn poll(&self) -> Result<Vec<IncomingMessage>>;

    /// Send a text message to a chat. Returns the message ID if available.
    async fn send_text(&self, chat_id: &str, text: &str) -> Result<String>;

    /// Send a voice message (Opus audio bytes) to a chat.
    async fn send_voice(&self, chat_id: &str, audio: &[u8]) -> Result<()>;

    /// Download a voice file from a channel-specific file reference.
    async fn download_voice(&self, file_ref: &str) -> Result<Vec<u8>>;

    /// Edit a previously sent message (for streaming updates).
    ///
    /// Default: no-op. Channels that support message editing (like Telegram)
    /// should override this.
    async fn edit_message(&self, _chat_id: &str, _msg_id: &str, _text: &str) -> Result<()> {
        Ok(())
    }

    /// Send a "typing" indicator to the chat.
    ///
    /// Default: no-op. Channels that support typing indicators (like Telegram)
    /// should override this.
    async fn send_typing(&self, _chat_id: &str) -> Result<()> {
        Ok(())
    }

    /// Close a streaming connection for a chat (e.g., send SSE `done` event).
    ///
    /// Default: no-op. Channels that support streaming (like HTTP API SSE)
    /// should override this.
    async fn close_stream(&self, _chat_id: &str) -> Result<()> {
        Ok(())
    }
}
