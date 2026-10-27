//! Railway keyless Cloud Run VM controller and authenticated GCP worker transport.
//! This is intentionally single-controller: deploy one Railway replica with persistent DATA_DIR.
use crate::{
    download::DownloadProgress,
    error::{AppError, ErrorCode, Result},
    extractor::Extractor,
    jobs::JobExecutor,
    model::{DownloadRequest, VideoInfo},
    storage::{LocalStorage, Storage, StoredOutput},
};
use async_trait::async_trait;
use reqwest::{header::HeaderValue, Client, Method, StatusCode, Url};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    env,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::{Duration, Instant},
};
use tokio::{
    fs,
    io::AsyncWriteExt,
    sync::{watch, Mutex},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn bad(message: &'static str) -> AppError {
    AppError::new(ErrorCode::Network, message)
}

fn required(name: &'static str) -> Result<String> {
    env::var(name)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| AppError::invalid(format!("{name} must be configured for remote mode")))
}
fn https_origin(name: &'static str, value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| AppError::invalid(format!("Invalid {name}")))?;
    if value.trim() != value
        || value.bytes().any(|b| b.is_ascii_control())
        || value.contains(['@', '\\'])
        || value.split_once("://").is_none_or(|(_, authority)| {
            authority
                .split_once('/')
                .is_some_and(|(_, path)| !path.is_empty())
        })
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::invalid(format!("{name} must be an HTTPS origin")));
    }
    Ok(url)
}

fn controller_token(value: &str) -> Result<HeaderValue> {
    if value.len() < 32 || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(AppError::invalid(
            "GCP_CONTROLLER_TOKEN must contain at least 32 visible ASCII characters without whitespace",
        ));
    }
    let mut token = HeaderValue::from_str(value)
        .map_err(|_| AppError::invalid("Invalid GCP_CONTROLLER_TOKEN"))?;
    token.set_sensitive(true);
    Ok(token)
}

fn controller_client() -> reqwest::ClientBuilder {
    Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(45))
        .redirect(reqwest::redirect::Policy::none())
}

const MAX_CONTROLLER_STATUS_BYTES: usize = 16 * 1024;
const LIFECYCLE_POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleReply {
    Accepted,
    Transient,
}

#[derive(Clone)]
struct Settings {
    controller: Url,
    controller_token: HeaderValue,
    worker: Url,
    worker_token: String,
    access_id: String,
    access_secret: String,
    idle: Duration,
    start_timeout: Duration,
    max_bytes: u64,
    max_attempts: usize,
}

#[derive(Deserialize)]
struct Instance {
    status: String,
}

#[derive(Deserialize)]
struct WorkerOutput {
    filename: String,
    size_bytes: u64,
}

#[derive(Deserialize)]
struct WorkerJob {
    job_id: Uuid,
    status: String,
    progress: DownloadProgress,
    output: Option<WorkerOutput>,
}

struct Activity<'a>(&'a Remote);
impl Drop for Activity<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        if let Ok(mut last) = self.0.last_activity.lock() {
            *last = Instant::now();
        }
    }
}

pub struct Remote {
    settings: Settings,
    controller: Client,
    worker: Client,
    storage: LocalStorage,
    lifecycle: Mutex<()>,
    active: AtomicUsize,
    last_activity: StdMutex<Instant>,
}

impl Remote {
    pub fn from_env(storage: LocalStorage, max_bytes: u64) -> Result<Arc<Self>> {
        let worker = https_origin("GCP_WORKER_URL", &required("GCP_WORKER_URL")?)?;
        let controller = https_origin("GCP_CONTROLLER_URL", &required("GCP_CONTROLLER_URL")?)?;
        let controller_token = controller_token(&required("GCP_CONTROLLER_TOKEN")?)?;
        let worker_token = required("GCP_WORKER_TOKEN")?;
        if worker_token.len() < 32 {
            return Err(AppError::invalid(
                "GCP_WORKER_TOKEN must be at least 32 characters",
            ));
        }
        let idle = env::var("GCP_IDLE_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(300);
        let start_timeout = env::var("GCP_START_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(300);
        if !(60..=3600).contains(&idle) || !(60..=900).contains(&start_timeout) {
            return Err(AppError::invalid("Invalid GCP idle or startup timeout"));
        }
        let controller_http = controller_client()
            .build()
            .map_err(|_| bad("VM controller HTTP client unavailable"))?;
        let worker_http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(1800))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| bad("Worker HTTP client unavailable"))?;
        let max_attempts = env::var("GCP_MAX_JOB_ATTEMPTS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(2);
        if !(1..=3).contains(&max_attempts) {
            return Err(AppError::invalid(
                "GCP_MAX_JOB_ATTEMPTS must be between 1 and 3",
            ));
        }
        Ok(Arc::new(Self {
            settings: Settings {
                controller,
                controller_token,
                worker,
                worker_token,
                access_id: required("CF_ACCESS_CLIENT_ID")?,
                access_secret: required("CF_ACCESS_CLIENT_SECRET")?,
                idle: Duration::from_secs(idle),
                start_timeout: Duration::from_secs(start_timeout),
                max_bytes,
                max_attempts,
            },
            controller: controller_http,
            worker: worker_http,
            storage,
            lifecycle: Mutex::new(()),
            active: AtomicUsize::new(0),
            last_activity: StdMutex::new(Instant::now()),
        }))
    }

    fn activity(&self) -> Activity<'_> {
        self.active.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut last) = self.last_activity.lock() {
            *last = Instant::now();
        }
        Activity(self)
    }

    fn controller_req(&self, method: Method, path: &'static str) -> reqwest::RequestBuilder {
        let url = self
            .settings
            .controller
            .join(path)
            .expect("fixed controller API path");
        self.controller
            .request(method, url)
            .header("X-Controller-Token", self.settings.controller_token.clone())
    }

    async fn vm_state(&self) -> Result<String> {
        let mut response = self
            .controller_req(Method::GET, "/status")
            .send()
            .await
            .map_err(|_| bad("Cannot reach VM controller for status"))?;
        if response.status() != StatusCode::OK {
            return Err(bad("VM controller status request failed"));
        }
        // Status is tiny; cap both declared and streamed bodies before parsing.
        if response
            .content_length()
            .is_some_and(|n| n > MAX_CONTROLLER_STATUS_BYTES as u64)
        {
            return Err(bad("VM controller status response is too large"));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| bad("Cannot read VM controller status"))?
        {
            if chunk.len() > MAX_CONTROLLER_STATUS_BYTES.saturating_sub(body.len()) {
                return Err(bad("VM controller status response is too large"));
            }
            body.extend_from_slice(&chunk);
        }
        let instance: Instance = serde_json::from_slice(&body)
            .map_err(|_| bad("Invalid VM controller status response"))?;
        if !matches!(
            instance.status.as_str(),
            "PROVISIONING"
                | "STAGING"
                | "RUNNING"
                | "STOPPING"
                | "SUSPENDING"
                | "SUSPENDED"
                | "REPAIRING"
                | "TERMINATED"
        ) {
            return Err(bad("VM controller returned an unknown VM state"));
        }
        Ok(instance.status)
    }

    async fn vm_action(&self, action: &'static str) -> Result<LifecycleReply> {
        let response = self
            .controller_req(Method::POST, action)
            .send()
            .await
            .map_err(|_| bad("Cannot reach VM controller for lifecycle request"))?;
        match response.status() {
            StatusCode::OK | StatusCode::ACCEPTED => Ok(LifecycleReply::Accepted),
            StatusCode::CONFLICT => Ok(LifecycleReply::Transient),
            _ => Err(bad("VM controller rejected lifecycle request")),
        }
    }

    fn worker_req(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let url = self
            .settings
            .worker
            .join(path)
            .expect("fixed internal API path");
        self.worker
            .request(method, url)
            .bearer_auth(&self.settings.worker_token)
            .header("CF-Access-Client-Id", &self.settings.access_id)
            .header("CF-Access-Client-Secret", &self.settings.access_secret)
    }

    async fn worker_ready(&self) -> bool {
        matches!(
            tokio::time::timeout(Duration::from_secs(7), self.worker_req(Method::GET, "/health").send()).await,
            Ok(Ok(response)) if response.status() == StatusCode::OK
        )
    }

    async fn ensure_ready(&self) -> Result<()> {
        let _lock = self.lifecycle.lock().await;
        // Bound the entire polling operation, including in-flight HTTP calls.
        tokio::time::timeout(self.settings.start_timeout, self.poll_until_ready())
            .await
            .map_err(|_| AppError::new(ErrorCode::Timeout, "GCP worker startup timed out"))?
    }

    async fn poll_until_ready(&self) -> Result<()> {
        let mut start_requested = false;
        loop {
            match self.vm_state().await?.as_str() {
                "RUNNING" => {
                    if self.worker_ready().await {
                        return Ok(());
                    }
                }
                "TERMINATED" => {
                    if !start_requested {
                        // A 409 is a lifecycle race: poll status and retry when stopped.
                        start_requested =
                            self.vm_action("/start").await? == LifecycleReply::Accepted;
                    }
                }
                "STOPPING" | "STAGING" | "PROVISIONING" | "REPAIRING" => (),
                _ => return Err(bad("GCP VM is in an unsupported state")),
            }
            tokio::time::sleep(LIFECYCLE_POLL_INTERVAL).await;
        }
    }

    pub fn start_idle_watch(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(15));
            loop {
                tick.tick().await;
                if let Err(err) = this.stop_if_idle().await {
                    tracing::warn!(code=?err.code, "Idle VM stop attempt failed");
                }
            }
        });
    }

    async fn worker_busy(&self) -> Result<bool> {
        let response = self
            .worker_req(Method::GET, "/api/v1/internal/activity")
            .send()
            .await
            .map_err(|_| bad("Cannot verify GCP worker is idle"))?;
        if response.status() != StatusCode::OK {
            return Err(bad("GCP worker activity check failed"));
        }
        let activity: Value = response
            .json()
            .await
            .map_err(|_| bad("Invalid GCP activity reply"))?;
        activity
            .get("busy")
            .and_then(Value::as_bool)
            .ok_or_else(|| bad("GCP activity reply was incomplete"))
    }

    async fn stop_if_idle(&self) -> Result<()> {
        if self.active.load(Ordering::SeqCst) != 0 {
            return Ok(());
        }
        let _lock = self.lifecycle.lock().await;
        if self.active.load(Ordering::SeqCst) != 0 {
            return Ok(());
        }
        let idle = self
            .last_activity
            .lock()
            .map_err(|_| bad("Idle tracking unavailable"))?
            .elapsed();
        if idle < self.settings.idle {
            return Ok(());
        }
        if self.vm_state().await? == "RUNNING" {
            // Never stop untracked work after a Railway restart or lost callback.
            if self.worker_busy().await? {
                if let Ok(mut last) = self.last_activity.lock() {
                    *last = Instant::now();
                }
                return Ok(());
            }
            // The lifecycle mutex blocks new dispatch until the stop request is accepted.
            if self.vm_action("/stop").await? == LifecycleReply::Accepted {
                tracing::info!("Idle GCP extractor stop accepted");
            }
            // A 409 leaves shutdown pending; the next idle tick checks state again.
        }
        Ok(())
    }

    async fn run_attempt(
        &self,
        request: &DownloadRequest,
        local: Uuid,
        remote: Uuid,
        cancel: &CancellationToken,
        progress: &watch::Sender<DownloadProgress>,
    ) -> Result<StoredOutput> {
        let mut dispatched = None;
        for attempt in 0..2 {
            match self
                .worker_req(Method::POST, "/api/v1/downloads")
                .header("X-Idempotency-Key", remote.to_string())
                .json(request)
                .send()
                .await
            {
                Ok(response) => {
                    dispatched = Some(response);
                    break;
                }
                Err(_) if attempt == 0 => tokio::time::sleep(Duration::from_secs(2)).await,
                Err(_) => return Err(bad("Cannot dispatch job to GCP")),
            }
        }
        let response = dispatched.ok_or_else(|| bad("GCP dispatch unavailable"))?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            return Err(AppError::new(ErrorCode::QueueFull, "GCP queue is full"));
        }
        if !response.status().is_success() {
            return Err(bad("GCP refused download request"));
        }
        let job: WorkerJob = response
            .json()
            .await
            .map_err(|_| bad("Invalid GCP dispatch reply"))?;
        if job.job_id != remote {
            return Err(bad("GCP returned a different job identifier"));
        }
        let result = async {
            loop {
                let view = tokio::select! {
                    _ = cancel.cancelled() => return Err(AppError::new(ErrorCode::Cancelled, "Job cancelled")),
                    reply = self.worker_job(remote) => reply?,
                };
                let _ = progress.send(view.progress);
                match view.status.as_str() {
                    "completed" => {
                        let output = view.output.ok_or_else(|| bad("Worker returned no completed file"))?;
                        return self.retrieve(local, remote, &output, cancel).await;
                    }
                    "failed" => return Err(AppError::new(ErrorCode::DownloadInterrupted, "GCP download failed")),
                    "cancelled" => return Err(AppError::new(ErrorCode::Cancelled, "GCP download cancelled")),
                    "queued" | "running" => (),
                    _ => return Err(bad("Unknown GCP job state")),
                }
                tokio::select! {
                    _ = cancel.cancelled() => return Err(AppError::new(ErrorCode::Cancelled, "Job cancelled")),
                    _ = tokio::time::sleep(Duration::from_secs(3)) => (),
                }
            }
        }.await;
        self.cancel_worker(remote).await;
        result
    }

    async fn worker_job(&self, id: Uuid) -> Result<WorkerJob> {
        let path = format!("/api/v1/downloads/{id}");
        let response = self
            .worker_req(Method::GET, &path)
            .send()
            .await
            .map_err(|_| bad("Cannot fetch worker job state"))?;
        if !response.status().is_success() {
            return Err(bad("GCP job lookup failed"));
        }
        response
            .json()
            .await
            .map_err(|_| bad("Invalid GCP job status"))
    }

    async fn cancel_worker(&self, id: Uuid) {
        let path = format!("/api/v1/downloads/{id}");
        let _ = self.worker_req(Method::DELETE, &path).send().await;
    }

    async fn retrieve(
        &self,
        job: Uuid,
        remote: Uuid,
        output: &WorkerOutput,
        cancel: &CancellationToken,
    ) -> Result<StoredOutput> {
        let ext = output.filename.rsplit('.').next().unwrap_or("");
        let mime = match ext {
            "mp4" => "video/mp4",
            "webm" => "video/webm",
            "mp3" => "audio/mpeg",
            "m4a" => "audio/mp4",
            _ => return Err(bad("GCP produced an unsupported media file")),
        };
        if output.size_bytes == 0 || output.size_bytes >= self.settings.max_bytes {
            return Err(AppError::new(
                ErrorCode::DownloadTooLarge,
                "Remote file exceeds size limit",
            ));
        }
        let dir = self.storage.create_job(job).await?;
        let result = async {
            let path = format!("/api/v1/downloads/{remote}/file");
            let mut response = self.worker_req(Method::GET, &path).send().await
                .map_err(|_| bad("Cannot stream media from GCP"))?;
            if response.status() != StatusCode::OK {
                return Err(bad("GCP media response failed"));
            }
            let name = dir.join(format!("source.{ext}"));
            let mut file = fs::File::create(&name).await
                .map_err(|_| AppError::new(ErrorCode::Storage, "Cannot create destination file"))?;
            let mut total = 0_u64;
            while let Some(chunk) = tokio::select! {
                _ = cancel.cancelled() => return Err(AppError::new(ErrorCode::Cancelled, "Transfer cancelled")),
                chunk = tokio::time::timeout(Duration::from_secs(60), response.chunk()) => {
                    chunk.map_err(|_| bad("GCP media transfer stalled"))?
                        .map_err(|_| bad("GCP media transfer interrupted"))?
                }
            } {
                total = total.saturating_add(chunk.len() as u64);
                if total >= self.settings.max_bytes || total > output.size_bytes {
                    return Err(AppError::new(ErrorCode::DownloadTooLarge, "Remote file exceeds size limit"));
                }
                file.write_all(&chunk).await
                    .map_err(|_| AppError::new(ErrorCode::Storage, "Cannot write downloaded file"))?;
            }
            if total != output.size_bytes {
                return Err(bad("GCP media transfer was incomplete"));
            }
            file.sync_all().await.map_err(|_| AppError::new(ErrorCode::Storage, "Cannot sync media"))?;
            drop(file);
            self.storage.publish(job, &name, ext, mime).await
        }.await;
        if result.is_err() {
            let _ = self.storage.cleanup(job).await;
        }
        result
    }
}

#[async_trait]
impl Extractor for Remote {
    async fn video_info(&self, url: &str) -> Result<VideoInfo> {
        let _active = self.activity();
        self.ensure_ready().await?;
        let response = self
            .worker_req(Method::POST, "/api/v1/videos/info")
            .json(&json!({"url":url}))
            .send()
            .await
            .map_err(|_| bad("GCP metadata request failed"))?;
        if !response.status().is_success() {
            return Err(AppError::new(
                ErrorCode::AccessRestricted,
                "GCP could not retrieve YouTube metadata",
            ));
        }
        response
            .json()
            .await
            .map_err(|_| bad("Invalid GCP metadata response"))
    }
}

#[async_trait]
impl JobExecutor for Remote {
    async fn run(
        &self,
        request: &DownloadRequest,
        id: Uuid,
        cancel: CancellationToken,
        progress: watch::Sender<DownloadProgress>,
    ) -> Result<StoredOutput> {
        let _active = self.activity();
        tokio::select! {
            _ = cancel.cancelled() => return Err(AppError::new(ErrorCode::Cancelled, "Job cancelled")),
            start = self.ensure_ready() => start?,
        }
        for attempt in 0..self.settings.max_attempts {
            if cancel.is_cancelled() {
                return Err(AppError::new(ErrorCode::Cancelled, "Job cancelled"));
            }
            // First attempt uses Railway's UUID. Subsequent attempts are independent,
            // but each retries uncertain HTTP delivery with a stable idempotency key.
            let remote = if attempt == 0 { id } else { Uuid::new_v4() };
            let result = self
                .run_attempt(request, id, remote, &cancel, &progress)
                .await;
            match result {
                Ok(output) => return Ok(output),
                Err(error)
                    if error.code == ErrorCode::DownloadInterrupted
                        && attempt + 1 < self.settings.max_attempts
                        && !cancel.is_cancelled() =>
                {
                    tracing::warn!(job_id=%id, attempt=attempt+1, "Retrying interrupted extraction");
                    tokio::select! {
                        _ = cancel.cancelled() => return Err(AppError::new(ErrorCode::Cancelled, "Job cancelled")),
                        _ = tokio::time::sleep(Duration::from_secs(3)) => (),
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Err(bad("Remote job attempts exhausted"))
    }

    async fn cleanup(&self, id: Uuid) -> Result<()> {
        self.storage.cleanup(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        extract::{Request, State},
        http::HeaderMap,
        response::Response,
        Router,
    };
    use std::collections::VecDeque;
    use tempfile::TempDir;
    use tokio::{net::TcpListener, task::JoinHandle};

    const CONTROLLER_TOKEN: &str = "controller-secret-at-least-32-characters";
    const WORKER_TOKEN: &str = "worker-secret-at-least-32-characters";

    struct Reply {
        status: StatusCode,
        body: String,
        location: Option<String>,
        delay: Duration,
        streamed: bool,
    }
    impl Reply {
        fn new(status: StatusCode, body: impl Into<String>) -> Self {
            Self {
                status,
                body: body.into(),
                location: None,
                delay: Duration::ZERO,
                streamed: false,
            }
        }
        fn state(state: &str) -> Self {
            Self::new(StatusCode::OK, json!({"status": state}).to_string())
        }
    }
    #[derive(Clone)]
    struct RequestLog {
        method: Method,
        path: String,
        headers: HeaderMap,
    }
    #[derive(Clone)]
    struct MockState {
        replies: Arc<StdMutex<VecDeque<Reply>>>,
        requests: Arc<StdMutex<Vec<RequestLog>>>,
    }
    struct Mock {
        origin: Url,
        requests: Arc<StdMutex<Vec<RequestLog>>>,
        task: JoinHandle<()>,
    }
    impl Drop for Mock {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    async fn respond(State(state): State<MockState>, request: Request) -> Response {
        state.requests.lock().unwrap().push(RequestLog {
            method: request.method().clone(),
            path: request.uri().path().to_string(),
            headers: request.headers().clone(),
        });
        let reply = state
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Reply::new(StatusCode::INTERNAL_SERVER_ERROR, "No scripted reply"));
        tokio::time::sleep(reply.delay).await;
        let mut response = Response::builder().status(reply.status);
        if let Some(location) = reply.location {
            response = response.header("Location", location);
        }
        let body = if reply.streamed {
            let chunks: Vec<_> = reply
                .body
                .as_bytes()
                .chunks(4096)
                .map(|chunk| {
                    Ok::<_, std::convert::Infallible>(bytes::Bytes::copy_from_slice(chunk))
                })
                .collect();
            Body::from_stream(futures_util::stream::iter(chunks))
        } else {
            Body::from(reply.body)
        };
        response.body(body).unwrap()
    }
    async fn mock(replies: Vec<Reply>) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let state = MockState {
            replies: Arc::new(StdMutex::new(replies.into())),
            requests: Arc::new(StdMutex::new(Vec::new())),
        };
        let requests = state.requests.clone();
        let app = Router::new().fallback(respond).with_state(state);
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Mock {
            origin,
            requests,
            task,
        }
    }
    async fn remote(controller: &Mock, worker: &Mock) -> (Arc<Remote>, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage = LocalStorage::new(dir.path().to_path_buf()).await.unwrap();
        // Local origins are injected only here. Production always uses https_origin.
        let remote = Remote {
            settings: Settings {
                controller: controller.origin.clone(),
                controller_token: controller_token(CONTROLLER_TOKEN).unwrap(),
                worker: worker.origin.clone(),
                worker_token: WORKER_TOKEN.into(),
                access_id: "test-access-id".into(),
                access_secret: "test-access-secret".into(),
                idle: Duration::from_secs(60),
                start_timeout: Duration::from_secs(30),
                max_bytes: 1024,
                max_attempts: 2,
            },
            controller: controller_client().no_proxy().build().unwrap(),
            worker: Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(1))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            storage,
            lifecycle: Mutex::new(()),
            active: AtomicUsize::new(0),
            last_activity: StdMutex::new(Instant::now()),
        };
        (Arc::new(remote), dir)
    }
    fn expire_idle(remote: &Remote) {
        *remote.last_activity.lock().unwrap() = Instant::now() - Duration::from_secs(61);
    }

    #[test]
    fn controller_configuration_rejects_invalid_origins_and_tokens() {
        for value in [
            "http://controller.example",
            "https://user:secret@controller.example",
            "https://@controller.example",
            "https://controller.example/path",
            "https://controller.example/path/..",
            "https://controller.example/.",
            "https://controller.example?secret=abc",
            "https://controller.example#fragment",
            "https://controller.example/\\path",
            " https://controller.example",
            "https://controller.example\n",
            "not a url",
            "",
        ] {
            let error = https_origin("GCP_CONTROLLER_URL", value).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidRequest);
            assert!(!error.message.contains(value) || value.is_empty());
        }
        for value in [
            "",
            "short",
            "                                ",
            "controller-token-with-a-newline-123\r\nX-Forged: yes",
            "controller-token-with-a-space-123 456",
            "éééééééééééééééé",
        ] {
            assert!(controller_token(value).is_err());
        }
        for value in [
            "https://controller.example",
            "https://controller.example/",
            "https://controller.example:8443",
        ] {
            assert!(https_origin("GCP_CONTROLLER_URL", value).is_ok());
        }
        let token = controller_token(CONTROLLER_TOKEN).unwrap();
        assert!(token.is_sensitive());
        assert!(!format!("{token:?}").contains(CONTROLLER_TOKEN));
        assert!(controller_token(&"x".repeat(32)).is_ok());
        assert!(controller_token(&"x".repeat(31)).is_err());
    }

    #[tokio::test]
    async fn controller_contract_and_worker_credentials_are_isolated() {
        let controller = mock(vec![
            Reply::state("RUNNING"),
            Reply::new(StatusCode::ACCEPTED, ""),
            Reply::new(StatusCode::OK, ""),
        ])
        .await;
        let worker = mock(vec![Reply::new(StatusCode::OK, "")]).await;
        let (remote, _dir) = remote(&controller, &worker).await;
        assert_eq!(remote.vm_state().await.unwrap(), "RUNNING");
        assert_eq!(
            remote.vm_action("/start").await.unwrap(),
            LifecycleReply::Accepted
        );
        assert_eq!(
            remote.vm_action("/stop").await.unwrap(),
            LifecycleReply::Accepted
        );
        assert!(remote.worker_ready().await);
        let requests = controller.requests.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .map(|r| (r.method.clone(), r.path.as_str()))
                .collect::<Vec<_>>(),
            [
                (Method::GET, "/status"),
                (Method::POST, "/start"),
                (Method::POST, "/stop")
            ]
        );
        for request in requests.iter() {
            assert_eq!(request.headers["X-Controller-Token"], CONTROLLER_TOKEN);
            assert!(!request.headers.contains_key("Authorization"));
            assert!(!request.headers.contains_key("CF-Access-Client-Id"));
            assert!(!request.headers.contains_key("CF-Access-Client-Secret"));
        }
        let requests = worker.requests.lock().unwrap();
        assert_eq!(requests[0].path, "/health");
        assert_eq!(
            requests[0].headers["Authorization"],
            format!("Bearer {WORKER_TOKEN}")
        );
        assert_eq!(requests[0].headers["CF-Access-Client-Id"], "test-access-id");
        assert_eq!(
            requests[0].headers["CF-Access-Client-Secret"],
            "test-access-secret"
        );
        assert!(!requests[0].headers.contains_key("X-Controller-Token"));
    }

    #[tokio::test]
    async fn malformed_and_unexpected_controller_responses_fail_closed() {
        let mut replies = vec![
            Reply::new(StatusCode::OK, "not-json"),
            Reply::new(StatusCode::OK, "{}"),
            Reply::new(StatusCode::OK, r#"{"status":42}"#),
            Reply::state(""),
            Reply::state("unknown-sensitive-response"),
            Reply::new(StatusCode::OK, "x".repeat(MAX_CONTROLLER_STATUS_BYTES + 1)),
            Reply::new(StatusCode::ACCEPTED, r#"{"status":"RUNNING"}"#),
        ];
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            replies.push(Reply::new(status, CONTROLLER_TOKEN));
        }
        let mut streamed = Reply::new(StatusCode::OK, "x".repeat(MAX_CONTROLLER_STATUS_BYTES + 1));
        streamed.streamed = true;
        replies.push(streamed);
        let count = replies.len();
        let controller = mock(replies).await;
        let worker = mock(vec![]).await;
        let (remote, _dir) = remote(&controller, &worker).await;
        for _ in 0..count {
            let error = remote.vm_state().await.unwrap_err();
            assert_eq!(error.code, ErrorCode::Network);
            assert!(!error.message.contains(CONTROLLER_TOKEN));
            assert!(!error.message.contains("unknown-sensitive-response"));
        }
        assert!(worker.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn controller_accepts_compute_engine_state_names() {
        let states = [
            "PROVISIONING",
            "STAGING",
            "RUNNING",
            "STOPPING",
            "SUSPENDING",
            "SUSPENDED",
            "REPAIRING",
            "TERMINATED",
        ];
        let controller = mock(states.iter().map(|s| Reply::state(s)).collect()).await;
        let worker = mock(vec![]).await;
        let (remote, _dir) = remote(&controller, &worker).await;
        for state in states {
            assert_eq!(remote.vm_state().await.unwrap(), state);
        }
    }

    #[tokio::test]
    async fn lifecycle_requires_200_or_202_and_recognizes_409() {
        let statuses = [
            StatusCode::OK,
            StatusCode::ACCEPTED,
            StatusCode::CONFLICT,
            StatusCode::CREATED,
            StatusCode::NO_CONTENT,
            StatusCode::UNAUTHORIZED,
            StatusCode::INTERNAL_SERVER_ERROR,
        ];
        let controller = mock(
            statuses
                .iter()
                .map(|&s| Reply::new(s, CONTROLLER_TOKEN))
                .collect(),
        )
        .await;
        let worker = mock(vec![]).await;
        let (remote, _dir) = remote(&controller, &worker).await;
        for status in statuses {
            let result = remote.vm_action("/start").await;
            match status {
                StatusCode::OK | StatusCode::ACCEPTED => {
                    assert_eq!(result.unwrap(), LifecycleReply::Accepted)
                }
                StatusCode::CONFLICT => assert_eq!(result.unwrap(), LifecycleReply::Transient),
                _ => assert!(!result.unwrap_err().message.contains(CONTROLLER_TOKEN)),
            }
        }
    }

    #[tokio::test]
    async fn controller_redirects_do_not_forward_the_token() {
        let destination = mock(vec![]).await;
        let mut redirect = Reply::new(StatusCode::TEMPORARY_REDIRECT, "");
        redirect.location = Some(destination.origin.join("status").unwrap().to_string());
        let controller = mock(vec![redirect]).await;
        let (remote, _dir) = remote(&controller, &destination).await;
        assert!(remote.vm_state().await.is_err());
        assert!(destination.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unavailable_and_slow_controller_requests_are_bounded() {
        let controller = mock(vec![]).await;
        let worker = mock(vec![]).await;
        let (mut remote, _dir) = remote(&controller, &worker).await;
        let mut slow = Reply::state("RUNNING");
        slow.delay = Duration::from_secs(5);
        let slow_controller = mock(vec![slow]).await;
        let remote_mut = Arc::get_mut(&mut remote).unwrap();
        remote_mut.settings.controller = slow_controller.origin.clone();
        remote_mut.controller = controller_client()
            .no_proxy()
            .timeout(Duration::from_millis(30))
            .build()
            .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(1), remote.vm_state())
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Network);
        Arc::get_mut(&mut remote).unwrap().settings.controller = controller.origin.clone();
        drop(controller);
        assert!(remote.vm_state().await.is_err());
    }

    #[tokio::test]
    async fn startup_retries_conflicts_then_waits_for_worker_health() {
        let controller = mock(vec![
            Reply::state("TERMINATED"),
            Reply::new(StatusCode::CONFLICT, ""),
            Reply::state("TERMINATED"),
            Reply::new(StatusCode::ACCEPTED, ""),
            Reply::state("STAGING"),
            Reply::state("RUNNING"),
            Reply::state("RUNNING"),
        ])
        .await;
        let worker = mock(vec![
            Reply::new(StatusCode::SERVICE_UNAVAILABLE, ""),
            Reply::new(StatusCode::OK, ""),
        ])
        .await;
        let (remote, _dir) = remote(&controller, &worker).await;
        remote.ensure_ready().await.unwrap();
        let requests = controller.requests.lock().unwrap();
        assert_eq!(requests.iter().filter(|r| r.path == "/start").count(), 2);
        assert_eq!(worker.requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn concurrent_startup_is_serialized_by_the_lifecycle_mutex() {
        let controller = mock(vec![
            Reply::state("TERMINATED"),
            Reply::new(StatusCode::OK, ""),
            Reply::state("RUNNING"),
            Reply::state("RUNNING"),
        ])
        .await;
        let worker = mock(vec![
            Reply::new(StatusCode::OK, ""),
            Reply::new(StatusCode::OK, ""),
        ])
        .await;
        let (remote, _dir) = remote(&controller, &worker).await;
        let (first, second) = tokio::join!(remote.ensure_ready(), remote.ensure_ready());
        first.unwrap();
        second.unwrap();
        assert_eq!(
            controller
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.path == "/start")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn startup_timeout_bounds_in_flight_status_requests() {
        let mut slow = Reply::state("RUNNING");
        slow.delay = Duration::from_secs(5);
        let controller = mock(vec![slow]).await;
        let worker = mock(vec![]).await;
        let (mut remote, _dir) = remote(&controller, &worker).await;
        Arc::get_mut(&mut remote).unwrap().settings.start_timeout = Duration::from_millis(30);
        let error = tokio::time::timeout(Duration::from_secs(1), remote.ensure_ready())
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Timeout);
        assert!(worker.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn idle_shutdown_defers_conflicts_and_never_stops_active_or_busy_work() {
        let controller = mock(vec![
            Reply::state("RUNNING"),
            Reply::state("RUNNING"),
            Reply::new(StatusCode::CONFLICT, ""),
            Reply::state("RUNNING"),
            Reply::new(StatusCode::ACCEPTED, ""),
        ])
        .await;
        let worker = mock(vec![
            Reply::new(StatusCode::OK, r#"{"busy":true}"#),
            Reply::new(StatusCode::OK, r#"{"busy":false}"#),
            Reply::new(StatusCode::OK, r#"{"busy":false}"#),
        ])
        .await;
        let (remote, _dir) = remote(&controller, &worker).await;
        {
            let _activity = remote.activity();
            expire_idle(&remote);
            remote.stop_if_idle().await.unwrap();
            assert!(controller.requests.lock().unwrap().is_empty());
        }
        expire_idle(&remote);
        remote.stop_if_idle().await.unwrap();
        assert!(!controller
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.path == "/stop"));
        // The busy check refreshed activity; an immediate tick does nothing.
        remote.stop_if_idle().await.unwrap();
        assert_eq!(controller.requests.lock().unwrap().len(), 1);
        expire_idle(&remote);
        remote.stop_if_idle().await.unwrap();
        remote.stop_if_idle().await.unwrap();
        assert_eq!(
            controller
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.path == "/stop")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn idle_shutdown_fails_closed_when_worker_activity_is_unknown() {
        let controller = mock(vec![Reply::state("RUNNING")]).await;
        let worker = mock(vec![Reply::new(StatusCode::OK, "{}")]).await;
        let (remote, _dir) = remote(&controller, &worker).await;
        expire_idle(&remote);
        assert!(remote.stop_if_idle().await.is_err());
        assert!(!controller
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.path == "/stop"));
    }

    #[tokio::test]
    async fn cancelling_startup_releases_lifecycle_and_activity() {
        let mut slow = Reply::state("RUNNING");
        slow.delay = Duration::from_secs(5);
        let controller = mock(vec![slow]).await;
        let worker = mock(vec![]).await;
        let (remote, _dir) = remote(&controller, &worker).await;
        let cancel = CancellationToken::new();
        let run_cancel = cancel.clone();
        let run_remote = remote.clone();
        let request = serde_json::from_value(
            json!({"url":"https://youtube.com/watch?v=abcdefghijk", "type":"audio", "format":"mp3"}),
        )
        .unwrap();
        let (progress, _) = watch::channel(DownloadProgress::default());
        let run = tokio::spawn(async move {
            run_remote
                .run(&request, Uuid::new_v4(), run_cancel, progress)
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while controller.requests.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        cancel.cancel();
        let error = tokio::time::timeout(Duration::from_secs(1), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Cancelled);
        assert_eq!(remote.active.load(Ordering::SeqCst), 0);
        assert!(remote.lifecycle.try_lock().is_ok());
        assert!(worker.requests.lock().unwrap().is_empty());
    }
}
