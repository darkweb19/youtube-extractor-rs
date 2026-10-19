//! Regression tests for the Apify-facing public request and response contracts.
use youtube_extractor::{extractor::normalize_url, model::{DownloadKind, DownloadRequest, OutputFormat, VideoInfo, VideoId}};

#[test]
fn frontend_video_request_remains_backward_compatible() {
    let request: DownloadRequest = serde_json::from_str(
        r#"{"url":"https://youtu.be/dQw4w9WgXcQ","type":"video","format":"mp4","quality":"720p"}"#
    ).unwrap();
    assert_eq!(request.kind, DownloadKind::Video);
    assert_eq!(request.format, OutputFormat::Mp4);
    assert!(request.validate().is_ok());
    assert_eq!(normalize_url(&request.url).unwrap(), VideoId::new("dQw4w9WgXcQ").unwrap());
}
#[test]
fn apify_metadata_keeps_the_public_response_shape_without_stream_secrets() {
    let info = VideoInfo {
        id: VideoId::new("dQw4w9WgXcQ").unwrap(),
        title: "Sample video".into(), description: String::new(), duration_seconds: Some(25),
        channel_id: None, channel_name: Some("Creator".into()),
        thumbnails: Vec::new(), formats: Vec::new(),
    };
    let json = serde_json::to_value(&info).unwrap();
    for field in ["id","title","description","duration_seconds","channel_id","channel_name","thumbnails","formats"] {
        assert!(json.get(field).is_some(), "{field}");
    }
    assert!(json["formats"].as_array().unwrap().is_empty());
    assert!(json.to_string().find("downloadUrl").is_none());
}
