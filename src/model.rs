use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, ErrorCode, Result};

/// A normalized video identifier. URL parsing belongs to the extractor module.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct VideoId(pub String);

impl VideoId {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.len() != 11
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(AppError::invalid(
                "Video identifier must contain 11 URL-safe characters",
            ));
        }
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for VideoId {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl AsRef<str> for VideoId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for VideoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thumbnail {
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoInfo {
    pub id: VideoId,
    pub title: String,
    pub description: String,
    pub duration_seconds: Option<u64>,
    pub channel_id: Option<String>,
    pub channel_name: Option<String>,
    pub thumbnails: Vec<Thumbnail>,
    pub formats: Vec<MediaFormat>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct MediaFormat {
    pub itag: u32,
    pub mime_type: String,
    pub codecs: Vec<String>,
    pub container: String,
    pub bitrate: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<u32>,
    pub content_length: Option<u64>,
    pub has_audio: bool,
    pub has_video: bool,
    /// Ephemeral upstream credentials must not cross the public API boundary.
    #[serde(default, skip_serializing)]
    pub url: Option<String>,
    #[serde(default, skip_serializing)]
    pub signature_cipher: Option<String>,
}

impl fmt::Debug for MediaFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaFormat")
            .field("itag", &self.itag)
            .field("mime_type", &self.mime_type)
            .field("codecs", &self.codecs)
            .field("container", &self.container)
            .field("has_audio", &self.has_audio)
            .field("has_video", &self.has_video)
            .field("url_present", &self.url.is_some())
            .field("signature_cipher_present", &self.signature_cipher.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadKind {
    Audio,
    Video,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Original,
    M4a,
    Mp3,
    Mp4,
    Webm,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadRequest {
    pub url: String,
    #[serde(rename = "type")]
    pub kind: DownloadKind,
    pub format: OutputFormat,
    pub quality: Option<String>,
    pub audio_bitrate_kbps: Option<u32>,
}

impl DownloadRequest {
    pub fn validate(&self) -> Result<()> {
        if self.url.is_empty() || self.url.len() > 2048 {
            return Err(AppError::invalid(
                "URL must contain between 1 and 2048 bytes",
            ));
        }
        match (self.kind, self.format) {
            (DownloadKind::Audio, OutputFormat::Mp4 | OutputFormat::Webm)
            | (DownloadKind::Video, OutputFormat::M4a | OutputFormat::Mp3) => {
                return Err(AppError::invalid(
                    "Output format is incompatible with download type",
                ));
            }
            _ => {}
        }
        if let Some(bitrate) = self.audio_bitrate_kbps {
            if !(32..=320).contains(&bitrate) || self.kind != DownloadKind::Audio {
                return Err(AppError::invalid(
                    "Audio bitrate must be between 32 and 320 kbps and applies only to audio jobs",
                ));
            }
        }
        if let Some(quality) = self.quality.as_deref() {
            if quality != "best" && quality != "worst" {
                let height = quality
                    .strip_suffix('p')
                    .and_then(|s| s.parse::<u32>().ok());
                if !matches!(height, Some(144..=8640)) || self.kind == DownloadKind::Audio {
                    return Err(AppError::invalid(
                        "Quality must be best, worst, or a video height such as 1080p",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobProgress {
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub percent: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobError {
    pub code: ErrorCode,
    pub message: String,
}

impl From<AppError> for JobError {
    fn from(value: AppError) -> Self {
        Self {
            code: value.code,
            message: value.message,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputInfo {
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobView {
    pub job_id: String,
    pub status: JobState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub progress: JobProgress,
    pub error: Option<JobError>,
    pub output: Option<OutputInfo>,
    pub download_url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_identifiers_including_deserialization() {
        assert!(VideoId::new("dQw4w9WgXcQ").is_ok());
        for id in ["short", "../abcdefgh", "é123456789", "abc/defghij"] {
            assert!(VideoId::new(id).is_err());
        }
        assert!(serde_json::from_str::<VideoId>("\"../invalid\"").is_err());
    }

    #[test]
    fn rejects_incompatible_format_and_unknown_request_fields() {
        let mut request: DownloadRequest = serde_json::from_str(r#"{"url":"https://youtu.be/dQw4w9WgXcQ","type":"audio","format":"mp3","audio_bitrate_kbps":192}"#).unwrap();
        assert!(request.validate().is_ok());
        request.kind = DownloadKind::Video;
        assert!(request.validate().is_err());
        assert!(serde_json::from_str::<DownloadRequest>(
            r#"{"url":"x","type":"video","format":"mp4","callback_url":"http://localhost"}"#
        )
        .is_err());
    }

    #[test]
    fn stream_credentials_are_never_serialized() {
        let format = MediaFormat {
            itag: 140,
            mime_type: "audio/mp4".into(),
            codecs: vec!["mp4a.40.2".into()],
            container: "m4a".into(),
            bitrate: Some(128_000),
            width: None,
            height: None,
            fps: None,
            content_length: None,
            has_audio: true,
            has_video: false,
            url: Some("https://example.invalid/?token=secret".into()),
            signature_cipher: Some("secret".into()),
        };
        let json = serde_json::to_string(&format).unwrap();
        assert!(!json.contains("secret"));
        assert!(!json.contains("signature_cipher"));
        assert!(!format!("{format:?}").contains("secret"));
    }
}
