//! Native parsing of the publicly served watch page's player response.
//! No private playback API, authentication emulation, JS challenge execution or access bypass.
use crate::{
    error::{AppError, ErrorCode, Result},
    model::{MediaFormat, Thumbnail, VideoId, VideoInfo},
    network::{validate_media_url, Destination, SafeHttpClient},
};
use async_trait::async_trait;
use serde::Deserialize;
use url::Url;

#[async_trait]
pub trait Extractor: Send + Sync {
    async fn video_info(&self, input: &str) -> Result<VideoInfo>;
}

pub struct YoutubeExtractor {
    http: SafeHttpClient,
    max_response_bytes: usize,
}
impl YoutubeExtractor {
    pub fn new(http: SafeHttpClient, max_response_bytes: usize) -> Self {
        Self {
            http,
            max_response_bytes,
        }
    }
}
#[async_trait]
impl Extractor for YoutubeExtractor {
    async fn video_info(&self, input: &str) -> Result<VideoInfo> {
        let id = normalize_url(input)?;
        let url = format!("https://www.youtube.com/watch?v={id}");
        let bytes = self
            .http
            .get_bytes(&url, Destination::Youtube, self.max_response_bytes)
            .await?;
        let html =
            std::str::from_utf8(&bytes).map_err(|_| changed("Watch page is not valid UTF-8"))?;
        parse_watch_page(html, &id)
    }
}

/// Accept only exact, unambiguous YouTube resource paths. All requests use a reconstructed HTTPS URL.
pub fn normalize_url(input: &str) -> Result<VideoId> {
    if input.len() > 2048
        || input.trim() != input
        || input.bytes().any(|b| b.is_ascii_control() || b == b'\\')
    {
        return Err(AppError::invalid("Invalid YouTube URL"));
    }
    let url = Url::parse(input).map_err(|_| AppError::invalid("A full YouTube URL is required"))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return Err(AppError::invalid(
            "YouTube URL must use HTTP or HTTPS without credentials or a custom port",
        ));
    }
    // Reject encoded/dot path normalization rather than interpreting alternate resource paths.
    let authority_end = input.find("://").map(|i| i + 3).unwrap_or(0);
    let raw_tail = &input[authority_end..];
    if raw_tail
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .contains('@')
    {
        return Err(AppError::invalid("YouTube URL must not contain userinfo"));
    }
    let raw_path = raw_tail.find('/').map(|i| &raw_tail[i..]).unwrap_or("");
    let raw_path = raw_path.split(['?', '#']).next().unwrap_or("");
    if raw_path.contains('%') || raw_path.split('/').any(|s| matches!(s, "." | "..")) {
        return Err(AppError::invalid(
            "Encoded or normalized YouTube paths are unsupported",
        ));
    }
    let host = url.host_str().unwrap_or("");
    let video_params: Vec<_> = url.query_pairs().filter(|(k, _)| k == "v").collect();
    match (host, url.path()) {
        ("youtube.com" | "www.youtube.com" | "m.youtube.com", "/watch") => {
            if video_params.len() != 1 {
                return Err(AppError::invalid(
                    "Watch URL must have exactly one video identifier",
                ));
            }
            VideoId::new(video_params[0].1.to_string())
        }
        ("youtu.be", path) => {
            if !video_params.is_empty() {
                return Err(AppError::invalid("Conflicting video identifiers"));
            }
            VideoId::new(path.strip_prefix('/').unwrap_or("").to_string())
        }
        ("youtube.com" | "www.youtube.com" | "m.youtube.com", path) => {
            if !video_params.is_empty() {
                return Err(AppError::invalid("Conflicting video identifiers"));
            }
            let id = path
                .strip_prefix("/shorts/")
                .or_else(|| path.strip_prefix("/live/"))
                .ok_or_else(|| AppError::invalid("Unsupported YouTube URL path"))?;
            VideoId::new(id.to_string())
        }
        _ => Err(AppError::invalid(
            "URL must identify a video on youtube.com or youtu.be",
        )),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayerResponse {
    playability_status: Playability,
    video_details: Option<VideoDetails>,
    streaming_data: Option<StreamingData>,
}
#[derive(Deserialize)]
struct Playability {
    status: String,
    reason: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VideoDetails {
    video_id: String,
    title: String,
    #[serde(default)]
    short_description: String,
    length_seconds: Option<String>,
    channel_id: Option<String>,
    author: Option<String>,
    #[serde(default)]
    is_live: bool,
    #[serde(default)]
    is_live_content: bool,
    thumbnail: Option<Thumbnails>,
}
#[derive(Deserialize)]
struct Thumbnails {
    #[serde(default)]
    thumbnails: Vec<Thumbnail>,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct StreamingData {
    #[serde(default)]
    formats: Vec<PlayerFormat>,
    #[serde(default)]
    adaptive_formats: Vec<PlayerFormat>,
    hls_manifest_url: Option<String>,
    dash_manifest_url: Option<String>,
    #[serde(default)]
    drm_families: Vec<serde_json::Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayerFormat {
    itag: u32,
    mime_type: String,
    bitrate: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    fps: Option<u32>,
    content_length: Option<String>,
    url: Option<String>,
    signature_cipher: Option<String>,
    cipher: Option<String>,
    audio_quality: Option<String>,
    audio_sample_rate: Option<String>,
    #[serde(default)]
    drm_families: Vec<serde_json::Value>,
    drm_track_type: Option<String>,
}

fn changed(message: &str) -> AppError {
    AppError::new(ErrorCode::PlayerChanged, message)
}
fn unsupported(message: &str) -> AppError {
    AppError::new(ErrorCode::UnsupportedFormat, message)
}

/// Parse an HTML watch response. The scan respects nested objects, quoted braces and JSON escapes.
pub fn parse_watch_page(html: &str, expected: &VideoId) -> Result<VideoInfo> {
    let mut remaining = html;
    let marker = "ytInitialPlayerResponse";
    while let Some(pos) = remaining.find(marker) {
        let tail = &remaining[pos + marker.len()..];
        let assignment = tail.trim_start_matches([' ', '\n', '\r', '\t', '"', '\'', ']']);
        if let Some(value) = assignment
            .strip_prefix('=')
            .or_else(|| assignment.strip_prefix(':'))
        {
            let value = value.trim_start();
            if value.starts_with('{') {
                let json = balanced_object(value)
                    .ok_or_else(|| changed("Truncated or malformed player response"))?;
                return parse_player_response(json, expected);
            }
        }
        remaining = tail;
    }
    let lower = html.to_ascii_lowercase();
    if [
        "sign in to confirm",
        "unusual traffic",
        "g-recaptcha",
        "consent.youtube.com",
        "verify you are human",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return Err(AppError::new(
            ErrorCode::AccessRestricted,
            "YouTube requires consent, sign-in or automated-access verification",
        ));
    }
    Err(changed(
        "Public watch page does not contain a supported player response",
    ))
}

fn balanced_object(input: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for (i, b) in input.bytes().enumerate() {
        if quoted {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                quoted = false;
            }
        } else {
            match b {
                b'"' => quoted = true,
                b'{' => depth = depth.checked_add(1)?,
                b'}' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(&input[..=i]);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

pub fn parse_player_response(json: &str, expected: &VideoId) -> Result<VideoInfo> {
    let player: PlayerResponse = serde_json::from_str(json)
        .map_err(|_| changed("Player response schema is unsupported or malformed"))?;
    if player.playability_status.status != "OK" {
        let status = player.playability_status.status.as_str();
        // Use the reason only for classification; raw upstream strings can contain sensitive data.
        let reason = player
            .playability_status
            .reason
            .unwrap_or_default()
            .to_ascii_lowercase();
        if reason.contains("too many requests") || reason.contains("rate limit") {
            return Err(AppError::new(
                ErrorCode::UpstreamRateLimited,
                "YouTube rate-limited playback",
            ));
        }
        return Err(match status {
            "LOGIN_REQUIRED" | "AGE_CHECK_REQUIRED" | "CONTENT_CHECK_REQUIRED" | "UNPLAYABLE" | "ERROR" =>
                AppError::new(ErrorCode::AccessRestricted, "YouTube content is unavailable or requires authentication, consent or verification"),
            "LIVE_STREAM_OFFLINE" => unsupported("Live broadcasts without a playable VOD are unsupported"),
            _ => changed("Unknown YouTube playability status"),
        });
    }
    let details = player
        .video_details
        .ok_or_else(|| changed("Player response has no video metadata"))?;
    let id = VideoId::new(details.video_id)
        .map_err(|_| changed("Player response contains an invalid video identifier"))?;
    if id != *expected {
        return Err(changed("Player response belongs to a different video"));
    }
    if details.is_live {
        return Err(unsupported(
            "Active live broadcasts are unsupported; finalized replay VODs may be supported",
        ));
    }
    let streams = player
        .streaming_data
        .ok_or_else(|| unsupported("No downloadable streams are exposed"))?;
    if !streams.drm_families.is_empty() {
        return Err(unsupported("DRM-protected streams are unsupported"));
    }
    let has_manifests = streams.hls_manifest_url.is_some() || streams.dash_manifest_url.is_some();
    let mut formats = Vec::new();
    for format in streams.formats.into_iter().chain(streams.adaptive_formats) {
        if !format.drm_families.is_empty() || format.drm_track_type.is_some() {
            continue;
        }
        if let Some(parsed) = parse_format(format)? {
            formats.push(parsed);
        }
    }
    if formats.is_empty() {
        return Err(unsupported(if has_manifests {
            "Manifest-only streams are unsupported in this release"
        } else {
            "No supported direct streams; signature or throttling JavaScript resolution may be required"
        }));
    }
    // isLiveContent also marks completed replays. Only isLive is an active-live rejection.
    let _is_replay = details.is_live_content;
    Ok(VideoInfo {
        id,
        title: details.title,
        description: details.short_description,
        duration_seconds: details.length_seconds.and_then(|s| s.parse().ok()),
        channel_id: details.channel_id,
        channel_name: details.author,
        thumbnails: details.thumbnail.map(|t| t.thumbnails).unwrap_or_default(),
        formats,
    })
}

fn parse_format(format: PlayerFormat) -> Result<Option<MediaFormat>> {
    let (base, attributes) = format
        .mime_type
        .split_once(';')
        .unwrap_or((&format.mime_type, ""));
    let base = base.trim();
    let container = match base {
        "audio/mp4" => "m4a",
        "video/mp4" => "mp4",
        "audio/webm" | "video/webm" => "webm",
        _ => return Ok(None),
    };
    let codecs: Vec<String> = attributes
        .split(';')
        .find_map(|a| a.trim().strip_prefix("codecs="))
        .map(|s| {
            s.trim_matches('"')
                .split(',')
                .map(|c| c.trim().to_string())
                .collect()
        })
        .unwrap_or_default();
    let has_video = base.starts_with("video/");
    let has_audio = base.starts_with("audio/")
        || format.audio_quality.is_some()
        || format.audio_sample_rate.is_some()
        || codecs
            .iter()
            .any(|c| c.starts_with("mp4a") || c.starts_with("opus") || c.starts_with("vorbis"));
    let cipher = format.signature_cipher.or(format.cipher);
    let direct_url = if let Some(url) = format.url {
        Some(url)
    } else if let Some(ref cipher) = cipher {
        let pairs: Vec<_> = url::form_urlencoded::parse(cipher.as_bytes()).collect();
        // An s value needs a changing JS decipher algorithm, explicitly deferred.
        if pairs.iter().any(|(k, _)| k == "s") {
            return Ok(None);
        }
        let urls: Vec<_> = pairs.iter().filter(|(k, _)| k == "url").collect();
        if urls.len() != 1 {
            return Err(changed("Malformed stream cipher URL"));
        }
        let mut parsed = validate_media_url(&urls[0].1)?;
        if let Some((_, signature)) = pairs.iter().find(|(k, _)| k == "sig" || k == "signature") {
            let key = pairs
                .iter()
                .find(|(k, _)| k == "sp")
                .map(|(_, v)| v.as_ref())
                .unwrap_or("signature");
            if !matches!(key, "sig" | "signature") {
                return Ok(None);
            }
            parsed.query_pairs_mut().append_pair(key, signature);
        }
        Some(parsed.to_string())
    } else {
        None
    };
    let Some(direct_url) = direct_url else {
        return Ok(None);
    };
    let url = validate_media_url(&direct_url)?;
    if url.query_pairs().any(|(k, _)| k == "n") {
        return Ok(None);
    }
    Ok(Some(MediaFormat {
        itag: format.itag,
        mime_type: format.mime_type.clone(),
        codecs,
        container: container.into(),
        bitrate: format.bitrate,
        width: format.width,
        height: format.height,
        fps: format.fps,
        content_length: format.content_length.and_then(|n| n.parse().ok()),
        has_audio,
        has_video,
        url: Some(url.into()),
        signature_cipher: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "abcDEFG_12-";
    #[test]
    fn parses_supported_input_paths_and_rejects_ambiguity() {
        for url in [
            format!("https://www.youtube.com/watch?v={ID}&t=10"),
            format!("https://youtu.be/{ID}?si=abc"),
            format!("http://m.youtube.com/shorts/{ID}"),
            format!("https://youtube.com/live/{ID}"),
        ] {
            assert_eq!(normalize_url(&url).unwrap().as_ref(), ID);
        }
        for url in [
            "https://www.youtube.com.evil/watch?v=abcDEFG_12-",
            "https://youtube.com@127.0.0.1/watch?v=abcDEFG_12-",
            "https://youtube.com:444/watch?v=abcDEFG_12-",
            "https://@youtube.com/watch?v=abcDEFG_12-",
            "https://youtu.be/abcDEFG_12-/extra",
            "https://youtube.com/watch?v=abcDEFG_12-&v=abcDEFG_12-",
            "https://youtube.com/live/abcDEFG_12-?v=abcDEFG_12-",
            "https://youtube.com/shorts/%61bcDEFG_12-",
            "https://youtube.com/a/../watch?v=abcDEFG_12-",
            "https://127.0.0.1/watch?v=abcDEFG_12-",
            "https://youtube.com/watch?v=short",
            "https://youtube.com/watch?V=abcDEFG_12-",
            "https://youtu.be/abcDEFG_12-?v=abcDEFG_12-",
            "https://youtube.com/watch?v=abcDEFG_12-%0a",
        ] {
            assert!(normalize_url(url).is_err(), "{url}");
        }
    }
    #[test]
    fn parses_fixture_and_escaped_balanced_json() {
        let info = parse_watch_page(
            include_str!("../tests/fixtures/player-watch.html"),
            &VideoId::new(ID).unwrap(),
        )
        .unwrap();
        assert_eq!(info.title, "Fixture {video} with \"quotes\" and \\ escape");
        assert_eq!(info.formats.len(), 3);
        assert_eq!(info.formats[0].fps, Some(30));
        assert!(info.formats[0].has_audio && info.formats[0].has_video);
        assert_eq!(info.formats[1].container, "m4a");
        assert_eq!(info.formats[2].height, Some(1080));
        assert!(!info.formats[2].has_audio);
        assert_eq!(info.duration_seconds, Some(42));
    }
    #[test]
    fn distinguishes_access_protocol_and_unsupported_streams() {
        let id = VideoId::new(ID).unwrap();
        assert_eq!(
            parse_player_response(
                include_str!("../tests/fixtures/player-restricted.json"),
                &id
            )
            .unwrap_err()
            .code,
            ErrorCode::AccessRestricted
        );
        assert_eq!(
            parse_watch_page("<p>Sign in to confirm you are not a bot</p>", &id)
                .unwrap_err()
                .code,
            ErrorCode::AccessRestricted
        );
        assert_eq!(
            parse_watch_page("<p>changed page</p>", &id)
                .unwrap_err()
                .code,
            ErrorCode::PlayerChanged
        );
        let mut json: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/player-direct.json")).unwrap();
        json["videoDetails"]["videoId"] = "different12".into();
        assert_eq!(
            parse_player_response(&json.to_string(), &id)
                .unwrap_err()
                .code,
            ErrorCode::PlayerChanged
        );
        json["videoDetails"]["videoId"] = ID.into();
        json["videoDetails"]["isLive"] = true.into();
        assert_eq!(
            parse_player_response(&json.to_string(), &id)
                .unwrap_err()
                .code,
            ErrorCode::UnsupportedFormat
        );
    }
    #[test]
    fn rejects_drm_signature_n_transform_and_manifest_only() {
        let id = VideoId::new(ID).unwrap();
        let original: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/player-direct.json")).unwrap();
        for streaming in [
            serde_json::json!({"hlsManifestUrl":"https://example.invalid/playlist.m3u8"}),
            serde_json::json!({"drmFamilies":["WIDEVINE"]}),
            serde_json::json!({"formats":[{"itag":18,"mimeType":"video/mp4","signatureCipher":"s=secret&url=https%3A%2F%2Fr1.googlevideo.com%2Fvideoplayback"}]}),
            serde_json::json!({"formats":[{"itag":18,"mimeType":"video/mp4","url":"https://r1.googlevideo.com/videoplayback?n=secret"}]}),
        ] {
            let mut json = original.clone();
            json["streamingData"] = streaming;
            assert_eq!(
                parse_player_response(&json.to_string(), &id)
                    .unwrap_err()
                    .code,
                ErrorCode::UnsupportedFormat
            );
        }
    }
    #[test]
    fn bounded_scanner_rejects_truncation_and_handles_window_assignment() {
        let id = VideoId::new(ID).unwrap();
        let json = include_str!("../tests/fixtures/player-direct.json");
        assert!(parse_watch_page(
            &format!("window[\"ytInitialPlayerResponse\"] = {json}; trailing"),
            &id
        )
        .is_ok());
        assert_eq!(
            parse_watch_page("ytInitialPlayerResponse = {\"x\": \"unterminated", &id)
                .unwrap_err()
                .code,
            ErrorCode::PlayerChanged
        );
    }
}
