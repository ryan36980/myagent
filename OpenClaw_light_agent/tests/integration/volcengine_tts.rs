//! Live integration test for Volcengine (火山引擎/豆包) TTS.
//!
//! Requires environment variables:
//!   VOLCENGINE_ACCESS_TOKEN  — access token from Volcengine console
//!   VOLCENGINE_TTS_APP_ID    — app ID
//!
//! Run with `.env` providing VOLCENGINE_ACCESS_TOKEN / VOLCENGINE_TTS_APP_ID.

use openclaw_light::config::VolcengineTtsConfig;
use openclaw_light::provider::tts::volcengine::VolcengineTtsProvider;
use openclaw_light::provider::tts::{AudioFormat, TtsProvider};

fn make_provider() -> Option<VolcengineTtsProvider> {
    let app_id = std::env::var("VOLCENGINE_TTS_APP_ID").ok()?;
    if app_id.is_empty() {
        return None;
    }
    let access_token = std::env::var("VOLCENGINE_ACCESS_TOKEN").ok()?;
    if access_token.is_empty() {
        return None;
    }

    let config = VolcengineTtsConfig {
        app_id,
        access_token,
        ..VolcengineTtsConfig::default()
    };
    let client = reqwest::Client::new();
    VolcengineTtsProvider::new(client, &config).ok()
}

#[tokio::test]
async fn volcengine_tts_synthesize_chinese() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("debug")
        .with_test_writer()
        .try_init();

    let provider = match make_provider() {
        Some(p) => p,
        None => {
            eprintln!("SKIP: VOLCENGINE_TTS_APP_ID / VOLCENGINE_ACCESS_TOKEN not set");
            return;
        }
    };

    let result = provider
        .synthesize("你好，这是一个火山引擎语音合成测试。", AudioFormat::OggOpus)
        .await;

    match &result {
        Ok(audio) => eprintln!("Volcengine TTS returned {} bytes", audio.len()),
        Err(e) => eprintln!("Volcengine TTS error: {}", e),
    }

    let audio = result.expect("Volcengine TTS synthesis failed");
    assert!(
        audio.len() > 100,
        "audio too short: {} bytes",
        audio.len()
    );

    // Volcengine returns native OGG Opus — should start with "OggS"
    assert_eq!(
        &audio[..4],
        b"OggS",
        "not a valid OGG file, first bytes: {:02X?}",
        &audio[..4.min(audio.len())]
    );

    // Check duration
    let duration_ms =
        openclaw_light::provider::tts::webm_to_ogg::ogg_opus_duration_ms(&audio);
    eprintln!(
        "Volcengine TTS OK: {} bytes, {}ms duration, valid OGG/Opus",
        audio.len(),
        duration_ms
    );
    assert!(
        duration_ms > 500,
        "duration too short: {}ms",
        duration_ms
    );
}

#[tokio::test]
async fn volcengine_tts_empty_text() {
    let provider = match make_provider() {
        Some(p) => p,
        None => {
            eprintln!("SKIP: VOLCENGINE_TTS_APP_ID / VOLCENGINE_ACCESS_TOKEN not set");
            return;
        }
    };

    let result = provider.synthesize("", AudioFormat::OggOpus).await;
    let audio = result.expect("empty text should succeed");
    assert!(audio.is_empty());
}
