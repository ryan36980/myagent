//! Telegram Bot API channel implementation.
//!
//! Uses long polling via getUpdates. No Telegram SDK dependency — all
//! API calls are hand-written for minimal footprint.

use std::sync::atomic::{AtomicI64, Ordering};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::types::{IncomingMessage, MessageContent};
use super::Channel;
use crate::config::TelegramConfig;
use crate::error::{GatewayError, Result};

/// Telegram channel using Bot API long polling.
pub struct TelegramChannel {
    client: reqwest::Client,
    bot_token: String,
    api_base: String,
    offset: AtomicI64,
    allowed_users: Vec<i64>,
}

impl TelegramChannel {
    pub fn new(client: reqwest::Client, config: &TelegramConfig) -> Self {
        let bot_token = config.bot_token.clone();
        let api_base = format!("https://api.telegram.org/bot{}", bot_token);
        Self {
            client,
            bot_token,
            api_base,
            offset: AtomicI64::new(0),
            allowed_users: config.allowed_users.clone(),
        }
    }

    fn is_allowed(&self, user_id: i64) -> bool {
        self.allowed_users.is_empty() || self.allowed_users.contains(&user_id)
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn id(&self) -> &str {
        "telegram"
    }

    async fn poll(&self) -> Result<Vec<IncomingMessage>> {
        let current_offset = self.offset.load(Ordering::Relaxed);
        let url = format!(
            "{}/getUpdates?offset={}&timeout=30&allowed_updates=[\"message\"]",
            self.api_base, current_offset
        );

        let resp: TgResponse<Vec<TgUpdate>> = self
            .client
            .get(&url)
            .send()
            .await?
            .json()
            .await?;

        if !resp.ok {
            return Err(GatewayError::Telegram(
                resp.description.unwrap_or_else(|| "getUpdates failed".into()),
            ));
        }

        let updates = resp.result.unwrap_or_default();
        let mut messages = Vec::new();

        for update in updates {
            self.offset.store(update.update_id + 1, Ordering::Relaxed);

            let Some(msg) = update.message else {
                continue;
            };

            let Some(from) = &msg.from else {
                continue;
            };

            if !self.is_allowed(from.id) {
                debug!(user_id = from.id, "ignoring message from non-allowed user");
                continue;
            }

            let chat_id = msg.chat.id.to_string();
            let sender_id = from.id.to_string();
            let timestamp = msg.date;

            let content = if let Some(voice) = msg.voice {
                MessageContent::Voice {
                    file_ref: voice.file_id,
                    mime: voice.mime_type.unwrap_or_else(|| "audio/ogg".into()),
                }
            } else if let Some(audio) = msg.audio {
                MessageContent::Voice {
                    file_ref: audio.file_id,
                    mime: audio.mime_type.unwrap_or_else(|| "audio/mpeg".into()),
                }
            } else if let Some(ref photos) = msg.photo {
                // Pick the largest photo by pixel count
                if let Some(best) = photos.iter().max_by_key(|p| p.width as u64 * p.height as u64)
                {
                    MessageContent::Image {
                        file_ref: best.file_id.clone(),
                        mime: "image/jpeg".into(),
                        caption: msg.caption.clone(),
                    }
                } else {
                    debug!("ignoring empty photo array");
                    continue;
                }
            } else if let Some(ref doc) = msg.document {
                // Accept image documents (image/jpeg, image/png, image/gif, image/webp)
                let mime = doc
                    .mime_type
                    .as_deref()
                    .unwrap_or("");
                if mime.starts_with("image/") {
                    MessageContent::Image {
                        file_ref: doc.file_id.clone(),
                        mime: mime.to_string(),
                        caption: msg.caption.clone(),
                    }
                } else {
                    debug!(mime, "ignoring non-image document");
                    continue;
                }
            } else if let Some(text) = msg.text {
                MessageContent::Text(text)
            } else {
                debug!("ignoring unsupported message type");
                continue;
            };

            messages.push(IncomingMessage {
                channel: "telegram".into(),
                chat_id,
                sender_id,
                content,
                timestamp,
            });
        }

        Ok(messages)
    }

    async fn send_text(&self, chat_id: &str, text: &str) -> Result<String> {
        let url = format!("{}/sendMessage", self.api_base);
        let mut last_msg_id = String::new();

        for chunk in chunk_text(text, 4000) {
            let body = SendMessageRequest {
                chat_id: chat_id.to_string(),
                text: chunk.to_string(),
                parse_mode: Some("Markdown".into()),
            };

            let resp: TgResponse<serde_json::Value> = self
                .client
                .post(&url)
                .json(&body)
                .send()
                .await?
                .json()
                .await?;

            if resp.ok {
                if let Some(ref result) = resp.result {
                    if let Some(mid) = result.get("message_id").and_then(|v| v.as_i64()) {
                        last_msg_id = mid.to_string();
                    }
                }
            } else {
                // Retry without parse_mode if Markdown parsing failed
                let body = SendMessageRequest {
                    chat_id: chat_id.to_string(),
                    text: chunk.to_string(),
                    parse_mode: None,
                };
                let resp: TgResponse<serde_json::Value> = self
                    .client
                    .post(&url)
                    .json(&body)
                    .send()
                    .await?
                    .json()
                    .await?;

                if !resp.ok {
                    return Err(GatewayError::Telegram(
                        resp.description.unwrap_or_else(|| "sendMessage failed".into()),
                    ));
                }
                if let Some(ref result) = resp.result {
                    if let Some(mid) = result.get("message_id").and_then(|v| v.as_i64()) {
                        last_msg_id = mid.to_string();
                    }
                }
            }
        }

        Ok(last_msg_id)
    }

    async fn send_typing(&self, chat_id: &str) -> Result<()> {
        let url = format!("{}/sendChatAction", self.api_base);
        let body = serde_json::json!({
            "chat_id": chat_id,
            "action": "typing"
        });
        let _ = self.client.post(&url).json(&body).send().await;
        Ok(())
    }

    async fn send_voice(&self, chat_id: &str, audio: &[u8]) -> Result<()> {
        let url = format!("{}/sendVoice", self.api_base);

        let (file_name, mime) = detect_audio_format(audio);
        let part = reqwest::multipart::Part::bytes(audio.to_vec())
            .file_name(file_name)
            .mime_str(mime)
            .map_err(|e| GatewayError::Telegram(e.to_string()))?;

        let form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part("voice", part);

        let resp: TgResponse<serde_json::Value> = self
            .client
            .post(&url)
            .multipart(form)
            .send()
            .await?
            .json()
            .await?;

        if !resp.ok {
            return Err(GatewayError::Telegram(
                resp.description.unwrap_or_else(|| "sendVoice failed".into()),
            ));
        }

        Ok(())
    }

    async fn edit_message(&self, chat_id: &str, msg_id: &str, text: &str) -> Result<()> {
        let url = format!("{}/editMessageText", self.api_base);
        let body = serde_json::json!({
            "chat_id": chat_id,
            "message_id": msg_id.parse::<i64>().unwrap_or(0),
            "text": text,
        });

        let resp: TgResponse<serde_json::Value> = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await?
            .json()
            .await?;

        if !resp.ok {
            return Err(GatewayError::Telegram(
                resp.description
                    .unwrap_or_else(|| "editMessageText failed".into()),
            ));
        }

        Ok(())
    }

    async fn download_voice(&self, file_ref: &str) -> Result<Vec<u8>> {
        // Step 1: Get file path from Telegram
        let url = format!("{}/getFile?file_id={}", self.api_base, file_ref);
        let resp: TgResponse<TgFile> = self
            .client
            .get(&url)
            .send()
            .await?
            .json()
            .await?;

        if !resp.ok {
            return Err(GatewayError::Telegram(
                resp.description.unwrap_or_else(|| "getFile failed".into()),
            ));
        }

        let file = resp.result.ok_or_else(|| {
            GatewayError::Telegram("getFile returned no result".into())
        })?;

        let file_path = file.file_path.ok_or_else(|| {
            GatewayError::Telegram("file has no file_path".into())
        })?;

        // Step 2: Download the file
        let download_url = format!(
            "https://api.telegram.org/file/bot{}/{}",
            self.bot_token, file_path
        );

        let bytes = self
            .client
            .get(&download_url)
            .send()
            .await?
            .bytes()
            .await?;

        Ok(bytes.to_vec())
    }
}

/// Split text into chunks that fit within the Telegram message size limit.
///
/// Prefers breaking at newlines, then spaces, then forces a hard break.
/// Handles UTF-8 boundaries safely.
fn chunk_text(text: &str, max_len: usize) -> Vec<&str> {
    if text.len() <= max_len {
        return vec![text];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining);
            break;
        }

        // Find a safe UTF-8 boundary near max_len
        let mut end = max_len;
        while end > 0 && !remaining.is_char_boundary(end) {
            end -= 1;
        }

        let slice = &remaining[..end];

        // Prefer breaking at newline
        if let Some(pos) = slice.rfind('\n') {
            chunks.push(&remaining[..pos + 1]);
            remaining = &remaining[pos + 1..];
        } else if let Some(pos) = slice.rfind(' ') {
            chunks.push(&remaining[..pos + 1]);
            remaining = &remaining[pos + 1..];
        } else {
            // Force break at safe boundary
            chunks.push(slice);
            remaining = &remaining[end..];
        }
    }

    chunks
}

/// Detect audio format from magic bytes, returning (file_name, mime_type).
///
/// - OGG/Opus: starts with `OggS` (0x4F676753)
/// - MP3: starts with `0xFF 0xFB/0xF3/0xF2` (sync word) or `ID3` (ID3v2 tag)
/// - Unknown: defaults to OGG
fn detect_audio_format(data: &[u8]) -> (&'static str, &'static str) {
    if data.len() >= 4 && &data[..4] == b"OggS" {
        ("voice.ogg", "audio/ogg")
    } else if data.len() >= 3 && &data[..3] == b"ID3" {
        ("voice.mp3", "audio/mpeg")
    } else if data.len() >= 2 && data[0] == 0xFF && (data[1] & 0xE0) == 0xE0 {
        ("voice.mp3", "audio/mpeg")
    } else {
        ("voice.ogg", "audio/ogg")
    }
}

// ========== Telegram API types ==========

#[derive(Deserialize)]
struct TgResponse<T> {
    ok: bool,
    description: Option<String>,
    result: Option<T>,
}

#[derive(Deserialize)]
struct TgUpdate {
    update_id: i64,
    message: Option<TgMessage>,
}

#[derive(Deserialize)]
struct TgMessage {
    #[allow(dead_code)]
    message_id: i64,
    from: Option<TgUser>,
    chat: TgChat,
    date: i64,
    text: Option<String>,
    caption: Option<String>,
    voice: Option<TgVoice>,
    audio: Option<TgAudio>,
    photo: Option<Vec<TgPhotoSize>>,
    document: Option<TgDocument>,
}

#[derive(Deserialize)]
struct TgUser {
    id: i64,
}

#[derive(Deserialize)]
struct TgChat {
    id: i64,
}

#[derive(Deserialize)]
struct TgVoice {
    file_id: String,
    mime_type: Option<String>,
}

#[derive(Deserialize)]
struct TgAudio {
    file_id: String,
    mime_type: Option<String>,
}

#[derive(Deserialize)]
struct TgPhotoSize {
    file_id: String,
    width: u32,
    height: u32,
}

#[derive(Deserialize)]
struct TgDocument {
    file_id: String,
    mime_type: Option<String>,
}

#[derive(Deserialize)]
struct TgFile {
    #[allow(dead_code)]
    file_id: String,
    file_path: Option<String>,
}

#[derive(Serialize)]
struct SendMessageRequest {
    chat_id: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parse_mode: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_ogg_opus_magic_bytes() {
        // OGG files start with "OggS"
        let ogg_data = b"OggS\x00\x02\x00\x00\x00\x00\x00\x00\x00\x00";
        let (name, mime) = detect_audio_format(ogg_data);
        assert_eq!(name, "voice.ogg");
        assert_eq!(mime, "audio/ogg");
    }

    #[test]
    fn detect_mp3_id3_tag() {
        // MP3 with ID3v2 header
        let mp3_data = b"ID3\x04\x00\x00\x00\x00\x00\x00";
        let (name, mime) = detect_audio_format(mp3_data);
        assert_eq!(name, "voice.mp3");
        assert_eq!(mime, "audio/mpeg");
    }

    #[test]
    fn detect_mp3_sync_word() {
        // MP3 frame sync: 0xFF 0xFB (MPEG1 Layer3)
        let mp3_data = &[0xFF, 0xFB, 0x90, 0x00, 0x00];
        let (name, mime) = detect_audio_format(mp3_data);
        assert_eq!(name, "voice.mp3");
        assert_eq!(mime, "audio/mpeg");
    }

    #[test]
    fn detect_unknown_defaults_to_ogg() {
        // Unknown format defaults to OGG
        let unknown = b"RIFF\x00\x00\x00\x00WAVE";
        let (name, mime) = detect_audio_format(unknown);
        assert_eq!(name, "voice.ogg");
        assert_eq!(mime, "audio/ogg");
    }

    #[test]
    fn detect_empty_data_defaults_to_ogg() {
        let (name, mime) = detect_audio_format(&[]);
        assert_eq!(name, "voice.ogg");
        assert_eq!(mime, "audio/ogg");
    }

    // -- chunk_text tests -------------------------------------------------------

    #[test]
    fn chunk_text_short_message() {
        let chunks = chunk_text("Hello, world!", 4000);
        assert_eq!(chunks, vec!["Hello, world!"]);
    }

    #[test]
    fn chunk_text_exact_limit() {
        let text = "a".repeat(4000);
        let chunks = chunk_text(&text, 4000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 4000);
    }

    #[test]
    fn chunk_text_breaks_at_newline() {
        let mut text = "a".repeat(3990);
        text.push('\n');
        text.push_str(&"b".repeat(3000));
        let chunks = chunk_text(&text, 4000);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].ends_with('\n'));
        assert!(chunks[1].starts_with('b'));
    }

    #[test]
    fn chunk_text_breaks_at_space() {
        // No newlines — should break at space
        let text = format!("{} {}", "a".repeat(2000), "b".repeat(2500));
        let chunks = chunk_text(&text, 4000);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].ends_with(' '));
    }

    #[test]
    fn chunk_text_force_break() {
        // Single long word without spaces or newlines
        let text = "a".repeat(5000);
        let chunks = chunk_text(&text, 4000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 4000);
        assert_eq!(chunks[1].len(), 1000);
    }

    #[test]
    fn chunk_text_multibyte_utf8() {
        // Emoji = 4 bytes each, fill up near the limit
        let text = "\u{1F600}".repeat(1100); // 4400 bytes
        let chunks = chunk_text(&text, 4000);
        assert!(chunks.len() >= 2);
        // Every chunk must be valid UTF-8 (it compiles, so it is)
        for chunk in &chunks {
            assert!(!chunk.is_empty());
        }
    }

    #[test]
    fn chunk_text_empty() {
        let chunks = chunk_text("", 4000);
        assert_eq!(chunks, vec![""]);
    }
}
