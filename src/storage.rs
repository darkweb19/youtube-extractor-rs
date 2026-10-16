//! Local filesystem storage. Mount a persistent volume for restart durability.
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::fs;
use uuid::Uuid;

use crate::error::{AppError, ErrorCode, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredOutput {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub filename: String,
    pub content_type: String,
}

/// Adapter boundary for future object storage. Upload implementations must avoid
/// publishing incomplete files and must enforce the same per-job ownership.
#[async_trait]
pub trait Storage: Send + Sync {
    async fn publish(
        &self,
        job: Uuid,
        source: &Path,
        extension: &str,
        content_type: &str,
    ) -> Result<StoredOutput>;
    async fn cleanup(&self, job: Uuid) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct LocalStorage {
    root: PathBuf,
}

impl LocalStorage {
    pub async fn new(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root).await.map_err(storage_error)?;
        let root = fs::canonicalize(root).await.map_err(storage_error)?;
        Ok(Self { root })
    }

    pub fn job_dir(&self, job: Uuid) -> PathBuf {
        self.root.join(job.to_string())
    }

    pub async fn create_job(&self, job: Uuid) -> Result<PathBuf> {
        let dir = self.job_dir(job);
        // Do not reuse an existing directory or follow an attacker-created link.
        fs::create_dir(&dir).await.map_err(storage_error)?;
        Ok(dir)
    }

    pub async fn cleanup(&self, job: Uuid) -> Result<()> {
        <Self as Storage>::cleanup(self, job).await
    }
}

#[async_trait]
impl Storage for LocalStorage {
    async fn publish(
        &self,
        job: Uuid,
        source: &Path,
        extension: &str,
        content_type: &str,
    ) -> Result<StoredOutput> {
        if !matches!(extension, "mp4" | "webm" | "m4a" | "mp3" | "ogg") {
            return Err(AppError::invalid("Unsupported storage extension"));
        }
        let dir = self.job_dir(job);
        let canonical_dir = fs::canonicalize(&dir).await.map_err(storage_error)?;
        if canonical_dir.parent() != Some(self.root.as_path()) {
            return Err(AppError::new(
                ErrorCode::Storage,
                "Invalid job storage directory",
            ));
        }
        let canonical_source = fs::canonicalize(source).await.map_err(storage_error)?;
        if canonical_source.parent() != Some(canonical_dir.as_path()) {
            return Err(AppError::new(
                ErrorCode::Storage,
                "Output must belong to its job",
            ));
        }
        let metadata = fs::metadata(&canonical_source)
            .await
            .map_err(storage_error)?;
        if !metadata.is_file() || metadata.len() == 0 {
            return Err(AppError::new(
                ErrorCode::Storage,
                "Empty or invalid completed output",
            ));
        }
        let filename = format!("{job}.{extension}");
        let path = dir.join(&filename);
        // Both paths are in the same filesystem. No completed name is visible
        // until the producing process has closed and validation has succeeded.
        fs::File::open(&canonical_source)
            .await
            .map_err(storage_error)?
            .sync_all()
            .await
            .map_err(storage_error)?;
        fs::rename(canonical_source, &path)
            .await
            .map_err(storage_error)?;
        fs::File::open(&dir)
            .await
            .map_err(storage_error)?
            .sync_all()
            .await
            .map_err(storage_error)?;
        Ok(StoredOutput {
            path,
            filename,
            size_bytes: metadata.len(),
            content_type: content_type.into(),
        })
    }

    async fn cleanup(&self, job: Uuid) -> Result<()> {
        match fs::remove_dir_all(self.job_dir(job)).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(storage_error(error)),
        }
    }
}

fn storage_error(_: std::io::Error) -> AppError {
    AppError::new(ErrorCode::Storage, "Local storage operation failed")
}
