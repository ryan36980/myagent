//! CLI channel — interactive stdin/stdout interface.
//!
//! Reads lines from stdin and prints responses to stdout.
//! Zero external dependencies, negligible memory overhead.

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;
use tracing::debug;

use super::types::{IncomingMessage, MessageContent};
use super::Channel;
use crate::error::Result;

/// CLI channel that reads from stdin and writes to stdout.
pub struct CliChannel {
    reader: Mutex<BufReader<tokio::io::Stdin>>,
    user_id: String,
}

impl CliChannel {
    pub fn new() -> Self {
        Self {
            reader: Mutex::new(BufReader::new(tokio::io::stdin())),
            user_id: "cli_user".into(),
        }
    }
}

#[async_trait]
impl Channel for CliChannel {
    fn id(&self) -> &str {
        "cli"
    }

    async fn poll(&self) -> Result<Vec<IncomingMessage>> {
        let mut reader = self.reader.lock().await;
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;

        if n == 0 {
            // EOF — shut down gracefully by returning empty
            debug!("CLI stdin closed (EOF)");
            return Ok(Vec::new());
        }

        let text = line.trim().to_string();
        if text.is_empty() {
            return Ok(Vec::new());
        }

        Ok(vec![IncomingMessage {
            channel: "cli".into(),
            chat_id: "cli".into(),
            sender_id: self.user_id.clone(),
            content: MessageContent::Text(text),
            timestamp: chrono::Local::now().timestamp(),
        }])
    }

    async fn send_text(&self, _chat_id: &str, text: &str) -> Result<String> {
        println!("{text}");
        Ok(String::new())
    }

    async fn send_voice(&self, _chat_id: &str, audio: &[u8]) -> Result<()> {
        println!("[voice message, {} bytes]", audio.len());
        Ok(())
    }

    async fn download_voice(&self, _file_ref: &str) -> Result<Vec<u8>> {
        Err(crate::error::GatewayError::Tool {
            tool: "cli".into(),
            message: "voice download not supported in CLI mode".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_id_is_cli() {
        let ch = CliChannel::new();
        assert_eq!(ch.id(), "cli");
    }

    #[tokio::test]
    async fn send_text_succeeds() {
        let ch = CliChannel::new();
        // send_text just prints to stdout — should not error
        let msg_id = ch.send_text("cli", "hello").await.unwrap();
        assert!(msg_id.is_empty()); // CLI returns empty msg_id
    }

    #[tokio::test]
    async fn send_voice_succeeds() {
        let ch = CliChannel::new();
        ch.send_voice("cli", b"fake-audio").await.unwrap();
    }

    #[tokio::test]
    async fn download_voice_returns_error() {
        let ch = CliChannel::new();
        let err = ch.download_voice("file_ref").await.unwrap_err();
        assert!(err.to_string().contains("not supported"));
    }
}
