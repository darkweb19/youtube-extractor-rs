//! Apify control-plane client and bounded retrieval of actor-produced media.
//! This module never sends the API credential to a non-Apify host.
use reqwest::{redirect::Policy, Client, StatusCode};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{fs::OpenOptions, io::AsyncWriteExt, sync::watch};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    download::DownloadProgress,
    error::{AppError, ErrorCode, Result},
    extractor::normalize_url,
    model::{MediaFormat, Thumbnail, VideoInfo},
};

#[derive(Clone)]
pub struct ApifyClient {
    http: Client,
    token: Arc<str>,
    pub downloader_actor: String,
    metadata_actor: String,
    media_hosts: Arc<Vec<String>>,
    timeout: Duration,
}

fn upstream(message: &'static str) -> AppError {
    AppError::new(ErrorCode::Network, message)
}
fn cancelled() -> AppError {
    AppError::new(ErrorCode::Cancelled, "Download was cancelled")
}
fn request_error(error: reqwest::Error) -> AppError {
    if error.is_timeout() {
        AppError::new(ErrorCode::Timeout, "Apify request timed out")
    } else {
        upstream("Apify request failed")
    }
}
fn status_error(status: StatusCode) -> AppError {
    match status.as_u16() {
        401 | 403 => AppError::new(
            ErrorCode::AccessRestricted,
            "Apify credentials or actor access denied",
        ),
        404 => AppError::new(ErrorCode::NotFound, "Apify resource is unavailable"),
        429 => AppError::new(
            ErrorCode::UpstreamRateLimited,
            "Apify request rate limit exceeded",
        ),
        408 | 504 => AppError::new(ErrorCode::Timeout, "Apify operation timed out"),
        _ => upstream("Apify returned an unsuccessful response"),
    }
}
fn actor_id(input: String) -> Result<String> {
    if input.is_empty()
        || input.len() > 128
        || !input
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'~' | b'.'))
    {
        return Err(AppError::invalid("Invalid Apify actor identifier"));
    }
    Ok(input.replace('/', "~"))
}
fn opaque_id(value: &str) -> Result<&str> {
    if value.is_empty() || value.len() > 128 || !value.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(upstream("Apify returned an invalid resource identifier"));
    }
    Ok(value)
}
impl ApifyClient {
    pub fn from_env(connect: Duration, read: Duration, timeout: Duration) -> Result<Self> {
        let token = std::env::var("APIFY_API_TOKEN")
            .map_err(|_| AppError::invalid("APIFY_API_TOKEN is required"))?;
        if token.trim().is_empty()
            || token != token.trim()
            || token.bytes().any(|b| b.is_ascii_control())
        {
            return Err(AppError::invalid(
                "APIFY_API_TOKEN must be a nonempty token",
            ));
        }
        let downloader_actor = actor_id(
            std::env::var("APIFY_DOWNLOAD_ACTOR")
                .unwrap_or_else(|_| "marielise.dev~youtube-video-downloader".into()),
        )?;
        let metadata_actor = actor_id(
            std::env::var("APIFY_METADATA_ACTOR")
                .unwrap_or_else(|_| "streamers~youtube-scraper".into()),
        )?;
        let additional = std::env::var("APIFY_MEDIA_HOSTS").unwrap_or_default();
        let mut media_hosts = vec!["api.apify.com".to_owned()];
        for host in additional
            .split(',')
            .map(str::trim)
            .filter(|h| !h.is_empty())
        {
            if host.len() > 253
                || host.starts_with('.')
                || host.ends_with('.')
                || !host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
            {
                return Err(AppError::invalid(
                    "APIFY_MEDIA_HOSTS must contain exact DNS hostnames",
                ));
            }
            media_hosts.push(host.to_ascii_lowercase());
        }
        let http = Client::builder()
            .redirect(Policy::none())
            .connect_timeout(connect)
            .read_timeout(read)
            .build()
            .map_err(|_| upstream("Cannot create Apify HTTP client"))?;
        Ok(Self {
            http,
            token: Arc::from(token),
            downloader_actor,
            metadata_actor,
            media_hosts: Arc::new(media_hosts),
            timeout,
        })
    }

    async fn json_response(&self, request: reqwest::RequestBuilder) -> Result<Value> {
        let response = request.send().await.map_err(request_error)?;
        if !response.status().is_success() {
            return Err(status_error(response.status()));
        }
        // Dataset requests are limited to 2 rows. Reject unexpected large API payloads.
        if response
            .content_length()
            .is_some_and(|n| n > 2 * 1024 * 1024)
        {
            return Err(upstream("Apify API response exceeded size limit"));
        }
        let mut response = response;
        let mut buffer = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(request_error)? {
            if buffer.len().saturating_add(chunk.len()) > 2 * 1024 * 1024 {
                return Err(upstream("Apify API response exceeded size limit"));
            }
            buffer.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&buffer).map_err(|_| upstream("Apify returned invalid JSON"))
    }

    async fn abort(&self, id: &str) {
        let url = format!("https://api.apify.com/v2/actor-runs/{id}/abort");
        let _ = self
            .http
            .post(url)
            .bearer_auth(self.token.as_ref())
            .send()
            .await;
    }

    pub async fn run_items(
        &self,
        actor: &str,
        input: Value,
        cancel: &CancellationToken,
    ) -> Result<Vec<Value>> {
        let deadline = Instant::now() + self.timeout;
        let url = format!("https://api.apify.com/v2/actors/{actor}/runs");
        let created = tokio::select! {
            _ = cancel.cancelled() => return Err(cancelled()),
            response = self.json_response(self.http.post(url).bearer_auth(self.token.as_ref())
                .json(&input).query(&[("timeout", self.timeout.as_secs().min(7 * 24 * 3600).to_string())])) => response?,
        };
        let run = created
            .get("data")
            .ok_or_else(|| upstream("Apify did not return a run"))?;
        let id = opaque_id(run.get("id").and_then(Value::as_str).unwrap_or(""))?.to_string();
        let mut run = run.clone();
        loop {
            if cancel.is_cancelled() {
                self.abort(&id).await;
                return Err(cancelled());
            }
            if Instant::now() >= deadline {
                self.abort(&id).await;
                return Err(AppError::new(
                    ErrorCode::Timeout,
                    "Apify actor run timed out",
                ));
            }
            match run.get("status").and_then(Value::as_str).unwrap_or("") {
                "SUCCEEDED" => break,
                "FAILED" | "ABORTED" | "TIMED-OUT" => {
                    return Err(upstream("Apify actor could not complete the request"))
                }
                "READY" | "RUNNING" | "" => (),
                _ => return Err(upstream("Apify returned an unknown run status")),
            }
            tokio::select! {
                _ = cancel.cancelled() => { self.abort(&id).await; return Err(cancelled()); },
                _ = tokio::time::sleep(Duration::from_secs(2)) => (),
            }
            let url = format!("https://api.apify.com/v2/actor-runs/{id}");
            let response = tokio::select! {
                _ = cancel.cancelled() => { self.abort(&id).await; return Err(cancelled()); },
                result = self.json_response(self.http.get(url).bearer_auth(self.token.as_ref())) => result?,
            };
            run = response
                .get("data")
                .cloned()
                .ok_or_else(|| upstream("Apify run status was missing"))?;
        }
        let dataset = opaque_id(
            run.get("defaultDatasetId")
                .and_then(Value::as_str)
                .unwrap_or(""),
        )?;
        let url = format!("https://api.apify.com/v2/datasets/{dataset}/items?clean=true&limit=2");
        let result = tokio::select! {
            _ = cancel.cancelled() => return Err(cancelled()),
            response = self.json_response(self.http.get(url).bearer_auth(self.token.as_ref())) => response?,
        };
        result
            .as_array()
            .cloned()
            .ok_or_else(|| upstream("Apify dataset did not return an array"))
    }

    fn check_media_url(&self, url: &Url) -> Result<()> {
        let host = url
            .host_str()
            .ok_or_else(|| upstream("Apify supplied an invalid media host"))?;
        let allowed = self.media_hosts.iter().any(|candidate| candidate == host);
        if url.scheme() != "https"
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || !allowed
            || (host == "api.apify.com" && !url.path().starts_with("/v2/key-value-stores/"))
        {
            return Err(upstream(
                "Apify supplied a media URL outside the trusted HTTPS hosts",
            ));
        }
        Ok(())
    }

    pub async fn download_media(
        &self,
        input: &str,
        path: &Path,
        expected: Option<u64>,
        max_bytes: u64,
        cancel: &CancellationToken,
        progress: &watch::Sender<DownloadProgress>,
    ) -> Result<u64> {
        let mut url =
            Url::parse(input).map_err(|_| upstream("Apify supplied an invalid media URL"))?;
        let mut response = None;
        for _ in 0..=5 {
            self.check_media_url(&url)?;
            let mut request = self.http.get(url.clone());
            if url.host_str() == Some("api.apify.com") {
                request = request.bearer_auth(self.token.as_ref());
            }
            let received = tokio::select! {
                _ = cancel.cancelled() => return Err(cancelled()),
                value = request.send() => value.map_err(request_error)?,
            };
            if received.status().is_redirection() {
                let location = received
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| upstream("Apify media redirect is invalid"))?;
                url = url
                    .join(location)
                    .map_err(|_| upstream("Apify media redirect is invalid"))?;
                continue;
            }
            if received.status() != StatusCode::OK {
                return Err(status_error(received.status()));
            }
            response = Some(received);
            break;
        }
        let mut response =
            response.ok_or_else(|| upstream("Apify media redirect limit exceeded"))?;
        if expected.is_some_and(|n| n >= max_bytes)
            || response.content_length().is_some_and(|n| n >= max_bytes)
        {
            return Err(AppError::new(
                ErrorCode::DownloadTooLarge,
                "Apify media exceeds job byte limit",
            ));
        }
        let declared = response.content_length().or(expected);
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .await
            .map_err(|_| AppError::new(ErrorCode::Storage, "Unable to create media file"))?;
        let mut bytes = 0u64;
        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => return Err(cancelled()),
                next = response.chunk() => next.map_err(request_error)?,
            };
            let Some(chunk) = chunk else { break };
            bytes = bytes.checked_add(chunk.len() as u64).ok_or_else(|| {
                AppError::new(ErrorCode::DownloadTooLarge, "Media size limit exceeded")
            })?;
            if bytes >= max_bytes || expected.is_some_and(|n| bytes > n) {
                return Err(AppError::new(
                    ErrorCode::DownloadTooLarge,
                    "Apify media exceeds job byte limit",
                ));
            }
            file.write_all(&chunk)
                .await
                .map_err(|_| AppError::new(ErrorCode::Storage, "Unable to write media file"))?;
            let _ = progress.send(DownloadProgress {
                downloaded_bytes: bytes,
                total_bytes: declared,
                stage: "downloading".into(),
            });
        }
        if bytes == 0
            || declared.is_some_and(|n| bytes != n)
            || expected.is_some_and(|n| bytes != n)
        {
            return Err(AppError::new(
                ErrorCode::DownloadInterrupted,
                "Apify media transfer was incomplete",
            ));
        }
        file.flush()
            .await
            .map_err(|_| AppError::new(ErrorCode::Storage, "Unable to flush media file"))?;
        Ok(bytes)
    }

    pub async fn video_info(&self, input: &str) -> Result<VideoInfo> {
        let id = normalize_url(input)?;
        let url = format!("https://www.youtube.com/watch?v={id}");
        let input = json!({"startUrls": [{"url": url}], "maxResults": 1, "maxResultsShorts": 1, "maxResultStreams": 0});
        let rows = self
            .run_items(&self.metadata_actor, input, &CancellationToken::new())
            .await?;
        let item = rows
            .iter()
            .find(|row| row.get("id").and_then(Value::as_str) == Some(id.as_ref()))
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::NotFound,
                    "Apify could not retrieve metadata for this video",
                )
            })?;
        if item.get("error").is_some_and(|v| !v.is_null()) {
            return Err(AppError::new(
                ErrorCode::AccessRestricted,
                "Apify could not access this video's metadata",
            ));
        }
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| upstream("Apify metadata did not contain a title"))?;
        let thumbnail = item
            .get("thumbnailUrl")
            .and_then(Value::as_str)
            .filter(|s| s.starts_with("https://"))
            .map(|url| Thumbnail {
                url: url.to_owned(),
                width: None,
                height: None,
            });
        let duration = item
            .get("durationSeconds")
            .and_then(Value::as_u64)
            .or_else(|| {
                item.get("duration")
                    .and_then(Value::as_str)
                    .and_then(parse_duration)
            });
        Ok(VideoInfo {
            id,
            title: title.to_owned(),
            description: item
                .get("text")
                .or_else(|| item.get("description"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            duration_seconds: duration,
            channel_id: item
                .get("channelId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            channel_name: item
                .get("channelName")
                .and_then(Value::as_str)
                .map(str::to_owned),
            thumbnails: thumbnail.into_iter().collect(),
            // Actor exposes delivery presets, not YouTube itags or raw playback URLs.
            formats: Vec::<MediaFormat>::new(),
        })
    }
}

fn parse_duration(duration: &str) -> Option<u64> {
    duration.split(':').try_fold(0u64, |acc, segment| {
        acc.checked_mul(60)?
            .checked_add(segment.parse::<u64>().ok()?)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duration_parser_is_bounded() {
        assert_eq!(parse_duration("1:02:03"), Some(3723));
        assert_eq!(parse_duration("3:49"), Some(229));
        assert_eq!(parse_duration("not a duration"), None);
    }
    #[test]
    fn actors_and_ids_do_not_accept_url_or_path_injection() {
        assert!(actor_id("owner~actor".into()).is_ok());
        assert!(actor_id("../actor".into()).is_err());
        assert!(opaque_id("abc123").is_ok());
        assert!(opaque_id("abc/123").is_err());
    }
}
