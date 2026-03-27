//! Speech-to-text provider trait and implementations.

pub mod google;
pub mod groq;
pub mod volcengine;

use async_trait::async_trait;

use crate::error::Result;

/// Trait for speech-to-text providers (Groq Whisper, OpenAI, etc.).
///
/// Adding a new STT provider requires implementing this trait in ~30 lines.
#[async_trait]
pub trait SttProvider: Send + Sync {
    /// Transcribe audio bytes to text.
    async fn transcribe(&self, audio: &[u8], mime: &str) -> Result<String>;
}
