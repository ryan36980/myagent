//! Streaming message preview — edits a single message as LLM generates text.
//!
//! Throttles edits to respect Telegram's rate limits (~1 edit/second).
//! Memory: ~120B struct + buffer String (max 4096 chars, released on finish).

use std::sync::Arc;

use tokio::time::Instant;
use tracing::warn;

use super::Channel;

/// Streams text deltas to a channel by editing a single message.
pub struct StreamingWriter {
    channel: Arc<dyn Channel>,
    chat_id: String,
    /// Message ID of the preview message (None until first send).
    msg_id: Option<String>,
    /// Accumulated text buffer.
    buffer: String,
    /// Timestamp of the last edit_message call.
    last_edit: Instant,
    /// Minimum interval between edits (milliseconds).
    throttle_ms: u64,
    /// Minimum chars before sending the first message (better push notifications).
    min_initial_chars: usize,
    /// Whether the writer has been stopped (tool_use or abort).
    stopped: bool,
}

impl StreamingWriter {
    pub fn new(channel: Arc<dyn Channel>, chat_id: String) -> Self {
        Self {
            channel,
            chat_id,
            msg_id: None,
            buffer: String::with_capacity(256),
            last_edit: Instant::now(),
            throttle_ms: 1000,
            min_initial_chars: 20,
            stopped: false,
        }
    }

    /// Append a text delta and maybe send/edit the message.
    pub async fn push(&mut self, delta: &str) {
        if self.stopped {
            return;
        }
        self.buffer.push_str(delta);

        if self.msg_id.is_none() {
            // First message: wait until we have enough chars
            if self.buffer.len() >= self.min_initial_chars {
                match self.channel.send_text(&self.chat_id, &self.buffer).await {
                    Ok(id) if !id.is_empty() => {
                        self.msg_id = Some(id);
                        self.last_edit = Instant::now();
                    }
                    Ok(_) => {
                        // Channel doesn't return msg_id (CLI, HTTP) — stop streaming
                        self.stopped = true;
                    }
                    Err(e) => {
                        warn!(error = %e, "streaming: failed to send initial message");
                        self.stopped = true;
                    }
                }
            }
        } else {
            // Subsequent edits: throttle
            let elapsed = self.last_edit.elapsed().as_millis() as u64;
            if elapsed >= self.throttle_ms {
                self.do_edit().await;
            }
        }
    }

    /// Finalize: send or edit with the complete text.
    pub async fn finish(&mut self) {
        if self.stopped {
            return;
        }
        if self.msg_id.is_none() {
            // Never sent anything — buffer too short; will be sent by dispatch_response
            return;
        }
        self.do_edit().await;
        // Release buffer memory
        self.buffer = String::new();
        self.stopped = true;
    }

    /// Stop accepting updates (on tool_use or abort).
    /// Flushes any buffered text so it's not silently lost.
    pub async fn stop(&mut self) {
        if self.stopped {
            return;
        }
        // Flush: send or edit with whatever we have
        if self.msg_id.is_some() {
            // Already sent — do a final edit with latest buffer
            self.do_edit().await;
        } else if !self.buffer.is_empty() {
            // Never sent initial message — send buffered text now
            match self.channel.send_text(&self.chat_id, &self.buffer).await {
                Ok(id) if !id.is_empty() => {
                    self.msg_id = Some(id);
                }
                _ => {}
            }
        }
        self.stopped = true;
    }

    /// Whether text was already streamed to the user (finish was meaningful).
    pub fn was_sent(&self) -> bool {
        self.msg_id.is_some()
    }

    /// Get the accumulated buffer text.
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    /// Edit the existing message with current buffer content.
    async fn do_edit(&mut self) {
        if let Some(ref msg_id) = self.msg_id {
            if let Err(e) = self
                .channel
                .edit_message(&self.chat_id, msg_id, &self.buffer)
                .await
            {
                warn!(error = %e, "streaming: failed to edit message");
                // Don't stop on edit failure — Telegram may return error for
                // "message is not modified" which is harmless
            }
            self.last_edit = Instant::now();
        }
    }
}
