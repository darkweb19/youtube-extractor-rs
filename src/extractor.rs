//! Strict YouTube URL parsing; Apify supplies all upstream metadata.
use std::sync::Arc;
use async_trait::async_trait;
use url::Url;
use crate::{
    apify::ApifyClient,
    error::{AppError, Result},
    model::{VideoId, VideoInfo},
};
#[async_trait]
pub trait Extractor: Send + Sync {
    async fn video_info(&self, input: &str) -> Result<VideoInfo>;
}

pub struct ApifyExtractor {
    client: Arc<ApifyClient>,
}
impl ApifyExtractor {
    pub fn new(client: Arc<ApifyClient>) -> Self { Self { client } }
}
#[async_trait]
impl Extractor for ApifyExtractor {
    async fn video_info(&self, input: &str) -> Result<VideoInfo> {
        self.client.video_info(input).await
    }
}

/// Only canonical watch/short/live IDs are sent to the remote actor.
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


#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepted_youtube_links_are_normalized() {
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ",
            "https://youtube.com/shorts/dQw4w9WgXcQ",
            "https://youtube.com/live/dQw4w9WgXcQ"
        ] {
            assert_eq!(normalize_url(url).unwrap().as_ref(), "dQw4w9WgXcQ");
        }
    }
    #[test]
    fn foreign_hosts_and_ambiguous_ids_are_rejected() {
        for url in [
            "https://evil.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&v=aqz-KE-bpKQ",
            "http://127.0.0.1/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/%77atch?v=dQw4w9WgXcQ",
        ] { assert!(normalize_url(url).is_err()); }
    }
}
