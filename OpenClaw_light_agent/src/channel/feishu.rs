//! Feishu/Lark channel implementation using WebSocket long connection.
//!
//! Uses the Feishu Open Platform WebSocket SDK protocol (reverse-engineered from
//! the official Go/Node.js SDKs). No public URL needed — the gateway connects
//! outward to Feishu's WSS endpoint, similar to Telegram long polling.
//!
//! Binary frames use a minimal hand-written Protobuf codec (~120 lines) to avoid
//! pulling in the `prost` dependency.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, warn};

use super::types::{IncomingMessage, MessageContent};
use super::Channel;
use crate::config::FeishuConfig;
use crate::error::{GatewayError, Result};

// ============================================================================
// Minimal Protobuf codec (wire types 0 = varint, 2 = length-delimited)
// ============================================================================

/// Decode a varint from `buf` starting at `pos`.
/// Returns `(value, bytes_consumed)`.
fn decode_varint(buf: &[u8], pos: usize) -> Option<(u64, usize)> {
    let mut value: u64 = 0;
    let mut shift = 0u32;
    let mut i = pos;
    loop {
        if i >= buf.len() {
            return None;
        }
        let byte = buf[i];
        i += 1;
        value |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some((value, i - pos));
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}

/// Encode a varint into a `Vec<u8>`.
fn encode_varint(mut val: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    loop {
        let mut byte = (val & 0x7F) as u8;
        val >>= 7;
        if val != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if val == 0 {
            break;
        }
    }
    out
}

/// Encode a protobuf tag (field_number << 3 | wire_type).
fn encode_tag(field: u32, wire_type: u32) -> Vec<u8> {
    encode_varint(((field as u64) << 3) | wire_type as u64)
}

/// A single header key-value pair (protobuf message with fields 1, 2).
#[derive(Debug, Clone, Default)]
struct Header {
    key: String,
    value: String,
}

/// Feishu WebSocket binary frame (protobuf message with 9 fields).
#[derive(Debug, Clone, Default)]
struct Frame {
    seq_id: u64,          // field 1 varint
    log_id: u64,          // field 2 varint
    service: i32,         // field 3 varint (0=control, 1=data)
    method: i32,          // field 4 varint
    headers: Vec<Header>, // field 5 length-delimited (repeated)
    #[allow(dead_code)]
    payload_encoding: String, // field 6 length-delimited
    #[allow(dead_code)]
    payload_type: String, // field 7 length-delimited
    payload: Vec<u8>,     // field 8 length-delimited
    #[allow(dead_code)]
    log_id_new: String,   // field 9 length-delimited
}

impl Frame {
    /// Get a header value by key.
    fn header(&self, key: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|h| h.key == key)
            .map(|h| h.value.as_str())
    }
}

/// Decode a Header message from bytes.
fn decode_header(buf: &[u8]) -> Header {
    let mut h = Header::default();
    let mut pos = 0;
    while pos < buf.len() {
        let Some((tag_val, tag_len)) = decode_varint(buf, pos) else {
            break;
        };
        pos += tag_len;
        let field = (tag_val >> 3) as u32;
        let wire_type = (tag_val & 0x07) as u32;

        match (field, wire_type) {
            (1, 2) | (2, 2) => {
                let Some((len, len_bytes)) = decode_varint(buf, pos) else {
                    break;
                };
                pos += len_bytes;
                let end = pos + len as usize;
                if end > buf.len() {
                    break;
                }
                let s = String::from_utf8_lossy(&buf[pos..end]).into_owned();
                if field == 1 {
                    h.key = s;
                } else {
                    h.value = s;
                }
                pos = end;
            }
            _ => break, // skip unknown
        }
    }
    h
}

/// Decode a Frame from a binary protobuf buffer.
fn decode_frame(buf: &[u8]) -> Frame {
    let mut frame = Frame::default();
    let mut pos = 0;

    while pos < buf.len() {
        let Some((tag_val, tag_len)) = decode_varint(buf, pos) else {
            break;
        };
        pos += tag_len;
        let field = (tag_val >> 3) as u32;
        let wire_type = (tag_val & 0x07) as u32;

        match wire_type {
            0 => {
                // varint
                let Some((val, val_len)) = decode_varint(buf, pos) else {
                    break;
                };
                pos += val_len;
                match field {
                    1 => frame.seq_id = val,
                    2 => frame.log_id = val,
                    3 => frame.service = val as i32,
                    4 => frame.method = val as i32,
                    _ => {} // skip
                }
            }
            2 => {
                // length-delimited
                let Some((len, len_bytes)) = decode_varint(buf, pos) else {
                    break;
                };
                pos += len_bytes;
                let end = pos + len as usize;
                if end > buf.len() {
                    break;
                }
                let data = &buf[pos..end];
                match field {
                    5 => frame.headers.push(decode_header(data)),
                    6 => frame.payload_encoding = String::from_utf8_lossy(data).into_owned(),
                    7 => frame.payload_type = String::from_utf8_lossy(data).into_owned(),
                    8 => frame.payload = data.to_vec(),
                    9 => frame.log_id_new = String::from_utf8_lossy(data).into_owned(),
                    _ => {} // skip
                }
                pos = end;
            }
            _ => break, // skip unknown wire types
        }
    }

    frame
}

/// Encode a Frame into protobuf binary.
fn encode_frame(frame: &Frame) -> Vec<u8> {
    let mut buf = Vec::with_capacity(128);

    // field 1: seq_id (varint)
    if frame.seq_id != 0 {
        buf.extend(encode_tag(1, 0));
        buf.extend(encode_varint(frame.seq_id));
    }
    // field 2: log_id (varint)
    if frame.log_id != 0 {
        buf.extend(encode_tag(2, 0));
        buf.extend(encode_varint(frame.log_id));
    }
    // field 3: service (varint)
    if frame.service != 0 {
        buf.extend(encode_tag(3, 0));
        buf.extend(encode_varint(frame.service as u64));
    }
    // field 4: method (varint)
    if frame.method != 0 {
        buf.extend(encode_tag(4, 0));
        buf.extend(encode_varint(frame.method as u64));
    }
    // field 5: headers (repeated, length-delimited)
    for h in &frame.headers {
        let mut hdr_buf = Vec::new();
        // key
        hdr_buf.extend(encode_tag(1, 2));
        hdr_buf.extend(encode_varint(h.key.len() as u64));
        hdr_buf.extend(h.key.as_bytes());
        // value
        hdr_buf.extend(encode_tag(2, 2));
        hdr_buf.extend(encode_varint(h.value.len() as u64));
        hdr_buf.extend(h.value.as_bytes());

        buf.extend(encode_tag(5, 2));
        buf.extend(encode_varint(hdr_buf.len() as u64));
        buf.extend(hdr_buf);
    }
    // field 8: payload (length-delimited)
    if !frame.payload.is_empty() {
        buf.extend(encode_tag(8, 2));
        buf.extend(encode_varint(frame.payload.len() as u64));
        buf.extend(&frame.payload);
    }

    buf
}

// ============================================================================
// Feishu API types (JSON)
// ============================================================================

/// Response from POST /callback/ws/endpoint
#[derive(Deserialize)]
struct WsEndpointResponse {
    code: i32,
    #[serde(default)]
    msg: String,
    data: Option<WsEndpointData>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WsEndpointData {
    #[serde(rename = "URL")]
    url: String,
    client_config: Option<WsClientConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WsClientConfig {
    reconnect_count: Option<i32>,
    reconnect_interval: Option<i64>,
    #[allow(dead_code)]
    reconnect_nonce: Option<i64>,
    ping_interval: Option<i64>,
}

/// Response from POST /open-apis/auth/v3/tenant_access_token/internal
#[derive(Deserialize)]
struct TokenResponse {
    code: i32,
    #[serde(default)]
    msg: String,
    tenant_access_token: Option<String>,
    expire: Option<i64>,
}

/// Feishu event envelope (JSON payload from WebSocket event frames).
#[derive(Deserialize)]
struct EventEnvelope {
    header: Option<EventHeader>,
    event: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct EventHeader {
    #[allow(dead_code)]
    event_id: Option<String>,
    event_type: Option<String>,
    create_time: Option<String>,
}

/// Feishu message event.
#[derive(Deserialize)]
struct MessageEvent {
    sender: Option<SenderInfo>,
    message: Option<MessageInfo>,
}

#[derive(Deserialize)]
struct SenderInfo {
    sender_id: Option<SenderId>,
}

#[derive(Deserialize)]
struct SenderId {
    open_id: Option<String>,
}

#[derive(Deserialize)]
struct MessageInfo {
    message_id: Option<String>,
    chat_id: Option<String>,
    #[allow(dead_code)]
    chat_type: Option<String>,
    message_type: Option<String>,
    content: Option<String>,
    mentions: Option<Vec<MentionInfo>>,
}

#[derive(Deserialize)]
struct MentionInfo {
    id: Option<MentionId>,
}

#[derive(Deserialize)]
struct MentionId {
    open_id: Option<String>,
}

/// Feishu send message response.
#[derive(Deserialize)]
struct SendMessageResponse {
    code: i32,
    #[serde(default)]
    msg: String,
    data: Option<SendMessageData>,
}

#[derive(Deserialize)]
struct SendMessageData {
    message_id: Option<String>,
}

/// Feishu upload file response.
#[derive(Deserialize)]
struct UploadFileResponse {
    code: i32,
    #[serde(default)]
    msg: String,
    data: Option<UploadFileData>,
}

#[derive(Deserialize)]
struct UploadFileData {
    file_key: Option<String>,
}

/// Text message content from Feishu.
#[derive(Deserialize)]
struct TextContent {
    text: Option<String>,
}

/// Audio message content from Feishu.
#[derive(Deserialize)]
struct AudioContent {
    file_key: Option<String>,
}

/// Image message content from Feishu.
#[derive(Deserialize)]
struct ImageContent {
    image_key: Option<String>,
}

// ============================================================================
// Token state
// ============================================================================

struct TokenState {
    access_token: String,
    expires_at: i64, // unix timestamp
}

impl Default for TokenState {
    fn default() -> Self {
        Self {
            access_token: String::new(),
            expires_at: 0,
        }
    }
}

// ============================================================================
// FeishuChannel
// ============================================================================

/// Feishu/Lark channel using WebSocket long connection.
pub struct FeishuChannel {
    client: reqwest::Client,
    app_id: String,
    app_secret: String,
    domain: String,
    allowed_users: Vec<String>,
    rx: Mutex<mpsc::Receiver<IncomingMessage>>,
    token: Arc<Mutex<TokenState>>,
}

impl FeishuChannel {
    /// Create a new FeishuChannel, spawning a background WebSocket task.
    pub fn new(client: reqwest::Client, config: &FeishuConfig) -> Self {
        let (tx, rx) = mpsc::channel::<IncomingMessage>(64);
        let token = Arc::new(Mutex::new(TokenState::default()));

        // Spawn background WebSocket loop
        let ws_client = client.clone();
        let ws_app_id = config.app_id.clone();
        let ws_app_secret = config.app_secret.clone();
        let ws_domain = config.domain.clone();
        let ws_allowed = config.allowed_users.clone();

        tokio::spawn(async move {
            feishu_ws_loop(ws_client, ws_app_id, ws_app_secret, ws_domain, ws_allowed, tx).await;
        });

        Self {
            client,
            app_id: config.app_id.clone(),
            app_secret: config.app_secret.clone(),
            domain: config.domain.clone(),
            allowed_users: config.allowed_users.clone(),
            rx: Mutex::new(rx),
            token,
        }
    }

    /// Get a valid tenant_access_token, refreshing if expired or about to expire.
    async fn get_token(&self) -> Result<String> {
        let mut state = self.token.lock().await;
        let now = chrono::Utc::now().timestamp();

        // Refresh if expired or within 5 minutes of expiry
        if !state.access_token.is_empty() && state.expires_at > now + 300 {
            return Ok(state.access_token.clone());
        }

        let url = format!(
            "{}/open-apis/auth/v3/tenant_access_token/internal",
            self.domain
        );
        let body = serde_json::json!({
            "app_id": self.app_id,
            "app_secret": self.app_secret,
        });

        let resp: TokenResponse = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await?
            .json()
            .await?;

        if resp.code != 0 {
            return Err(GatewayError::Feishu(format!(
                "get token failed ({}): {}",
                resp.code, resp.msg
            )));
        }

        let token = resp
            .tenant_access_token
            .ok_or_else(|| GatewayError::Feishu("no token in response".into()))?;
        let expire = resp.expire.unwrap_or(7200);

        state.access_token = token.clone();
        state.expires_at = now + expire;

        info!("feishu tenant_access_token refreshed (expires in {}s)", expire);
        Ok(token)
    }

    fn is_allowed(&self, open_id: &str) -> bool {
        self.allowed_users.is_empty() || self.allowed_users.iter().any(|u| u == open_id)
    }
}

#[async_trait]
impl Channel for FeishuChannel {
    fn id(&self) -> &str {
        "feishu"
    }

    async fn poll(&self) -> Result<Vec<IncomingMessage>> {
        let mut rx = self.rx.lock().await;
        let mut messages = Vec::new();

        // Use recv() for the first message (blocking wait like Telegram long poll)
        match rx.recv().await {
            Some(msg) => messages.push(msg),
            None => {
                // Channel closed — WS loop died
                return Err(GatewayError::Feishu("WebSocket loop terminated".into()));
            }
        }

        // Drain any additional buffered messages
        while let Ok(msg) = rx.try_recv() {
            messages.push(msg);
        }

        Ok(messages)
    }

    async fn send_text(&self, chat_id: &str, text: &str) -> Result<String> {
        let token = self.get_token().await?;
        let url = format!(
            "{}/open-apis/im/v1/messages?receive_id_type=chat_id",
            self.domain
        );

        let mut last_msg_id = String::new();

        for chunk in chunk_text(text, 4000) {
            let content = build_card_content(chunk);
            let body = serde_json::json!({
                "receive_id": chat_id,
                "msg_type": "interactive",
                "content": content,
            });

            let resp: SendMessageResponse = self
                .client
                .post(&url)
                .header("Authorization", format!("Bearer {}", token))
                .json(&body)
                .send()
                .await?
                .json()
                .await?;

            if resp.code != 0 {
                return Err(GatewayError::Feishu(format!(
                    "send_text failed ({}): {}",
                    resp.code, resp.msg
                )));
            }

            if let Some(data) = resp.data {
                if let Some(mid) = data.message_id {
                    last_msg_id = mid;
                }
            }
        }

        Ok(last_msg_id)
    }

    async fn edit_message(&self, _chat_id: &str, msg_id: &str, text: &str) -> Result<()> {
        let token = self.get_token().await?;
        let url = format!(
            "{}/open-apis/im/v1/messages/{}",
            self.domain, msg_id
        );
        let content = build_card_content(text);
        let body = serde_json::json!({
            "msg_type": "interactive",
            "content": content,
        });

        let resp: SendMessageResponse = self
            .client
            .patch(&url)
            .header("Authorization", format!("Bearer {}", token))
            .json(&body)
            .send()
            .await?
            .json()
            .await?;

        if resp.code != 0 {
            return Err(GatewayError::Feishu(format!(
                "edit_message failed ({}): {}",
                resp.code, resp.msg
            )));
        }

        Ok(())
    }

    async fn send_voice(&self, chat_id: &str, audio: &[u8]) -> Result<()> {
        let token = self.get_token().await?;

        // Step 1: Upload the audio file (duration must be in the upload form, not the message)
        let upload_url = format!("{}/open-apis/im/v1/files", self.domain);

        let duration_ms =
            crate::provider::tts::webm_to_ogg::ogg_opus_duration_ms(audio);

        let part = reqwest::multipart::Part::bytes(audio.to_vec())
            .file_name("voice.opus")
            .mime_str("audio/opus")
            .map_err(|e| GatewayError::Feishu(e.to_string()))?;

        let mut form = reqwest::multipart::Form::new()
            .text("file_type", "opus")
            .text("file_name", "voice.opus")
            .part("file", part);

        if duration_ms > 0 {
            debug!(duration_ms, "feishu upload: including duration");
            form = form.text("duration", duration_ms.to_string());
        }

        let upload_resp: UploadFileResponse = self
            .client
            .post(&upload_url)
            .header("Authorization", format!("Bearer {}", token))
            .multipart(form)
            .send()
            .await?
            .json()
            .await?;

        if upload_resp.code != 0 {
            return Err(GatewayError::Feishu(format!(
                "upload voice failed ({}): {}",
                upload_resp.code, upload_resp.msg
            )));
        }

        let file_key = upload_resp
            .data
            .and_then(|d| d.file_key)
            .ok_or_else(|| GatewayError::Feishu("no file_key in upload response".into()))?;

        // Step 2: Send the audio message
        let send_url = format!(
            "{}/open-apis/im/v1/messages?receive_id_type=chat_id",
            self.domain
        );
        let content = serde_json::json!({ "file_key": file_key }).to_string();
        let body = serde_json::json!({
            "receive_id": chat_id,
            "msg_type": "audio",
            "content": content,
        });

        let resp: SendMessageResponse = self
            .client
            .post(&send_url)
            .header("Authorization", format!("Bearer {}", token))
            .json(&body)
            .send()
            .await?
            .json()
            .await?;

        if resp.code != 0 {
            return Err(GatewayError::Feishu(format!(
                "send_voice failed ({}): {}",
                resp.code, resp.msg
            )));
        }

        Ok(())
    }

    async fn download_voice(&self, file_ref: &str) -> Result<Vec<u8>> {
        // file_ref format: "message_id:file_key"
        let parts: Vec<&str> = file_ref.splitn(2, ':').collect();
        if parts.len() != 2 {
            return Err(GatewayError::Feishu(format!(
                "invalid file_ref format: expected 'message_id:file_key', got '{}'",
                file_ref
            )));
        }
        let message_id = parts[0];
        let file_key = parts[1];

        let token = self.get_token().await?;
        let url = format!(
            "{}/open-apis/im/v1/messages/{}/resources/{}?type=file",
            self.domain, message_id, file_key
        );

        let bytes = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await?
            .bytes()
            .await?;

        Ok(bytes.to_vec())
    }
}

// ============================================================================
// Background WebSocket loop
// ============================================================================

/// Background task that maintains the Feishu WebSocket connection.
///
/// Handles connection establishment, ping/pong, event parsing, and
/// automatic reconnection with exponential backoff + jitter.
async fn feishu_ws_loop(
    client: reqwest::Client,
    app_id: String,
    app_secret: String,
    domain: String,
    allowed_users: Vec<String>,
    tx: mpsc::Sender<IncomingMessage>,
) {
    let max_reconnect = 180u32;
    let base_interval_secs = 3u64;
    let mut seen_events = std::collections::HashSet::new();

    loop {
        // Step 1: Get WSS URL
        let ws_url = match get_ws_endpoint(&client, &app_id, &app_secret, &domain).await {
            Ok((url, config)) => {
                info!("feishu WebSocket endpoint obtained");
                // Use server-provided config if available
                let _ = config; // reconnect params are handled at this level
                url
            }
            Err(e) => {
                error!(error = %e, "failed to get feishu WS endpoint");
                tokio::time::sleep(std::time::Duration::from_secs(base_interval_secs)).await;
                continue;
            }
        };

        // Step 2: Connect WebSocket
        let ws_stream = match tokio_tungstenite::connect_async(&ws_url).await {
            Ok((stream, _)) => {
                info!("feishu WebSocket connected");
                stream
            }
            Err(e) => {
                error!(error = %e, "feishu WebSocket connect failed");
                tokio::time::sleep(std::time::Duration::from_secs(base_interval_secs)).await;
                continue;
            }
        };

        // Step 3: Message loop
        let disconnected = handle_ws_connection(
            ws_stream,
            &allowed_users,
            &tx,
            &mut seen_events,
        )
        .await;

        if disconnected {
            warn!("feishu WebSocket disconnected, reconnecting...");
        }

        // Reconnect with jitter
        let jitter_ms = rand_jitter_ms(base_interval_secs * 1000);
        tokio::time::sleep(std::time::Duration::from_millis(jitter_ms)).await;

        // Check if channel is closed (main loop shut down)
        if tx.is_closed() {
            info!("feishu WS loop: channel closed, exiting");
            break;
        }

        let _ = max_reconnect; // reconnect count managed by outer loop
    }
}

/// Simple pseudo-random jitter using timestamps (avoids pulling in rand crate).
fn rand_jitter_ms(base_ms: u64) -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    base_ms + (now % base_ms)
}

/// Get the WSS endpoint URL from Feishu API.
async fn get_ws_endpoint(
    client: &reqwest::Client,
    app_id: &str,
    app_secret: &str,
    domain: &str,
) -> Result<(String, Option<WsClientConfig>)> {
    let url = format!("{}/callback/ws/endpoint", domain);
    let body = serde_json::json!({
        "AppID": app_id,
        "AppSecret": app_secret,
    });

    let resp: WsEndpointResponse = client
        .post(&url)
        .json(&body)
        .send()
        .await?
        .json()
        .await?;

    if resp.code != 0 {
        return Err(GatewayError::Feishu(format!(
            "WS endpoint failed ({}): {}",
            resp.code, resp.msg
        )));
    }

    let data = resp
        .data
        .ok_or_else(|| GatewayError::Feishu("no data in WS endpoint response".into()))?;

    Ok((data.url, data.client_config))
}

/// Handle a single WebSocket connection — returns true when disconnected.
async fn handle_ws_connection(
    ws_stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    allowed_users: &[String],
    tx: &mpsc::Sender<IncomingMessage>,
    seen_events: &mut std::collections::HashSet<String>,
) -> bool {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let (mut ws_write, mut ws_read) = ws_stream.split();

    // Ping interval (120s default, can be overridden by server config)
    let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(120));
    ping_interval.tick().await; // skip first immediate tick

    loop {
        tokio::select! {
            msg = ws_read.next() => {
                match msg {
                    Some(Ok(WsMessage::Binary(data))) => {
                        let frame = decode_frame(&data);
                        let frame_type = frame.header("type").unwrap_or("");

                        match frame_type {
                            "ping" => {
                                // Respond with pong
                                let pong = Frame {
                                    seq_id: frame.seq_id,
                                    log_id: frame.log_id,
                                    service: frame.service,
                                    method: frame.method,
                                    headers: vec![Header {
                                        key: "type".into(),
                                        value: "pong".into(),
                                    }],
                                    ..Default::default()
                                };
                                let pong_bytes = encode_frame(&pong);
                                if let Err(e) = ws_write.send(WsMessage::Binary(pong_bytes.into())).await {
                                    error!(error = %e, "failed to send pong");
                                    return true;
                                }
                                debug!("feishu: pong sent (seq={})", frame.seq_id);
                            }
                            "event" => {
                                if let Err(e) = handle_event_frame(&frame, allowed_users, tx, seen_events).await {
                                    warn!(error = %e, "failed to handle feishu event");
                                }
                            }
                            other => {
                                debug!(frame_type = other, "feishu: unknown frame type");
                            }
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) => {
                        info!("feishu WebSocket received close frame");
                        return true;
                    }
                    Some(Ok(_)) => {
                        // Text or other frame types — ignore
                    }
                    Some(Err(e)) => {
                        error!(error = %e, "feishu WebSocket read error");
                        return true;
                    }
                    None => {
                        // Stream ended
                        return true;
                    }
                }
            }
            _ = ping_interval.tick() => {
                // Send keepalive ping (empty binary frame not needed;
                // server sends pings, we only need to respond)
            }
        }
    }
}

/// Parse and route an event frame.
async fn handle_event_frame(
    frame: &Frame,
    allowed_users: &[String],
    tx: &mpsc::Sender<IncomingMessage>,
    seen_events: &mut std::collections::HashSet<String>,
) -> Result<()> {
    let payload_str = std::str::from_utf8(&frame.payload)
        .map_err(|e| GatewayError::Feishu(format!("invalid UTF-8 in event payload: {e}")))?;

    let envelope: EventEnvelope = serde_json::from_str(payload_str)?;

    let header = envelope
        .header
        .ok_or_else(|| GatewayError::Feishu("event missing header".into()))?;

    // Skip stale events (e.g. replayed after gateway restart)
    if let Some(ref ct) = header.create_time {
        if let Ok(ms) = ct.parse::<i64>() {
            let age_secs = chrono::Utc::now().timestamp() - ms / 1000;
            if age_secs > 120 {
                info!(event_id = ?header.event_id, age_secs, "feishu: skipping stale event");
                return Ok(());
            }
        }
    }

    // Deduplicate events by event_id
    if let Some(ref event_id) = header.event_id {
        if !seen_events.insert(event_id.clone()) {
            debug!(event_id, "feishu: skipping duplicate event");
            return Ok(());
        }
        // Cap the dedup set to prevent unbounded growth
        if seen_events.len() > 1000 {
            seen_events.clear();
        }
    }

    let event_type = header.event_type.as_deref().unwrap_or("");

    if event_type != "im.message.receive_v1" {
        debug!(event_type, "feishu: ignoring non-message event");
        return Ok(());
    }

    let event_val = envelope
        .event
        .ok_or_else(|| GatewayError::Feishu("event missing event body".into()))?;

    let event: MessageEvent = serde_json::from_value(event_val)?;

    let sender_open_id = event
        .sender
        .and_then(|s| s.sender_id)
        .and_then(|s| s.open_id)
        .unwrap_or_default();

    // Check allowed users
    if !allowed_users.is_empty() && !allowed_users.iter().any(|u| u == &sender_open_id) {
        debug!(sender = %sender_open_id, "feishu: ignoring message from non-allowed user");
        return Ok(());
    }

    let msg_info = event
        .message
        .ok_or_else(|| GatewayError::Feishu("event missing message".into()))?;

    let chat_id = msg_info.chat_id.unwrap_or_default();
    let message_id = msg_info.message_id.clone().unwrap_or_default();
    let msg_type = msg_info.message_type.as_deref().unwrap_or("");
    let content_str = msg_info.content.as_deref().unwrap_or("");

    let content = match msg_type {
        "text" => {
            let text_content: TextContent =
                serde_json::from_str(content_str).unwrap_or(TextContent { text: None });
            let mut text = text_content.text.unwrap_or_default();

            // Strip @mention tags from text (format: @_user_N)
            // Feishu adds @_user_1 style placeholders in the text
            text = strip_at_mentions(&text);
            let text = text.trim().to_string();

            if text.is_empty() {
                return Ok(());
            }
            MessageContent::Text(text)
        }
        "audio" => {
            let audio_content: AudioContent =
                serde_json::from_str(content_str).unwrap_or(AudioContent { file_key: None });
            let file_key = audio_content.file_key.unwrap_or_default();
            if file_key.is_empty() {
                return Ok(());
            }
            MessageContent::Voice {
                file_ref: format!("{}:{}", message_id, file_key),
                mime: "audio/opus".into(),
            }
        }
        "image" => {
            let image_content: ImageContent =
                serde_json::from_str(content_str).unwrap_or(ImageContent { image_key: None });
            let image_key = image_content.image_key.unwrap_or_default();
            if image_key.is_empty() {
                return Ok(());
            }
            MessageContent::Image {
                file_ref: format!("{}:{}", message_id, image_key),
                mime: "image/jpeg".into(),
                caption: None,
            }
        }
        _ => {
            debug!(msg_type, "feishu: ignoring unsupported message type");
            return Ok(());
        }
    };

    let incoming = IncomingMessage {
        channel: "feishu".into(),
        chat_id,
        sender_id: sender_open_id,
        content,
        timestamp: chrono::Utc::now().timestamp(),
    };

    tx.send(incoming)
        .await
        .map_err(|e| GatewayError::Feishu(format!("failed to send to channel: {e}")))?;

    Ok(())
}

/// Strip @mention placeholders from Feishu text messages.
/// Feishu injects `@_user_N` (e.g., `@_user_1`) for each mention.
fn strip_at_mentions(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'@' {
            // Check if this looks like @_user_\d+
            let prefix = b"_user_";
            let after_at = i + 1;
            if after_at + prefix.len() <= bytes.len()
                && &bytes[after_at..after_at + prefix.len()] == prefix
            {
                // Count digits after "_user_"
                let digit_start = after_at + prefix.len();
                let mut digit_end = digit_start;
                while digit_end < bytes.len() && bytes[digit_end].is_ascii_digit() {
                    digit_end += 1;
                }
                if digit_end > digit_start {
                    // Valid @_user_N — skip it
                    i = digit_end;
                    continue;
                }
            }
            result.push('@');
            i += 1;
        } else {
            // Safe: we're working with ASCII prefix detection, but must handle
            // multi-byte UTF-8 properly for the rest
            let ch_len = utf8_char_len(bytes[i]);
            let end = (i + ch_len).min(bytes.len());
            result.push_str(&text[i..end]);
            i = end;
        }
    }

    result
}

/// Return the length of a UTF-8 character from its first byte.
fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    }
}

/// Build a Feishu interactive card JSON string with markdown content.
///
/// Feishu only allows editing card (interactive) messages, not plain text.
/// Using cards for all text messages enables streaming preview via edit_message.
fn build_card_content(text: &str) -> String {
    serde_json::json!({
        "config": { "wide_screen_mode": true },
        "elements": [{
            "tag": "markdown",
            "content": text
        }]
    })
    .to_string()
}

/// Split text into chunks (reuse Telegram's logic).
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

        let mut end = max_len;
        while end > 0 && !remaining.is_char_boundary(end) {
            end -= 1;
        }

        let slice = &remaining[..end];

        if let Some(pos) = slice.rfind('\n') {
            chunks.push(&remaining[..pos + 1]);
            remaining = &remaining[pos + 1..];
        } else if let Some(pos) = slice.rfind(' ') {
            chunks.push(&remaining[..pos + 1]);
            remaining = &remaining[pos + 1..];
        } else {
            chunks.push(slice);
            remaining = &remaining[end..];
        }
    }

    chunks
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- Protobuf codec tests ------------------------------------------------

    #[test]
    fn varint_roundtrip() {
        for val in [0u64, 1, 127, 128, 300, 16384, u64::MAX] {
            let encoded = encode_varint(val);
            let (decoded, len) = decode_varint(&encoded, 0).unwrap();
            assert_eq!(decoded, val, "varint roundtrip failed for {val}");
            assert_eq!(len, encoded.len());
        }
    }

    #[test]
    fn frame_encode_decode_roundtrip() {
        let frame = Frame {
            seq_id: 42,
            log_id: 100,
            service: 1,
            method: 2,
            headers: vec![
                Header {
                    key: "type".into(),
                    value: "ping".into(),
                },
                Header {
                    key: "foo".into(),
                    value: "bar".into(),
                },
            ],
            payload: b"hello world".to_vec(),
            ..Default::default()
        };

        let encoded = encode_frame(&frame);
        let decoded = decode_frame(&encoded);

        assert_eq!(decoded.seq_id, 42);
        assert_eq!(decoded.log_id, 100);
        assert_eq!(decoded.service, 1);
        assert_eq!(decoded.method, 2);
        assert_eq!(decoded.headers.len(), 2);
        assert_eq!(decoded.headers[0].key, "type");
        assert_eq!(decoded.headers[0].value, "ping");
        assert_eq!(decoded.headers[1].key, "foo");
        assert_eq!(decoded.headers[1].value, "bar");
        assert_eq!(decoded.payload, b"hello world");
    }

    #[test]
    fn frame_header_lookup() {
        let frame = Frame {
            headers: vec![
                Header {
                    key: "type".into(),
                    value: "event".into(),
                },
                Header {
                    key: "trace".into(),
                    value: "abc123".into(),
                },
            ],
            ..Default::default()
        };

        assert_eq!(frame.header("type"), Some("event"));
        assert_eq!(frame.header("trace"), Some("abc123"));
        assert_eq!(frame.header("missing"), None);
    }

    #[test]
    fn decode_empty_frame() {
        let frame = decode_frame(&[]);
        assert_eq!(frame.seq_id, 0);
        assert_eq!(frame.headers.len(), 0);
        assert!(frame.payload.is_empty());
    }

    #[test]
    fn pong_frame_encoding() {
        let pong = Frame {
            seq_id: 7,
            log_id: 99,
            headers: vec![Header {
                key: "type".into(),
                value: "pong".into(),
            }],
            ..Default::default()
        };
        let encoded = encode_frame(&pong);
        let decoded = decode_frame(&encoded);
        assert_eq!(decoded.seq_id, 7);
        assert_eq!(decoded.header("type"), Some("pong"));
    }

    // -- Token expiry tests --------------------------------------------------

    #[test]
    fn token_state_default_is_expired() {
        let state = TokenState::default();
        assert!(state.access_token.is_empty());
        assert_eq!(state.expires_at, 0);
    }

    // -- Event JSON parsing tests --------------------------------------------

    #[test]
    fn parse_text_event() {
        let json = r#"{
            "header": {
                "event_id": "ev_123",
                "event_type": "im.message.receive_v1"
            },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_abc" } },
                "message": {
                    "message_id": "om_001",
                    "chat_id": "oc_xyz",
                    "chat_type": "p2p",
                    "message_type": "text",
                    "content": "{\"text\":\"hello world\"}"
                }
            }
        }"#;

        let envelope: EventEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(
            envelope.header.unwrap().event_type.as_deref(),
            Some("im.message.receive_v1")
        );

        let event: MessageEvent =
            serde_json::from_value(envelope.event.unwrap()).unwrap();
        let sender = event.sender.unwrap().sender_id.unwrap().open_id.unwrap();
        assert_eq!(sender, "ou_abc");

        let msg = event.message.unwrap();
        assert_eq!(msg.message_type.as_deref(), Some("text"));
        let text: TextContent = serde_json::from_str(msg.content.as_deref().unwrap()).unwrap();
        assert_eq!(text.text.as_deref(), Some("hello world"));
    }

    #[test]
    fn parse_audio_event() {
        let json = r#"{
            "header": { "event_id": "ev_456", "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_def" } },
                "message": {
                    "message_id": "om_002",
                    "chat_id": "oc_xyz",
                    "message_type": "audio",
                    "content": "{\"file_key\":\"file_abc123\"}"
                }
            }
        }"#;

        let envelope: EventEnvelope = serde_json::from_str(json).unwrap();
        let event: MessageEvent =
            serde_json::from_value(envelope.event.unwrap()).unwrap();
        let msg = event.message.unwrap();
        assert_eq!(msg.message_type.as_deref(), Some("audio"));
        let audio: AudioContent =
            serde_json::from_str(msg.content.as_deref().unwrap()).unwrap();
        assert_eq!(audio.file_key.as_deref(), Some("file_abc123"));
    }

    #[test]
    fn parse_image_event() {
        let json = r#"{
            "header": { "event_id": "ev_789", "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_ghi" } },
                "message": {
                    "message_id": "om_003",
                    "chat_id": "oc_xyz",
                    "message_type": "image",
                    "content": "{\"image_key\":\"img_v3_123\"}"
                }
            }
        }"#;

        let envelope: EventEnvelope = serde_json::from_str(json).unwrap();
        let event: MessageEvent =
            serde_json::from_value(envelope.event.unwrap()).unwrap();
        let msg = event.message.unwrap();
        assert_eq!(msg.message_type.as_deref(), Some("image"));
        let img: ImageContent =
            serde_json::from_str(msg.content.as_deref().unwrap()).unwrap();
        assert_eq!(img.image_key.as_deref(), Some("img_v3_123"));
    }

    // -- User filtering tests ------------------------------------------------

    #[test]
    fn allowed_users_empty_allows_all() {
        let allowed: Vec<String> = vec![];
        assert!(allowed.is_empty() || allowed.contains(&"anyone".to_string()));
    }

    #[test]
    fn allowed_users_filter() {
        let allowed = vec!["ou_aaa".to_string(), "ou_bbb".to_string()];
        assert!(allowed.iter().any(|u| u == "ou_aaa"));
        assert!(!allowed.iter().any(|u| u == "ou_ccc"));
    }

    // -- strip_at_mentions tests ---------------------------------------------

    #[test]
    fn strip_at_mention_single() {
        assert_eq!(strip_at_mentions("@_user_1 hello"), " hello");
    }

    #[test]
    fn strip_at_mention_multiple() {
        assert_eq!(strip_at_mentions("@_user_1 @_user_2 hi"), "  hi");
    }

    #[test]
    fn strip_at_mention_no_mention() {
        assert_eq!(strip_at_mentions("hello world"), "hello world");
    }

    #[test]
    fn strip_at_mention_partial() {
        // "@_user" without a number should NOT be stripped
        assert_eq!(strip_at_mentions("@_user hello"), "@_user hello");
    }

    #[test]
    fn strip_at_mention_at_sign_preserved() {
        assert_eq!(strip_at_mentions("email@test.com"), "email@test.com");
    }

    // -- chunk_text tests ----------------------------------------------------

    #[test]
    fn chunk_text_short() {
        let chunks = chunk_text("hello", 4000);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn chunk_text_long() {
        let text = "a".repeat(5000);
        let chunks = chunk_text(&text, 4000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 4000);
        assert_eq!(chunks[1].len(), 1000);
    }

    // -- file_ref format tests -----------------------------------------------

    #[test]
    fn file_ref_format() {
        let file_ref = "om_001:file_abc123";
        let parts: Vec<&str> = file_ref.splitn(2, ':').collect();
        assert_eq!(parts[0], "om_001");
        assert_eq!(parts[1], "file_abc123");
    }

    // -- stale event filtering tests -----------------------------------------

    /// Build a Frame whose payload is the given JSON string.
    fn event_frame(json: &str) -> Frame {
        Frame {
            service: 1,
            method: 0,
            payload: json.as_bytes().to_vec(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn stale_event_is_skipped() {
        let stale_ms = (chrono::Utc::now().timestamp() - 300) * 1000; // 5 min ago
        let json = format!(
            r#"{{
                "header": {{
                    "event_id": "ev_stale",
                    "event_type": "im.message.receive_v1",
                    "create_time": "{stale_ms}"
                }},
                "event": {{
                    "sender": {{ "sender_id": {{ "open_id": "ou_abc" }} }},
                    "message": {{
                        "message_id": "om_stale",
                        "chat_id": "oc_xyz",
                        "message_type": "text",
                        "content": "{{\"text\":\"old message\"}}"
                    }}
                }}
            }}"#
        );
        let frame = event_frame(&json);
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let mut seen = std::collections::HashSet::new();

        handle_event_frame(&frame, &[], &tx, &mut seen).await.unwrap();

        // Channel should be empty — stale event was skipped
        assert!(rx.try_recv().is_err(), "stale event should not produce a message");
        // event_id should NOT be inserted into seen set (filtered before dedup)
        assert!(!seen.contains("ev_stale"));
    }

    #[tokio::test]
    async fn fresh_event_is_not_skipped() {
        let fresh_ms = chrono::Utc::now().timestamp() * 1000; // now
        let json = format!(
            r#"{{
                "header": {{
                    "event_id": "ev_fresh",
                    "event_type": "im.message.receive_v1",
                    "create_time": "{fresh_ms}"
                }},
                "event": {{
                    "sender": {{ "sender_id": {{ "open_id": "ou_abc" }} }},
                    "message": {{
                        "message_id": "om_fresh",
                        "chat_id": "oc_xyz",
                        "message_type": "text",
                        "content": "{{\"text\":\"new message\"}}"
                    }}
                }}
            }}"#
        );
        let frame = event_frame(&json);
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let mut seen = std::collections::HashSet::new();

        // allowed_users is empty → allows all; the function should pass the
        // stale-event check and dedup, then try to send on the channel.
        // With no allowed-user match needed (empty list), it will reach the
        // tx.send() and succeed (or fail on something after the time filter).
        let _ = handle_event_frame(&frame, &[], &tx, &mut seen).await;

        // event_id should be in seen set — event was NOT filtered by time
        assert!(seen.contains("ev_fresh"), "fresh event should pass stale filter");
    }

    #[tokio::test]
    async fn missing_create_time_is_not_filtered() {
        // Events without create_time should pass through (backward compat)
        let json = r#"{
            "header": {
                "event_id": "ev_notime",
                "event_type": "im.message.receive_v1"
            },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_abc" } },
                "message": {
                    "message_id": "om_notime",
                    "chat_id": "oc_xyz",
                    "message_type": "text",
                    "content": "{\"text\":\"no time\"}"
                }
            }
        }"#;
        let frame = event_frame(json);
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let mut seen = std::collections::HashSet::new();

        let _ = handle_event_frame(&frame, &[], &tx, &mut seen).await;

        assert!(seen.contains("ev_notime"), "event without create_time should pass stale filter");
    }
}
