//! AI provider abstraction layer.
//!
//! Isolates API-specific details behind traits so the agent core
//! never directly depends on any particular provider.

pub mod llm;
pub mod stt;
pub mod tts;
