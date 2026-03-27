//! Live integration test for Volcengine (豆包) STT provider.
//!
//! Requires environment variables:
//!   VOLCENGINE_ACCESS_TOKEN — access token / access key
//!   VOLCENGINE_APP_ID       — application ID
//!
//! Run with `.env` providing VOLCENGINE_ACCESS_TOKEN / VOLCENGINE_APP_ID.

use openclaw_light::config::VolcengineSttConfig;
use openclaw_light::provider::stt::volcengine::VolcengineSttProvider;
use openclaw_light::provider::stt::SttProvider;

fn make_provider() -> Option<VolcengineSttProvider> {
    let token = std::env::var("VOLCENGINE_ACCESS_TOKEN").ok()?;
    let app_id = std::env::var("VOLCENGINE_APP_ID").ok()?;
    if token.is_empty() || app_id.is_empty() {
        return None;
    }
    let config = VolcengineSttConfig {
        app_id,
        access_token: token,
        cluster: String::new(),
        ws_url: String::new(),
    };
    VolcengineSttProvider::new(&config).ok()
}

/// Real speech test: load a pre-generated WAV file and transcribe it.
#[tokio::test]
async fn volcengine_stt_real_speech() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("debug")
        .with_test_writer()
        .try_init();

    let stt = match make_provider() {
        Some(p) => p,
        None => {
            eprintln!("SKIP: VOLCENGINE_ACCESS_TOKEN / VOLCENGINE_APP_ID not set");
            return;
        }
    };

    let audio = include_bytes!("../fixtures/test_speech.wav");
    eprintln!("Loaded test_speech.wav: {} bytes", audio.len());

    let result = stt.transcribe(audio, "audio/wav").await;
    match &result {
        Ok(text) => eprintln!("Volcengine STT returned: {:?}", text),
        Err(e) => eprintln!("Volcengine STT error: {}", e),
    }

    let text = result.expect("Volcengine STT transcription failed");
    eprintln!("Transcription: {:?}", text);
}
