//! Asynchronous Apify download jobs with bounded local output and FFmpeg conversion.
use std::{path::Path, sync::Arc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{fs, sync::watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use crate::{
    apify::ApifyClient,
    error::{AppError, ErrorCode, Result},
    extractor::normalize_url,
    media::Ffmpeg,
    model::{DownloadKind, DownloadRequest, OutputFormat},
    storage::{LocalStorage, Storage, StoredOutput},
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub stage: String,
}

#[derive(Clone)]
pub struct DownloadPipeline {
    apify: Arc<ApifyClient>,
    storage: LocalStorage,
    ffmpeg: Ffmpeg,
    max_bytes: u64,
}
impl DownloadPipeline {
    pub fn new(apify: Arc<ApifyClient>, storage: LocalStorage, ffmpeg: Ffmpeg, max_bytes: u64) -> Self {
        Self { apify, storage, ffmpeg, max_bytes }
    }
    pub async fn cleanup(&self, job: Uuid) -> Result<()> {
        self.storage.cleanup(job).await
    }
    pub async fn run(
        &self, request: &DownloadRequest, job: Uuid, cancel: CancellationToken,
        progress: watch::Sender<DownloadProgress>,
    ) -> Result<StoredOutput> {
        request.validate()?;
        let directory = self.storage.create_job(job).await?;
        let outcome = self.run_inner(request, job, &directory, &cancel, &progress).await;
        if outcome.is_err() {
            if let Err(error) = self.storage.cleanup(job).await {
                tracing::warn!(code = ?error.code, "Could not remove failed Apify job files");
            }
        }
        outcome
    }
    async fn run_inner(
        &self, request: &DownloadRequest, job: Uuid, dir: &Path,
        cancel: &CancellationToken, progress: &watch::Sender<DownloadProgress>,
    ) -> Result<StoredOutput> {
        let id = normalize_url(&request.url)?;
        let plan = OutputPlan::from_request(request)?;
        let canonical = format!("https://www.youtube.com/watch?v={id}");
        let _ = progress.send(DownloadProgress {stage: "extracting".into(), ..Default::default()});
        let input = json!({
            "urls": [{"url": canonical}],
            "format": plan.actor_format,
            "quality": plan.actor_quality,
            "residentialProxyMode": "disabled"
        });
        let rows = self.apify.run_items(&self.apify.downloader_actor, input, cancel).await?;
        let row = rows.iter().find(|row| row.get("videoId").and_then(Value::as_str) == Some(id.as_ref()))
            .ok_or_else(|| AppError::new(ErrorCode::NotFound, "Apify returned no result for the requested video"))?;
        if row.get("status").and_then(Value::as_str) != Some("success") {
            return Err(failure(row.get("errorCode").and_then(Value::as_str).unwrap_or("")));
        }
        let url = row.get("downloadUrl").and_then(Value::as_str).filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::new(ErrorCode::DownloadInterrupted, "Apify did not provide a downloadable file"))?;
        let expected = row.get("fileSizeBytes").and_then(Value::as_u64).filter(|n| *n > 0);
        let input_path = dir.join(if plan.is_audio { "input.mp3" } else { "input.mp4" });
        let transferred = self.apify.download_media(url, &input_path, expected, self.max_bytes, cancel, progress).await?;
        let remaining = self.max_bytes.saturating_sub(transferred);
        if remaining == 0 {
            return Err(AppError::new(ErrorCode::DownloadTooLarge, "Media workspace budget exhausted"));
        }
        let output = dir.join(format!("processed.{}", plan.extension));
        let _ = progress.send(DownloadProgress {
            downloaded_bytes: transferred, total_bytes: expected, stage: "processing".into(),
        });
        self.ffmpeg.process(
            if plan.is_audio {None} else {Some(input_path.as_path())},
            if plan.is_audio {Some(input_path.as_path())} else {None},
            &output, plan.output_format, request.audio_bitrate_kbps, remaining, cancel,
        ).await?;
        if cancel.is_cancelled() {return Err(AppError::new(ErrorCode::Cancelled, "Download was cancelled"));}
        let stored = self.storage.publish(job, &output, plan.extension, plan.mime_type).await?;
        fs::remove_file(input_path).await.map_err(|_| AppError::new(ErrorCode::Storage, "Cannot remove temporary media"))?;
        let _ = progress.send(DownloadProgress {
            downloaded_bytes: transferred, total_bytes: Some(transferred), stage: "completed".into(),
        });
        Ok(stored)
    }
}
fn failure(code: &str) -> AppError {
    match code {
        "FILE_TOO_LARGE" => AppError::new(ErrorCode::DownloadTooLarge, "Apify rejected the video size"),
        "QUALITY_UNAVAILABLE" => AppError::new(ErrorCode::UnsupportedFormat, "The requested video quality is unavailable"),
        "VIDEO_UNAVAILABLE" | "PRIVATE_VIDEO" | "COPYRIGHT_BLOCKED" => AppError::new(ErrorCode::AccessRestricted, "Video is not publicly accessible"),
        "AGE_RESTRICTED" | "LOGIN_REQUIRED" | "BOT_DETECTION" | "HTTP_403" => AppError::new(ErrorCode::AccessRestricted, "Apify could not access this video"),
        "DOWNLOAD_TIMEOUT" | "CONFIG_TIMEOUT_TOO_LOW" => AppError::new(ErrorCode::Timeout, "Apify video download timed out"),
        _ => AppError::new(ErrorCode::Network, "Apify could not download this video"),
    }
}
#[derive(Debug)]
struct OutputPlan {
    actor_format: &'static str,
    actor_quality: &'static str,
    extension: &'static str,
    mime_type: &'static str,
    output_format: OutputFormat,
    is_audio: bool,
}
impl OutputPlan {
    fn from_request(request: &DownloadRequest) -> Result<Self> {
        let is_audio = request.kind == DownloadKind::Audio;
        let (extension, mime_type, output_format) = match (request.kind, request.format) {
            (DownloadKind::Video, OutputFormat::Original | OutputFormat::Mp4) => ("mp4", "video/mp4", OutputFormat::Mp4),
            (DownloadKind::Video, OutputFormat::Webm) => ("webm", "video/webm", OutputFormat::Webm),
            (DownloadKind::Audio, OutputFormat::Original | OutputFormat::Mp3) => ("mp3", "audio/mpeg", OutputFormat::Mp3),
            (DownloadKind::Audio, OutputFormat::M4a) => ("m4a", "audio/mp4", OutputFormat::M4a),
            _ => return Err(AppError::new(ErrorCode::UnsupportedFormat, "Requested type and format are incompatible")),
        };
        let quality = match request.quality.as_deref().unwrap_or("best") {
            "best" => "1080",
            "worst" => "360",
            value => {
                let height = value.strip_suffix('p').and_then(|s| s.parse::<u32>().ok())
                    .ok_or_else(|| AppError::invalid("Invalid video quality"))?;
                match height {
                    360..=479 => "360",
                    480..=719 => "480",
                    720..=1079 => "720",
                    1080 => "1080",
                    _ => return Err(AppError::new(ErrorCode::UnsupportedFormat, "Apify supports video resolutions from 360p to 1080p")),
                }
            }
        };
        Ok(Self { actor_format: if is_audio {"mp3"} else {"default"},
            actor_quality: quality, extension, mime_type, output_format, is_audio })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn request(kind: DownloadKind, format: OutputFormat, quality: Option<&str>) -> DownloadRequest {
        DownloadRequest {url: "https://youtu.be/dQw4w9WgXcQ".into(),kind,format,quality:quality.map(str::to_owned),audio_bitrate_kbps:None}
    }
    #[test]
    fn maps_public_formats_to_actor_and_local_outputs() {
        let plan=OutputPlan::from_request(&request(DownloadKind::Video,OutputFormat::Mp4,Some("720p"))).unwrap();
        assert_eq!((plan.actor_format,plan.actor_quality,plan.extension),("default","720","mp4"));
        let audio=OutputPlan::from_request(&request(DownloadKind::Audio,OutputFormat::M4a,None)).unwrap();
        assert_eq!((audio.actor_format,audio.extension),("mp3","m4a"));
        assert!(OutputPlan::from_request(&request(DownloadKind::Video,OutputFormat::Webm,Some("2160p"))).is_err());
    }
    #[test]
    fn actor_failures_are_redacted_and_classified() {
        assert_eq!(failure("HTTP_403").code, ErrorCode::AccessRestricted);
        assert_eq!(failure("FILE_TOO_LARGE").code, ErrorCode::DownloadTooLarge);
        assert!(!failure("UNKNOWN").message.contains("token"));
    }
}
