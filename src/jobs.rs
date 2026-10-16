//! Bounded single-process jobs with atomic local-volume state and cooperative cancellation.
use crate::{
    download::{DownloadPipeline, DownloadProgress},
    error::{AppError, ErrorCode, Result},
    model::DownloadRequest,
    storage::StoredOutput,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{mpsc, watch, Mutex},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[async_trait]
pub trait JobExecutor: Send + Sync {
    async fn run(
        &self,
        request: &DownloadRequest,
        id: Uuid,
        cancel: CancellationToken,
        progress: watch::Sender<DownloadProgress>,
    ) -> Result<StoredOutput>;
    async fn cleanup(&self, id: Uuid) -> Result<()>;
}
#[async_trait]
impl JobExecutor for DownloadPipeline {
    async fn run(
        &self,
        request: &DownloadRequest,
        id: Uuid,
        cancel: CancellationToken,
        progress: watch::Sender<DownloadProgress>,
    ) -> Result<StoredOutput> {
        self.run(request, id, cancel, progress).await
    }
    async fn cleanup(&self, id: Uuid) -> Result<()> {
        self.cleanup(id).await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}
impl JobStatus {
    fn active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub job_id: Uuid,
    pub status: JobStatus,
    pub progress: DownloadProgress,
    pub output: Option<StoredOutput>,
    pub error: Option<AppError>,
    pub created_at: u64,
    pub updated_at: u64,
    pub expires_at: u64,
    #[serde(default)]
    pub cleanup_pending: bool,
}
struct Entry {
    record: JobRecord,
    cancel: CancellationToken,
    progress: Option<watch::Receiver<DownloadProgress>>,
    delete_requested: bool,
    in_flight: bool,
    readers: Arc<AtomicUsize>,
}
struct Work {
    id: Uuid,
    request: DownloadRequest,
}
#[derive(Clone)]
pub struct JobSettings {
    pub data_dir: PathBuf,
    pub workers: usize,
    pub queue_capacity: usize,
    pub max_jobs: usize,
    pub max_download_bytes: u64,
    pub max_storage_bytes: u64,
    pub ttl: Duration,
    pub timeout: Duration,
}
pub struct JobManager {
    settings: JobSettings,
    entries: Mutex<HashMap<Uuid, Entry>>,
    executor: Arc<dyn JobExecutor>,
    sender: mpsc::Sender<Work>,
    stop: CancellationToken,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub struct FileLease(Arc<AtomicUsize>);
impl Drop for FileLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
fn storage_error() -> AppError {
    AppError::new(ErrorCode::Storage, "Job state storage failed")
}

impl JobManager {
    pub async fn new(settings: JobSettings, executor: Arc<dyn JobExecutor>) -> Result<Arc<Self>> {
        if settings.workers == 0 || settings.queue_capacity == 0 || settings.max_jobs == 0 {
            return Err(AppError::invalid("Job limits must be positive"));
        }
        tokio::fs::create_dir_all(settings.data_dir.join("state"))
            .await
            .map_err(|_| storage_error())?;
        let (sender, receiver) = mpsc::channel(settings.queue_capacity);
        let manager = Arc::new(Self {
            settings,
            entries: Mutex::new(HashMap::new()),
            executor,
            sender,
            stop: CancellationToken::new(),
            tasks: Mutex::new(Vec::new()),
        });
        manager.recover().await?;
        let receiver = Arc::new(Mutex::new(receiver));
        let mut handles = manager.tasks.lock().await;
        for _ in 0..manager.settings.workers {
            let (manager, receiver) = (manager.clone(), receiver.clone());
            handles.push(tokio::spawn(async move {
                loop {
                    let work = tokio::select! { biased; _ = manager.stop.cancelled() => break, work = async { receiver.lock().await.recv().await } => work };
                    let Some(work) = work else { break };
                    manager.execute(work).await;
                }
            }));
        }
        let cleaner = manager.clone();
        handles.push(tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            loop { tokio::select! { _ = cleaner.stop.cancelled() => break, _ = tick.tick() => { if cleaner.cleanup_expired().await.is_err() { tracing::warn!("Job retention cleanup failed"); } } } }
        }));
        drop(handles);
        Ok(manager)
    }
    fn record_path(&self, id: Uuid) -> PathBuf {
        self.settings
            .data_dir
            .join("state")
            .join(format!("{id}.json"))
    }
    async fn persist(&self, record: &JobRecord) -> Result<()> {
        let bytes = serde_json::to_vec(record).map_err(|_| storage_error())?;
        let destination = self.record_path(record.job_id);
        let temporary = destination.with_extension("tmp");
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::File::create(&temporary)
            .await
            .map_err(|_| storage_error())?;
        file.write_all(&bytes).await.map_err(|_| storage_error())?;
        file.sync_all().await.map_err(|_| storage_error())?;
        drop(file);
        tokio::fs::rename(&temporary, &destination)
            .await
            .map_err(|_| storage_error())?;
        // Flush the directory entry so acknowledged state survives a local-volume crash.
        tokio::fs::File::open(self.settings.data_dir.join("state"))
            .await
            .map_err(|_| storage_error())?
            .sync_all()
            .await
            .map_err(|_| storage_error())?;
        Ok(())
    }
    async fn recover(&self) -> Result<()> {
        let mut files = tokio::fs::read_dir(self.settings.data_dir.join("state"))
            .await
            .map_err(|_| storage_error())?;
        let mut entries = self.entries.lock().await;
        while let Some(file) = files.next_entry().await.map_err(|_| storage_error())? {
            let filename = file.file_name();
            let Some(id) = filename
                .to_str()
                .and_then(|s| s.strip_suffix(".json"))
                .and_then(|s| Uuid::parse_str(s).ok())
            else {
                continue;
            };
            let metadata = file.metadata().await.map_err(|_| storage_error())?;
            if !metadata.is_file() || metadata.len() > 64 * 1024 {
                return Err(storage_error());
            }
            let bytes = tokio::fs::read(file.path())
                .await
                .map_err(|_| storage_error())?;
            let mut record: JobRecord =
                serde_json::from_slice(&bytes).map_err(|_| storage_error())?;
            if record.job_id != id {
                return Err(storage_error());
            }
            if record.status.active()
                || record.status == JobStatus::Cancelled
                || record.cleanup_pending
            {
                record.status = JobStatus::Failed;
                record.updated_at = now();
                record.error = Some(AppError::new(
                    ErrorCode::DownloadInterrupted,
                    "Service restarted before this job completed",
                ));
                record.output = None;
                self.executor.cleanup(id).await?;
                record.cleanup_pending = false;
                self.persist(&record).await?;
            }
            if record.expires_at <= now() || entries.len() >= self.settings.max_jobs {
                self.executor.cleanup(id).await?;
                tokio::fs::remove_file(file.path())
                    .await
                    .map_err(|_| storage_error())?;
                continue;
            }
            entries.insert(
                id,
                Entry {
                    record,
                    cancel: CancellationToken::new(),
                    progress: None,
                    delete_requested: false,
                    in_flight: false,
                    readers: Arc::new(AtomicUsize::new(0)),
                },
            );
        }
        let mut directories = tokio::fs::read_dir(&self.settings.data_dir)
            .await
            .map_err(|_| storage_error())?;
        while let Some(directory) = directories
            .next_entry()
            .await
            .map_err(|_| storage_error())?
        {
            if let Some(id) = directory
                .file_name()
                .to_str()
                .and_then(|name| Uuid::parse_str(name).ok())
            {
                if !entries.contains_key(&id) {
                    self.executor.cleanup(id).await?;
                }
            }
        }
        Ok(())
    }
    pub async fn submit(&self, request: DownloadRequest) -> Result<JobRecord> {
        request.validate()?;
        crate::extractor::normalize_url(&request.url)?;
        if self.stop.is_cancelled() {
            return Err(AppError::new(
                ErrorCode::QueueFull,
                "Service is shutting down",
            ));
        }
        let permit = self
            .sender
            .try_reserve()
            .map_err(|_| AppError::new(ErrorCode::QueueFull, "Download queue is full"))?;
        let mut entries = self.entries.lock().await;
        if self.stop.is_cancelled() {
            return Err(AppError::new(
                ErrorCode::QueueFull,
                "Service is shutting down",
            ));
        }
        if entries.len() >= self.settings.max_jobs {
            return Err(AppError::new(
                ErrorCode::QueueFull,
                "Job retention capacity reached; remove completed jobs",
            ));
        }
        let reserved: u64 = entries
            .values()
            .map(|entry| {
                if entry.in_flight || entry.record.status.active() || entry.record.cleanup_pending {
                    self.settings.max_download_bytes
                } else {
                    entry
                        .record
                        .output
                        .as_ref()
                        .map_or(0, |output| output.size_bytes)
                }
            })
            .fold(0_u64, u64::saturating_add);
        if reserved.saturating_add(self.settings.max_download_bytes)
            > self.settings.max_storage_bytes
        {
            return Err(AppError::new(
                ErrorCode::QueueFull,
                "Storage capacity is reserved by existing jobs",
            ));
        }
        let id = Uuid::new_v4();
        let record = JobRecord {
            job_id: id,
            status: JobStatus::Queued,
            progress: DownloadProgress::default(),
            output: None,
            error: None,
            created_at: now(),
            updated_at: now(),
            expires_at: now().saturating_add(self.settings.ttl.as_secs()),
            cleanup_pending: false,
        };
        self.persist(&record).await?;
        entries.insert(
            id,
            Entry {
                record: record.clone(),
                cancel: CancellationToken::new(),
                progress: None,
                delete_requested: false,
                in_flight: false,
                readers: Arc::new(AtomicUsize::new(0)),
            },
        );
        permit.send(Work { id, request });
        tracing::info!(job_id=%id, status="queued", "Download job accepted");
        Ok(record)
    }
    async fn execute(&self, work: Work) {
        let (cancel, sender) = {
            let mut entries = self.entries.lock().await;
            let Some(entry) = entries.get_mut(&work.id) else {
                return;
            };
            if entry.cancel.is_cancelled() || entry.delete_requested {
                return;
            }
            entry.record.updated_at = now();
            entry.record.status = JobStatus::Running;
            entry.in_flight = true;
            let (tx, rx) = watch::channel(DownloadProgress::default());
            entry.progress = Some(rx);
            if self.persist(&entry.record).await.is_err() {
                entry.in_flight = false;
                entry.record.status = JobStatus::Failed;
                entry.record.error = Some(storage_error());
                return;
            }
            (entry.cancel.clone(), tx)
        };
        tracing::info!(job_id=%work.id, status="running", "Download job started");
        // Never drop a running executor on cancellation: it must reap children and close files.
        let operation = self
            .executor
            .run(&work.request, work.id, cancel.clone(), sender);
        tokio::pin!(operation);
        let (result, timed_out) = tokio::select! {
            result = &mut operation => (result, false),
            _ = tokio::time::sleep(self.settings.timeout) => { cancel.cancel(); let _ = operation.await; (Err(AppError::new(ErrorCode::Timeout, "Job execution timed out")), true) }
        };
        let mut entries = self.entries.lock().await;
        let Some(entry) = entries.get_mut(&work.id) else {
            return;
        };
        if let Some(progress) = &entry.progress {
            entry.record.progress = progress.borrow().clone();
        }
        entry.progress = None;
        entry.in_flight = false;
        if entry.delete_requested || (entry.cancel.is_cancelled() && !timed_out) {
            // Serialize completion with cancellation; cleanup only after run has returned.
            entry.record.cleanup_pending = self.executor.cleanup(work.id).await.is_err();
            entry.record.status = JobStatus::Cancelled;
            entry.record.output = None;
            entry.record.error = Some(AppError::new(ErrorCode::Cancelled, "Job cancelled"));
            if entry.delete_requested && !entry.record.cleanup_pending {
                if tokio::fs::remove_file(self.record_path(work.id))
                    .await
                    .is_ok()
                {
                    entries.remove(&work.id);
                    return;
                }
                entry.record.cleanup_pending = true;
            }
        } else {
            match result {
                Ok(output) => {
                    entry.record.status = JobStatus::Completed;
                    entry.record.output = Some(output);
                }
                Err(error) => {
                    entry.record.status = JobStatus::Failed;
                    entry.record.error = Some(error);
                    entry.record.cleanup_pending = self.executor.cleanup(work.id).await.is_err();
                }
            }
        }
        tracing::info!(job_id=%work.id, status=?entry.record.status, error_code=?entry.record.error.as_ref().map(|e| e.code), "Download job finished");
        entry.record.updated_at = now();
        entry.record.expires_at = now().saturating_add(self.settings.ttl.as_secs());
        if self.persist(&entry.record).await.is_err() {
            entry.record.status = JobStatus::Failed;
            entry.record.output = None;
            entry.record.error = Some(storage_error());
            entry.record.cleanup_pending = true;
        }
    }
    pub async fn get(&self, id: Uuid) -> Result<JobRecord> {
        let entries = self.entries.lock().await;
        let entry = entries
            .get(&id)
            .ok_or_else(|| AppError::new(ErrorCode::NotFound, "Job not found"))?;
        let mut record = entry.record.clone();
        if let Some(progress) = &entry.progress {
            record.progress = progress.borrow().clone();
        }
        Ok(record)
    }
    pub async fn cancel(&self, id: Uuid) -> Result<()> {
        let mut entries = self.entries.lock().await;
        let Some(entry) = entries.get_mut(&id) else {
            return Ok(());
        };
        entry.cancel.cancel();
        tracing::info!(job_id=%id, "Download cancellation requested");
        entry.delete_requested = true;
        entry.record.cleanup_pending = true;
        let was_running = entry.in_flight;
        entry.record.status = JobStatus::Cancelled;
        entry.record.updated_at = now();
        // Preserve a completed output reservation while a file response has an open inode.
        entry.record.error = Some(AppError::new(ErrorCode::Cancelled, "Job cancelled"));
        self.persist(&entry.record).await?;
        if !was_running && entry.readers.load(Ordering::SeqCst) == 0 {
            self.executor.cleanup(id).await?;
            match tokio::fs::remove_file(self.record_path(id)).await {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(_) => return Err(storage_error()),
            }
            entries.remove(&id);
        }
        Ok(())
    }
    pub async fn open_completed(
        &self,
        id: Uuid,
    ) -> Result<(tokio::fs::File, StoredOutput, FileLease)> {
        let entries = self.entries.lock().await;
        let entry = entries
            .get(&id)
            .ok_or_else(|| AppError::new(ErrorCode::NotFound, "Job not found"))?;
        if entry.record.status != JobStatus::Completed {
            return Err(AppError::invalid("Job has no completed file"));
        }
        let output = entry.record.output.as_ref().ok_or_else(storage_error)?;
        // Persisted paths are untrusted after restart. Canonical containment also rejects symlinks.
        let expected = tokio::fs::canonicalize(self.settings.data_dir.join(id.to_string()))
            .await
            .map_err(|_| storage_error())?;
        let path = tokio::fs::canonicalize(&output.path)
            .await
            .map_err(|_| storage_error())?;
        let root = tokio::fs::canonicalize(&self.settings.data_dir)
            .await
            .map_err(|_| storage_error())?;
        if expected != root.join(id.to_string()) || path.parent() != Some(expected.as_path()) {
            return Err(storage_error());
        }
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|_| storage_error())?;
        entry.readers.fetch_add(1, Ordering::SeqCst);
        Ok((file, output.clone(), FileLease(entry.readers.clone())))
    }
    async fn cleanup_expired(&self) -> Result<()> {
        let mut entries = self.entries.lock().await;
        let expired: Vec<_> = entries
            .iter()
            .filter(|(_, e)| {
                !e.in_flight
                    && e.readers.load(Ordering::SeqCst) == 0
                    && !e.record.status.active()
                    && (e.delete_requested
                        || e.record.cleanup_pending
                        || e.record.expires_at <= now())
            })
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.executor.cleanup(id).await?;
            tokio::fs::remove_file(self.record_path(id))
                .await
                .map_err(|_| storage_error())?;
            entries.remove(&id);
        }
        Ok(())
    }
    pub async fn begin_shutdown(&self) {
        self.stop.cancel();
        let entries = self.entries.lock().await;
        for entry in entries.values() {
            if entry.record.status.active() || entry.in_flight {
                entry.cancel.cancel();
            }
        }
    }
    pub async fn shutdown(&self) {
        self.begin_shutdown().await;
        let handles = std::mem::take(&mut *self.tasks.lock().await);
        for handle in handles {
            let _ = handle.await;
        }
        let mut entries = self.entries.lock().await;
        for (id, entry) in entries.iter_mut() {
            if entry.record.status.active() {
                entry.record.status = JobStatus::Failed;
                entry.record.output = None;
                entry.record.error = Some(AppError::new(
                    ErrorCode::DownloadInterrupted,
                    "Service shut down before job completed",
                ));
                let _ = self.executor.cleanup(*id).await;
                let _ = self.persist(&entry.record).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DownloadKind, OutputFormat};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Semaphore;
    struct Controlled {
        root: PathBuf,
        started: Semaphore,
        release: Semaphore,
        cleanups: AtomicUsize,
        fail_cleanup: std::sync::atomic::AtomicBool,
    }
    #[async_trait]
    impl JobExecutor for Controlled {
        async fn run(
            &self,
            _: &DownloadRequest,
            id: Uuid,
            _: CancellationToken,
            _: watch::Sender<DownloadProgress>,
        ) -> Result<StoredOutput> {
            let dir = self.root.join(id.to_string());
            tokio::fs::create_dir_all(&dir).await.unwrap();
            let path = dir.join("output.mp4");
            tokio::fs::write(&path, b"controlled-media").await.unwrap();
            self.started.add_permits(1);
            self.release.acquire().await.unwrap().forget();
            Ok(StoredOutput {
                path,
                filename: "output.mp4".into(),
                content_type: "video/mp4".into(),
                size_bytes: 16,
            })
        }
        async fn cleanup(&self, id: Uuid) -> Result<()> {
            if self.fail_cleanup.load(Ordering::SeqCst) {
                return Err(storage_error());
            }
            self.cleanups.fetch_add(1, Ordering::SeqCst);
            let _ = tokio::fs::remove_dir_all(self.root.join(id.to_string())).await;
            Ok(())
        }
    }
    fn request() -> DownloadRequest {
        DownloadRequest {
            url: "https://youtu.be/aqz-KE-bpKQ".into(),
            kind: DownloadKind::Video,
            format: OutputFormat::Mp4,
            quality: None,
            audio_bitrate_kbps: None,
        }
    }
    fn settings(root: &Path) -> JobSettings {
        JobSettings {
            data_dir: root.into(),
            workers: 1,
            queue_capacity: 1,
            max_jobs: 5,
            max_download_bytes: 100,
            max_storage_bytes: 500,
            ttl: Duration::from_secs(60),
            timeout: Duration::from_secs(10),
        }
    }
    use std::path::Path;
    fn controlled(root: &Path) -> Arc<Controlled> {
        Arc::new(Controlled {
            root: root.into(),
            started: Semaphore::new(0),
            release: Semaphore::new(0),
            cleanups: AtomicUsize::new(0),
            fail_cleanup: std::sync::atomic::AtomicBool::new(false),
        })
    }
    async fn wait_status(manager: &JobManager, id: Uuid, status: JobStatus) -> JobRecord {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(record) = manager.get(id).await {
                    if record.status == status {
                        return record;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn bounded_queue_and_repeated_cancel_cannot_cleanup_live_worker() {
        let temp = tempfile::tempdir().unwrap();
        let executor = controlled(temp.path());
        let manager = JobManager::new(settings(temp.path()), executor.clone())
            .await
            .unwrap();
        let first = manager.submit(request()).await.unwrap();
        executor.started.acquire().await.unwrap().forget();
        let second = manager.submit(request()).await.unwrap();
        assert_eq!(
            manager.submit(request()).await.unwrap_err().code,
            ErrorCode::QueueFull
        );
        manager.cancel(first.job_id).await.unwrap();
        manager.cancel(first.job_id).await.unwrap();
        manager
            .entries
            .lock()
            .await
            .get_mut(&first.job_id)
            .unwrap()
            .record
            .expires_at = 0;
        manager.cleanup_expired().await.unwrap();
        assert_eq!(executor.cleanups.load(Ordering::SeqCst), 0);
        assert!(temp.path().join(first.job_id.to_string()).exists());
        assert!(
            manager
                .entries
                .lock()
                .await
                .get(&first.job_id)
                .unwrap()
                .in_flight
        );
        manager.cancel(second.job_id).await.unwrap();
        executor.release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(2), async {
            while manager.get(first.job_id).await.is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!temp.path().join(first.job_id.to_string()).exists());
        manager.shutdown().await;
    }
    #[tokio::test]
    async fn cancellation_retains_quota_until_worker_exits_and_timeout_remains_timeout() {
        let temp = tempfile::tempdir().unwrap();
        let executor = controlled(temp.path());
        let mut options = settings(temp.path());
        options.max_storage_bytes = 100;
        options.timeout = Duration::from_millis(20);
        let manager = JobManager::new(options, executor.clone()).await.unwrap();
        let first = manager.submit(request()).await.unwrap();
        executor.started.acquire().await.unwrap().forget();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            manager.submit(request()).await.unwrap_err().code,
            ErrorCode::QueueFull
        );
        executor.release.add_permits(1);
        let record = wait_status(&manager, first.job_id, JobStatus::Failed).await;
        assert_eq!(record.error.unwrap().code, ErrorCode::Timeout);
        manager.shutdown().await;
    }
    #[tokio::test]
    async fn completed_file_lease_holds_quota_after_delete() {
        let temp = tempfile::tempdir().unwrap();
        let executor = controlled(temp.path());
        let mut options = settings(temp.path());
        options.max_storage_bytes = 100;
        let manager = JobManager::new(options, executor.clone()).await.unwrap();
        executor.release.add_permits(1);
        let job = manager.submit(request()).await.unwrap();
        wait_status(&manager, job.job_id, JobStatus::Completed).await;
        let (file, _, lease) = manager.open_completed(job.job_id).await.unwrap();
        manager.cancel(job.job_id).await.unwrap();
        manager.cancel(job.job_id).await.unwrap();
        assert_eq!(
            manager.submit(request()).await.unwrap_err().code,
            ErrorCode::QueueFull
        );
        manager.cleanup_expired().await.unwrap();
        assert!(temp.path().join(job.job_id.to_string()).exists());
        drop(file);
        drop(lease);
        manager.cleanup_expired().await.unwrap();
        assert!(!temp.path().join(job.job_id.to_string()).exists());
        manager.shutdown().await;
    }
    #[tokio::test]
    async fn restart_recovers_completed_state_and_fails_interrupted_jobs() {
        let temp = tempfile::tempdir().unwrap();
        let executor = controlled(temp.path());
        let options = settings(temp.path());
        let manager = JobManager::new(options.clone(), executor.clone())
            .await
            .unwrap();
        executor.release.add_permits(1);
        let done = manager.submit(request()).await.unwrap();
        wait_status(&manager, done.job_id, JobStatus::Completed).await;
        let interrupted = JobRecord {
            job_id: Uuid::new_v4(),
            status: JobStatus::Running,
            progress: DownloadProgress::default(),
            output: None,
            error: None,
            created_at: now(),
            updated_at: now(),
            expires_at: now() + 60,
            cleanup_pending: false,
        };
        manager.persist(&interrupted).await.unwrap();
        manager.shutdown().await;
        let recovered = JobManager::new(options, executor.clone()).await.unwrap();
        assert_eq!(
            recovered.get(done.job_id).await.unwrap().status,
            JobStatus::Completed
        );
        let failed = recovered.get(interrupted.job_id).await.unwrap();
        assert_eq!(failed.status, JobStatus::Failed);
        assert_eq!(failed.error.unwrap().code, ErrorCode::DownloadInterrupted);
        let (file, _, lease) = recovered.open_completed(done.job_id).await.unwrap();
        drop(file);
        drop(lease);
        recovered.shutdown().await;
    }
    #[tokio::test]
    async fn persisted_path_cannot_serve_outside_uuid_directory() {
        let temp = tempfile::tempdir().unwrap();
        let executor = controlled(temp.path());
        let manager = JobManager::new(settings(temp.path()), executor.clone())
            .await
            .unwrap();
        executor.release.add_permits(1);
        let job = manager.submit(request()).await.unwrap();
        wait_status(&manager, job.job_id, JobStatus::Completed).await;
        let secret = temp.path().join("secret");
        tokio::fs::write(&secret, "private").await.unwrap();
        manager
            .entries
            .lock()
            .await
            .get_mut(&job.job_id)
            .unwrap()
            .record
            .output
            .as_mut()
            .unwrap()
            .path = secret;
        assert!(manager.open_completed(job.job_id).await.is_err());
        manager.shutdown().await;
    }
    #[tokio::test]
    async fn cleanup_failure_keeps_budget_reserved_until_retried() {
        let temp = tempfile::tempdir().unwrap();
        let executor = controlled(temp.path());
        let mut options = settings(temp.path());
        options.max_storage_bytes = 100;
        let manager = JobManager::new(options, executor.clone()).await.unwrap();
        let job = manager.submit(request()).await.unwrap();
        executor.started.acquire().await.unwrap().forget();
        manager.cancel(job.job_id).await.unwrap();
        executor.fail_cleanup.store(true, Ordering::SeqCst);
        executor.release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if !manager
                    .entries
                    .lock()
                    .await
                    .get(&job.job_id)
                    .unwrap()
                    .in_flight
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(manager.get(job.job_id).await.unwrap().cleanup_pending);
        assert_eq!(
            manager.submit(request()).await.unwrap_err().code,
            ErrorCode::QueueFull
        );
        assert!(manager.cleanup_expired().await.is_err());
        executor.fail_cleanup.store(false, Ordering::SeqCst);
        manager.cleanup_expired().await.unwrap();
        executor.release.add_permits(1);
        assert!(manager.submit(request()).await.is_ok());
        manager.shutdown().await;
    }
}
