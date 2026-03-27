//! ElevenLabs TTS provider.
//!
//! Sends a POST request to `/text-to-speech/{voice_id}` and receives raw
//! audio bytes.

use async_trait::async_trait;
use serde_json::json;
use tracing::debug;

use super::{AudioFormat, TtsProvider};
use crate::config::ElevenLabsTtsConfig;
use crate::error::{GatewayError, Result};

/// ElevenLabs TTS provider using the Text-to-Speech API.
#[derive(Debug)]
pub struct ElevenLabsTtsProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model_id: String,
    voice_id: String,
}

impl ElevenLabsTtsProvider {
    pub fn new(client: reqwest::Client, config: &ElevenLabsTtsConfig) -> Result<Self> {
        let api_key = std::env::var(&config.api_key_env).map_err(|_| {
            GatewayError::Config(format!(
                "environment variable {} is not set (required by ElevenLabs TTS)",
                config.api_key_env
            ))
        })?;

        if config.voice_id.is_empty() {
            return Err(GatewayError::Config(
                "ElevenLabs TTS requires a voice_id to be configured".into(),
            ));
        }

        let base_url = config
            .base_url
            .clone()
            .unwrap_or_else(|| "https://api.elevenlabs.io/v1".into());

        Ok(Self {
            client,
            base_url,
            api_key,
            model_id: config.model_id.clone(),
            voice_id: config.voice_id.clone(),
        })
    }
}

#[async_trait]
impl TtsProvider for ElevenLabsTtsProvider {
    async fn synthesize(&self, text: &str, _format: AudioFormat) -> Result<Vec<u8>> {
        if text.is_empty() {
            return Ok(Vec::new());
        }

        debug!(model_id = %self.model_id, voice_id = %self.voice_id, text_len = text.len(), "synthesizing speech via ElevenLabs");

        let url = format!("{}/text-to-speech/{}", self.base_url, self.voice_id);
        let body = json!({
            "text": text,
            "model_id": self.model_id
        });

        let resp = self
            .client
            .post(&url)
            .header("xi-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::Tts(format!("ElevenLabs TTS request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::Tts(format!(
                "ElevenLabs TTS returned {}: {}",
                status, err_body
            )));
        }

        let audio = resp
            .bytes()
            .await
            .map_err(|e| GatewayError::Tts(format!("failed to read audio response: {}", e)))?;

        debug!(audio_bytes = audio.len(), "ElevenLabs TTS synthesis complete");
        Ok(audio.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_requires_api_key_env() {
        let config = ElevenLabsTtsConfig {
            base_url: None,
            api_key_env: "_UNSET_ELEVENLABS_KEY_12345".into(),
            model_id: "eleven_multilingual_v2".into(),
            voice_id: "test-voice".into(),
        };
        let client = reqwest::Client::new();
        let err = ElevenLabsTtsProvider::new(client, &config).unwrap_err();
        match err {
            GatewayError::Config(msg) => {
                assert!(msg.contains("_UNSET_ELEVENLABS_KEY_12345"));
            }
            other => panic!("expected Config error, got: {other:?}"),
        }
    }

    #[test]
    fn new_requires_voice_id() {
        std::env::set_var("_TEST_ELEVENLABS_KEY", "test-key");
        let config = ElevenLabsTtsConfig {
            base_url: None,
            api_key_env: "_TEST_ELEVENLABS_KEY".into(),
            model_id: "eleven_multilingual_v2".into(),
            voice_id: String::new(), // empty!
        };
        let client = reqwest::Client::new();
        let err = ElevenLabsTtsProvider::new(client, &config).unwrap_err();
        match err {
            GatewayError::Config(msg) => {
                assert!(msg.contains("voice_id"));
            }
            other => panic!("expected Config error, got: {other:?}"),
        }
        std::env::remove_var("_TEST_ELEVENLABS_KEY");
    }

    #[test]
    fn new_with_valid_config() {
        std::env::set_var("_TEST_ELEVENLABS_KEY2", "test-key");
        let config = ElevenLabsTtsConfig {
            base_url: Some("https://custom.api.com/v1".into()),
            api_key_env: "_TEST_ELEVENLABS_KEY2".into(),
            model_id: "eleven_turbo_v2_5".into(),
            voice_id: "abc123".into(),
        };
        let client = reqwest::Client::new();
        let provider = ElevenLabsTtsProvider::new(client, &config).unwrap();
        assert_eq!(provider.base_url, "https://custom.api.com/v1");
        assert_eq!(provider.model_id, "eleven_turbo_v2_5");
        assert_eq!(provider.voice_id, "abc123");
        std::env::remove_var("_TEST_ELEVENLABS_KEY2");
    }

    #[tokio::test]
    async fn synthesize_empty_text_returns_empty() {
        std::env::set_var("_TEST_ELEVENLABS_KEY3", "test-key");
        let config = ElevenLabsTtsConfig {
            base_url: None,
            api_key_env: "_TEST_ELEVENLABS_KEY3".into(),
            model_id: "eleven_multilingual_v2".into(),
            voice_id: "test-voice".into(),
        };
        let client = reqwest::Client::new();
        let provider = ElevenLabsTtsProvider::new(client, &config).unwrap();
        let result = provider.synthesize("", AudioFormat::default()).await.unwrap();
        assert!(result.is_empty());
        std::env::remove_var("_TEST_ELEVENLABS_KEY3");
    }
}
