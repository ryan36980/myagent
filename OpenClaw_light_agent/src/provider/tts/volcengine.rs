//! Volcengine (火山引擎/豆包) TTS provider.
//!
//! Sends a POST request to the TTS HTTP REST API and receives base64-encoded
//! OGG/Opus audio.  Native `ogg_opus` encoding — no container conversion needed.

use async_trait::async_trait;
use serde::Deserialize;
use tracing::debug;

use super::{AudioFormat, TtsProvider};
use crate::config::VolcengineTtsConfig;
use crate::error::{GatewayError, Result};

const DEFAULT_BASE_URL: &str = "https://openspeech.bytedance.com";
const DEFAULT_CLUSTER: &str = "volcano_tts";
const DEFAULT_VOICE: &str = "zh_female_vv_uranus_bigtts";

/// Volcengine TTS provider using the HTTP REST API.
#[derive(Debug)]
pub struct VolcengineTtsProvider {
    client: reqwest::Client,
    base_url: String,
    app_id: String,
    access_token: String,
    cluster: String,
    voice_type: String,
    speed_ratio: f32,
    volume_ratio: f32,
    pitch_ratio: f32,
}

impl VolcengineTtsProvider {
    pub fn new(client: reqwest::Client, config: &VolcengineTtsConfig) -> Result<Self> {
        if config.app_id.is_empty() {
            return Err(GatewayError::Config(
                "Volcengine TTS requires appId to be configured".into(),
            ));
        }
        if config.access_token.is_empty() {
            return Err(GatewayError::Config(
                "Volcengine TTS requires accessToken to be configured".into(),
            ));
        }
        let access_token = config.access_token.clone();

        let base_url = config
            .base_url
            .clone()
            .unwrap_or_else(|| DEFAULT_BASE_URL.into());
        let cluster = if config.cluster.is_empty() {
            DEFAULT_CLUSTER.into()
        } else {
            config.cluster.clone()
        };
        let voice_type = if config.voice_type.is_empty() {
            DEFAULT_VOICE.into()
        } else {
            config.voice_type.clone()
        };

        Ok(Self {
            client,
            base_url,
            app_id: config.app_id.clone(),
            access_token,
            cluster,
            voice_type,
            speed_ratio: config.speed_ratio.unwrap_or(1.0),
            volume_ratio: config.volume_ratio.unwrap_or(1.0),
            pitch_ratio: config.pitch_ratio.unwrap_or(1.0),
        })
    }
}

/// Response from the Volcengine TTS API.
#[derive(Deserialize)]
struct TtsResponse {
    code: i32,
    message: String,
    #[serde(default)]
    data: Option<String>, // base64-encoded audio
}

#[async_trait]
impl TtsProvider for VolcengineTtsProvider {
    async fn synthesize(&self, text: &str, _format: AudioFormat) -> Result<Vec<u8>> {
        if text.is_empty() {
            return Ok(Vec::new());
        }

        debug!(voice = %self.voice_type, text_len = text.len(), "synthesizing speech via Volcengine TTS");

        let url = format!("{}/api/v1/tts", self.base_url);
        let reqid = uuid::Uuid::new_v4().to_string();

        let body = serde_json::json!({
            "app": {
                "appid": self.app_id,
                "token": self.access_token,
                "cluster": self.cluster,
            },
            "user": {
                "uid": "openclaw"
            },
            "audio": {
                "voice_type": self.voice_type,
                "encoding": "ogg_opus",
                "speed_ratio": self.speed_ratio,
                "volume_ratio": self.volume_ratio,
                "pitch_ratio": self.pitch_ratio,
            },
            "request": {
                "reqid": reqid,
                "text": text,
                "text_type": "plain",
                "operation": "query",
            }
        });

        // Note: Volcengine uses "Bearer;" (semicolon) not "Bearer " (space)
        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer;{}", self.access_token))
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::Tts(format!("Volcengine TTS request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::Tts(format!(
                "Volcengine TTS returned {}: {}",
                status, err_body
            )));
        }

        let tts_resp: TtsResponse = resp
            .json()
            .await
            .map_err(|e| GatewayError::Tts(format!("failed to parse TTS response: {}", e)))?;

        if tts_resp.code != 3000 {
            return Err(GatewayError::Tts(format!(
                "Volcengine TTS error ({}): {}",
                tts_resp.code, tts_resp.message
            )));
        }

        let b64_data = tts_resp
            .data
            .ok_or_else(|| GatewayError::Tts("Volcengine TTS returned no audio data".into()))?;

        use base64::Engine;
        let audio = base64::engine::general_purpose::STANDARD
            .decode(&b64_data)
            .map_err(|e| GatewayError::Tts(format!("failed to decode base64 audio: {}", e)))?;

        debug!(audio_bytes = audio.len(), "Volcengine TTS synthesis complete");
        Ok(audio)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_requires_app_id() {
        let config = VolcengineTtsConfig::default();
        let client = reqwest::Client::new();
        let err = VolcengineTtsProvider::new(client, &config).unwrap_err();
        match err {
            GatewayError::Config(msg) => assert!(msg.contains("appId")),
            other => panic!("expected Config error, got: {other:?}"),
        }
    }

    #[test]
    fn new_requires_access_token() {
        let config = VolcengineTtsConfig {
            app_id: "test-app".into(),
            ..VolcengineTtsConfig::default()
        };
        let client = reqwest::Client::new();
        let err = VolcengineTtsProvider::new(client, &config).unwrap_err();
        match err {
            GatewayError::Config(msg) => assert!(msg.contains("accessToken")),
            other => panic!("expected Config error, got: {other:?}"),
        }
    }

    #[test]
    fn new_with_valid_config() {
        let config = VolcengineTtsConfig {
            app_id: "test-app".into(),
            access_token: "test-token".into(),
            base_url: Some("https://custom.api.com".into()),
            cluster: "custom_cluster".into(),
            voice_type: "BV700_streaming".into(),
            speed_ratio: Some(1.2),
            volume_ratio: Some(0.8),
            pitch_ratio: None,
        };
        let client = reqwest::Client::new();
        let provider = VolcengineTtsProvider::new(client, &config).unwrap();
        assert_eq!(provider.base_url, "https://custom.api.com");
        assert_eq!(provider.cluster, "custom_cluster");
        assert_eq!(provider.voice_type, "BV700_streaming");
        assert_eq!(provider.speed_ratio, 1.2);
        assert_eq!(provider.volume_ratio, 0.8);
        assert_eq!(provider.pitch_ratio, 1.0); // default
    }

    #[tokio::test]
    async fn synthesize_empty_text_returns_empty() {
        let config = VolcengineTtsConfig {
            app_id: "test-app".into(),
            access_token: "test-token".into(),
            ..VolcengineTtsConfig::default()
        };
        let client = reqwest::Client::new();
        let provider = VolcengineTtsProvider::new(client, &config).unwrap();
        let result = provider
            .synthesize("", AudioFormat::default())
            .await
            .unwrap();
        assert!(result.is_empty());
    }
}
