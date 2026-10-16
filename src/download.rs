//! Bounded, resumable streamed media transfer and the local processing pipeline.
use std::{path::Path, sync::Arc, time::Duration};

use reqwest::{header, StatusCode};
use serde::{Deserialize, Serialize};
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
    sync::watch,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    error::{AppError, ErrorCode, Result},
    extractor::Extractor,
    media::Ffmpeg,
    model::{DownloadKind, DownloadRequest, MediaFormat, OutputFormat},
    network::SafeHttpClient,
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
    extractor: Arc<dyn Extractor>,
    http: SafeHttpClient,
    storage: LocalStorage,
    ffmpeg: Ffmpeg,
    max_bytes: u64,
    retries: usize,
}

impl DownloadPipeline {
    pub fn new(
        extractor: Arc<dyn Extractor>,
        http: SafeHttpClient,
        storage: LocalStorage,
        ffmpeg: Ffmpeg,
        max_bytes: u64,
        retries: usize,
    ) -> Self {
        Self {
            extractor,
            http,
            storage,
            ffmpeg,
            max_bytes,
            retries: retries.min(5),
        }
    }

    pub async fn cleanup(&self, job: Uuid) -> Result<()> {
        self.storage.cleanup(job).await
    }

    pub async fn run(
        &self,
        request: &DownloadRequest,
        job: Uuid,
        cancel: CancellationToken,
        progress: watch::Sender<DownloadProgress>,
    ) -> Result<StoredOutput> {
        request.validate()?;
        let dir = self.storage.create_job(job).await?;
        let result = self.run_inner(request, job, &dir, &cancel, &progress).await;
        if result.is_err() {
            if let Err(error) = self.storage.cleanup(job).await {
                tracing::warn!(code = ?error.code, "Could not remove failed job files");
            }
        }
        result
    }

    async fn run_inner(
        &self,
        request: &DownloadRequest,
        job: Uuid,
        dir: &Path,
        cancel: &CancellationToken,
        progress: &watch::Sender<DownloadProgress>,
    ) -> Result<StoredOutput> {
        publish_progress(progress, 0, None, "extracting");
        let info = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(cancelled()),
            result = self.extractor.video_info(&request.url) => result?,
        };
        let selection = select_formats(&info.formats, request)?;
        let expected = selection
            .video
            .iter()
            .chain(selection.audio.iter())
            .try_fold(0u64, |total, format| {
                format
                    .content_length
                    .and_then(|size| total.checked_add(size))
            });
        if expected.is_some_and(|size| size >= self.max_bytes) {
            return Err(too_large());
        }
        let mut downloaded = 0;
        let mut video_path = None;
        let mut audio_path = None;
        for (name, format, path_slot) in [
            ("video.part", selection.video.as_ref(), &mut video_path),
            ("audio.part", selection.audio.as_ref(), &mut audio_path),
        ] {
            if let Some(format) = format {
                let path = dir.join(name);
                let url = format.url.as_deref().ok_or_else(|| {
                    AppError::new(
                        ErrorCode::UnsupportedFormat,
                        "Selected stream requires unsupported URL resolution",
                    )
                })?;
                let bytes = stream_to_file(
                    &self.http,
                    url,
                    &path,
                    format.content_length,
                    self.max_bytes.saturating_sub(downloaded),
                    self.retries,
                    cancel,
                    progress,
                    downloaded,
                    expected,
                )
                .await?;
                downloaded = downloaded.checked_add(bytes).ok_or_else(too_large)?;
                *path_slot = Some(path);
            }
        }
        let remaining = self.max_bytes.saturating_sub(downloaded);
        if remaining == 0 {
            return Err(too_large());
        }
        let output = dir.join(format!("processed.{}", selection.extension));
        publish_progress(progress, downloaded, expected, "processing");
        self.ffmpeg
            .process(
                video_path.as_deref(),
                audio_path.as_deref(),
                &output,
                selection.output_format,
                request.audio_bitrate_kbps,
                remaining,
                cancel,
            )
            .await?;
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let stored = self
            .storage
            .publish(job, &output, &selection.extension, &selection.content_type)
            .await?;
        // Completed jobs retain only their final artifact; input names never
        // contain titles, user strings, signed URLs or external path components.
        for path in [video_path, audio_path].into_iter().flatten() {
            fs::remove_file(path).await.map_err(|_| storage_error())?;
        }
        publish_progress(progress, downloaded, expected, "completed");
        Ok(stored)
    }
}

#[derive(Debug, Clone)]
pub struct FormatSelection {
    pub video: Option<MediaFormat>,
    pub audio: Option<MediaFormat>,
    pub extension: String,
    pub content_type: String,
    pub output_format: OutputFormat,
}

pub fn select_formats(
    formats: &[MediaFormat],
    request: &DownloadRequest,
) -> Result<FormatSelection> {
    request.validate()?;
    let usable = |format: &&MediaFormat| format.url.is_some();
    let worst = request.quality.as_deref() == Some("worst");
    let best_audio = |compatible: &dyn Fn(&MediaFormat) -> bool| {
        let candidates = formats
            .iter()
            .filter(usable)
            .filter(|f| f.has_audio && !f.has_video && compatible(f));
        if worst {
            candidates
                .min_by_key(|f| f.bitrate.unwrap_or(u64::MAX))
                .cloned()
        } else {
            candidates.max_by_key(|f| f.bitrate.unwrap_or(0)).cloned()
        }
    };
    if request.kind == DownloadKind::Audio {
        let compatible = |f: &MediaFormat| {
            matches!(f.container.as_str(), "mp4" | "m4a" | "webm" | "ogg") && f.has_audio
        };
        let preferred = |f: &MediaFormat| {
            compatible(f) && f.codecs.iter().any(|c| c.starts_with("mp4a") || c == "aac")
        };
        let audio = if request.format == OutputFormat::M4a {
            best_audio(&preferred).or_else(|| best_audio(&compatible))
        } else {
            best_audio(&compatible)
        }
        .or_else(|| {
            let candidates = formats.iter().filter(usable).filter(|f| compatible(f));
            if worst {
                candidates
                    .min_by_key(|f| f.bitrate.unwrap_or(u64::MAX))
                    .cloned()
            } else {
                candidates.max_by_key(|f| f.bitrate.unwrap_or(0)).cloned()
            }
        })
        .ok_or_else(unsupported)?;
        let (extension, content_type, output_format) = match request.format {
            OutputFormat::Mp3 => ("mp3", "audio/mpeg", OutputFormat::Mp3),
            OutputFormat::M4a => ("m4a", "audio/mp4", OutputFormat::M4a),
            OutputFormat::Original => match audio.container.as_str() {
                "mp4" | "m4a" => ("m4a", "audio/mp4", OutputFormat::Original),
                "webm" => ("webm", "audio/webm", OutputFormat::Original),
                "ogg" => ("ogg", "audio/ogg", OutputFormat::Original),
                _ => return Err(unsupported()),
            },
            _ => return Err(unsupported()),
        };
        return Ok(FormatSelection {
            video: None,
            audio: Some(audio),
            extension: extension.into(),
            content_type: content_type.into(),
            output_format,
        });
    }
    let height_cap = request
        .quality
        .as_deref()
        .and_then(|s| s.strip_suffix('p'))
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(u32::MAX);
    let video_compatible = |f: &MediaFormat| match request.format {
        OutputFormat::Mp4 => {
            f.container == "mp4"
                && f.codecs.iter().any(|c| !is_audio_codec(c))
                && f.codecs.iter().filter(|c| !is_audio_codec(c)).all(|c| {
                    c.starts_with("avc1")
                        || c.starts_with("avc3")
                        || c.starts_with("hvc1")
                        || c.starts_with("hev1")
                        || c.starts_with("av01")
                })
                && (!f.has_audio || f.codecs.iter().any(|c| c.starts_with("mp4a")))
        }
        OutputFormat::Webm => {
            f.container == "webm"
                && f.codecs.iter().any(|c| !is_audio_codec(c))
                && f.codecs.iter().filter(|c| !is_audio_codec(c)).all(|c| {
                    c.starts_with("vp8")
                        || c.starts_with("vp9")
                        || c.starts_with("vp09")
                        || c.starts_with("av01")
                })
        }
        OutputFormat::Original => matches!(f.container.as_str(), "mp4" | "webm"),
        _ => false,
    };
    let mut candidates: Vec<_> = formats
        .iter()
        .filter(usable)
        .filter(|f| f.has_video && f.height.is_some_and(|h| h <= height_cap) && video_compatible(f))
        .collect();
    candidates.sort_by_key(|f| {
        (
            f.height.unwrap_or(0),
            f.fps.unwrap_or(0),
            f.bitrate.unwrap_or(0),
        )
    });
    if request.quality.as_deref() != Some("worst") {
        candidates.reverse();
    }
    for video in candidates {
        let container = if request.format == OutputFormat::Webm
            || (request.format == OutputFormat::Original && video.container == "webm")
        {
            "webm"
        } else {
            "mp4"
        };
        let compatible_audio = |f: &MediaFormat| {
            if container == "mp4" {
                !f.codecs.is_empty()
                    && f.codecs.iter().all(|c| c.starts_with("mp4a") || c == "aac")
                    && matches!(f.container.as_str(), "mp4" | "m4a")
            } else {
                f.container == "webm"
                    && !f.codecs.is_empty()
                    && f.codecs.iter().all(|c| c == "opus" || c == "vorbis")
            }
        };
        let audio = if video.has_audio {
            None
        } else {
            best_audio(&compatible_audio)
        };
        if !video.has_audio && audio.is_none() {
            continue;
        }
        return Ok(FormatSelection {
            video: Some(video.clone()),
            audio,
            extension: container.into(),
            content_type: format!("video/{container}"),
            output_format: if container == "mp4" {
                OutputFormat::Mp4
            } else {
                OutputFormat::Webm
            },
        });
    }
    Err(unsupported())
}

fn is_audio_codec(codec: &str) -> bool {
    codec.starts_with("mp4a") || matches!(codec, "aac" | "opus" | "vorbis" | "mp3")
}

/// Stream one object directly to disk. `budget` is the remaining cumulative
/// per-job workspace limit, not a per-stream limit. Partial bytes are never
/// discarded and redownloaded silently, and ignored Range responses are errors.
#[allow(clippy::too_many_arguments)]
pub async fn stream_to_file(
    http: &SafeHttpClient,
    url: &str,
    path: &Path,
    mut expected: Option<u64>,
    budget: u64,
    retries: usize,
    cancel: &CancellationToken,
    progress: &watch::Sender<DownloadProgress>,
    base: u64,
    total: Option<u64>,
) -> Result<u64> {
    if expected.is_some_and(|size| size > budget) {
        return Err(too_large());
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .map_err(|_| storage_error())?;
    let mut written = 0u64;
    let mut validator = None;
    for attempt in 0..=retries.min(5) {
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(cancelled()),
            response = http.get_with_range(url, written) => response,
        };
        let mut response = match response {
            Ok(response) => response,
            Err(error)
                if attempt < retries.min(5)
                    && matches!(error.code, ErrorCode::Network | ErrorCode::Timeout) =>
            {
                retry_delay(attempt, cancel).await?;
                continue;
            }
            Err(error) => return Err(error),
        };
        let mut response_end = None;
        if written > 0 || response.status() == StatusCode::PARTIAL_CONTENT {
            if response.status() != StatusCode::PARTIAL_CONTENT {
                return Err(interrupted("Upstream ignored the required resume range"));
            }
            let (start, end, object_total) = parse_content_range(
                response
                    .headers()
                    .get(header::CONTENT_RANGE)
                    .and_then(|value| value.to_str().ok()),
            )?;
            if start != written || expected.is_some_and(|size| size != object_total) {
                return Err(interrupted(
                    "Upstream resume range does not match the partial file",
                ));
            }
            if response
                .content_length()
                .is_some_and(|size| size != end - start + 1)
            {
                return Err(interrupted("Upstream range length is inconsistent"));
            }
            expected = Some(object_total);
            response_end = Some(end + 1);
        } else if response.status() != StatusCode::OK {
            return Err(interrupted("Unexpected media response status"));
        } else if let Some(length) = response.content_length() {
            if expected.is_some_and(|size| size != length) {
                return Err(interrupted("Upstream media size changed"));
            }
            expected = Some(length);
        }
        if expected.is_some_and(|size| size > budget) {
            return Err(too_large());
        }
        let current_validator = response
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.starts_with("W/"))
            .map(str::to_owned);
        if written > 0
            && validator
                .as_ref()
                .is_some_and(|old| current_validator.as_ref() != Some(old))
        {
            return Err(interrupted("Upstream media changed during resume"));
        }
        if written == 0 {
            validator = current_validator;
        }
        let mut failed = false;
        loop {
            let chunk = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(cancelled()),
                chunk = response.chunk() => chunk,
            };
            match chunk {
                Ok(Some(bytes)) => {
                    let length = u64::try_from(bytes.len()).map_err(|_| too_large())?;
                    let next = written.checked_add(length).ok_or_else(too_large)?;
                    if next > budget {
                        return Err(too_large());
                    }
                    if expected.is_some_and(|size| next > size)
                        || response_end.is_some_and(|end| next > end)
                    {
                        return Err(interrupted("Upstream sent bytes beyond the declared range"));
                    }
                    file.write_all(&bytes).await.map_err(|_| storage_error())?;
                    written = next;
                    publish_progress(progress, base.saturating_add(written), total, "downloading");
                }
                Ok(None) => break,
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        if !failed
            && response_end.is_none_or(|end| written == end)
            && expected.is_none_or(|size| written == size)
        {
            if written == 0 {
                return Err(interrupted("Upstream returned an empty media stream"));
            }
            file.flush().await.map_err(|_| storage_error())?;
            return Ok(written);
        }
        if attempt == retries.min(5) {
            return Err(interrupted(
                "Media download was interrupted after bounded retries",
            ));
        }
        retry_delay(attempt, cancel).await?;
    }
    Err(interrupted("Media download could not be completed"))
}

fn parse_content_range(value: Option<&str>) -> Result<(u64, u64, u64)> {
    let parse = || {
        let value = value?.strip_prefix("bytes ")?;
        let (range, total) = value.split_once('/')?;
        let (start, end) = range.split_once('-')?;
        let (start, end, total) = (
            start.parse::<u64>().ok()?,
            end.parse::<u64>().ok()?,
            total.parse::<u64>().ok()?,
        );
        (start <= end && end < total).then_some((start, end, total))
    };
    parse().ok_or_else(|| interrupted("Invalid upstream Content-Range"))
}

async fn retry_delay(attempt: usize, cancel: &CancellationToken) -> Result<()> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(cancelled()),
        _ = tokio::time::sleep(Duration::from_millis(100 * (1 << attempt.min(4)))) => Ok(()),
    }
}

fn publish_progress(
    sender: &watch::Sender<DownloadProgress>,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    stage: &str,
) {
    sender.send_replace(DownloadProgress {
        downloaded_bytes,
        total_bytes,
        stage: stage.into(),
    });
}
fn unsupported() -> AppError {
    AppError::new(
        ErrorCode::UnsupportedFormat,
        "No directly resolvable streams satisfy the requested output and quality",
    )
}
fn too_large() -> AppError {
    AppError::new(
        ErrorCode::DownloadTooLarge,
        "Combined media inputs and output exceed the job byte limit",
    )
}
fn interrupted(message: &str) -> AppError {
    AppError::new(ErrorCode::DownloadInterrupted, message)
}
fn storage_error() -> AppError {
    AppError::new(ErrorCode::Storage, "Could not write local media file")
}
fn cancelled() -> AppError {
    AppError::new(ErrorCode::Cancelled, "Download cancelled")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{VideoId, VideoInfo};
    use std::{collections::VecDeque, process::Stdio, sync::Mutex};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        process::Command,
        task::JoinHandle,
    };

    struct Reply {
        status: &'static str,
        headers: String,
        body: Vec<u8>,
    }
    impl Reply {
        fn whole(body: &[u8]) -> Self {
            Self {
                status: "200 OK",
                headers: format!("Content-Length: {}\r\nETag: \"fixture\"\r\n", body.len()),
                body: body.to_vec(),
            }
        }
        fn partial(status: &'static str, headers: &str, body: &[u8]) -> Self {
            Self {
                status,
                headers: headers.into(),
                body: body.to_vec(),
            }
        }
    }
    struct Fixture {
        origin: String,
        requests: Arc<Mutex<Vec<String>>>,
        server: JoinHandle<()>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.server.abort();
        }
    }
    async fn fixture(replies: Vec<Reply>) -> Fixture {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let saved = requests.clone();
        let server = tokio::spawn(async move {
            let mut replies: VecDeque<_> = replies.into();
            while let Some(reply) = replies.pop_front() {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut byte = [0u8; 1];
                    if stream.read(&mut byte).await.unwrap() == 0 {
                        break;
                    }
                    request.push(byte[0]);
                    if request.ends_with(b"\r\n\r\n") {
                        break;
                    }
                    assert!(request.len() < 16_384);
                }
                saved
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(request).unwrap());
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {}\r\n{}Connection: close\r\n\r\n",
                            reply.status, reply.headers
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                stream.write_all(&reply.body).await.unwrap();
                stream.flush().await.unwrap();
                // Make the partial bytes observable before a deliberate close.
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        Fixture {
            origin,
            requests,
            server,
        }
    }
    async fn transfer(
        fixture: &Fixture,
        path: &Path,
        expected: Option<u64>,
        budget: u64,
        retries: usize,
    ) -> Result<u64> {
        let (progress, _) = watch::channel(DownloadProgress::default());
        stream_to_file(
            &SafeHttpClient::for_test_origin(&fixture.origin),
            &format!("{}/media", fixture.origin),
            path,
            expected,
            budget,
            retries,
            &CancellationToken::new(),
            &progress,
            0,
            expected,
        )
        .await
    }

    #[tokio::test]
    async fn interrupted_stream_resumes_at_exact_offset() {
        let fixture = fixture(vec![
            Reply::partial(
                "200 OK",
                "Content-Length: 10\r\nETag: \"fixture\"\r\n",
                b"abcd",
            ),
            Reply::partial(
                "206 Partial Content",
                "Content-Length: 6\r\nContent-Range: bytes 4-9/10\r\nETag: \"fixture\"\r\n",
                b"efghij",
            ),
        ])
        .await;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("stream");
        assert_eq!(
            transfer(&fixture, &path, Some(10), 100, 2).await.unwrap(),
            10
        );
        assert_eq!(fs::read(path).await.unwrap(), b"abcdefghij");
        assert!(fixture.requests.lock().unwrap()[1]
            .to_lowercase()
            .contains("range: bytes=4-"));
    }

    #[tokio::test]
    async fn ignored_mismatched_and_changed_resume_responses_are_not_appended() {
        for second in [
            Reply::whole(b"abcdefghij"),
            Reply::partial(
                "206 Partial Content",
                "Content-Length: 7\r\nContent-Range: bytes 3-9/10\r\nETag: \"fixture\"\r\n",
                b"defghij",
            ),
            Reply::partial(
                "206 Partial Content",
                "Content-Length: 6\r\nContent-Range: bytes 4-9/11\r\nETag: \"fixture\"\r\n",
                b"efghij",
            ),
            Reply::partial(
                "206 Partial Content",
                "Content-Length: 6\r\nContent-Range: bytes 4-9/10\r\nETag: \"changed\"\r\n",
                b"efghij",
            ),
            Reply::partial(
                "206 Partial Content",
                "Content-Length: 6\r\nContent-Range: bytes 4-10/10\r\nETag: \"fixture\"\r\n",
                b"efghij",
            ),
        ] {
            let fixture = fixture(vec![
                Reply::partial(
                    "200 OK",
                    "Content-Length: 10\r\nETag: \"fixture\"\r\n",
                    b"abcd",
                ),
                second,
            ])
            .await;
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("stream");
            assert_eq!(
                transfer(&fixture, &path, Some(10), 100, 1)
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::DownloadInterrupted
            );
            assert_eq!(fs::read(&path).await.unwrap(), b"abcd");
        }
    }

    #[tokio::test]
    async fn unknown_length_streaming_obeys_budget_without_buffering_whole_file() {
        for budget in [5u64, 20] {
            let fixture = fixture(vec![Reply::partial(
                "200 OK",
                "Transfer-Encoding: chunked\r\n",
                b"4\r\nabcd\r\n6\r\nefghij\r\n0\r\n\r\n",
            )])
            .await;
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("stream");
            let result = transfer(&fixture, &path, None, budget, 0).await;
            if budget == 5 {
                assert_eq!(result.unwrap_err().code, ErrorCode::DownloadTooLarge);
                assert!(fs::metadata(path).await.unwrap().len() <= budget);
            } else {
                assert_eq!(result.unwrap(), 10);
                assert_eq!(fs::read(path).await.unwrap(), b"abcdefghij");
            }
        }
    }

    #[tokio::test]
    async fn repeated_interruptions_stop_after_bounded_retries() {
        let fixture = fixture(vec![
            Reply::partial("200 OK", "Content-Length: 10\r\n", b"abcd"),
            Reply::partial(
                "206 Partial Content",
                "Content-Length: 6\r\nContent-Range: bytes 4-9/10\r\n",
                b"ef",
            ),
            Reply::partial(
                "206 Partial Content",
                "Content-Length: 4\r\nContent-Range: bytes 6-9/10\r\n",
                b"gh",
            ),
        ])
        .await;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("stream");
        assert_eq!(
            transfer(&fixture, &path, Some(10), 100, 2)
                .await
                .unwrap_err()
                .code,
            ErrorCode::DownloadInterrupted
        );
        assert_eq!(fixture.requests.lock().unwrap().len(), 3);
        // A prematurely closed HTTP response may report its framing error
        // before the client exposes every partial body byte. Verify that retry
        // exhaustion preserves an uncorrupted prefix without requiring the
        // transport to deliver a timing-dependent number of bytes.
        let partial = fs::read(path).await.unwrap();
        assert!(b"abcdefgh".starts_with(&partial));
        assert!(partial.len() <= 8);
    }

    #[tokio::test]
    async fn cancellation_and_expected_size_stop_before_request() {
        let fixture = fixture(vec![]).await;
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(
            transfer(&fixture, &temp.path().join("too-large"), Some(101), 100, 0)
                .await
                .unwrap_err()
                .code,
            ErrorCode::DownloadTooLarge
        );
        let token = CancellationToken::new();
        token.cancel();
        let (progress, _) = watch::channel(DownloadProgress::default());
        assert_eq!(
            stream_to_file(
                &SafeHttpClient::for_test_origin(&fixture.origin),
                &fixture.origin,
                &temp.path().join("cancelled"),
                None,
                100,
                0,
                &token,
                &progress,
                0,
                None
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::Cancelled
        );
        assert!(fixture.requests.lock().unwrap().is_empty());
    }

    struct FixtureExtractor(VideoInfo);
    #[async_trait::async_trait]
    impl Extractor for FixtureExtractor {
        async fn video_info(&self, _: &str) -> Result<VideoInfo> {
            Ok(self.0.clone())
        }
    }
    fn media_format(url: String, bytes: usize, video: bool) -> MediaFormat {
        MediaFormat {
            itag: if video { 137 } else { 140 },
            mime_type: if video { "video/mp4" } else { "audio/mp4" }.into(),
            codecs: vec![if video { "avc1.64001f" } else { "mp4a.40.2" }.into()],
            container: "mp4".into(),
            bitrate: Some(128_000),
            width: video.then_some(160),
            height: video.then_some(90),
            fps: video.then_some(12),
            content_length: Some(bytes as u64),
            has_audio: !video,
            has_video: video,
            url: Some(url),
            signature_cipher: None,
        }
    }
    async fn synth(path: &Path, video: bool) {
        let mut command = Command::new("ffmpeg");
        command.args([
            "-nostdin",
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
        ]);
        if video {
            command.args([
                "color=c=blue:s=160x90:r=12",
                "-an",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ]);
        } else {
            command.args(["sine=frequency=440:sample_rate=44100", "-vn", "-c:a", "aac"]);
        }
        assert!(command
            .args(["-t", "0.6", "-threads", "1"])
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .await
            .unwrap()
            .success());
    }

    #[tokio::test]
    async fn full_controlled_http_to_mp3_and_mp4_pipeline() {
        let temp = tempfile::tempdir().unwrap();
        let video_input = temp.path().join("video.mp4");
        let audio_input = temp.path().join("audio.m4a");
        synth(&video_input, true).await;
        synth(&audio_input, false).await;
        let video_bytes = fs::read(&video_input).await.unwrap();
        let audio_bytes = fs::read(&audio_input).await.unwrap();
        let fixture = fixture(vec![
            Reply::whole(&audio_bytes),
            Reply::whole(&video_bytes),
            Reply::whole(&audio_bytes),
        ])
        .await;
        let info = VideoInfo {
            id: VideoId::new("abcdefghijk").unwrap(),
            title: "../../untrusted title".into(),
            description: "".into(),
            duration_seconds: Some(1),
            channel_id: None,
            channel_name: None,
            thumbnails: vec![],
            formats: vec![
                media_format(format!("{}/video", fixture.origin), video_bytes.len(), true),
                media_format(
                    format!("{}/audio", fixture.origin),
                    audio_bytes.len(),
                    false,
                ),
            ],
        };
        let storage = LocalStorage::new(temp.path().join("jobs")).await.unwrap();
        let ffmpeg = Ffmpeg::new("ffmpeg".into(), "ffprobe".into(), Duration::from_secs(20));
        let pipeline = DownloadPipeline::new(
            Arc::new(FixtureExtractor(info)),
            SafeHttpClient::for_test_origin(&fixture.origin),
            storage.clone(),
            ffmpeg.clone(),
            4_000_000,
            1,
        );
        for (kind, format) in [
            (DownloadKind::Audio, OutputFormat::Mp3),
            (DownloadKind::Video, OutputFormat::Mp4),
        ] {
            let job = Uuid::new_v4();
            let (progress, receiver) = watch::channel(DownloadProgress::default());
            let output = pipeline
                .run(
                    &DownloadRequest {
                        url: "https://youtu.be/abcdefghijk".into(),
                        kind,
                        format,
                        quality: None,
                        audio_bitrate_kbps: None,
                    },
                    job,
                    CancellationToken::new(),
                    progress,
                )
                .await
                .unwrap();
            let probe = ffmpeg
                .probe(&output.path, &CancellationToken::new())
                .await
                .unwrap();
            assert!(probe.streams.iter().any(|s| s.codec_type == "audio"));
            assert_eq!(
                probe.streams.iter().any(|s| s.codec_type == "video"),
                kind == DownloadKind::Video
            );
            assert!(output.size_bytes > 0);
            assert_eq!(receiver.borrow().stage, "completed");
            let mut entries = fs::read_dir(storage.job_dir(job)).await.unwrap();
            assert!(entries.next_entry().await.unwrap().is_some());
            assert!(
                entries.next_entry().await.unwrap().is_none(),
                "inputs removed after atomic publication"
            );
            pipeline.cleanup(job).await.unwrap();
        }
    }

    #[tokio::test]
    async fn pipeline_failure_removes_partial_files() {
        let temp = tempfile::tempdir().unwrap();
        let fixture = fixture(vec![Reply::whole(b"not media")]).await;
        let info = VideoInfo {
            id: VideoId::new("abcdefghijk").unwrap(),
            title: "test".into(),
            description: "".into(),
            duration_seconds: None,
            channel_id: None,
            channel_name: None,
            thumbnails: vec![],
            formats: vec![media_format(fixture.origin.clone(), 9, false)],
        };
        let storage = LocalStorage::new(temp.path().to_path_buf()).await.unwrap();
        let pipeline = DownloadPipeline::new(
            Arc::new(FixtureExtractor(info)),
            SafeHttpClient::for_test_origin(&fixture.origin),
            storage.clone(),
            Ffmpeg::new("ffmpeg".into(), "ffprobe".into(), Duration::from_secs(20)),
            1_000_000,
            0,
        );
        let (progress, _) = watch::channel(DownloadProgress::default());
        let job = Uuid::new_v4();
        let result = pipeline
            .run(
                &DownloadRequest {
                    url: "https://youtu.be/abcdefghijk".into(),
                    kind: DownloadKind::Audio,
                    format: OutputFormat::Mp3,
                    quality: None,
                    audio_bitrate_kbps: None,
                },
                job,
                CancellationToken::new(),
                progress,
            )
            .await;
        assert_eq!(result.unwrap_err().code, ErrorCode::FfmpegFailed);
        assert!(!storage.job_dir(job).exists());
    }
}
