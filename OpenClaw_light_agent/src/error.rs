//! Unified error type for OpenClaw Light.
//!
//! Every module in the gateway propagates errors through [`GatewayError`],
//! which covers transport, serialisation, configuration, and domain-specific
//! failure modes.  The companion [`Result<T>`] alias keeps call sites concise.

use thiserror::Error;

/// All errors that can occur inside OpenClaw Light.
///
/// Variants that wrap external error types (`reqwest`, `tungstenite`,
/// `serde_json`, `std::io`) provide automatic `From` conversions so the `?`
/// operator works transparently.  Domain variants (`Config`, `Agent`, `Stt`,
/// `Tts`, `Tool`, `Session`, `Telegram`) carry a human-readable message and
/// are constructed explicitly in the relevant modules.
#[derive(Error, Debug)]
pub enum GatewayError {
    /// HTTP / network transport error originating from `reqwest`.
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// Configuration loading or validation error.
    #[error("config error: {0}")]
    Config(String),

    /// Error inside the agentic tool-use loop.
    #[error("agent error: {0}")]
    Agent(String),

    /// Speech-to-text transcription error.
    #[error("stt error: {0}")]
    Stt(String),

    /// Text-to-speech synthesis error.
    #[error("tts error: {0}")]
    Tts(String),

    /// A tool invocation failed.  Both the tool name and a descriptive message
    /// are preserved so callers can decide whether to retry or surface the
    /// error to the user.
    #[error("tool error: {tool}: {message}")]
    Tool {
        /// Name of the tool that failed (e.g. `"ha_control"`).
        tool: String,
        /// Human-readable description of what went wrong.
        message: String,
    },

    /// Session persistence or retrieval error.
    #[error("session error: {0}")]
    Session(String),

    /// Telegram Bot API error.
    #[error("telegram error: {0}")]
    Telegram(String),

    /// Feishu/Lark API error.
    #[error("feishu error: {0}")]
    Feishu(String),

    /// WebSocket transport error originating from `tungstenite`.
    #[error("websocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    /// JSON serialisation / deserialisation error.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// Filesystem or other I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenience alias used throughout the gateway crate.
pub type Result<T> = std::result::Result<T, GatewayError>;
