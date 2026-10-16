//! FFmpeg is used only on completed local files, never as a network client.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use serde::Deserialize;
use tokio::{fs, io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;

use crate::{
    error::{AppError, ErrorCode, Result},
    model::OutputFormat,
};

const INPUT_FORMATS: &str = "mov,matroska,webm,mp3,ogg,aac,wav,flac";

#[derive(Clone, Debug)]
pub struct Ffmpeg {
    binary: PathBuf,
    probe_binary: PathBuf,
    timeout: Duration,
}

#[derive(Debug, Deserialize)]
pub struct MediaProbe {
    pub streams: Vec<ProbeStream>,
    pub format: ProbeFormat,
}

#[derive(Debug, Deserialize)]
pub struct ProbeFormat {
    pub duration: Option<String>,
}

impl MediaProbe {
    fn duration_seconds(&self) -> Option<f64> {
        self.format
            .duration
            .as_ref()?
            .parse::<f64>()
            .ok()
            .filter(|d| d.is_finite() && *d > 0.0)
    }
}

#[derive(Debug, Deserialize)]
pub struct ProbeStream {
    pub codec_type: String,
    pub codec_name: Option<String>,
    pub bit_rate: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl Ffmpeg {
    pub fn new(binary: PathBuf, probe_binary: PathBuf, timeout: Duration) -> Self {
        Self {
            binary,
            probe_binary,
            timeout,
        }
    }

    // Keep the process boundary explicit: local inputs, output encoding,
    // resource budget, and cancellation are independently supplied by the job.
    #[allow(clippy::too_many_arguments)]
    pub async fn process(
        &self,
        video: Option<&Path>,
        audio: Option<&Path>,
        output: &Path,
        format: OutputFormat,
        audio_bitrate_kbps: Option<u32>,
        max_bytes: u64,
        cancel: &CancellationToken,
    ) -> Result<MediaProbe> {
        let expected_duration = self
            .probe(video.or(audio).ok_or_else(ffmpeg_error)?, cancel)
            .await?
            .duration_seconds();
        let mut args: Vec<OsString> = [
            "-nostdin",
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-max_alloc",
            "67108864",
            "-threads",
            "1",
            "-filter_threads",
            "1",
            "-filter_complex_threads",
            "1",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        let mut input_count = 0;
        for input in [video, audio].into_iter().flatten() {
            let path = local_file(input).await?;
            args.extend([
                "-protocol_whitelist".into(),
                "file".into(),
                "-format_whitelist".into(),
                INPUT_FORMATS.into(),
                "-max_streams".into(),
                "8".into(),
                "-probesize".into(),
                "5000000".into(),
                "-analyzeduration".into(),
                "5000000".into(),
                "-i".into(),
                path.into_os_string(),
            ]);
            input_count += 1;
        }
        if input_count == 0 {
            return Err(AppError::new(
                ErrorCode::FfmpegFailed,
                "No local media input",
            ));
        }
        if video.is_some() {
            args.extend(["-map".into(), "0:v:0".into()]);
            if audio.is_some() {
                args.extend(["-map".into(), "1:a:0".into()]);
            } else {
                args.extend(["-map".into(), "0:a:0?".into()]);
            }
            args.extend(["-c".into(), "copy".into()]);
        } else {
            args.extend(["-map".into(), "0:a:0".into(), "-vn".into()]);
            match format {
                OutputFormat::Mp3 => args.extend([
                    "-c:a".into(),
                    "libmp3lame".into(),
                    "-b:a".into(),
                    format!("{}k", audio_bitrate_kbps.unwrap_or(192)).into(),
                ]),
                OutputFormat::M4a => {
                    let probe = self.probe(audio.expect("input checked"), cancel).await?;
                    if audio_bitrate_kbps.is_none()
                        && probe.streams.iter().any(|stream| {
                            stream.codec_type == "audio"
                                && stream.codec_name.as_deref() == Some("aac")
                        })
                    {
                        args.extend(["-c:a".into(), "copy".into()]);
                    } else {
                        args.extend([
                            "-c:a".into(),
                            "aac".into(),
                            "-b:a".into(),
                            format!("{}k", audio_bitrate_kbps.unwrap_or(192)).into(),
                        ]);
                    }
                }
                _ => args.extend(["-c:a".into(), "copy".into()]),
            }
        }
        if matches!(format, OutputFormat::Mp4 | OutputFormat::M4a) {
            args.extend(["-movflags".into(), "+faststart".into()]);
        }
        args.extend([
            "-threads".into(),
            "1".into(),
            "-fs".into(),
            max_bytes.to_string().into(),
            output.as_os_str().to_owned(),
        ]);
        let mut child = Command::new(&self.binary)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| ffmpeg_error())?;
        let status = tokio::select! {
            biased;
            _ = cancel.cancelled() => { let _ = child.kill().await; return Err(cancelled()); }
            result = tokio::time::timeout(self.timeout, child.wait()) => match result {
                Ok(Ok(status)) => status,
                Ok(Err(_)) => return Err(ffmpeg_error()),
                Err(_) => { let _ = child.kill().await; return Err(AppError::new(ErrorCode::Timeout, "Media processing timed out")); }
            }
        };
        if !status.success() {
            return Err(ffmpeg_error());
        }
        let size = fs::metadata(output)
            .await
            .map_err(|_| ffmpeg_error())?
            .len();
        if size == 0 {
            return Err(ffmpeg_error());
        }
        if size >= max_bytes {
            return Err(AppError::new(
                ErrorCode::DownloadTooLarge,
                "Processed output reached the byte limit",
            ));
        }
        let probe = self.probe(output, cancel).await?;
        if let Some(expected) = expected_duration {
            // FFmpeg may exit successfully after reaching -fs; reject a
            // shortened artifact instead of publishing a partial success.
            let actual = probe.duration_seconds().ok_or_else(ffmpeg_error)?;
            if actual + 0.25 < expected {
                return Err(AppError::new(
                    ErrorCode::DownloadTooLarge,
                    "Output was truncated by the job byte limit",
                ));
            }
        }
        let has_video = probe
            .streams
            .iter()
            .any(|stream| stream.codec_type == "video");
        let has_audio = probe
            .streams
            .iter()
            .any(|stream| stream.codec_type == "audio");
        if (video.is_some() && !has_video) || (audio.is_some() && !has_audio) {
            return Err(ffmpeg_error());
        }
        Ok(probe)
    }

    pub async fn probe(&self, input: &Path, cancel: &CancellationToken) -> Result<MediaProbe> {
        let input = local_file(input).await?;
        let mut child = Command::new(&self.probe_binary)
            .args([
                "-v",
                "error",
                "-max_alloc",
                "67108864",
                "-protocol_whitelist",
                "file",
                "-format_whitelist",
                INPUT_FORMATS,
                "-max_streams",
                "8",
                "-probesize",
                "5000000",
                "-analyzeduration",
                "5000000",
                "-show_entries",
                "stream=codec_type,codec_name,bit_rate,width,height:format=duration",
                "-of",
                "json",
            ])
            .arg(input)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| ffmpeg_error())?;
        let stdout = child.stdout.take().ok_or_else(ffmpeg_error)?;
        let result = {
            let read = async {
                let mut bytes = Vec::new();
                stdout
                    .take(16_385)
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(|_| ffmpeg_error())?;
                if bytes.len() > 16_384 {
                    return Err(ffmpeg_error());
                }
                let status = child.wait().await.map_err(|_| ffmpeg_error())?;
                if !status.success() {
                    return Err(ffmpeg_error());
                }
                let result: MediaProbe =
                    serde_json::from_slice(&bytes).map_err(|_| ffmpeg_error())?;
                if result.streams.is_empty() {
                    return Err(ffmpeg_error());
                }
                Ok(result)
            };
            tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(cancelled()),
                result = tokio::time::timeout(self.timeout, read) => result.unwrap_or_else(|_| Err(AppError::new(ErrorCode::Timeout, "Media validation timed out"))),
            }
        };
        if result.is_err() {
            // Await termination before the job releases storage/quota.
            let _ = child.kill().await;
        }
        result
    }
}

async fn local_file(path: &Path) -> Result<PathBuf> {
    let path = fs::canonicalize(path).await.map_err(|_| ffmpeg_error())?;
    if !fs::metadata(&path)
        .await
        .map_err(|_| ffmpeg_error())?
        .is_file()
    {
        return Err(ffmpeg_error());
    }
    Ok(path)
}

fn ffmpeg_error() -> AppError {
    AppError::new(
        ErrorCode::FfmpegFailed,
        "Local media processing or validation failed",
    )
}
fn cancelled() -> AppError {
    AppError::new(ErrorCode::Cancelled, "Download cancelled")
}
