//! Managed yt-dlp adapter. Axum owns auth, job lifecycle, storage and file delivery.
//! yt-dlp handles only public YouTube metadata, stream selection and media transfer.
use crate::{
    download::DownloadProgress,
    error::{AppError, ErrorCode, Result},
    extractor::{normalize_url, Extractor},
    jobs::JobExecutor,
    media::Ffmpeg,
    model::{
        DownloadKind, DownloadRequest, MediaFormat, OutputFormat, Thumbnail, VideoId, VideoInfo,
    },
    storage::{LocalStorage, Storage, StoredOutput},
};
use async_trait::async_trait;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, sync::watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Clone)]
pub struct YtDlp {
    executable: PathBuf,
    storage: LocalStorage,
    ffmpeg: Ffmpeg,
    timeout: Duration,
    max_bytes: u64,
}

impl YtDlp {
    pub fn new(
        executable: PathBuf,
        storage: LocalStorage,
        ffmpeg: Ffmpeg,
        timeout: Duration,
        max_bytes: u64,
    ) -> Self {
        Self {
            executable,
            storage,
            ffmpeg,
            timeout,
            max_bytes,
        }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.executable);
        cmd.args([
            "--ignore-config",
            "--no-playlist",
            "--no-warnings",
            "--no-progress",
            "--no-cache-dir",
            "--no-write-info-json",
            "--no-write-thumbnail",
            "--no-write-subs",
            "--no-write-auto-subs",
            "--js-runtimes",
            "node",
        ]);
        cmd.stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        cmd
    }

    async fn info(&self, id: &VideoId) -> Result<VideoInfo> {
        let mut cmd = self.command();
        cmd.args(["--dump-single-json", "--skip-download"]);
        cmd.arg("--")
            .arg(format!("https://www.youtube.com/watch?v={id}"));
        const MAX_METADATA_BYTES: usize = 8 * 1024 * 1024;
        cmd.stdout(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|_| AppError::new(ErrorCode::Internal, "yt-dlp executable is unavailable"))?;
        let stdout = child.stdout.take().ok_or_else(|| {
            AppError::new(ErrorCode::Internal, "yt-dlp metadata pipe is unavailable")
        })?;
        let read = async {
            let mut bytes = Vec::new();
            stdout
                .take((MAX_METADATA_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| AppError::new(ErrorCode::Network, "Failed to read yt-dlp metadata"))?;
            if bytes.len() > MAX_METADATA_BYTES {
                return Err(AppError::new(
                    ErrorCode::PlayerChanged,
                    "yt-dlp metadata exceeded the response size limit",
                ));
            }
            let status = child.wait().await.map_err(|_| {
                AppError::new(
                    ErrorCode::DownloadInterrupted,
                    "yt-dlp metadata worker failed",
                )
            })?;
            if !status.success() {
                return Err(AppError::new(
                    ErrorCode::AccessRestricted,
                    "yt-dlp could not retrieve metadata; the server may be restricted by YouTube",
                ));
            }
            Ok(bytes)
        };
        let outcome = tokio::time::timeout(self.timeout.min(Duration::from_secs(90)), read).await;
        let result = match outcome {
            Ok(result) => result,
            Err(_) => Err(AppError::new(
                ErrorCode::Timeout,
                "YouTube metadata request timed out",
            )),
        };
        if result.is_err() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        parse_info(&result?, id)
    }

    fn download_command(&self, request: &DownloadRequest, dir: &Path) -> Result<Command> {
        request.validate()?;
        let id = normalize_url(&request.url)?;
        let mut cmd = self.command();
        cmd.args(["--no-simulate", "--no-part", "--no-continue", "--no-mtime"]);
        cmd.arg("--max-filesize").arg(self.max_bytes.to_string());
        cmd.arg("-o").arg(dir.join("source.%(ext)s"));
        let extension = match request.kind {
            DownloadKind::Video => {
                let ext = if request.format == OutputFormat::Webm {
                    "webm"
                } else {
                    "mp4"
                };
                let audio_ext = if ext == "webm" { "webm" } else { "m4a" };
                let height = request
                    .quality
                    .as_deref()
                    .and_then(|q| q.strip_suffix('p'))
                    .and_then(|q| q.parse::<u32>().ok());
                let selector = match (height, request.quality.as_deref()) {
                    (Some(height), _) => format!("bv[ext={ext}][height<={height}]+ba[ext={audio_ext}]/b[ext={ext}][height<={height}]"),
                    (_, Some("worst")) => format!("wv[ext={ext}]+wa[ext={audio_ext}]/w[ext={ext}]"),
                    _ => format!("bv[ext={ext}]+ba[ext={audio_ext}]/b[ext={ext}]"),
                };
                cmd.args(["-f", &selector, "--merge-output-format", ext]);
                ext
            }
            DownloadKind::Audio => {
                cmd.args(["-f", "bestaudio/best", "-x"]);
                let format = match request.format {
                    OutputFormat::Mp3 => "mp3",
                    OutputFormat::M4a | OutputFormat::Original => "m4a",
                    _ => return Err(AppError::invalid("Incompatible audio format")),
                };
                cmd.args(["--audio-format", format]);
                if let Some(kbps) = request.audio_bitrate_kbps {
                    cmd.arg("--audio-quality").arg(format!("{kbps}K"));
                }
                format
            }
        };
        if !matches!(extension, "mp4" | "webm" | "mp3" | "m4a") {
            return Err(AppError::invalid("Invalid output format"));
        }
        cmd.arg("--")
            .arg(format!("https://www.youtube.com/watch?v={id}"));
        Ok(cmd)
    }

    async fn run_job(
        &self,
        request: &DownloadRequest,
        id: Uuid,
        cancel: &CancellationToken,
        progress: &watch::Sender<DownloadProgress>,
    ) -> Result<StoredOutput> {
        let dir = self.storage.create_job(id).await?;
        let result = self.download_job(request, id, &dir, cancel, progress).await;
        if result.is_err() {
            if self.storage.cleanup(id).await.is_err() {
                tracing::warn!(job_id = %id, "Failed to clean up incomplete yt-dlp job");
            }
        }
        result
    }

    async fn download_job(
        &self,
        request: &DownloadRequest,
        id: Uuid,
        dir: &Path,
        cancel: &CancellationToken,
        progress: &watch::Sender<DownloadProgress>,
    ) -> Result<StoredOutput> {
        let mut command = self.download_command(request, dir)?;
        command.stdout(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|_| AppError::new(ErrorCode::Internal, "yt-dlp executable is unavailable"))?;
        let mut quota_tick = tokio::time::interval(Duration::from_millis(250));
        let deadline = tokio::time::Instant::now() + self.timeout;
        let result: Result<()> = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    break Err(AppError::new(ErrorCode::Cancelled, "Download cancelled"));
                }
                _ = tokio::time::sleep_until(deadline) => {
                    break Err(AppError::new(ErrorCode::Timeout, "YouTube download timed out"));
                }
                _ = quota_tick.tick() => {
                    let bytes = workspace_bytes(dir).await?;
                    let _ = progress.send(DownloadProgress {
                        downloaded_bytes: bytes,
                        total_bytes: None,
                        stage: "downloading".into(),
                    });
                    if bytes >= self.max_bytes {
                        break Err(AppError::new(ErrorCode::DownloadTooLarge,
                            "Download exceeded the per-job storage limit"));
                    }
                }
                outcome = child.wait() => {
                    let status = outcome.map_err(|_| AppError::new(ErrorCode::DownloadInterrupted,
                        "yt-dlp process terminated unexpectedly"))?;
                    break if status.success() { Ok(()) } else {
                        Err(AppError::new(ErrorCode::AccessRestricted,
                            "yt-dlp could not download this public video; check local and hosting access"))
                    };
                }
            }
        };
        if result.is_err() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        result?;
        let ext = match request.kind {
            DownloadKind::Video if request.format == OutputFormat::Webm => "webm",
            DownloadKind::Video => "mp4",
            DownloadKind::Audio if request.format == OutputFormat::Mp3 => "mp3",
            DownloadKind::Audio => "m4a",
        };
        let output = dir.join(format!("source.{ext}"));
        let bytes = tokio::fs::metadata(&output)
            .await
            .map_err(|_| {
                AppError::new(
                    ErrorCode::UnsupportedFormat,
                    "yt-dlp produced no supported output file",
                )
            })?
            .len();
        if bytes == 0 || bytes >= self.max_bytes || workspace_bytes(dir).await? >= self.max_bytes {
            return Err(AppError::new(
                ErrorCode::DownloadTooLarge,
                "Output reached the storage limit",
            ));
        }
        let _ = progress.send(DownloadProgress {
            downloaded_bytes: bytes,
            total_bytes: Some(bytes),
            stage: "processing".into(),
        });
        let probe = self.ffmpeg.probe(&output, cancel).await?;
        let has_audio = probe.streams.iter().any(|s| s.codec_type == "audio");
        let has_video = probe.streams.iter().any(|s| s.codec_type == "video");
        if !has_audio
            || (request.kind == DownloadKind::Video && !has_video)
            || (request.kind == DownloadKind::Audio && has_video)
        {
            return Err(AppError::new(
                ErrorCode::FfmpegFailed,
                "Downloaded file has unexpected media streams",
            ));
        }
        if cancel.is_cancelled() {
            return Err(AppError::new(ErrorCode::Cancelled, "Download cancelled"));
        }
        let mime = match ext {
            "mp4" => "video/mp4",
            "webm" => "video/webm",
            "mp3" => "audio/mpeg",
            _ => "audio/mp4",
        };
        let stored = self.storage.publish(id, &output, ext, mime).await?;
        let _ = progress.send(DownloadProgress {
            downloaded_bytes: bytes,
            total_bytes: Some(bytes),
            stage: "completed".into(),
        });
        Ok(stored)
    }
}

async fn workspace_bytes(dir: &Path) -> Result<u64> {
    let mut reader = tokio::fs::read_dir(dir)
        .await
        .map_err(|_| AppError::new(ErrorCode::Storage, "Cannot inspect download workspace"))?;
    let mut total = 0_u64;
    while let Some(entry) = reader
        .next_entry()
        .await
        .map_err(|_| AppError::new(ErrorCode::Storage, "Cannot inspect download workspace"))?
    {
        let metadata = entry
            .metadata()
            .await
            .map_err(|_| AppError::new(ErrorCode::Storage, "Cannot inspect download workspace"))?;
        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

fn parse_info(json: &[u8], expected: &VideoId) -> Result<VideoInfo> {
    let data: Value = serde_json::from_slice(json)
        .map_err(|_| AppError::new(ErrorCode::PlayerChanged, "Invalid yt-dlp metadata response"))?;
    if data.get("id").and_then(Value::as_str) != Some(expected.as_ref()) {
        return Err(AppError::new(
            ErrorCode::PlayerChanged,
            "yt-dlp returned a different video",
        ));
    }
    let text = |key: &str| {
        data.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let optional = |key: &str| data.get(key).and_then(Value::as_str).map(str::to_owned);
    let formats = data
        .get("formats")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(parse_format).collect())
        .unwrap_or_default();
    let thumbnails = data
        .get("thumbnails")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|f| {
                    Some(Thumbnail {
                        url: f.get("url")?.as_str()?.to_owned(),
                        width: f
                            .get("width")
                            .and_then(Value::as_u64)
                            .and_then(|n| n.try_into().ok()),
                        height: f
                            .get("height")
                            .and_then(Value::as_u64)
                            .and_then(|n| n.try_into().ok()),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(VideoInfo {
        id: expected.clone(),
        title: text("title"),
        description: text("description"),
        duration_seconds: data
            .get("duration")
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && *n >= 0.0)
            .map(|n| n as u64),
        channel_id: optional("channel_id"),
        channel_name: optional("channel"),
        thumbnails,
        formats,
    })
}

fn parse_format(f: &Value) -> Option<MediaFormat> {
    let ext = f.get("ext")?.as_str()?;
    if !matches!(ext, "mp4" | "m4a" | "webm" | "mp3") {
        return None;
    }
    let vcodec = f.get("vcodec").and_then(Value::as_str).unwrap_or("none");
    let acodec = f.get("acodec").and_then(Value::as_str).unwrap_or("none");
    let has_video = vcodec != "none";
    let has_audio = acodec != "none";
    if !has_video && !has_audio {
        return None;
    }
    let codecs = [vcodec, acodec]
        .into_iter()
        .filter(|c| *c != "none")
        .map(str::to_owned)
        .collect();
    let itag = f
        .get("format_id")
        .and_then(Value::as_str)
        .and_then(|id| id.split('-').next()?.parse().ok())
        .unwrap_or(0);
    Some(MediaFormat {
        itag,
        mime_type: format!("{}/{}", if has_video { "video" } else { "audio" }, ext),
        codecs,
        container: ext.to_owned(),
        bitrate: f
            .get("tbr")
            .and_then(Value::as_f64)
            .map(|n| (n * 1000.0) as u64),
        width: f
            .get("width")
            .and_then(Value::as_u64)
            .and_then(|n| n.try_into().ok()),
        height: f
            .get("height")
            .and_then(Value::as_u64)
            .and_then(|n| n.try_into().ok()),
        fps: f.get("fps").and_then(Value::as_f64).map(|n| n as u32),
        content_length: f
            .get("filesize")
            .or_else(|| f.get("filesize_approx"))
            .and_then(Value::as_u64),
        has_audio,
        has_video,
        url: None,
        signature_cipher: None,
    })
}

#[async_trait]
impl Extractor for YtDlp {
    async fn video_info(&self, input: &str) -> Result<VideoInfo> {
        self.info(&normalize_url(input)?).await
    }
}

#[async_trait]
impl JobExecutor for YtDlp {
    async fn run(
        &self,
        request: &DownloadRequest,
        id: Uuid,
        cancel: CancellationToken,
        progress: watch::Sender<DownloadProgress>,
    ) -> Result<StoredOutput> {
        self.run_job(request, id, &cancel, &progress).await
    }
    async fn cleanup(&self, id: Uuid) -> Result<()> {
        self.storage.cleanup(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "SAFrydhpgJo";
    #[test]
    fn metadata_never_exposes_upstream_signed_urls() {
        let json = serde_json::json!({"id":ID,"title":"Test",
            "formats":[{"format_id":"136","ext":"mp4","vcodec":"avc1.4D401F",
                "acodec":"none","height":720,"url":"https://example/?token=private"},
                {"format_id":"140-20","ext":"m4a","vcodec":"none","acodec":"mp4a.40.2"}]});
        let info = parse_info(json.to_string().as_bytes(), &VideoId::new(ID).unwrap()).unwrap();
        assert_eq!(info.formats.len(), 2);
        assert!(!serde_json::to_string(&info).unwrap().contains("private"));
    }
    #[tokio::test]
    async fn mp4_selector_caps_height_and_requires_matching_audio_container() {
        let temp = tempfile::tempdir().unwrap();
        let storage = LocalStorage::new(temp.path().join("jobs")).await.unwrap();
        let worker = YtDlp::new(
            "yt-dlp".into(),
            storage,
            Ffmpeg::new("ffmpeg".into(), "ffprobe".into(), Duration::from_secs(1)),
            Duration::from_secs(3),
            1_024_000,
        );
        let request = DownloadRequest {
            url: "https://youtu.be/SAFrydhpgJo".into(),
            kind: DownloadKind::Video,
            format: OutputFormat::Mp4,
            quality: Some("720p".into()),
            audio_bitrate_kbps: None,
        };
        let command = worker.download_command(&request, temp.path()).unwrap();
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy())
            .collect();
        assert!(args
            .iter()
            .any(|arg| arg.contains("bv[ext=mp4][height<=720]+ba[ext=m4a]")));
        assert!(args.iter().any(|arg| arg == "--"));
        assert!(args
            .iter()
            .any(|arg| arg == "https://www.youtube.com/watch?v=SAFrydhpgJo"));
        assert!(!args.iter().any(|arg| arg.contains("youtu.be")));
    }

    #[test]
    fn rejects_mismatched_id_and_invalid_json() {
        let id = VideoId::new(ID).unwrap();
        assert!(parse_info(br#"{"id":"different12"}"#, &id).is_err());
        assert!(parse_info(b"invalid", &id).is_err());
    }
    #[test]
    fn rejects_non_youtube_input_before_process_spawn() {
        assert!(normalize_url("https://127.0.0.1/private").is_err());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn invokes_ytdlp_with_normalized_url_and_ignores_titles_as_filenames() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let program = temp.path().join("mock-yt-dlp");
        let invoked = temp.path().join("args");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '%s\\n' '{{\"id\":\"SAFrydhpgJo\",\"title\":\"../../unsafe title\",\"formats\":[{{\"format_id\":\"136\",\"ext\":\"mp4\",\"vcodec\":\"avc1\",\"acodec\":\"none\"}}]}}'\n",
            invoked.display()
        );
        tokio::fs::write(&program, script).await.unwrap();
        let mut permissions = tokio::fs::metadata(&program).await.unwrap().permissions();
        permissions.set_mode(0o700);
        tokio::fs::set_permissions(&program, permissions)
            .await
            .unwrap();
        let storage = LocalStorage::new(temp.path().join("jobs")).await.unwrap();
        let worker = YtDlp::new(
            program,
            storage,
            Ffmpeg::new("ffmpeg".into(), "ffprobe".into(), Duration::from_secs(3)),
            Duration::from_secs(3),
            1024 * 1024,
        );
        let info = worker
            .video_info("https://youtu.be/SAFrydhpgJo?si=anything")
            .await
            .unwrap();
        assert_eq!(info.title, "../../unsafe title");
        let args = tokio::fs::read_to_string(invoked).await.unwrap();
        assert!(args.contains("--ignore-config"));
        assert!(args.contains("--js-runtimes"));
        assert!(args.contains("node"));
        assert!(args.contains("https://www.youtube.com/watch?v=SAFrydhpgJo"));
        assert!(!args.contains("youtu.be"));
        assert!(!args.contains("anything"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_terminates_ytdlp_and_cleans_temp_job() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let program = temp.path().join("mock-yt-dlp");
        tokio::fs::write(&program, "#!/bin/sh\nexec sleep 30\n")
            .await
            .unwrap();
        let mut permissions = tokio::fs::metadata(&program).await.unwrap().permissions();
        permissions.set_mode(0o700);
        tokio::fs::set_permissions(&program, permissions)
            .await
            .unwrap();
        let storage = LocalStorage::new(temp.path().join("jobs")).await.unwrap();
        let worker = YtDlp::new(
            program,
            storage.clone(),
            Ffmpeg::new("ffmpeg".into(), "ffprobe".into(), Duration::from_secs(3)),
            Duration::from_secs(3),
            1024 * 1024,
        );
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let (tx, _rx) = watch::channel(DownloadProgress::default());
        let id = Uuid::new_v4();
        let request = DownloadRequest {
            url: "https://youtube.com/watch?v=SAFrydhpgJo".into(),
            kind: DownloadKind::Video,
            format: OutputFormat::Mp4,
            quality: Some("720p".into()),
            audio_bitrate_kbps: None,
        };
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            worker.run(&request, id, cancelled, tx),
        )
        .await
        .unwrap();
        assert_eq!(result.unwrap_err().code, ErrorCode::Cancelled);
        assert!(!storage.job_dir(id).exists());
    }
}
