//! Groq Whisper STT implementation (OpenAI-compatible API).

use async_trait::async_trait;
use serde::Deserialize;
use tracing::{debug, error};

use super::SttProvider;
use crate::config::SttConfig;
use crate::error::{GatewayError, Result};

/// Groq Whisper speech-to-text provider.
pub struct GroqSttProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl GroqSttProvider {
    pub fn new(client: reqwest::Client, config: &SttConfig) -> Self {
        Self {
            client,
            api_key: config.api_key.clone(),
            model: config
                .model
                .clone()
                .unwrap_or_else(|| "whisper-large-v3-turbo".into()),
            base_url: config
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.groq.com/openai/v1".into()),
        }
    }
}

#[async_trait]
impl SttProvider for GroqSttProvider {
    async fn transcribe(&self, audio: &[u8], mime: &str) -> Result<String> {
        if self.api_key.is_empty() {
            return Err(GatewayError::Stt(
                "STT not configured: set GROQ_API_KEY environment variable or audio.apiKey in config".into(),
            ));
        }

        let extension = match mime {
            "audio/ogg" | "audio/opus" => "ogg",
            "audio/mpeg" | "audio/mp3" => "mp3",
            "audio/wav" | "audio/x-wav" => "wav",
            _ => "ogg",
        };

        let filename = format!("audio.{}", extension);

        let file_part = reqwest::multipart::Part::bytes(audio.to_vec())
            .file_name(filename)
            .mime_str(mime)
            .map_err(|e| GatewayError::Stt(e.to_string()))?;

        let form = reqwest::multipart::Form::new()
            .part("file", file_part)
            .text("model", self.model.clone())
            .text("response_format", "json");

        let url = format!("{}/audio/transcriptions", self.base_url);

        debug!(model = %self.model, mime = %mime, "transcribing audio");

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .multipart(form)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!(status = %status, body = %body, "Groq STT error");
            return Err(GatewayError::Stt(format!(
                "Groq API returned {}: {}",
                status, body
            )));
        }

        let result: WhisperResponse = response.json().await?;
        debug!(text_len = result.text.len(), "transcription complete");

        Ok(result.text)
    }
}

#[derive(Deserialize)]
struct WhisperResponse {
    text: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SttConfig;

    #[test]
    fn new_with_defaults() {
        let config = SttConfig::default();
        let provider = GroqSttProvider::new(reqwest::Client::new(), &config);
        assert_eq!(provider.model, "whisper-large-v3-turbo");
        assert_eq!(provider.base_url, "https://api.groq.com/openai/v1");
    }

    #[test]
    fn new_with_custom_config() {
        let config = SttConfig {
            provider: "groq".into(),
            model: Some("whisper-large-v3".into()),
            api_key: "sk-test".into(),
            base_url: Some("https://my-proxy.com/v1".into()),
            volcengine: None,
            google: None,
        };
        let provider = GroqSttProvider::new(reqwest::Client::new(), &config);
        assert_eq!(provider.model, "whisper-large-v3");
        assert_eq!(provider.base_url, "https://my-proxy.com/v1");
        assert_eq!(provider.api_key, "sk-test");
    }

    #[tokio::test]
    async fn transcribe_success() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/audio/transcriptions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"text": "hello world"})),
            )
            .mount(&server)
            .await;

        let config = SttConfig {
            provider: "groq".into(),
            model: Some("whisper-large-v3-turbo".into()),
            api_key: "test-key".into(),
            base_url: Some(server.uri()),
            volcengine: None,
            google: None,
        };
        let provider = GroqSttProvider::new(reqwest::Client::new(), &config);
        let result = provider.transcribe(b"fake-audio", "audio/ogg").await.unwrap();
        assert_eq!(result, "hello world");
    }

    #[tokio::test]
    async fn transcribe_api_error() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/audio/transcriptions"))
            .respond_with(
                wiremock::ResponseTemplate::new(401).set_body_string("Unauthorized"),
            )
            .mount(&server)
            .await;

        let config = SttConfig {
            provider: "groq".into(),
            model: Some("whisper-large-v3-turbo".into()),
            api_key: "bad-key".into(),
            base_url: Some(server.uri()),
            volcengine: None,
            google: None,
        };
        let provider = GroqSttProvider::new(reqwest::Client::new(), &config);
        let err = provider.transcribe(b"fake-audio", "audio/ogg").await.unwrap_err();
        assert!(err.to_string().contains("401"));
    }
}
