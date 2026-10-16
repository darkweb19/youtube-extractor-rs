use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use youtube_extractor::{
    error::ErrorCode,
    media::Ffmpeg,
    model::OutputFormat,
    storage::{LocalStorage, Storage},
};

async fn synthetic(path: &Path, video: bool, seconds: &str) {
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
            "-t",
            seconds,
            "-an",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ]);
    } else {
        command.args([
            "sine=frequency=440:sample_rate=44100",
            "-t",
            seconds,
            "-vn",
            "-c:a",
            "aac",
        ]);
    }
    let status = command
        .args(["-threads", "1"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .await
        .expect("ffmpeg must be installed for media integration tests");
    assert!(status.success());
}

fn processor() -> Ffmpeg {
    Ffmpeg::new(
        PathBuf::from("ffmpeg"),
        PathBuf::from("ffprobe"),
        Duration::from_secs(20),
    )
}

#[tokio::test]
async fn synthetic_video_audio_mux_and_atomic_storage() {
    let temp = tempfile::tempdir().unwrap();
    let storage = LocalStorage::new(temp.path().to_path_buf()).await.unwrap();
    let job = Uuid::new_v4();
    let dir = storage.create_job(job).await.unwrap();
    let video = dir.join("source.mp4");
    let audio = dir.join("source.m4a");
    synthetic(&video, true, "0.6").await;
    synthetic(&audio, false, "0.6").await;
    let output = dir.join("processed.mp4");
    let probe = processor()
        .process(
            Some(&video),
            Some(&audio),
            &output,
            OutputFormat::Mp4,
            Some(128),
            1_000_000,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(probe.streams.iter().any(|s| s.codec_type == "video"));
    assert!(probe.streams.iter().any(|s| s.codec_type == "audio"));
    let stored = storage
        .publish(job, &output, "mp4", "video/mp4")
        .await
        .unwrap();
    assert!(stored.path.is_file());
    assert!(!output.exists());
    assert_eq!(stored.filename, format!("{job}.mp4"));
    storage.cleanup(job).await.unwrap();
    assert!(!dir.exists());
}

#[tokio::test]
async fn synthetic_audio_remux_mp3_and_output_budget() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("source.m4a");
    synthetic(&input, false, "3").await;
    let cancel = CancellationToken::new();
    let m4a = temp.path().join("output.m4a");
    let probe = processor()
        .process(
            None,
            Some(&input),
            &m4a,
            OutputFormat::M4a,
            None,
            1_000_000,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(probe.streams[0].codec_name.as_deref(), Some("aac"));
    let quality = temp.path().join("quality.m4a");
    let probe = processor()
        .process(
            None,
            Some(&input),
            &quality,
            OutputFormat::M4a,
            Some(48),
            1_000_000,
            &cancel,
        )
        .await
        .unwrap();
    let bitrate = probe.streams[0]
        .bit_rate
        .as_ref()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!(
        (32_000..60_000).contains(&bitrate),
        "explicit M4A bitrate is honored: {bitrate}"
    );
    let mp3 = temp.path().join("output.mp3");
    let probe = processor()
        .process(
            None,
            Some(&input),
            &mp3,
            OutputFormat::Mp3,
            Some(96),
            1_000_000,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(probe.streams[0].codec_name.as_deref(), Some("mp3"));
    let limited = temp.path().join("limited.mp3");
    let error = processor()
        .process(
            None,
            Some(&input),
            &limited,
            OutputFormat::Mp3,
            Some(128),
            4000,
            &cancel,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::DownloadTooLarge);
}

#[tokio::test]
async fn cancelled_processing_and_invalid_local_input_fail() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("invalid.mp4");
    tokio::fs::write(&input, b"not media").await.unwrap();
    let output = temp.path().join("output.mp4");
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(
        processor()
            .process(
                Some(&input),
                None,
                &output,
                OutputFormat::Mp4,
                Some(128),
                100_000,
                &token
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Cancelled
    );
    assert_eq!(
        processor()
            .process(
                Some(&input),
                None,
                &output,
                OutputFormat::Mp4,
                Some(128),
                100_000,
                &CancellationToken::new()
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::FfmpegFailed
    );
}

#[tokio::test]
async fn storage_rejects_cross_job_publication_and_existing_directory() {
    let temp = tempfile::tempdir().unwrap();
    let storage = LocalStorage::new(temp.path().to_path_buf()).await.unwrap();
    let job = Uuid::new_v4();
    storage.create_job(job).await.unwrap();
    assert!(storage.create_job(job).await.is_err());
    let outside = temp.path().join("outside.mp4");
    tokio::fs::write(&outside, b"test").await.unwrap();
    assert_eq!(
        storage
            .publish(job, &outside, "mp4", "video/mp4")
            .await
            .unwrap_err()
            .code,
        ErrorCode::Storage
    );
    assert!(outside.exists());
    storage.cleanup(job).await.unwrap();
}
