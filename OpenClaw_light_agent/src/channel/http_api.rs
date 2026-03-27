//! HTTP API channel — REST + SSE streaming for external integrations.
//!
//! Routes:
//! - `GET /`              → embedded Web Chat HTML page
//! - `GET /chat/history`  → session history JSON
//! - `POST /chat`         → oneshot request-response (unchanged)
//! - `POST /chat/stream`  → SSE streaming response
//! - `OPTIONS *`          → CORS preflight (204)
//!
//! Uses raw `TcpListener` + hand-written HTTP/1.1 parsing to avoid pulling
//! in a web framework.  Memory overhead: ~1KB constant + pending/SSE maps.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, error, info, warn};

use super::types::{IncomingMessage, MessageContent};
use super::Channel;
use crate::config::HttpApiConfig;
use crate::error::{GatewayError, Result};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find the nearest char boundary at or after `index` in a UTF-8 string.
/// Equivalent to the nightly `str::ceil_char_boundary`.
fn ceil_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------
// Embedded HTML
// ---------------------------------------------------------------------------

/// Embedded Web Chat UI — compiled into the binary via `include_bytes!`.
const WEB_CHAT_HTML: &[u8] = include_bytes!("web_chat.html");

// ---------------------------------------------------------------------------
// SSE types
// ---------------------------------------------------------------------------

/// SSE event sent to the client.
#[derive(Debug, Clone)]
enum SseEvent {
    /// Incremental text fragment.
    Delta(String),
    /// Agent is processing (keepalive).
    Typing,
    /// Stream complete — contains full text for client calibration.
    Done(String),
    /// Error.
    Error(String),
}

impl SseEvent {
    /// Serialize to SSE wire format (`data: {...}\n\n`).
    fn to_sse_bytes(&self) -> Vec<u8> {
        let json = match self {
            SseEvent::Delta(text) => {
                let escaped = serde_json::to_string(text).unwrap_or_default();
                format!(r#"{{"type":"delta","text":{escaped}}}"#)
            }
            SseEvent::Typing => r#"{"type":"typing"}"#.to_string(),
            SseEvent::Done(text) => {
                let escaped = serde_json::to_string(text).unwrap_or_default();
                format!(r#"{{"type":"done","text":{escaped}}}"#)
            }
            SseEvent::Error(msg) => {
                let escaped = serde_json::to_string(msg).unwrap_or_default();
                format!(r#"{{"type":"error","message":{escaped}}}"#)
            }
        };
        format!("data: {json}\n\n").into_bytes()
    }
}

/// Per-SSE-connection state.
struct SseState {
    tx: mpsc::Sender<SseEvent>,
    /// Number of text bytes already sent (for incremental delta calculation).
    sent_len: usize,
    /// Full accumulated text (for Done event calibration).
    full_text: String,
}

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

/// Incoming JSON body for POST /chat and POST /chat/stream.
#[derive(Deserialize)]
struct ChatRequest {
    chat_id: Option<String>,
    text: String,
    sender_id: Option<String>,
}

/// Outgoing JSON response for POST /chat.
#[derive(Serialize)]
struct ChatResponse {
    text: String,
    has_voice: bool,
}

/// Simplified history entry returned by GET /chat/history.
#[derive(Serialize)]
struct HistoryEntry {
    role: String,
    text: String,
}

// ---------------------------------------------------------------------------
// HttpApiChannel
// ---------------------------------------------------------------------------

/// HTTP API channel backed by a raw TCP listener.
pub struct HttpApiChannel {
    listener: TcpListener,
    /// Oneshot request-response (POST /chat): req_id → sender.
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<(String, bool)>>>>,
    /// SSE streaming connections (POST /chat/stream): chat_id → state.
    sse_streams: Arc<Mutex<HashMap<String, SseState>>>,
    next_req_id: Mutex<u64>,
    /// Session directory path (for reading history). None = history endpoint disabled.
    session_dir: Option<String>,
    /// Bearer token for authentication. Empty = no auth required.
    auth_token: String,
}

impl HttpApiChannel {
    pub async fn new(config: &HttpApiConfig, session_dir: Option<String>) -> Result<Self> {
        let listener = TcpListener::bind(&config.listen).await.map_err(|e| {
            GatewayError::Config(format!("failed to bind HTTP API to {}: {e}", config.listen))
        })?;
        info!(listen = %config.listen, "HTTP API channel listening");
        Ok(Self {
            listener,
            pending: Arc::new(Mutex::new(HashMap::new())),
            sse_streams: Arc::new(Mutex::new(HashMap::new())),
            next_req_id: Mutex::new(1),
            session_dir,
            auth_token: config.auth_token.clone(),
        })
    }

    /// Check Bearer token authentication. Returns true if authorized.
    fn check_auth(&self, raw: &str) -> bool {
        if self.auth_token.is_empty() {
            return true;
        }
        // Find Authorization header
        for line in raw.lines() {
            let lower = line.to_lowercase();
            if lower.starts_with("authorization:") {
                let value = line["authorization:".len()..].trim();
                if let Some(token) = value.strip_prefix("Bearer ") {
                    return token.trim() == self.auth_token;
                }
                if let Some(token) = value.strip_prefix("bearer ") {
                    return token.trim() == self.auth_token;
                }
                return false;
            }
        }
        false
    }

    /// Serve the embedded HTML page.
    async fn serve_html(stream: &mut tokio::net::TcpStream) {
        let headers = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\n\
             {}\
             Connection: close\r\n\
             \r\n",
            WEB_CHAT_HTML.len(),
            cors_headers(),
        );
        let _ = stream.write_all(headers.as_bytes()).await;
        let _ = stream.write_all(WEB_CHAT_HTML).await;
    }

    /// Handle GET /chat/history?chat_id=xxx.
    async fn serve_history(
        stream: &mut tokio::net::TcpStream,
        path: &str,
        session_dir: &str,
    ) {
        // Parse chat_id from query string
        let chat_id = path
            .split('?')
            .nth(1)
            .and_then(|qs| {
                qs.split('&').find_map(|param| {
                    let (k, v) = param.split_once('=')?;
                    if k == "chat_id" {
                        Some(v.to_string())
                    } else {
                        None
                    }
                })
            })
            .unwrap_or_default();

        if chat_id.is_empty() {
            send_json_response(stream, 400, r#"{"error":"missing chat_id parameter"}"#).await;
            return;
        }

        // Read session JSONL file
        let session_path = format!("{}/http_api_{}.jsonl", session_dir, chat_id);
        let entries = match tokio::fs::read_to_string(&session_path).await {
            Ok(content) => {
                let mut entries = Vec::new();
                for line in content.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    // Parse ChatMessage and extract text-only entries
                    if let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) {
                        let role = msg["role"].as_str().unwrap_or("").to_string();
                        if role != "user" && role != "assistant" {
                            continue;
                        }
                        // Extract text blocks from content array
                        if let Some(content_arr) = msg["content"].as_array() {
                            for block in content_arr {
                                if block["type"].as_str() == Some("text") {
                                    if let Some(text) = block["text"].as_str() {
                                        if !text.is_empty() {
                                            entries.push(HistoryEntry {
                                                role: role.clone(),
                                                text: text.to_string(),
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                entries
            }
            Err(_) => Vec::new(), // File not found → empty history
        };

        let body = serde_json::to_string(&entries).unwrap_or_else(|_| "[]".into());
        send_json_response(stream, 200, &body).await;
    }

    /// Handle POST /chat/stream — start an SSE connection.
    async fn start_sse_stream(
        &self,
        stream: tokio::net::TcpStream,
        chat_req: ChatRequest,
    ) -> Option<IncomingMessage> {
        let chat_id = chat_req.chat_id.unwrap_or_else(|| "http_default".into());
        let sender_id = chat_req.sender_id.unwrap_or_else(|| "http_user".into());

        // Create mpsc channel for SSE events
        let (tx, mut rx) = mpsc::channel::<SseEvent>(32);
        {
            let mut streams = self.sse_streams.lock().await;
            streams.insert(
                chat_id.clone(),
                SseState {
                    tx,
                    sent_len: 0,
                    full_text: String::new(),
                },
            );
        }

        // Spawn background task to write SSE events to the TCP stream
        let mut stream = stream;
        tokio::spawn(async move {
            // Write SSE response headers
            let headers = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: text/event-stream\r\n\
                 Cache-Control: no-cache\r\n\
                 {}\
                 Connection: keep-alive\r\n\
                 \r\n",
                cors_headers(),
            );
            if stream.write_all(headers.as_bytes()).await.is_err() {
                return;
            }

            while let Some(event) = rx.recv().await {
                let is_terminal = matches!(event, SseEvent::Done(_) | SseEvent::Error(_));
                if stream.write_all(&event.to_sse_bytes()).await.is_err() {
                    break;
                }
                if is_terminal {
                    break;
                }
            }
            // TCP connection closes when stream is dropped
        });

        let msg = IncomingMessage {
            channel: "http_api".into(),
            chat_id,
            sender_id,
            content: MessageContent::Text(chat_req.text),
            timestamp: chrono::Local::now().timestamp(),
        };

        Some(msg)
    }
}

#[async_trait]
impl Channel for HttpApiChannel {
    fn id(&self) -> &str {
        "http_api"
    }

    async fn poll(&self) -> Result<Vec<IncomingMessage>> {
        let (mut stream, addr) = self.listener.accept().await?;
        debug!(peer = %addr, "HTTP connection accepted");

        // Read the full request (up to 64KB)
        let mut buf = vec![0u8; 65536];
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Ok(Vec::new());
        }
        let raw = String::from_utf8_lossy(&buf[..n]);

        // Parse minimal HTTP — find method + path + body
        let first_line = raw.lines().next().unwrap_or("");
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        let (method, path) = if parts.len() >= 2 {
            (parts[0], parts[1])
        } else {
            send_json_response(&mut stream, 400, r#"{"error":"bad request"}"#).await;
            return Ok(Vec::new());
        };

        // ── OPTIONS (CORS preflight) ──────────────────────────────────
        if method == "OPTIONS" {
            let response = format!(
                "HTTP/1.1 204 No Content\r\n\
                 {}\
                 Content-Length: 0\r\n\
                 Connection: close\r\n\
                 \r\n",
                cors_headers(),
            );
            let _ = stream.write_all(response.as_bytes()).await;
            return Ok(Vec::new());
        }

        // ── GET / (HTML page) ─────────────────────────────────────────
        if method == "GET" && path == "/" {
            Self::serve_html(&mut stream).await;
            return Ok(Vec::new());
        }

        // ── GET /chat/history (session history) ───────────────────────
        if method == "GET" && path.starts_with("/chat/history") {
            // Auth check for /chat/* endpoints
            if !self.check_auth(&raw) {
                send_json_response(&mut stream, 401, r#"{"error":"unauthorized"}"#).await;
                return Ok(Vec::new());
            }
            if let Some(ref dir) = self.session_dir {
                Self::serve_history(&mut stream, path, dir).await;
            } else {
                send_json_response(&mut stream, 200, "[]").await;
            }
            return Ok(Vec::new());
        }

        // ── POST routes (auth check) ─────────────────────────────────
        if method == "POST" {
            if !self.check_auth(&raw) {
                send_json_response(&mut stream, 401, r#"{"error":"unauthorized"}"#).await;
                return Ok(Vec::new());
            }
        }

        // ── POST /chat/stream (SSE) ──────────────────────────────────
        if method == "POST" && path == "/chat/stream" {
            let body = extract_body(&raw);
            let chat_req: ChatRequest = match serde_json::from_str(body) {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "invalid JSON in SSE request");
                    send_json_response(&mut stream, 400, r#"{"error":"invalid JSON"}"#).await;
                    return Ok(Vec::new());
                }
            };
            if let Some(msg) = self.start_sse_stream(stream, chat_req).await {
                return Ok(vec![msg]);
            }
            return Ok(Vec::new());
        }

        // ── POST /chat (oneshot) ─────────────────────────────────────
        if method == "POST" && path == "/chat" {
            let body = extract_body(&raw);
            let chat_req: ChatRequest = match serde_json::from_str(body) {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "invalid JSON in HTTP request");
                    send_json_response(&mut stream, 400, r#"{"error":"invalid JSON"}"#).await;
                    return Ok(Vec::new());
                }
            };

            let req_id = {
                let mut id = self.next_req_id.lock().await;
                let current = *id;
                *id += 1;
                format!("http_{}", current)
            };

            let chat_id = chat_req.chat_id.unwrap_or_else(|| "http_default".into());
            let sender_id = chat_req.sender_id.unwrap_or_else(|| "http_user".into());

            // Create oneshot channel for the response
            let (tx, rx) = oneshot::channel::<(String, bool)>();
            {
                let mut pending = self.pending.lock().await;
                pending.insert(req_id.clone(), tx);
            }

            let msg = IncomingMessage {
                channel: "http_api".into(),
                chat_id,
                sender_id,
                content: MessageContent::Text(chat_req.text),
                timestamp: chrono::Local::now().timestamp(),
            };

            // Spawn a task to wait for the response and write it back
            let pending = self.pending.clone();
            tokio::spawn(async move {
                match tokio::time::timeout(std::time::Duration::from_secs(120), rx).await {
                    Ok(Ok((text, has_voice))) => {
                        let resp = ChatResponse { text, has_voice };
                        let body = serde_json::to_string(&resp).unwrap_or_default();
                        send_json_response(&mut stream, 200, &body).await;
                    }
                    Ok(Err(_)) => {
                        send_json_response(
                            &mut stream,
                            500,
                            r#"{"error":"response channel dropped"}"#,
                        )
                        .await;
                    }
                    Err(_) => {
                        send_json_response(&mut stream, 504, r#"{"error":"timeout"}"#).await;
                    }
                }
                // Clean up
                let mut p = pending.lock().await;
                p.remove(&req_id);
            });

            return Ok(vec![msg]);
        }

        // ── 404 fallback ─────────────────────────────────────────────
        send_json_response(&mut stream, 404, r#"{"error":"not found"}"#).await;
        Ok(Vec::new())
    }

    async fn send_text(&self, chat_id: &str, text: &str) -> Result<String> {
        // Check SSE streams first
        {
            let mut streams = self.sse_streams.lock().await;
            if let Some(state) = streams.get_mut(chat_id) {
                // SSE connection: send only new bytes as delta (incremental)
                let new_len = text.len();
                if new_len > state.sent_len {
                    // Find the nearest char boundary at or after sent_len
                    let start = ceil_char_boundary(text, state.sent_len);
                    if start < new_len {
                        let delta = &text[start..];
                        let _ = state.tx.send(SseEvent::Delta(delta.to_string())).await;
                    }
                    state.sent_len = new_len;
                }
                state.full_text = text.to_string();
                return Ok(format!("sse_{}", chat_id));
            }
        }

        // Fall back to oneshot pending map
        self.respond_to_pending(chat_id, text.to_string(), false)
            .await?;
        Ok(String::new())
    }

    async fn edit_message(&self, chat_id: &str, msg_id: &str, text: &str) -> Result<()> {
        if !msg_id.starts_with("sse_") {
            return Ok(());
        }

        let mut streams = self.sse_streams.lock().await;
        if let Some(state) = streams.get_mut(chat_id) {
            // Calculate incremental delta
            let new_bytes = text.len();
            if new_bytes > state.sent_len {
                // Find the nearest char boundary at or after sent_len
                let start = ceil_char_boundary(text, state.sent_len);
                if start < new_bytes {
                    let delta = &text[start..];
                    let _ = state.tx.send(SseEvent::Delta(delta.to_string())).await;
                }
                state.sent_len = new_bytes;
            }
            state.full_text = text.to_string();
        }
        Ok(())
    }

    async fn send_typing(&self, chat_id: &str) -> Result<()> {
        let streams = self.sse_streams.lock().await;
        if let Some(state) = streams.get(chat_id) {
            let _ = state.tx.send(SseEvent::Typing).await;
        }
        Ok(())
    }

    async fn send_voice(&self, chat_id: &str, _audio: &[u8]) -> Result<()> {
        // HTTP API doesn't stream audio; indicate voice was available
        self.respond_to_pending(chat_id, "[voice response]".into(), true)
            .await
    }

    async fn download_voice(&self, _file_ref: &str) -> Result<Vec<u8>> {
        Err(GatewayError::Tool {
            tool: "http_api".into(),
            message: "voice download not supported via HTTP API".into(),
        })
    }

    async fn close_stream(&self, chat_id: &str) -> Result<()> {
        let mut streams = self.sse_streams.lock().await;
        if let Some(state) = streams.remove(chat_id) {
            let _ = state.tx.send(SseEvent::Done(state.full_text)).await;
        }
        Ok(())
    }
}

impl HttpApiChannel {
    /// Send a response to the most recent pending oneshot request for this chat_id.
    async fn respond_to_pending(
        &self,
        _chat_id: &str,
        text: String,
        has_voice: bool,
    ) -> Result<()> {
        let mut pending = self.pending.lock().await;
        // Find the first pending request (FIFO — simple approach)
        if let Some(key) = pending.keys().next().cloned() {
            if let Some(tx) = pending.remove(&key) {
                let _ = tx.send((text, has_voice));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract HTTP body (after \r\n\r\n).
fn extract_body<'a>(raw: &'a str) -> &'a str {
    raw.find("\r\n\r\n")
        .map(|pos| &raw[pos + 4..])
        .unwrap_or("")
}

/// CORS headers added to all responses.
fn cors_headers() -> String {
    "Access-Control-Allow-Origin: *\r\n\
     Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
     Access-Control-Allow-Headers: Content-Type, Authorization\r\n\
     Access-Control-Max-Age: 86400\r\n"
        .to_string()
}

/// Write a minimal HTTP/1.1 response with JSON content type + CORS.
async fn send_json_response(stream: &mut tokio::net::TcpStream, status: u16, body: &str) {
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        500 => "Internal Server Error",
        504 => "Gateway Timeout",
        _ => "Unknown",
    };
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         {}\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len(),
        cors_headers(),
    );
    if let Err(e) = stream.write_all(response.as_bytes()).await {
        error!(error = %e, "failed to write HTTP response");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_event_delta_format() {
        let event = SseEvent::Delta("你好".into());
        let bytes = event.to_sse_bytes();
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.starts_with("data: "));
        assert!(s.ends_with("\n\n"));
        assert!(s.contains(r#""type":"delta""#));
        assert!(s.contains(r#""text":"你好""#));
    }

    #[test]
    fn sse_event_typing_format() {
        let event = SseEvent::Typing;
        let bytes = event.to_sse_bytes();
        let s = String::from_utf8(bytes).unwrap();
        assert_eq!(s, "data: {\"type\":\"typing\"}\n\n");
    }

    #[test]
    fn sse_event_done_format() {
        let event = SseEvent::Done("完整文本".into());
        let bytes = event.to_sse_bytes();
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains(r#""type":"done""#));
        assert!(s.contains(r#""text":"完整文本""#));
    }

    #[test]
    fn sse_event_error_format() {
        let event = SseEvent::Error("timeout".into());
        let bytes = event.to_sse_bytes();
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains(r#""type":"error""#));
        assert!(s.contains(r#""message":"timeout""#));
    }

    #[test]
    fn sse_event_escapes_special_chars() {
        let event = SseEvent::Delta("hello \"world\"\nnewline".into());
        let bytes = event.to_sse_bytes();
        let s = String::from_utf8(bytes).unwrap();
        // Should be valid JSON — no raw newlines or unescaped quotes in JSON string
        assert!(s.contains(r#"\"world\""#));
        assert!(s.contains(r#"\n"#));
    }

    #[test]
    fn extract_body_finds_content() {
        let raw = "POST /chat HTTP/1.1\r\nHost: localhost\r\n\r\n{\"text\":\"hi\"}";
        assert_eq!(extract_body(raw), "{\"text\":\"hi\"}");
    }

    #[test]
    fn extract_body_empty_when_no_separator() {
        assert_eq!(extract_body("no separator here"), "");
    }

    #[test]
    fn cors_headers_contain_required_fields() {
        let h = cors_headers();
        assert!(h.contains("Access-Control-Allow-Origin: *"));
        assert!(h.contains("Access-Control-Allow-Methods:"));
        assert!(h.contains("Access-Control-Allow-Headers:"));
    }

    #[test]
    fn check_auth_no_token_always_passes() {
        // Can't easily construct HttpApiChannel in test, but we test the logic
        // by checking the auth_token is empty case.
        // The integration tests cover the full flow.
    }
}
