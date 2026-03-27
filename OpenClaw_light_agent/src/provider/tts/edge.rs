//! Edge TTS (Microsoft) WebSocket implementation.
//!
//! Connects to Microsoft's Edge TTS service via WebSocket,
//! sends SSML, and receives audio data chunks.
//! Includes Sec-MS-GEC DRM token generation required since late 2024.

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio_tungstenite::{connect_async, tungstenite::client::IntoClientRequest, tungstenite::Message as WsMessage};
use tracing::{debug, warn};
use futures_util::{SinkExt, StreamExt};

use super::{AudioFormat, TtsProvider};
use crate::config::TtsConfig;
use crate::error::{GatewayError, Result};

const TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
const DEFAULT_CHROMIUM_VERSION: &str = "143.0.3650.75";
/// Seconds between Unix epoch (1970) and Windows FILETIME epoch (1601).
const WIN_EPOCH_OFFSET: u64 = 11_644_473_600;

/// Generate Sec-MS-GEC DRM token.
///
/// Algorithm: current time → Windows FILETIME epoch → round to 5 min →
/// convert to 100ns ticks → concat with trusted token → SHA-256 → uppercase hex.
fn gen_sec_ms_gec() -> String {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // Convert to Windows epoch, round down to 5-minute boundary
    let win_secs = now_secs + WIN_EPOCH_OFFSET;
    let rounded = win_secs - (win_secs % 300);
    // Convert to 100-nanosecond intervals (Windows FILETIME)
    let ticks = rounded as u128 * 10_000_000;

    let to_hash = format!("{ticks}{TRUSTED_CLIENT_TOKEN}");
    let hash = Sha256::digest(to_hash.as_bytes());
    // Uppercase hex
    hash.iter().map(|b| format!("{b:02X}")).collect()
}

/// Generate a random MUID (32 uppercase hex chars).
fn gen_muid() -> String {
    let id = uuid::Uuid::new_v4();
    id.to_string().replace('-', "").to_uppercase()
}

/// Edge TTS provider using Microsoft's WebSocket endpoint.
pub struct EdgeTtsProvider {
    voice: String,
    rate: String,
    pitch: String,
    volume: String,
    chromium_version: String,
}

impl EdgeTtsProvider {
    pub fn new(config: &TtsConfig) -> Self {
        let edge = config.edge.as_ref();
        Self {
            voice: edge
                .and_then(|e| e.voice.clone())
                .unwrap_or_else(|| "zh-CN-XiaoxiaoNeural".into()),
            rate: edge
                .and_then(|e| e.rate.clone())
                .unwrap_or_else(|| "+0%".into()),
            pitch: edge
                .and_then(|e| e.pitch.clone())
                .unwrap_or_else(|| "+0Hz".into()),
            volume: edge
                .and_then(|e| e.volume.clone())
                .unwrap_or_else(|| "+0%".into()),
            chromium_version: edge
                .and_then(|e| e.chromium_version.clone())
                .unwrap_or_else(|| DEFAULT_CHROMIUM_VERSION.into()),
        }
    }

    fn build_ssml(&self, text: &str) -> String {
        // Escape XML special characters in the text
        let escaped = text
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;");

        format!(
            "<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='zh-CN'>\
             <voice name='{}'>\
             <prosody rate='{}' pitch='{}' volume='{}'>\
             {}\
             </prosody>\
             </voice>\
             </speak>",
            self.voice, self.rate, self.pitch, self.volume, escaped
        )
    }
}

#[async_trait]
impl TtsProvider for EdgeTtsProvider {
    async fn synthesize(&self, text: &str, format: AudioFormat) -> Result<Vec<u8>> {
        if text.is_empty() {
            return Ok(Vec::new());
        }

        let request_id = uuid::Uuid::new_v4().to_string().replace('-', "");
        let sec_ms_gec = gen_sec_ms_gec();
        let muid = gen_muid();
        let sec_ms_gec_version = format!("1-{}", self.chromium_version);

        let ws_url = format!(
            "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1\
             ?TrustedClientToken={TRUSTED_CLIENT_TOKEN}\
             &ConnectionId={request_id}\
             &Sec-MS-GEC={sec_ms_gec}\
             &Sec-MS-GEC-Version={sec_ms_gec_version}"
        );

        debug!(voice = %self.voice, text_len = text.len(), "synthesizing speech");

        // Build request with required DRM headers
        let mut request = ws_url
            .as_str()
            .into_client_request()
            .map_err(|e| GatewayError::Tts(format!("build request failed: {e}")))?;
        let headers = request.headers_mut();
        headers.insert("Pragma", "no-cache".parse().unwrap());
        headers.insert("Cache-Control", "no-cache".parse().unwrap());
        headers.insert("Accept-Encoding", "gzip, deflate, br, zstd".parse().unwrap());
        headers.insert("Accept-Language", "en-US,en;q=0.9".parse().unwrap());
        headers.insert(
            "Origin",
            "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold"
                .parse()
                .unwrap(),
        );
        // User-Agent: major version only for Chrome/Edg (matches real Edge browser)
        let major = self.chromium_version.split('.').next().unwrap_or("143");
        headers.insert(
            "User-Agent",
            format!(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/{major}.0.0.0 Safari/537.36 Edg/{major}.0.0.0"
            )
            .parse()
            .unwrap(),
        );
        headers.insert("Cookie", format!("muid={muid};").parse().unwrap());

        let (ws_stream, _) = connect_async(request)
            .await
            .map_err(|e| GatewayError::Tts(format!("WebSocket connect failed: {e}")))?;

        let (mut write, mut read) = ws_stream.split();

        // Send configuration — output format depends on channel preference
        let output_format = match format {
            AudioFormat::Mp3 => "audio-24khz-48kbitrate-mono-mp3",
            AudioFormat::OggOpus => "webm-24khz-16bit-mono-opus",
        };
        let config_msg = format!(
            "X-Timestamp:Thu Jan 01 1970 00:00:00 GMT+0000\r\n\
             Content-Type:application/json; charset=utf-8\r\n\
             Path:speech.config\r\n\r\n\
             {{\"context\":{{\"synthesis\":{{\"audio\":{{\"metadataoptions\":{{\
             \"sentenceBoundaryEnabled\":\"false\",\
             \"wordBoundaryEnabled\":\"false\"}},\
             \"outputFormat\":\"{output_format}\"\
             }}}}}}}}"
        );
        write
            .send(WsMessage::Text(config_msg.into()))
            .await
            .map_err(|e| GatewayError::Tts(format!("send config failed: {e}")))?;

        // Send SSML synthesis request
        let ssml = self.build_ssml(text);
        let synth_msg = format!(
            "X-RequestId:{request_id}\r\n\
             Content-Type:application/ssml+xml\r\n\
             X-Timestamp:Thu Jan 01 1970 00:00:00 GMT+0000\r\n\
             Path:ssml\r\n\r\n\
             {ssml}"
        );
        write
            .send(WsMessage::Text(synth_msg.into()))
            .await
            .map_err(|e| GatewayError::Tts(format!("send SSML failed: {e}")))?;

        // Collect audio data from response
        let mut audio_data = Vec::with_capacity(64 * 1024); // Pre-allocate 64KB
        let header_separator = b"Path:audio\r\n";

        while let Some(msg_result) = read.next().await {
            match msg_result {
                Ok(WsMessage::Binary(data)) => {
                    // Binary messages contain audio data after the header
                    if let Some(pos) = find_subsequence(&data, header_separator) {
                        let audio_start = pos + header_separator.len();
                        if audio_start < data.len() {
                            audio_data.extend_from_slice(&data[audio_start..]);
                        }
                    }
                }
                Ok(WsMessage::Text(ref text)) => {
                    let text: &str = text.as_ref();
                    if text.contains("Path:turn.end") {
                        debug!(audio_bytes = audio_data.len(), "synthesis complete");
                        break;
                    }
                }
                Ok(WsMessage::Close(_)) => break,
                Err(e) => {
                    warn!(error = %e, "WebSocket error during TTS");
                    break;
                }
                _ => {}
            }
        }

        if audio_data.is_empty() {
            return Err(GatewayError::Tts("no audio data received".into()));
        }

        // Convert WebM Opus → OGG Opus when the channel requested OggOpus
        if format == AudioFormat::OggOpus {
            debug!(webm_bytes = audio_data.len(), "converting WebM Opus → OGG Opus");
            return super::webm_to_ogg::webm_opus_to_ogg_opus(&audio_data);
        }

        Ok(audio_data)
    }
}

/// Find a subsequence in a byte slice.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EdgeTtsConfig, TtsConfig};

    #[test]
    fn new_with_defaults() {
        let config = TtsConfig::default();
        let provider = EdgeTtsProvider::new(&config);
        assert_eq!(provider.voice, "zh-CN-XiaoxiaoNeural");
        assert_eq!(provider.rate, "+0%");
        assert_eq!(provider.pitch, "+0Hz");
        assert_eq!(provider.volume, "+0%");
    }

    #[test]
    fn new_with_custom_config() {
        let config = TtsConfig {
            edge: Some(EdgeTtsConfig {
                voice: Some("en-US-GuyNeural".into()),
                rate: Some("+10%".into()),
                pitch: Some("+5Hz".into()),
                volume: Some("-10%".into()),
                chromium_version: None,
            }),
            ..TtsConfig::default()
        };
        let provider = EdgeTtsProvider::new(&config);
        assert_eq!(provider.voice, "en-US-GuyNeural");
        assert_eq!(provider.rate, "+10%");
        assert_eq!(provider.pitch, "+5Hz");
        assert_eq!(provider.volume, "-10%");
    }

    #[test]
    fn build_ssml_escapes_xml() {
        let config = TtsConfig::default();
        let provider = EdgeTtsProvider::new(&config);
        let ssml = provider.build_ssml("Hello <world> & \"friends\"");
        assert!(ssml.contains("Hello &lt;world&gt; &amp; &quot;friends&quot;"));
        assert!(ssml.contains("zh-CN-XiaoxiaoNeural"));
    }

    #[test]
    fn build_ssml_contains_voice_and_prosody() {
        let config = TtsConfig {
            edge: Some(EdgeTtsConfig {
                voice: Some("en-US-JennyNeural".into()),
                rate: Some("+20%".into()),
                pitch: None,
                volume: None,
                chromium_version: None,
            }),
            ..TtsConfig::default()
        };
        let provider = EdgeTtsProvider::new(&config);
        let ssml = provider.build_ssml("test");
        assert!(ssml.contains("en-US-JennyNeural"));
        assert!(ssml.contains("rate='+20%'"));
        assert!(ssml.contains("test"));
    }

    #[test]
    fn find_subsequence_found() {
        let haystack = b"headerPath:audio\r\naudio-data-here";
        let needle = b"Path:audio\r\n";
        assert_eq!(find_subsequence(haystack, needle), Some(6));
    }

    #[test]
    fn find_subsequence_not_found() {
        let haystack = b"some random bytes";
        let needle = b"Path:audio\r\n";
        assert!(find_subsequence(haystack, needle).is_none());
    }

    #[tokio::test]
    async fn synthesize_empty_text_returns_empty() {
        let config = TtsConfig::default();
        let provider = EdgeTtsProvider::new(&config);
        let result = provider.synthesize("", AudioFormat::default()).await.unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn gen_sec_ms_gec_format() {
        let token = gen_sec_ms_gec();
        // SHA-256 uppercase hex = 64 chars
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()));
    }

    #[test]
    fn gen_sec_ms_gec_stable_within_window() {
        // Same 5-minute window → same token
        let t1 = gen_sec_ms_gec();
        let t2 = gen_sec_ms_gec();
        assert_eq!(t1, t2);
    }

    #[test]
    fn gen_muid_format() {
        let muid = gen_muid();
        assert_eq!(muid.len(), 32);
        assert!(muid.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()));
    }
}
