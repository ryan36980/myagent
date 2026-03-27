//! Live integration test for Google Cloud Speech-to-Text provider.
//!
//! Requires environment variable:
//!   GOOGLE_STT_API_KEY — Google Cloud API key with Speech-to-Text enabled
//!
//! Run with `.env` providing GOOGLE_STT_API_KEY.

use openclaw_light::config::SttConfig;
use openclaw_light::provider::stt::google::GoogleSttProvider;
use openclaw_light::provider::stt::SttProvider;

fn make_provider() -> Option<GoogleSttProvider> {
    let api_key = std::env::var("GOOGLE_STT_API_KEY").ok()?;
    if api_key.is_empty() {
        return None;
    }
    let config = SttConfig {
        provider: "google".into(),
        api_key,
        ..SttConfig::default()
    };
    Some(GoogleSttProvider::new(reqwest::Client::new(), &config))
}

/// Real speech test: load a pre-generated WAV file and transcribe it.
#[tokio::test]
async fn google_stt_real_speech() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("debug")
        .with_test_writer()
        .try_init();

    let stt = match make_provider() {
        Some(p) => p,
        None => {
            eprintln!("SKIP: GOOGLE_STT_API_KEY not set");
            return;
        }
    };

    let audio = include_bytes!("../fixtures/test_speech.wav");
    eprintln!("Loaded test_speech.wav: {} bytes", audio.len());

    let result = stt.transcribe(audio, "audio/wav").await;
    match &result {
        Ok(text) => eprintln!("Google STT returned: {:?}", text),
        Err(e) => eprintln!("Google STT error: {}", e),
    }

    let text = result.expect("Google STT transcription failed");
    eprintln!("Transcription: {:?}", text);
}
