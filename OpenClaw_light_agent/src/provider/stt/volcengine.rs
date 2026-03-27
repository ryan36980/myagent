//! Volcengine (豆包) STT implementation using WebSocket v3 binary protocol.
//!
//! Uses the BigModel ASR API at `wss://openspeech.bytedance.com/api/v3/sauc/bigmodel`.
//! Binary protocol: 4-byte header + 4-byte sequence + 4-byte payload_size + gzip(payload).
//!
//! Reference: VoiceAssistant `VolcengineASRProtocolAdapter.java`.

use async_trait::async_trait;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, error};

use super::SttProvider;
use crate::config::VolcengineSttConfig;
use crate::error::{GatewayError, Result};

use std::io::{Read, Write};

const DEFAULT_WS_URL: &str = "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel";
const DEFAULT_CLUSTER: &str = "volc.bigasr.sauc.duration";

// Protocol constants
const PROTOCOL_VERSION: u8 = 0b0001;
const MSG_TYPE_FULL_CLIENT_REQUEST: u8 = 0b0001;
const MSG_TYPE_AUDIO_ONLY_REQUEST: u8 = 0b0010;
const MSG_TYPE_FULL_SERVER_RESPONSE: u8 = 0b1001;
#[allow(dead_code)]
const MSG_TYPE_SERVER_ACK: u8 = 0b1011;
const MSG_TYPE_ERROR: u8 = 0b1111;

const MSG_SERIALIZATION_JSON: u8 = 0b0001;
const MSG_COMPRESSION_GZIP: u8 = 0b0001;

/// Volcengine (豆包) speech-to-text provider using v3 BigModel WebSocket API.
#[derive(Debug)]
pub struct VolcengineSttProvider {
    app_id: String,
    access_token: String,
    cluster: String,
    ws_url: String,
}

impl VolcengineSttProvider {
    pub fn new(config: &VolcengineSttConfig) -> std::result::Result<Self, GatewayError> {
        if config.access_token.is_empty() {
            return Err(GatewayError::Stt(
                "Volcengine STT not configured: set VOLCENGINE_ACCESS_TOKEN or volcengine.accessToken in config".into(),
            ));
        }
        if config.app_id.is_empty() {
            return Err(GatewayError::Stt(
                "Volcengine STT not configured: set volcengine.appId in config".into(),
            ));
        }
        Ok(Self {
            app_id: config.app_id.clone(),
            access_token: config.access_token.clone(),
            cluster: if config.cluster.is_empty() {
                DEFAULT_CLUSTER.into()
            } else {
                config.cluster.clone()
            },
            ws_url: if config.ws_url.is_empty() {
                DEFAULT_WS_URL.into()
            } else {
                config.ws_url.clone()
            },
        })
    }
}

/// Parse WAV header: returns `(sample_rate, data_offset)` if valid RIFF/WAVE.
/// Handles both standard 16-byte and extended 18-byte fmt chunks.
fn wav_parse(audio: &[u8]) -> Option<(u32, usize)> {
    if audio.len() < 44 || &audio[0..4] != b"RIFF" || &audio[8..12] != b"WAVE" {
        return None;
    }
    let sample_rate = u32::from_le_bytes([audio[24], audio[25], audio[26], audio[27]]);
    // Find "data" chunk — skip fmt chunk (16-byte header + variable chunk size)
    let fmt_size = u32::from_le_bytes([audio[16], audio[17], audio[18], audio[19]]) as usize;
    let data_offset = 20 + fmt_size; // end of fmt chunk data
    // Scan for "data" sub-chunk
    let mut pos = data_offset;
    while pos + 8 <= audio.len() {
        if &audio[pos..pos + 4] == b"data" {
            let _data_size = u32::from_le_bytes([
                audio[pos + 4], audio[pos + 5], audio[pos + 6], audio[pos + 7],
            ]) as usize;
            return Some((sample_rate, pos + 8)); // PCM data starts after "data" + size
        }
        // Skip unknown chunk: 4-byte id + 4-byte size + size bytes
        let chunk_size = u32::from_le_bytes([
            audio[pos + 4], audio[pos + 5], audio[pos + 6], audio[pos + 7],
        ]) as usize;
        pos += 8 + chunk_size;
    }
    None
}

/// Map MIME type to volcengine audio format and codec.
fn audio_params_for_mime(mime: &str) -> (&'static str, &'static str) {
    match mime {
        "audio/ogg" | "audio/opus" => ("ogg", "opus"),
        "audio/wav" | "audio/x-wav" | "audio/pcm" => ("pcm", "raw"),
        "audio/mpeg" | "audio/mp3" => ("mp3", "raw"),
        _ => ("ogg", "opus"),
    }
}

fn gzip_compress(data: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).expect("gzip compress");
    encoder.finish().expect("gzip finish")
}

fn gzip_decompress(data: &[u8]) -> std::result::Result<Vec<u8>, std::io::Error> {
    let mut decoder = GzDecoder::new(data);
    let mut result = Vec::new();
    decoder.read_to_end(&mut result)?;
    Ok(result)
}

/// Build the v3 binary protocol start message (full_client_request, sequence=1).
fn build_start_message(format: &str, codec: &str, sample_rate: u32) -> Vec<u8> {
    let json = serde_json::json!({
        "user": { "uid": "openclaw-light" },
        "audio": {
            "format": format,
            "codec": codec,
            "rate": sample_rate,
            "bits": 16,
            "channel": 1,
        },
        "request": {
            "model_name": "bigmodel",
            "enable_itn": true,
            "enable_punc": true,
            "enable_ddc": true,
            "show_utterances": true,
            "enable_nonstream": false,
        }
    });

    let json_bytes = json.to_string().into_bytes();
    let compressed = gzip_compress(&json_bytes);

    let mut msg = Vec::with_capacity(12 + compressed.len());

    // Header (4 bytes)
    msg.push((PROTOCOL_VERSION << 4) | 0x01); // version=1, header_size=1 (×4 = 4 bytes)
    msg.push((MSG_TYPE_FULL_CLIENT_REQUEST << 4) | 0x01); // type=full_client, flags=has_sequence
    msg.push((MSG_SERIALIZATION_JSON << 4) | MSG_COMPRESSION_GZIP); // JSON + GZIP
    msg.push(0x00); // reserved

    // Sequence = 1 (big-endian i32)
    msg.extend_from_slice(&1i32.to_be_bytes());

    // Payload size (big-endian u32) + payload
    msg.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
    msg.extend_from_slice(&compressed);

    msg
}

/// Build the v3 binary protocol audio frame.
fn build_audio_frame(audio: &[u8], sequence: i32, is_last: bool) -> Vec<u8> {
    let compressed = gzip_compress(audio);
    let flags: u8 = if is_last { 0b0011 } else { 0b0001 }; // has_sequence + is_last
    let seq = if is_last { -sequence } else { sequence };

    let mut msg = Vec::with_capacity(12 + compressed.len());

    // Header (4 bytes)
    msg.push((PROTOCOL_VERSION << 4) | 0x01);
    msg.push((MSG_TYPE_AUDIO_ONLY_REQUEST << 4) | flags);
    msg.push((MSG_SERIALIZATION_JSON << 4) | MSG_COMPRESSION_GZIP); // matches VoiceAssistant
    msg.push(0x00);

    // Sequence (big-endian i32)
    msg.extend_from_slice(&seq.to_be_bytes());

    // Payload size + payload
    msg.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
    msg.extend_from_slice(&compressed);

    msg
}

/// Decompress payload if gzip-compressed.
fn decompress_payload(payload: &[u8], compression: u8) -> String {
    if compression == MSG_COMPRESSION_GZIP && !payload.is_empty() {
        match gzip_decompress(payload) {
            Ok(data) => String::from_utf8_lossy(&data).into_owned(),
            Err(_) => String::from_utf8_lossy(payload).into_owned(),
        }
    } else {
        String::from_utf8_lossy(payload).into_owned()
    }
}

/// Parse a v3 binary protocol response frame.
/// Returns `Ok(Some(text))` for final results, `Ok(None)` for non-final/ack,
/// `Err` for protocol or server errors.
fn parse_response(data: &[u8]) -> std::result::Result<Option<String>, String> {
    if data.len() < 4 {
        return Err("response too short".into());
    }

    let header_size = (data[0] & 0x0F) as usize * 4;
    let message_type = (data[1] & 0xF0) >> 4;
    let message_flags = data[1] & 0x0F;
    let compression = data[2] & 0x0F;

    debug!(
        header_size,
        message_type,
        message_flags,
        compression,
        data_len = data.len(),
        "parsing response frame"
    );

    if data.len() < header_size {
        return Err("response shorter than header".into());
    }

    let mut offset = header_size;

    // Flag bit 0 (0x01): has sequence field
    if (message_flags & 0x01) != 0 && data.len() >= offset + 4 {
        let seq = i32::from_be_bytes([data[offset], data[offset+1], data[offset+2], data[offset+3]]);
        debug!(sequence = seq, "response sequence");
        offset += 4;
    }

    // Flag bit 1 (0x02): is last package
    let is_last = (message_flags & 0x02) != 0;

    // Flag bit 2 (0x04): has event field
    if (message_flags & 0x04) != 0 && data.len() >= offset + 4 {
        offset += 4; // skip event
    }

    match message_type {
        MSG_TYPE_FULL_SERVER_RESPONSE => {
            // payload_size(4) + payload
            if data.len() < offset + 4 {
                return Ok(None);
            }
            let payload_size =
                u32::from_be_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]])
                    as usize;
            offset += 4;
            if payload_size == 0 {
                return Ok(None);
            }
            let end = (offset + payload_size).min(data.len());
            let json_str = decompress_payload(&data[offset..end], compression);
            debug!(json = %json_str, is_last, "server response JSON");
            parse_json_result(&json_str, is_last)
        }
        MSG_TYPE_ERROR => {
            // error_code(4) + payload_size(4) + payload
            let error_code = if data.len() >= offset + 4 {
                let c = u32::from_be_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]]);
                offset += 4;
                c
            } else {
                0
            };
            let payload_size = if data.len() >= offset + 4 {
                let s = u32::from_be_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]]) as usize;
                offset += 4;
                s
            } else {
                0
            };
            if payload_size > 0 && data.len() >= offset + payload_size {
                let text = decompress_payload(&data[offset..offset + payload_size], compression);
                Err(format!("volcengine error {}: {}", error_code, text))
            } else {
                Err(format!("volcengine error code {}", error_code))
            }
        }
        _ => Ok(None), // ACK, unknown — skip
    }
}

/// Parse the JSON payload from a server response.
///
/// Actual v3 bigmodel response format:
/// ```json
/// {
///   "result": {
///     "text": "Today is a good day.",
///     "utterances": [{"text": "...", "definite": true, ...}]
///   }
/// }
/// ```
fn parse_json_result(json_str: &str, is_last: bool) -> std::result::Result<Option<String>, String> {
    let v: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| format!("JSON parse error: {}", e))?;

    // Check error code
    if let Some(code) = v.get("code").and_then(|c| c.as_i64()) {
        if code != 0 && code != 1000 {
            let msg = v.get("message").and_then(|m| m.as_str()).unwrap_or("unknown");
            return Err(format!("volcengine error {}: {}", code, msg));
        }
        if code == 1000 {
            return Ok(None); // session end
        }
    }

    let result = match v.get("result") {
        Some(r) => r,
        None => return Ok(None),
    };

    // v3 bigmodel format: result is an object with "text" and optional "utterances"
    if let Some(obj) = result.as_object() {
        // When is_last, prefer the top-level result.text which contains the full
        // concatenated transcription across all utterances.
        if is_last {
            if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
                if !text.is_empty() {
                    return Ok(Some(text.to_string()));
                }
            }
        }
        // For non-final frames, check individual utterances for definite results.
        if let Some(utterances) = obj.get("utterances").and_then(|u| u.as_array()) {
            for utt in utterances {
                if let Some(text) = utt.get("text").and_then(|t| t.as_str()) {
                    if !text.is_empty() {
                        let definite = utt.get("definite").and_then(|d| d.as_bool()).unwrap_or(false);
                        if definite {
                            return Ok(Some(text.to_string()));
                        }
                    }
                }
            }
        }
    }

    // Legacy format: result is an array of {text, type}
    if let Some(arr) = result.as_array() {
        for item in arr {
            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                if !text.is_empty() {
                    let rtype = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if rtype == "final" || rtype == "complete" || is_last {
                        return Ok(Some(text.to_string()));
                    }
                }
            }
        }
    }

    Ok(None)
}

#[async_trait]
impl SttProvider for VolcengineSttProvider {
    async fn transcribe(&self, audio: &[u8], mime: &str) -> Result<String> {
        let (mut format, codec) = audio_params_for_mime(mime);

        // For WAV files: strip header, send raw PCM (avoids WAV decoder compat issues)
        let (audio_data, sample_rate) = if matches!(mime, "audio/wav" | "audio/x-wav") {
            if let Some((sr, data_offset)) = wav_parse(audio) {
                debug!(sample_rate = sr, data_offset, "stripped WAV header, sending as PCM");
                format = "pcm";
                (&audio[data_offset..], sr)
            } else {
                (audio, 16000u32)
            }
        } else {
            (audio, 16000u32)
        };

        debug!(
            format,
            codec,
            audio_len = audio_data.len(),
            sample_rate,
            ws_url = %self.ws_url,
            "volcengine v3 STT transcribing"
        );

        // Build WebSocket request with v3 auth headers
        let mut request = self
            .ws_url
            .as_str()
            .into_client_request()
            .map_err(|e| GatewayError::Stt(format!("ws request build error: {}", e)))?;

        let connect_id = uuid::Uuid::new_v4().to_string();
        let headers = request.headers_mut();
        headers.insert(
            "X-Api-App-Key",
            self.app_id.parse().map_err(|_| GatewayError::Stt("invalid app_id".into()))?,
        );
        headers.insert(
            "X-Api-Access-Key",
            self.access_token
                .parse()
                .map_err(|_| GatewayError::Stt("invalid access_token".into()))?,
        );
        headers.insert(
            "X-Api-Resource-Id",
            self.cluster.parse().map_err(|_| GatewayError::Stt("invalid cluster".into()))?,
        );
        headers.insert(
            "X-Api-Connect-Id",
            connect_id.parse().map_err(|_| GatewayError::Stt("invalid connect_id".into()))?,
        );

        // Connect
        let (ws_stream, _response) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| GatewayError::Stt(format!("WebSocket connect error: {}", e)))?;

        let (mut write, mut read) = ws_stream.split();

        // 1. Send start message (full_client_request, sequence=1)
        let start_msg = build_start_message(format, codec, sample_rate);
        debug!(msg_len = start_msg.len(), "sending start message (seq=1)");
        write
            .send(WsMessage::Binary(start_msg.into()))
            .await
            .map_err(|e| GatewayError::Stt(format!("ws send start error: {}", e)))?;

        // 2. Send audio in chunks (~100ms of 16kHz 16-bit mono PCM each)
        //    Streaming avoids server-side 3s processing timeout on large audio.
        const CHUNK_SIZE: usize = 3200;
        let mut sequence = 2i32;
        for chunk in audio_data.chunks(CHUNK_SIZE) {
            let frame = build_audio_frame(chunk, sequence, false);
            debug!(frame_len = frame.len(), seq = sequence, "sending audio chunk");
            write
                .send(WsMessage::Binary(frame.into()))
                .await
                .map_err(|e| GatewayError::Stt(format!("ws send audio error: {}", e)))?;
            sequence += 1;
        }

        // 3. Send empty finish frame (is_last=true → negative sequence)
        let finish_frame = build_audio_frame(&[], sequence, true);
        debug!(frame_len = finish_frame.len(), seq = -sequence, "sending finish frame");
        write
            .send(WsMessage::Binary(finish_frame.into()))
            .await
            .map_err(|e| GatewayError::Stt(format!("ws send finish error: {}", e)))?;

        // 4. Read responses until final result or connection close
        let timeout = tokio::time::Duration::from_secs(30);
        let mut last_text = String::new();
        let result = tokio::time::timeout(timeout, async {
            let mut frame_count = 0u32;
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(WsMessage::Binary(data)) => {
                        frame_count += 1;
                        debug!(frame = frame_count, data_len = data.len(), "received Binary frame");
                        match parse_response(&data) {
                            Ok(Some(text)) => {
                                debug!(text_len = text.len(), frame = frame_count, "volcengine result text");
                                last_text = text;
                            }
                            Ok(None) => {
                                debug!(frame = frame_count, "volcengine non-final/ack frame");
                            }
                            Err(e) => {
                                error!(error = %e, frame = frame_count, "volcengine response error");
                                return Err(GatewayError::Stt(e));
                            }
                        }
                    }
                    Ok(WsMessage::Text(text)) => {
                        debug!(text = %text, "received Text frame (unexpected)");
                    }
                    Ok(WsMessage::Close(frame)) => {
                        let reason = frame
                            .as_ref()
                            .map(|f| format!("code={}, reason={}", f.code, f.reason))
                            .unwrap_or_else(|| "no frame".into());
                        debug!(reason = %reason, frames_received = frame_count, "WebSocket closed");
                        break;
                    }
                    Ok(WsMessage::Ping(_)) => debug!("received Ping"),
                    Ok(WsMessage::Pong(_)) => debug!("received Pong"),
                    Err(e) => {
                        error!(error = %e, "ws read error");
                        return Err(GatewayError::Stt(format!("ws read error: {}", e)));
                    }
                    _ => {}
                }
            }
            Ok(())
        })
        .await;

        // Close gracefully
        let _ = write.send(WsMessage::Close(None)).await;

        match result {
            Ok(Ok(())) => {
                debug!(text_len = last_text.len(), "volcengine transcription complete");
                Ok(last_text)
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err(GatewayError::Stt("volcengine STT timeout (30s)".into())),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_with_defaults() {
        let config = VolcengineSttConfig {
            app_id: "test-app".into(),
            access_token: "test-token".into(),
            cluster: String::new(),
            ws_url: String::new(),
        };
        let provider = VolcengineSttProvider::new(&config).unwrap();
        assert_eq!(provider.app_id, "test-app");
        assert_eq!(provider.access_token, "test-token");
        assert_eq!(provider.cluster, DEFAULT_CLUSTER);
        assert_eq!(provider.ws_url, DEFAULT_WS_URL);
    }

    #[test]
    fn new_with_custom_config() {
        let config = VolcengineSttConfig {
            app_id: "my-app".into(),
            access_token: "my-token".into(),
            cluster: "my_cluster".into(),
            ws_url: "wss://custom.example.com/asr".into(),
        };
        let provider = VolcengineSttProvider::new(&config).unwrap();
        assert_eq!(provider.app_id, "my-app");
        assert_eq!(provider.cluster, "my_cluster");
        assert_eq!(provider.ws_url, "wss://custom.example.com/asr");
    }

    #[test]
    fn new_missing_token_errors() {
        let config = VolcengineSttConfig {
            app_id: "test-app".into(),
            access_token: String::new(),
            cluster: String::new(),
            ws_url: String::new(),
        };
        let err = VolcengineSttProvider::new(&config).unwrap_err();
        assert!(err.to_string().contains("VOLCENGINE_ACCESS_TOKEN"));
    }

    #[test]
    fn new_missing_app_id_errors() {
        let config = VolcengineSttConfig {
            app_id: String::new(),
            access_token: "test-token".into(),
            cluster: String::new(),
            ws_url: String::new(),
        };
        let err = VolcengineSttProvider::new(&config).unwrap_err();
        assert!(err.to_string().contains("appId"));
    }

    #[test]
    fn wav_parse_standard_header() {
        // Build a standard 44-byte WAV header + 4 bytes of PCM data
        let mut wav = vec![0u8; 48];
        wav[0..4].copy_from_slice(b"RIFF");
        wav[4..8].copy_from_slice(&40u32.to_le_bytes()); // file size - 8
        wav[8..12].copy_from_slice(b"WAVE");
        wav[12..16].copy_from_slice(b"fmt ");
        wav[16..20].copy_from_slice(&16u32.to_le_bytes()); // standard fmt size
        wav[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
        wav[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
        wav[24..28].copy_from_slice(&22050u32.to_le_bytes()); // sample rate
        wav[28..32].copy_from_slice(&44100u32.to_le_bytes()); // byte rate
        wav[32..34].copy_from_slice(&2u16.to_le_bytes()); // block align
        wav[34..36].copy_from_slice(&16u16.to_le_bytes()); // bits
        wav[36..40].copy_from_slice(b"data");
        wav[40..44].copy_from_slice(&4u32.to_le_bytes()); // data size
        // 4 bytes PCM at offset 44

        let result = wav_parse(&wav);
        assert_eq!(result, Some((22050, 44)));
    }

    #[test]
    fn wav_parse_extended_fmt() {
        // Build WAV with 18-byte fmt chunk (Windows TTS common)
        let mut wav = vec![0u8; 50];
        wav[0..4].copy_from_slice(b"RIFF");
        wav[4..8].copy_from_slice(&42u32.to_le_bytes());
        wav[8..12].copy_from_slice(b"WAVE");
        wav[12..16].copy_from_slice(b"fmt ");
        wav[16..20].copy_from_slice(&18u32.to_le_bytes()); // extended fmt
        wav[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
        wav[22..24].copy_from_slice(&1u16.to_le_bytes());
        wav[24..28].copy_from_slice(&16000u32.to_le_bytes());
        wav[28..32].copy_from_slice(&32000u32.to_le_bytes());
        wav[32..34].copy_from_slice(&2u16.to_le_bytes());
        wav[34..36].copy_from_slice(&16u16.to_le_bytes());
        wav[36..38].copy_from_slice(&0u16.to_le_bytes()); // cbSize=0
        wav[38..42].copy_from_slice(b"data");
        wav[42..46].copy_from_slice(&4u32.to_le_bytes());

        let result = wav_parse(&wav);
        assert_eq!(result, Some((16000, 46)));
    }

    #[test]
    fn wav_parse_invalid() {
        assert_eq!(wav_parse(b"OggS...."), None);
        assert_eq!(wav_parse(b"RIFF"), None);
        assert_eq!(wav_parse(&[0u8; 44]), None); // no RIFF header
    }

    #[test]
    fn audio_params_for_common_mimes() {
        assert_eq!(audio_params_for_mime("audio/ogg"), ("ogg", "opus"));
        assert_eq!(audio_params_for_mime("audio/opus"), ("ogg", "opus"));
        assert_eq!(audio_params_for_mime("audio/wav"), ("pcm", "raw"));
        assert_eq!(audio_params_for_mime("audio/mpeg"), ("mp3", "raw"));
        assert_eq!(audio_params_for_mime("audio/pcm"), ("pcm", "raw"));
        assert_eq!(audio_params_for_mime("unknown"), ("ogg", "opus"));
    }

    #[test]
    fn gzip_roundtrip() {
        let original = b"hello world, this is a test of gzip compression";
        let compressed = gzip_compress(original);
        assert_ne!(&compressed, original);
        let decompressed = gzip_decompress(&compressed).unwrap();
        assert_eq!(&decompressed, original);
    }

    #[test]
    fn build_start_message_header() {
        let msg = build_start_message("ogg", "opus", 16000);
        assert!(msg.len() >= 12);
        // Header byte 0: version=1, header_size=1
        assert_eq!(msg[0], 0x11);
        // Header byte 1: type=full_client_request(0001), flags=has_sequence(0001)
        assert_eq!(msg[1], 0x11);
        // Header byte 2: JSON(0001) + GZIP(0001)
        assert_eq!(msg[2], 0x11);
        // Header byte 3: reserved
        assert_eq!(msg[3], 0x00);
        // Sequence = 1
        assert_eq!(&msg[4..8], &1i32.to_be_bytes());
    }

    #[test]
    fn build_audio_frame_last() {
        let audio = b"fake-audio-data";
        let msg = build_audio_frame(audio, 2, true);
        assert!(msg.len() >= 12);
        // Header byte 1: type=audio_only(0010), flags=has_sequence+is_last(0011)
        assert_eq!(msg[1], 0x23);
        // Sequence = -2 (last packet negates)
        assert_eq!(&msg[4..8], &(-2i32).to_be_bytes());
    }

    #[test]
    fn build_audio_frame_not_last() {
        let audio = b"chunk";
        let msg = build_audio_frame(audio, 3, false);
        // Header byte 1: type=audio_only(0010), flags=has_sequence(0001)
        assert_eq!(msg[1], 0x21);
        // Sequence = 3 (positive)
        assert_eq!(&msg[4..8], &3i32.to_be_bytes());
    }

    #[test]
    fn parse_response_server_response_v3_object() {
        // Actual v3 bigmodel format: result is an object
        let json_str = r#"{"result":{"text":"你好世界","utterances":[{"text":"你好世界","definite":true}]}}"#;
        let compressed = gzip_compress(json_str.as_bytes());

        let mut data = Vec::new();
        data.push(0x11);
        data.push(0x93); // type=server_response, flags=has_sequence+is_last
        data.push(0x11); // JSON + GZIP
        data.push(0x00);
        data.extend_from_slice(&1i32.to_be_bytes());
        data.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
        data.extend_from_slice(&compressed);

        let result = parse_response(&data).unwrap();
        assert_eq!(result, Some("你好世界".to_string()));
    }

    #[test]
    fn parse_response_server_response_legacy_array() {
        // Legacy format: result is an array
        let json_str = r#"{"result":[{"text":"hello","type":"final"}]}"#;
        let compressed = gzip_compress(json_str.as_bytes());

        let mut data = Vec::new();
        data.push(0x11);
        data.push(0x93);
        data.push(0x11);
        data.push(0x00);
        data.extend_from_slice(&1i32.to_be_bytes());
        data.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
        data.extend_from_slice(&compressed);

        let result = parse_response(&data).unwrap();
        assert_eq!(result, Some("hello".to_string()));
    }

    #[test]
    fn parse_response_error() {
        let json_str = r#"{"message":"auth failed"}"#;
        let compressed = gzip_compress(json_str.as_bytes());

        let mut data = Vec::new();
        // Header
        data.push(0x11);
        data.push(0xF0); // type=error(1111), flags=0
        data.push(0x11); // JSON + GZIP
        data.push(0x00);
        // Error code
        data.extend_from_slice(&40000u32.to_be_bytes());
        // Payload size + payload
        data.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
        data.extend_from_slice(&compressed);

        let err = parse_response(&data).unwrap_err();
        assert!(err.contains("40000"));
        assert!(err.contains("auth failed"));
    }

    #[test]
    fn parse_response_ack() {
        let data = vec![
            0x11, // header
            0xB0, // type=server_ack(1011), flags=0
            0x00, 0x00,
        ];
        let result = parse_response(&data).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn parse_json_result_v3_utterances() {
        // v3 format: utterances nested inside result object
        let json = r#"{"result":{"text":"测试","utterances":[{"text":"测试","definite":true}]}}"#;
        let result = parse_json_result(json, false).unwrap();
        assert_eq!(result, Some("测试".to_string()));
    }

    #[test]
    fn parse_json_result_non_definite_skipped() {
        let json = r#"{"result":{"text":"partial","utterances":[{"text":"partial","definite":false}]}}"#;
        let result = parse_json_result(json, false).unwrap();
        assert_eq!(result, None); // not definite and not last → skip
    }

    #[test]
    fn parse_json_result_is_last_overrides() {
        let json = r#"{"result":{"text":"final","utterances":[{"text":"final","definite":false}]}}"#;
        let result = parse_json_result(json, true).unwrap(); // is_last=true
        assert_eq!(result, Some("final".to_string()));
    }

    #[test]
    fn parse_json_result_session_end() {
        let json = r#"{"code":1000}"#;
        let result = parse_json_result(json, false).unwrap();
        assert_eq!(result, None);
    }
}
