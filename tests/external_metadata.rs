//! Explicit opt-in compatibility probe. Use only a video you are authorized to access/download.
use std::time::Duration;
use youtube_extractor::{
    extractor::{Extractor, YoutubeExtractor},
    network::SafeHttpClient,
};

#[tokio::test]
#[ignore = "requires AUTHORIZED_TEST_YOUTUBE_URL and approved YouTube egress"]
async fn public_youtube_metadata_compatibility() {
    let url = std::env::var("AUTHORIZED_TEST_YOUTUBE_URL")
        .expect("set AUTHORIZED_TEST_YOUTUBE_URL to authorized test media");
    let http = SafeHttpClient::new(Duration::from_secs(10), Duration::from_secs(30)).unwrap();
    let extractor = YoutubeExtractor::new(http, 8 * 1024 * 1024);
    let info = extractor.video_info(&url).await.unwrap();
    assert!(!info.title.is_empty());
    assert!(!info.formats.is_empty());
    assert!(info.formats.iter().all(|f| f.url.is_some()));
}
