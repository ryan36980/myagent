//! Text-to-speech provider trait and implementations.

pub mod edge;
pub mod elevenlabs;
pub mod openai;
pub mod volcengine;
pub mod webm_to_ogg;

use async_trait::async_trait;

use crate::error::Result;

/// Preferred audio output format for TTS synthesis.
///
/// Each channel declares its preferred format via `Channel::preferred_audio_format()`.
/// TTS providers that support format selection use this hint; others fall back to
/// their native format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    /// MP3 (MPEG Layer 3) — widely supported, used by Telegram.
    Mp3,
    /// OGG/Opus — smaller size, better quality at low bitrate, used by Feishu.
    OggOpus,
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self::OggOpus
    }
}

/// Trait for text-to-speech providers (Edge TTS, OpenAI, ElevenLabs, etc.).
///
/// Adding a new TTS provider requires implementing this trait in ~30 lines.
#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// Synthesize text into audio bytes.
    ///
    /// The `format` hint indicates the channel's preferred audio format.
    /// Providers that support format selection should honor it; others may
    /// ignore it and return their native format.
    async fn synthesize(&self, text: &str, format: AudioFormat) -> Result<Vec<u8>>;
}
