//! OpenAI-compatible TTS provider.
//!
//! Sends a POST request to `/audio/speech` and receives raw audio bytes.
//! Works with OpenAI, Azure, and any OpenAI-compatible TTS endpoint.

use async_trait::async_trait;
use serde_json::json;
use tracing::debug;

use super::{AudioFormat, TtsProvider};
use crate::config::OpenAiTtsConfig;
use crate::error::{GatewayError, Result};

/// OpenAI TTS provider using the Audio Speech API.
#[derive(Debug)]
pub struct OpenAiTtsProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    voice: String,
}

impl OpenAiTtsProvider {
    pub fn new(client: reqwest::Client, config: &OpenAiTtsConfig) -> Result<Self> {
        let api_key = std::env::var(&config.api_key_env).map_err(|_| {
            GatewayError::Config(format!(
                "environment variable {} is not set (required by OpenAI TTS)",
                config.api_key_env
            ))
        })?;

        let base_url = config
            .base_url
            .clone()
            .unwrap_or_else(|| "https://api.openai.com/v1".into());

        Ok(Self {
            client,
            base_url,
            api_key,
            model: config.model.clone(),
            voice: config.voice.clone(),
        })
    }
}

#[async_trait]
impl TtsProvider for OpenAiTtsProvider {
    async fn synthesize(&self, text: &str, _format: AudioFormat) -> Result<Vec<u8>> {
        if text.is_empty() {
            return Ok(Vec::new());
        }

        debug!(model = %self.model, voice = %self.voice, text_len = text.len(), "synthesizing speech via OpenAI TTS");

        let url = format!("{}/audio/speech", self.base_url);
        let body = json!({
            "model": self.model,
            "input": text,
            "voice": self.voice,
            "response_format": "opus"
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::Tts(format!("OpenAI TTS request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::Tts(format!(
                "OpenAI TTS returned {}: {}",
                status, err_body
            )));
        }

        let audio = resp
            .bytes()
            .await
            .map_err(|e| GatewayError::Tts(format!("failed to read audio response: {}", e)))?;

        debug!(audio_bytes = audio.len(), "OpenAI TTS synthesis complete");
        Ok(audio.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_requires_api_key_env() {
        let config = OpenAiTtsConfig {
            base_url: None,
            api_key_env: "_UNSET_OPENAI_TTS_KEY_12345".into(),
            model: "tts-1".into(),
            voice: "alloy".into(),
        };
        let client = reqwest::Client::new();
        let err = OpenAiTtsProvider::new(client, &config).unwrap_err();
        match err {
            GatewayError::Config(msg) => {
                assert!(msg.contains("_UNSET_OPENAI_TTS_KEY_12345"));
            }
            other => panic!("expected Config error, got: {other:?}"),
        }
    }

    #[test]
    fn new_with_defaults() {
        std::env::set_var("_TEST_OPENAI_TTS_KEY", "sk-test-123");
        let config = OpenAiTtsConfig::default();
        let config = OpenAiTtsConfig {
            api_key_env: "_TEST_OPENAI_TTS_KEY".into(),
            ..config
        };
        let client = reqwest::Client::new();
        let provider = OpenAiTtsProvider::new(client, &config).unwrap();
        assert_eq!(provider.base_url, "https://api.openai.com/v1");
        assert_eq!(provider.model, "tts-1");
        assert_eq!(provider.voice, "alloy");
        std::env::remove_var("_TEST_OPENAI_TTS_KEY");
    }

    #[test]
    fn new_with_custom_base_url() {
        std::env::set_var("_TEST_OPENAI_TTS_KEY2", "sk-test");
        let config = OpenAiTtsConfig {
            base_url: Some("https://my-proxy.example.com/v1".into()),
            api_key_env: "_TEST_OPENAI_TTS_KEY2".into(),
            model: "tts-1-hd".into(),
            voice: "nova".into(),
        };
        let client = reqwest::Client::new();
        let provider = OpenAiTtsProvider::new(client, &config).unwrap();
        assert_eq!(provider.base_url, "https://my-proxy.example.com/v1");
        assert_eq!(provider.model, "tts-1-hd");
        assert_eq!(provider.voice, "nova");
        std::env::remove_var("_TEST_OPENAI_TTS_KEY2");
    }

    #[tokio::test]
    async fn synthesize_empty_text_returns_empty() {
        std::env::set_var("_TEST_OPENAI_TTS_KEY3", "sk-test");
        let config = OpenAiTtsConfig {
            api_key_env: "_TEST_OPENAI_TTS_KEY3".into(),
            ..OpenAiTtsConfig::default()
        };
        let client = reqwest::Client::new();
        let provider = OpenAiTtsProvider::new(client, &config).unwrap();
        let result = provider.synthesize("", AudioFormat::default()).await.unwrap();
        assert!(result.is_empty());
        std::env::remove_var("_TEST_OPENAI_TTS_KEY3");
    }
}
