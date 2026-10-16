//! Public format-selection integration tests. Literal-loopback streamed network
//! fixtures live in download.rs unit tests so production has no localhost bypass.
use youtube_extractor::{
    download::select_formats,
    model::{DownloadKind, DownloadRequest, MediaFormat, OutputFormat},
};

fn format(
    itag: u32,
    height: Option<u32>,
    audio: bool,
    bitrate: u64,
    codec: &str,
    container: &str,
) -> MediaFormat {
    MediaFormat {
        itag,
        mime_type: format!(
            "{}/{container}",
            if height.is_some() { "video" } else { "audio" }
        ),
        codecs: vec![codec.into()],
        container: container.into(),
        bitrate: Some(bitrate),
        width: height.map(|h| h * 16 / 9),
        height,
        fps: height.map(|_| 30),
        content_length: Some(100),
        has_audio: audio,
        has_video: height.is_some(),
        url: Some("https://r1.googlevideo.com/media".into()),
        signature_cipher: None,
    }
}

fn request(kind: DownloadKind, output: OutputFormat, quality: Option<&str>) -> DownloadRequest {
    DownloadRequest {
        url: "https://youtu.be/abcdefghijk".into(),
        kind,
        format: output,
        quality: quality.map(str::to_owned),
        audio_bitrate_kbps: None,
    }
}

#[test]
fn selection_respects_height_container_and_codec_compatibility() {
    let formats = vec![
        format(1, Some(1080), false, 2_000_000, "avc1.640028", "mp4"),
        format(2, Some(720), false, 1_000_000, "avc1.64001f", "mp4"),
        format(3, Some(2160), false, 5_000_000, "vp9", "webm"),
        format(4, None, true, 128_000, "mp4a.40.2", "m4a"),
        format(5, None, true, 160_000, "opus", "webm"),
    ];
    let selected = select_formats(
        &formats,
        &request(DownloadKind::Video, OutputFormat::Mp4, Some("720p")),
    )
    .unwrap();
    assert_eq!(selected.video.unwrap().itag, 2);
    assert_eq!(selected.audio.unwrap().itag, 4);
    let selected = select_formats(
        &formats,
        &request(DownloadKind::Video, OutputFormat::Webm, Some("best")),
    )
    .unwrap();
    assert_eq!(selected.video.unwrap().itag, 3);
    assert_eq!(selected.audio.unwrap().itag, 5);
    assert!(select_formats(
        &formats,
        &request(DownloadKind::Video, OutputFormat::Mp4, Some("360p"))
    )
    .is_err());
}

#[test]
fn audio_worst_and_m4a_prefer_aac_are_explicit() {
    let formats = vec![
        format(1, None, true, 64_000, "opus", "webm"),
        format(2, None, true, 160_000, "opus", "webm"),
        format(3, None, true, 128_000, "mp4a.40.2", "m4a"),
    ];
    assert_eq!(
        select_formats(
            &formats,
            &request(DownloadKind::Audio, OutputFormat::Original, Some("worst"))
        )
        .unwrap()
        .audio
        .unwrap()
        .itag,
        1
    );
    assert_eq!(
        select_formats(
            &formats,
            &request(DownloadKind::Audio, OutputFormat::M4a, Some("best"))
        )
        .unwrap()
        .audio
        .unwrap()
        .itag,
        3
    );
    assert_eq!(
        select_formats(
            &formats,
            &request(DownloadKind::Audio, OutputFormat::Mp3, Some("best"))
        )
        .unwrap()
        .audio
        .unwrap()
        .itag,
        2
    );
}

#[test]
fn selection_rejects_unresolved_missing_audio_and_unknown_codecs() {
    let mut video = format(1, Some(1080), false, 2_000_000, "avc1.640028", "mp4");
    assert!(select_formats(
        &[video.clone()],
        &request(DownloadKind::Video, OutputFormat::Mp4, None)
    )
    .is_err());
    let audio = format(2, None, true, 128_000, "mp4a.40.2", "m4a");
    video.codecs.clear();
    assert!(select_formats(
        &[video.clone(), audio.clone()],
        &request(DownloadKind::Video, OutputFormat::Mp4, None)
    )
    .is_err());
    video.codecs.push("avc1.640028".into());
    video.url = None;
    assert!(select_formats(
        &[video, audio],
        &request(DownloadKind::Video, OutputFormat::Mp4, None)
    )
    .is_err());
}
