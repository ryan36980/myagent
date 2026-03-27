//! Live integration test for Edge TTS with DRM (Sec-MS-GEC).
//!
//! This test connects to the real Edge TTS service to verify
//! that the DRM token generation works correctly.
//!
use openclaw_light::config::TtsConfig;
use openclaw_light::provider::tts::edge::EdgeTtsProvider;
use openclaw_light::provider::tts::{AudioFormat, TtsProvider};

#[tokio::test]
async fn edge_tts_synthesize_opus() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("debug")
        .with_test_writer()
        .try_init();

    let config = TtsConfig::default();
    let provider = EdgeTtsProvider::new(&config);

    let result = provider.synthesize("你好世界", AudioFormat::OggOpus).await;
    match &result {
        Ok(audio) => eprintln!("Edge TTS returned {} bytes of audio", audio.len()),
        Err(e) => eprintln!("Edge TTS error: {}", e),
    }

    let audio = result.expect("Edge TTS synthesis failed");
    // WebM→OGG conversion must produce a file much larger than just headers (~91 bytes)
    assert!(audio.len() > 5000, "OGG audio too short: {} bytes (conversion likely broken)", audio.len());
    // OGG: starts with "OggS" magic bytes
    assert_eq!(&audio[..4], b"OggS", "not a valid OGG file, first bytes: {:02X?}", &audio[..4.min(audio.len())]);
    // Count OGG pages: should have 2 header pages + many audio pages
    let page_count = audio.windows(4).filter(|w| *w == b"OggS").count();
    assert!(page_count > 10, "too few OGG pages: {} (expected many audio frames)", page_count);
    eprintln!("Edge TTS OK: {} bytes, {} OGG pages, valid OGG/Opus format", audio.len(), page_count);
}

#[tokio::test]
async fn edge_tts_synthesize_mp3() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("debug")
        .with_test_writer()
        .try_init();

    let config = TtsConfig::default();
    let provider = EdgeTtsProvider::new(&config);

    let result = provider.synthesize("你好世界", AudioFormat::Mp3).await;
    let audio = result.expect("Edge TTS MP3 synthesis failed");
    assert!(audio.len() > 100, "audio too short: {} bytes", audio.len());
    // MP3: starts with ID3 tag or MPEG sync word (0xFF 0xFB/0xF3/0xF2)
    let is_mp3 = (audio.len() >= 3 && &audio[..3] == b"ID3")
        || (audio.len() >= 2 && audio[0] == 0xFF && (audio[1] & 0xE0) == 0xE0);
    assert!(is_mp3, "not a valid MP3 file, first bytes: {:02X?}", &audio[..4.min(audio.len())]);
    eprintln!("Edge TTS OK: {} bytes, valid MP3 format", audio.len());
}
