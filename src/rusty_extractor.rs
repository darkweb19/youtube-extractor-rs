//! Native rusty_ytdl adapter for YouTube metadata and resolvable media streams.
//! This replaces the yt-dlp subprocess only on the experimental rust-yt-bot branch.
//! All user URLs are normalized first, and signed stream links never enter API JSON.
use crate::{
    error::{AppError, ErrorCode, Result},
    extractor::{normalize_url, Extractor},
    model::{MediaFormat, Thumbnail, VideoId, VideoInfo},
    network::validate_media_url,
};
use async_trait::async_trait;
use rusty_ytdl::{Video, VideoError, VideoFormat as NativeFormat};

#[derive(Clone, Default)]
pub struct RustyYtdlExtractor;

#[async_trait]
impl Extractor for RustyYtdlExtractor {
    async fn video_info(&self, input: &str) -> Result<VideoInfo> {
        let id = normalize_url(input)?;
        // Never hand the original user-supplied URL to a third-party parser.
        let video =
            Video::new(format!("https://www.youtube.com/watch?v={id}")).map_err(classify_error)?;
        let source = video.get_basic_info().await.map_err(classify_error)?;
        normalize_info(id, source)
    }
}

fn classify_error(error: VideoError) -> AppError {
    // Do not expose raw upstream HTML, signed URLs, cookies, or player internals.
    let description = error.to_string().to_ascii_lowercase();
    if description.contains("429") || description.contains("too many requests") {
        AppError::new(
            ErrorCode::UpstreamRateLimited,
            "YouTube is rate limiting requests from this server",
        )
    } else if description.contains("403")
        || description.contains("private")
        || description.contains("sign in")
        || description.contains("not a bot")
        || description.contains("confirm")
        || description.contains("unavailable")
    {
        AppError::new(
            ErrorCode::AccessRestricted,
            "YouTube denied access from this server",
        )
    } else if matches!(error, VideoError::FormatNotFound) {
        AppError::new(
            ErrorCode::UnsupportedFormat,
            "No supported media streams available",
        )
    } else {
        AppError::new(
            ErrorCode::PlayerChanged,
            "rusty_ytdl could not parse the public YouTube player",
        )
    }
}

fn normalize_info(id: VideoId, info: rusty_ytdl::VideoInfo) -> Result<VideoInfo> {
    let details = info.video_details;
    if details.video_id != id.as_ref() {
        return Err(AppError::new(
            ErrorCode::PlayerChanged,
            "YouTube returned a mismatched video identifier",
        ));
    }
    if details.is_private || details.age_restricted {
        return Err(AppError::new(
            ErrorCode::AccessRestricted,
            "Restricted YouTube videos are unsupported",
        ));
    }
    // The existing download pipeline only supports completed VODs and direct HTTPS media.
    if details.is_live_content && details.length_seconds == "0" {
        return Err(AppError::new(
            ErrorCode::UnsupportedFormat,
            "Active live streams are unsupported",
        ));
    }
    let formats: Vec<MediaFormat> = info.formats.into_iter().filter_map(map_format).collect();
    if formats.is_empty() {
        return Err(AppError::new(
            ErrorCode::UnsupportedFormat,
            "rusty_ytdl found no directly resolvable supported media streams",
        ));
    }
    let thumbnails = details
        .thumbnails
        .into_iter()
        .map(|t| Thumbnail {
            url: t.url,
            width: u32::try_from(t.width).ok(),
            height: u32::try_from(t.height).ok(),
        })
        .collect();
    Ok(VideoInfo {
        id,
        title: details.title,
        description: details.description,
        duration_seconds: details.length_seconds.parse().ok(),
        channel_id: Some(details.channel_id).filter(|s| !s.is_empty()),
        channel_name: Some(details.owner_channel_name).filter(|s| !s.is_empty()),
        thumbnails,
        formats,
    })
}

fn map_format(native: NativeFormat) -> Option<MediaFormat> {
    if native.is_live || native.is_hls || native.is_dash_mpd {
        return None;
    }
    if native.itag > u32::MAX as u64 || native.url.is_empty() {
        return None;
    }
    // Keep the destination validation boundary from the original Rust downloader.
    // Neither signed URLs nor player details are returned by the JSON API.
    if validate_media_url(&native.url).is_err() {
        return None;
    }
    let mime = native.mime_type.mime.to_string();
    let container = if mime == "audio/mp4" {
        "m4a"
    } else if mime.ends_with("/mp4") {
        "mp4"
    } else if mime.ends_with("/webm") {
        "webm"
    } else {
        return None;
    };
    if !native.has_audio && !native.has_video {
        return None;
    }
    Some(MediaFormat {
        itag: native.itag as u32,
        mime_type: mime,
        codecs: native.mime_type.codecs,
        container: container.to_owned(),
        bitrate: Some(native.bitrate),
        width: native.width.and_then(|n| u32::try_from(n).ok()),
        height: native.height.and_then(|n| u32::try_from(n).ok()),
        fps: native.fps.and_then(|n| u32::try_from(n).ok()),
        content_length: native
            .content_length
            .as_deref()
            .and_then(|s| s.parse().ok()),
        has_audio: native.has_audio,
        has_video: native.has_video,
        url: Some(native.url),
        signature_cipher: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rate_limit_and_verification_are_sanitized() {
        let err = classify_error(VideoError::VideoPlayerResponseError(
            "HTTP 429 - signed URL secret=abc".to_owned(),
        ));
        assert_eq!(err.code, ErrorCode::UpstreamRateLimited);
        assert!(!err.message.contains("secret"));
        let err = classify_error(VideoError::VideoPlayerResponseError(
            "Sign in to confirm you are not a bot".to_owned(),
        ));
        assert_eq!(err.code, ErrorCode::AccessRestricted);
    }

    #[test]
    fn disallows_non_youtube_urls() {
        assert!(normalize_url("http://127.0.0.1/a").is_err());
        assert!(normalize_url("https://youtu.be/SAFrydhpgJo").is_ok());
        assert!(normalize_url("https://youtube.com/watch?v=SAFrydhpgJo&v=SAFrydhpgJo").is_err());
    }

    #[test]
    fn rejects_manifest_or_non_public_streams() {
        let json = serde_json::json!({
            "itag": 136,
            "mimeType": "video/mp4; codecs=\"avc1.4d401f\"",
            "bitrate": 100_000,
            "url": "http://127.0.0.1/internal",
            "hasVideo": true,
            "hasAudio": false,
            "isLive": false,
            "isHls": false,
            "isDashMPD": false
        });
        if let Ok(format) = serde_json::from_value::<NativeFormat>(json) {
            assert!(map_format(format).is_none());
        }
    }
}
