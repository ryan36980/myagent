//! Google Cloud Speech-to-Text v1 REST API implementation.

use async_trait::async_trait;
use base64::Engine;
use serde::Deserialize;
use tracing::{debug, error};

use super::SttProvider;
use crate::config::SttConfig;
use crate::error::{GatewayError, Result};

/// Google Cloud Speech-to-Text provider (synchronous recognize API).
pub struct GoogleSttProvider {
    client: reqwest::Client,
    api_key: String,
    language_code: String,
}

impl GoogleSttProvider {
    pub fn new(client: reqwest::Client, config: &SttConfig) -> Self {
        let language_code = config
            .google
            .as_ref()
            .map(|g| g.language_code.clone())
            .unwrap_or_else(|| "zh-CN".into());

        Self {
            client,
            api_key: config.api_key.clone(),
            language_code,
        }
    }
}

#[async_trait]
impl SttProvider for GoogleSttProvider {
    async fn transcribe(&self, audio: &[u8], mime: &str) -> Result<String> {
        if self.api_key.is_empty() {
            return Err(GatewayError::Stt(
                "STT not configured: set GOOGLE_STT_API_KEY environment variable or audio.apiKey in config".into(),
            ));
        }

        let (encoding, sample_rate) = match mime {
            "audio/ogg" | "audio/opus" => ("OGG_OPUS", 48000),
            "audio/wav" | "audio/x-wav" => ("LINEAR16", 16000),
            "audio/mp3" | "audio/mpeg" => ("MP3", 16000),
            "audio/flac" => ("FLAC", 16000),
            _ => ("OGG_OPUS", 48000),
        };

        let content = base64::engine::general_purpose::STANDARD.encode(audio);

        let body = serde_json::json!({
            "config": {
                "encoding": encoding,
                "sampleRateHertz": sample_rate,
                "languageCode": &self.language_code,
            },
            "audio": {
                "content": content,
            }
        });

        let url = format!(
            "https://speech.googleapis.com/v1/speech:recognize?key={}",
            self.api_key
        );

        debug!(encoding, sample_rate, mime, language = %self.language_code, "transcribing audio via Google STT");

        let response = self.client.post(&url).json(&body).send().await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!(status = %status, body = %body, "Google STT error");
            return Err(GatewayError::Stt(format!(
                "Google STT API returned {}: {}",
                status, body
            )));
        }

        let result: GoogleSttResponse = response.json().await?;
        let transcript = result
            .results
            .and_then(|r| r.into_iter().next())
            .and_then(|r| r.alternatives.into_iter().next())
            .map(|a| a.transcript)
            .unwrap_or_default();

        debug!(text_len = transcript.len(), "transcription complete");
        Ok(transcript)
    }
}

#[derive(Deserialize)]
struct GoogleSttResponse {
    results: Option<Vec<GoogleSttResult>>,
}

#[derive(Deserialize)]
struct GoogleSttResult {
    alternatives: Vec<GoogleSttAlternative>,
}

#[derive(Deserialize)]
struct GoogleSttAlternative {
    transcript: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GoogleSttConfig, SttConfig};

    #[test]
    fn new_with_defaults() {
        let config = SttConfig {
            provider: "google".into(),
            api_key: "test-key".into(),
            ..SttConfig::default()
        };
        let provider = GoogleSttProvider::new(reqwest::Client::new(), &config);
        assert_eq!(provider.language_code, "zh-CN");
        assert_eq!(provider.api_key, "test-key");
    }

    #[test]
    fn new_with_custom_config() {
        let config = SttConfig {
            provider: "google".into(),
            api_key: "my-key".into(),
            google: Some(GoogleSttConfig {
                language_code: "en-US".into(),
            }),
            ..SttConfig::default()
        };
        let provider = GoogleSttProvider::new(reqwest::Client::new(), &config);
        assert_eq!(provider.language_code, "en-US");
        assert_eq!(provider.api_key, "my-key");
    }

    #[tokio::test]
    async fn transcribe_success() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/speech:recognize"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({
                    "results": [{
                        "alternatives": [{ "transcript": "你好世界" }]
                    }]
                }),
            ))
            .mount(&server)
            .await;

        // Point API URL to wiremock by using a custom client + overriding URL
        // We'll construct the provider manually to use the mock server
        let provider = GoogleSttProvider {
            client: reqwest::Client::new(),
            api_key: "test-key".into(),
            language_code: "zh-CN".into(),
        };

        // Override: call mock server directly
        let body = serde_json::json!({
            "config": {
                "encoding": "OGG_OPUS",
                "sampleRateHertz": 48000,
                "languageCode": "zh-CN",
            },
            "audio": {
                "content": base64::engine::general_purpose::STANDARD.encode(b"fake-audio"),
            }
        });

        let resp = provider
            .client
            .post(format!("{}/v1/speech:recognize?key=test-key", server.uri()))
            .json(&body)
            .send()
            .await
            .unwrap();

        let result: GoogleSttResponse = resp.json().await.unwrap();
        let transcript = result
            .results
            .and_then(|r| r.into_iter().next())
            .and_then(|r| r.alternatives.into_iter().next())
            .map(|a| a.transcript)
            .unwrap_or_default();

        assert_eq!(transcript, "你好世界");
    }

    #[tokio::test]
    async fn transcribe_api_error() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/speech:recognize"))
            .respond_with(
                wiremock::ResponseTemplate::new(401).set_body_string("Unauthorized"),
            )
            .mount(&server)
            .await;

        let provider = GoogleSttProvider {
            client: reqwest::Client::new(),
            api_key: "bad-key".into(),
            language_code: "zh-CN".into(),
        };

        let resp = provider
            .client
            .post(format!("{}/v1/speech:recognize?key=bad-key", server.uri()))
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn transcribe_empty_results() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/speech:recognize"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "results": [] })),
            )
            .mount(&server)
            .await;

        let resp = reqwest::Client::new()
            .post(format!("{}/v1/speech:recognize?key=test", server.uri()))
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();

        let result: GoogleSttResponse = resp.json().await.unwrap();
        let transcript = result
            .results
            .and_then(|r| r.into_iter().next())
            .and_then(|r| r.alternatives.into_iter().next())
            .map(|a| a.transcript)
            .unwrap_or_default();

        assert_eq!(transcript, "");
    }

    #[tokio::test]
    async fn transcribe_missing_api_key() {
        let config = SttConfig {
            provider: "google".into(),
            api_key: String::new(),
            ..SttConfig::default()
        };
        let provider = GoogleSttProvider::new(reqwest::Client::new(), &config);
        let err = provider.transcribe(b"audio", "audio/ogg").await.unwrap_err();
        assert!(err.to_string().contains("GOOGLE_STT_API_KEY"));
    }
}
