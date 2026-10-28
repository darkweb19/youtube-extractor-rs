//! Railway keyless Cloud Run VM controller and authenticated GCP worker transport.
//! This is intentionally single-controller: deploy one Railway replica with persistent DATA_DIR.
use crate::{
    download::DownloadProgress,
    error::{AppError, ErrorCode, Result},
    extractor::Extractor,
    jobs::JobExecutor,
    model::{DownloadRequest, JobState, JobView, OutputInfo, VideoInfo},
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

#[derive(Debug, Clone, Copy)]
enum ControllerOperation {
    Status,
    Start,
    Stop,
}
impl ControllerOperation {
    fn details(self) -> (&'static str, Method, &'static str) {
        match self {
            Self::Status => ("status", Method::GET, "/status"),
            Self::Start => ("start", Method::POST, "/start"),
            Self::Stop => ("stop", Method::POST, "/stop"),
        }
    }
}

fn transport_category(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_body() || error.is_decode() {
        "body"
    } else {
        "other"
    }
}

fn controller_rejection(
    operation: ControllerOperation,
    status: StatusCode,
    elapsed: Duration,
) -> AppError {
    let (operation, method, path) = operation.details();
    let hint = match status.as_u16() {
        401 => "Check GCP_CONTROLLER_TOKEN matches the controller secret.",
        403 => "Check the controller token and Cloud Run invocation permissions; inspect controller VM runtime IAM and logs if the request reached the controller.",
        404 => "Check GCP_CONTROLLER_URL, controller routes, and the controller VM project/zone/instance target.",
        405 => "Check the controller route and HTTP method contract.",
        429 => "Check controller request limits and retry after rate limiting clears.",
        300..=399 => "Unexpected redirect; configure the controller HTTPS origin directly.",
        500..=599 => "Inspect Cloud Run controller logs, runtime IAM permissions, and VM target configuration.",
        200..=299 => "Unexpected success status; verify the controller HTTP response contract.",
        _ => "Inspect Cloud Run controller logs and verify its HTTP API contract.",
    };
    tracing::warn!(event = "controller_request_rejected", operation, method = %method,
        path, http_status = status.as_u16(), elapsed_ms = elapsed.as_millis() as u64, "VM controller rejected request");
    AppError::new(
        ErrorCode::Network,
        format!(
            "VM controller {method} {path} returned HTTP {}. {hint}",
            status.as_u16()
        ),
    )
}

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

    async fn controller_request(
        &self,
        operation: ControllerOperation,
    ) -> Result<reqwest::Response> {
        let (operation, method, path) = operation.details();
        let started = Instant::now();
        tracing::info!(event = "controller_request_started", operation, method = %method,
            path, "Sending VM controller request");
        let url = self
            .settings
            .controller
            .join(path)
            .expect("fixed controller API path");
        let mut request = self
            .controller
            .request(method.clone(), url)
            .header("X-Controller-Token", self.settings.controller_token.clone());
        // Some controllers require an explicit Content-Length even for empty POST requests.
        if method == Method::POST {
            request = request
                .header(reqwest::header::CONTENT_LENGTH, "0")
                .body(Vec::new());
        }
        let response = request.send().await.map_err(|error| {
                tracing::warn!(event = "controller_transport_failed", operation, method = %method,
                    path, elapsed_ms = started.elapsed().as_millis() as u64,
                    error_category = transport_category(&error), "VM controller transport failed");
                AppError::new(ErrorCode::Network,
                    format!("VM controller {method} {path} unavailable ({}). Check controller reachability and Cloud Run logs.", transport_category(&error)))
            })?;
        tracing::info!(event = "controller_response_received", operation, method = %method,
            path, http_status = response.status().as_u16(),
            elapsed_ms = started.elapsed().as_millis() as u64, "Received VM controller response");
        Ok(response)
    }

    async fn vm_state(&self) -> Result<String> {
        let started = Instant::now();
        let mut response = self.controller_request(ControllerOperation::Status).await?;
        if response.status() != StatusCode::OK {
            return Err(controller_rejection(
                ControllerOperation::Status,
                response.status(),
                started.elapsed(),
            ));
        }
        // Status is tiny; cap both declared and streamed bodies before parsing.
        if response
            .content_length()
            .is_some_and(|n| n > MAX_CONTROLLER_STATUS_BYTES as u64)
        {
            tracing::warn!(
                event = "controller_status_invalid",
                reason = "oversized_body",
                "VM controller status response rejected"
            );
            return Err(bad("VM controller GET /status response is too large"));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            tracing::warn!(
                event = "controller_status_invalid",
                reason = "body_unavailable",
                error_category = transport_category(&error),
                "Cannot read VM controller status"
            );
            bad("Cannot read VM controller GET /status response")
        })? {
            if chunk.len() > MAX_CONTROLLER_STATUS_BYTES.saturating_sub(body.len()) {
                tracing::warn!(
                    event = "controller_status_invalid",
                    reason = "oversized_body",
                    "VM controller status response rejected"
                );
                return Err(bad("VM controller GET /status response is too large"));
            }
            body.extend_from_slice(&chunk);
        }
        let instance: Instance = serde_json::from_slice(&body).map_err(|_| {
            tracing::warn!(
                event = "controller_status_invalid",
                reason = "malformed_json",
                "VM controller status response rejected"
            );
            bad("Invalid VM controller GET /status JSON response")
        })?;
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
            tracing::warn!(
                event = "controller_status_invalid",
                reason = "unknown_state",
                "VM controller status response rejected"
            );
            return Err(bad(
                "VM controller GET /status returned an unknown VM state",
            ));
        }
        tracing::info!(event = "vm_state_observed", vm_state = %instance.status,
            "Observed VM state");
        Ok(instance.status)
    }

    async fn vm_action(&self, action: ControllerOperation) -> Result<LifecycleReply> {
        let started = Instant::now();
        let response = self.controller_request(action).await?;
        let (operation, _, _) = action.details();
        match response.status() {
            StatusCode::OK | StatusCode::ACCEPTED => {
                tracing::info!(
                    event = "vm_lifecycle_accepted",
                    operation,
                    http_status = response.status().as_u16(),
                    "VM lifecycle request accepted"
                );
                Ok(LifecycleReply::Accepted)
            }
            StatusCode::CONFLICT => {
                tracing::info!(
                    event = "vm_lifecycle_conflict",
                    operation,
                    http_status = 409,
                    "VM lifecycle transient conflict; readiness polling or idle watcher will retry"
                );
                Ok(LifecycleReply::Transient)
            }
            status => Err(controller_rejection(action, status, started.elapsed())),
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
        let started = Instant::now();
        tracing::info!(
            event = "worker_health_started",
            "Checking authenticated worker health"
        );
        match tokio::time::timeout(
            Duration::from_secs(7),
            self.worker_req(Method::GET, "/health").send(),
        )
        .await
        {
            Ok(Ok(response)) => {
                let ready = response.status() == StatusCode::OK;
                tracing::info!(
                    event = "worker_health_checked",
                    ready,
                    http_status = response.status().as_u16(),
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "Worker health check finished"
                );
                ready
            }
            Ok(Err(error)) => {
                tracing::info!(
                    event = "worker_health_waiting",
                    error_category = transport_category(&error),
                    "Worker health unavailable; waiting for readiness"
                );
                false
            }
            Err(_) => {
                tracing::info!(
                    event = "worker_health_waiting",
                    error_category = "timeout",
                    "Worker health timed out; waiting for readiness"
                );
                false
            }
        }
    }

    async fn ensure_ready(&self) -> Result<()> {
        let started = Instant::now();
        tracing::info!(
            event = "vm_lifecycle_lock_waiting",
            "Waiting for VM lifecycle lock"
        );
        let _lock = self.lifecycle.lock().await;
        tracing::info!(
            event = "vm_lifecycle_lock_acquired",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "Acquired VM lifecycle lock"
        );
        // Bound the entire polling operation, including in-flight HTTP calls.
        let result = tokio::time::timeout(self.settings.start_timeout, self.poll_until_ready())
            .await
            .map_err(|_| {
                tracing::warn!(
                    event = "worker_startup_timeout",
                    timeout_secs = self.settings.start_timeout.as_secs(),
                    "GCP worker startup timed out"
                );
                AppError::new(ErrorCode::Timeout, "GCP worker startup timed out")
            })?;
        match &result {
            Ok(()) => tracing::info!(event = "worker_ready", "GCP worker is ready for dispatch"),
            Err(error) => tracing::warn!(event = "worker_startup_failed", error_code = ?error.code,
                "GCP worker readiness failed"),
        }
        result
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
                        start_requested = self.vm_action(ControllerOperation::Start).await?
                            == LifecycleReply::Accepted;
                    }
                }
                "STOPPING" | "STAGING" | "PROVISIONING" | "REPAIRING" => (),
                _ => {
                    tracing::warn!(
                        event = "worker_startup_unsupported_state",
                        "VM state cannot serve worker jobs"
                    );
                    return Err(bad("GCP VM is in an unsupported state"));
                }
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
            .map_err(|error| {
                tracing::warn!(
                    event = "worker_activity_failed",
                    error_category = transport_category(&error),
                    "Cannot verify worker is idle"
                );
                bad("Cannot verify GCP worker is idle")
            })?;
        if response.status() != StatusCode::OK {
            tracing::warn!(
                event = "worker_activity_failed",
                http_status = response.status().as_u16(),
                "Worker activity HTTP request failed; idle shutdown blocked"
            );
            return Err(bad("GCP worker activity check failed"));
        }
        let activity: Value = response.json().await.map_err(|_| {
            tracing::warn!(
                event = "worker_activity_invalid",
                reason = "malformed_json",
                "Idle shutdown blocked by invalid worker activity"
            );
            bad("Invalid GCP activity reply")
        })?;
        activity
            .get("busy")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                tracing::warn!(
                    event = "worker_activity_invalid",
                    reason = "missing_busy_field",
                    "Idle shutdown blocked by incomplete worker activity"
                );
                bad("GCP activity reply was incomplete")
            })
    }

    async fn stop_if_idle(&self) -> Result<()> {
        if self.active.load(Ordering::SeqCst) != 0 {
            tracing::debug!(
                event = "idle_shutdown_skipped",
                reason = "active_work",
                "VM remains active"
            );
            return Ok(());
        }
        let _lock = self.lifecycle.lock().await;
        if self.active.load(Ordering::SeqCst) != 0 {
            tracing::debug!(
                event = "idle_shutdown_skipped",
                reason = "active_work_after_lock",
                "VM remains active"
            );
            return Ok(());
        }
        let idle = self
            .last_activity
            .lock()
            .map_err(|_| bad("Idle tracking unavailable"))?
            .elapsed();
        if idle < self.settings.idle {
            tracing::debug!(
                event = "idle_shutdown_skipped",
                reason = "idle_timeout_pending",
                "VM idle interval not elapsed"
            );
            return Ok(());
        }
        if self.vm_state().await? == "RUNNING" {
            // Never stop untracked work after a Railway restart or lost callback.
            if self.worker_busy().await? {
                tracing::debug!(
                    event = "idle_shutdown_skipped",
                    reason = "worker_busy",
                    "Worker reports active work"
                );
                if let Ok(mut last) = self.last_activity.lock() {
                    *last = Instant::now();
                }
                return Ok(());
            }
            // The lifecycle mutex blocks new dispatch until the stop request is accepted.
            tracing::info!(
                event = "idle_shutdown_requested",
                "Requesting idle VM shutdown"
            );
            if self.vm_action(ControllerOperation::Stop).await? == LifecycleReply::Accepted {
                tracing::info!("Idle GCP extractor stop accepted");
            }
            // A 409 leaves shutdown pending; the next idle tick checks state again.
        } else {
            tracing::debug!(
                event = "idle_shutdown_skipped",
                reason = "vm_not_running",
                "VM shutdown not needed"
            );
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
            tracing::info!(event = "worker_dispatch_started", worker_job_id = %remote, delivery_attempt = attempt + 1,
                "Dispatching remote download with stable idempotency key");
            match self
                .worker_req(Method::POST, "/api/v1/downloads")
                .header("X-Idempotency-Key", remote.to_string())
                .json(request)
                .send()
                .await
            {
                Ok(response) => {
                    tracing::info!(event = "worker_dispatch_response", worker_job_id = %remote,
                        http_status = response.status().as_u16(), "Worker dispatch response received");
                    dispatched = Some(response);
                    break;
                }
                Err(error) if attempt == 0 => {
                    tracing::warn!(event = "worker_dispatch_retry", worker_job_id = %remote,
                        error_category = transport_category(&error), "Retrying uncertain delivery with the same idempotency key");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                Err(error) => {
                    tracing::warn!(event = "worker_dispatch_failed", worker_job_id = %remote,
                        error_category = transport_category(&error), "Worker dispatch transport failed");
                    return Err(bad("Cannot dispatch job to GCP"));
                }
            }
        }
        let response = dispatched.ok_or_else(|| bad("GCP dispatch unavailable"))?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            return Err(AppError::new(ErrorCode::QueueFull, "GCP queue is full"));
        }
        if !response.status().is_success() {
            return Err(bad("GCP refused download request"));
        }
        let http_status = response.status().as_u16();
        let job: JobView = response.json().await.map_err(|error| {
            tracing::warn!(
                event = "worker_dispatch_invalid",
                worker_job_id = %remote,
                http_status,
                reason = "invalid_response_body",
                error_category = transport_category(&error),
                "Worker dispatch response rejected"
            );
            bad("Invalid GCP dispatch reply")
        })?;
        let job_id = Uuid::parse_str(&job.job_id).map_err(|_| {
            tracing::warn!(event = "worker_dispatch_invalid", worker_job_id = %remote,
                reason = "invalid_uuid", "Worker dispatch response rejected");
            bad("Invalid GCP dispatch reply")
        })?;
        if job_id != remote {
            tracing::warn!(
                event = "worker_dispatch_invalid",
                worker_job_id = %remote,
                reason = "job_id_mismatch",
                "Worker dispatch response rejected"
            );
            return Err(bad("GCP returned a different job identifier"));
        }
        tracing::info!(event = "worker_dispatch_accepted", worker_job_id = %remote, "Remote download accepted");
        let result = async {
            let mut previous_state = None;
            loop {
                let view = tokio::select! {
                    _ = cancel.cancelled() => return Err(AppError::new(ErrorCode::Cancelled, "Job cancelled")),
                    reply = self.worker_job(remote) => reply?,
                };
                // The public worker view contains byte counts, not the pipeline's internal stage.
                // Derive the latter from the typed worker state instead of accepting worker text.
                let state = match view.status {
                    JobState::Queued => "queued",
                    JobState::Running => "running",
                    JobState::Completed => "completed",
                    JobState::Failed => "failed",
                    JobState::Cancelled => "cancelled",
                };
                let _ = progress.send(DownloadProgress {
                    downloaded_bytes: view.progress.downloaded_bytes,
                    total_bytes: view.progress.total_bytes,
                    stage: state.to_owned(),
                });
                if previous_state != Some(state) {
                    tracing::info!(event = "worker_job_state_changed", worker_job_id = %remote,
                        worker_state = state, "Remote download state changed");
                    previous_state = Some(state);
                }
                match view.status {
                    JobState::Completed => {
                        let output = view.output.ok_or_else(|| bad("Worker returned no completed file"))?;
                        return self.retrieve(local, remote, &output, cancel).await;
                    }
                    JobState::Failed => return Err(AppError::new(ErrorCode::DownloadInterrupted, "GCP download failed")),
                    JobState::Cancelled => return Err(AppError::new(ErrorCode::Cancelled, "GCP download cancelled")),
                    JobState::Queued | JobState::Running => (),
                }
                tokio::select! {
                    _ = cancel.cancelled() => return Err(AppError::new(ErrorCode::Cancelled, "Job cancelled")),
                    _ = tokio::time::sleep(Duration::from_secs(3)) => (),
                }
            }
        }.await;
        if let Err(error) = &result {
            tracing::warn!(event = "worker_attempt_finished", worker_job_id = %remote,
                error_code = ?error.code, "Remote download attempt failed or was cancelled");
        }
        self.cancel_worker(remote).await;
        result
    }

    async fn worker_job(&self, id: Uuid) -> Result<JobView> {
        let path = format!("/api/v1/downloads/{id}");
        let response = self
            .worker_req(Method::GET, &path)
            .send()
            .await
            .map_err(|error| {
                tracing::warn!(event = "worker_job_poll_failed", worker_job_id = %id,
                    error_category = transport_category(&error), "Worker job poll transport failed");
                bad("Cannot fetch worker job state")
            })?;
        if !response.status().is_success() {
            tracing::warn!(event = "worker_job_poll_failed", worker_job_id = %id,
                http_status = response.status().as_u16(), "Worker job poll HTTP request failed");
            return Err(bad("GCP job lookup failed"));
        }
        let http_status = response.status().as_u16();
        let job: JobView = response.json().await.map_err(|error| {
            tracing::warn!(event = "worker_job_poll_invalid", worker_job_id = %id,
                    http_status, reason = "invalid_response_body", error_category = transport_category(&error),
                    "Worker job poll response rejected");
            bad("Invalid GCP job status")
        })?;
        let job_id = Uuid::parse_str(&job.job_id).map_err(|_| {
            tracing::warn!(event = "worker_job_poll_invalid", worker_job_id = %id,
                reason = "invalid_uuid", "Worker job poll response rejected");
            bad("Invalid GCP job status")
        })?;
        if job_id != id {
            tracing::warn!(event = "worker_job_poll_invalid", worker_job_id = %id,
                reason = "job_id_mismatch", "Worker job poll response rejected");
            return Err(bad("GCP returned a different job identifier"));
        }
        Ok(job)
    }

    async fn cancel_worker(&self, id: Uuid) {
        let path = format!("/api/v1/downloads/{id}");
        tracing::info!(event = "worker_cleanup_started", worker_job_id = %id, "Requesting remote job cleanup");
        match self.worker_req(Method::DELETE, &path).send().await {
            Ok(response) => tracing::info!(event = "worker_cleanup_response", worker_job_id = %id,
                http_status = response.status().as_u16(), "Remote job cleanup response received"),
            Err(error) => tracing::warn!(event = "worker_cleanup_failed", worker_job_id = %id,
                error_category = transport_category(&error), "Remote job cleanup transport failed"),
        }
    }

    async fn retrieve(
        &self,
        job: Uuid,
        remote: Uuid,
        output: &OutputInfo,
        cancel: &CancellationToken,
    ) -> Result<StoredOutput> {
        let started = Instant::now();
        tracing::info!(event = "media_transfer_started", worker_job_id = %remote,
            expected_bytes = output.size_bytes, "Starting remote media transfer");
        let ext = output.filename.rsplit('.').next().unwrap_or("");
        let mime = match ext {
            "mp4" => "video/mp4",
            "webm" => "video/webm",
            "mp3" => "audio/mpeg",
            "m4a" => "audio/mp4",
            _ => {
                tracing::warn!(
                    event = "media_transfer_failed",
                    reason = "unsupported_extension",
                    "Remote media file type rejected"
                );
                return Err(bad("GCP produced an unsupported media file"));
            }
        };
        if output.size_bytes == 0 || output.size_bytes >= self.settings.max_bytes {
            tracing::warn!(
                event = "media_transfer_failed",
                reason = "size_limit",
                "Remote media size rejected"
            );
            return Err(AppError::new(
                ErrorCode::DownloadTooLarge,
                "Remote file exceeds size limit",
            ));
        }
        let dir = self.storage.create_job(job).await.inspect_err(|error| {
            tracing::warn!(event = "media_transfer_failed", error_code = ?error.code,
                reason = "local_storage", "Cannot prepare local transfer storage");
        })?;
        let result = async {
            let path = format!("/api/v1/downloads/{remote}/file");
            let mut response = self.worker_req(Method::GET, &path).send().await
                .map_err(|_| bad("Cannot stream media from GCP"))?;
            tracing::info!(event = "media_transfer_response", http_status = response.status().as_u16(),
                "Remote media transfer response received");
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
        match &result {
            Ok(output) => {
                tracing::info!(event = "media_transfer_completed", worker_job_id = %remote,
                size_bytes = output.size_bytes, elapsed_ms = started.elapsed().as_millis() as u64,
                "Remote media transfer completed")
            }
            Err(error) => {
                tracing::warn!(event = "media_transfer_failed", worker_job_id = %remote,
                    error_code = ?error.code, cancelled = error.code == ErrorCode::Cancelled,
                    elapsed_ms = started.elapsed().as_millis() as u64, "Remote media transfer failed or was cancelled");
                tracing::info!(
                    event = "local_partial_cleanup_started",
                    "Cleaning up incomplete local transfer"
                );
                if self.storage.cleanup(job).await.is_err() {
                    tracing::warn!(
                        event = "local_partial_cleanup_failed",
                        "Incomplete local transfer cleanup failed"
                    );
                }
            }
        }
        result
    }
}

#[async_trait]
impl Extractor for Remote {
    #[tracing::instrument(name = "remote_metadata", skip_all, fields(operation = "metadata"))]
    async fn video_info(&self, url: &str) -> Result<VideoInfo> {
        let started = Instant::now();
        tracing::info!(
            event = "metadata_started",
            "Starting remote metadata request"
        );
        let result = async {
            let _active = self.activity();
            self.ensure_ready().await?;
            let response = self
                .worker_req(Method::POST, "/api/v1/videos/info")
                .json(&json!({"url":url}))
                .send()
                .await
                .map_err(|error| {
                    tracing::warn!(
                        event = "metadata_transport_failed",
                        error_category = transport_category(&error),
                        "Worker metadata transport failed"
                    );
                    bad("GCP metadata request failed")
                })?;
            tracing::info!(
                event = "metadata_response_received",
                http_status = response.status().as_u16(),
                "Worker metadata response received"
            );
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
        .await;
        match &result {
            Ok(_) => tracing::info!(
                event = "metadata_completed",
                elapsed_ms = started.elapsed().as_millis() as u64,
                "Remote metadata request completed"
            ),
            Err(error) => tracing::warn!(event = "metadata_failed", error_code = ?error.code,
                elapsed_ms = started.elapsed().as_millis() as u64, "Remote metadata request failed"),
        }
        result
    }
}

#[async_trait]
impl JobExecutor for Remote {
    #[tracing::instrument(name = "remote_job", skip_all, fields(job_id = %id))]
    async fn run(
        &self,
        request: &DownloadRequest,
        id: Uuid,
        cancel: CancellationToken,
        progress: watch::Sender<DownloadProgress>,
    ) -> Result<StoredOutput> {
        let started = Instant::now();
        tracing::info!(
            event = "remote_job_started",
            "Starting remote download execution"
        );
        let result = async {
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
                    tracing::warn!(event = "remote_extraction_retry", job_id=%id, attempt=attempt+1,
                        "Retrying interrupted extraction with a fresh worker job identifier");
                    tokio::select! {
                        _ = cancel.cancelled() => return Err(AppError::new(ErrorCode::Cancelled, "Job cancelled")),
                        _ = tokio::time::sleep(Duration::from_secs(3)) => (),
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Err(bad("Remote job attempts exhausted"))
        }.await;
        match &result {
            Ok(output) => tracing::info!(
                event = "remote_job_completed",
                size_bytes = output.size_bytes,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "Remote download completed"
            ),
            Err(error) => tracing::warn!(event = "remote_job_failed", error_code = ?error.code,
                cancelled = error.code == ErrorCode::Cancelled,
                elapsed_ms = started.elapsed().as_millis() as u64, "Remote download failed or was cancelled"),
        }
        result
    }

    #[tracing::instrument(name = "remote_cleanup", skip_all, fields(job_id = %id))]
    async fn cleanup(&self, id: Uuid) -> Result<()> {
        tracing::info!(
            event = "local_cleanup_started",
            "Cleaning up local remote-job output"
        );
        let result = self.storage.cleanup(id).await;
        match &result {
            Ok(()) => tracing::info!(
                event = "local_cleanup_completed",
                "Local remote-job output cleanup completed"
            ),
            Err(error) => tracing::warn!(event = "local_cleanup_failed", error_code = ?error.code,
                "Local remote-job output cleanup failed"),
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    mod worker_tests;
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
            remote.vm_action(ControllerOperation::Start).await.unwrap(),
            LifecycleReply::Accepted
        );
        assert_eq!(
            remote.vm_action(ControllerOperation::Stop).await.unwrap(),
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
            if request.method == Method::POST {
                assert_eq!(request.headers[reqwest::header::CONTENT_LENGTH], "0");
            }
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
            let result = remote.vm_action(ControllerOperation::Start).await;
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

    #[derive(Clone, Default)]
    struct LogCapture(Arc<StdMutex<Vec<u8>>>);
    impl std::io::Write for LogCapture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }
    fn log_subscriber(logs: LogCapture) -> impl tracing::Subscriber + Send + Sync {
        // Keep a second dispatcher registered while a capture is active. With
        // only one dispatcher, tracing's callsite cache can consult the default
        // on an unrelated test thread and incorrectly disable the callsite.
        static NOOP_DISPATCH: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
        NOOP_DISPATCH
            .get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::new()));
        tracing_subscriber::fmt()
            .json()
            .with_ansi(false)
            .with_env_filter("youtube_extractor::remote=debug")
            .with_writer(logs)
            .finish()
    }

    #[test]
    fn captured_events_survive_unsubscribed_callsite_registration() {
        fn emit_event() {
            tracing::warn!(event = "capture_registration_test", "Test capture");
        }

        let logs = LogCapture::default();
        let subscriber = tracing::Dispatch::new(log_subscriber(logs.clone()));
        // Register this callsite first on a thread without a scoped subscriber,
        // while the capture subscriber is active on the test thread.
        std::thread::spawn(emit_event).join().unwrap();
        tracing::dispatcher::with_default(&subscriber, emit_event);
        let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        let events: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(events
            .iter()
            .any(|event| event["fields"]["event"] == "capture_registration_test"));
    }

    #[tokio::test]
    async fn lifecycle_rejections_include_fixed_operation_status_and_actionable_hint() {
        let cases = [
            (StatusCode::UNAUTHORIZED, "GCP_CONTROLLER_TOKEN"),
            (StatusCode::FORBIDDEN, "Cloud Run invocation"),
            (StatusCode::NOT_FOUND, "project/zone/instance"),
            (StatusCode::METHOD_NOT_ALLOWED, "HTTP method"),
            (StatusCode::TOO_MANY_REQUESTS, "rate limiting"),
            (StatusCode::SERVICE_UNAVAILABLE, "runtime IAM"),
            (StatusCode::TEMPORARY_REDIRECT, "Unexpected redirect"),
            (StatusCode::NO_CONTENT, "response contract"),
        ];
        let controller = mock(cases.iter().map(|&(status, _)| Reply::new(status,
            format!("{CONTROLLER_TOKEN} {WORKER_TOKEN} https://sensitive.example/private?token=secret"))).collect()).await;
        let worker = mock(vec![]).await;
        let (remote, _dir) = remote(&controller, &worker).await;
        for (status, hint) in cases {
            let error = remote
                .vm_action(ControllerOperation::Start)
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::Network);
            assert!(error.message.starts_with(&format!(
                "VM controller POST /start returned HTTP {}.",
                status.as_u16()
            )));
            assert!(error.message.contains(hint));
            assert!(!error.message.contains(CONTROLLER_TOKEN));
            assert!(!error.message.contains(WORKER_TOKEN));
            assert!(!error.message.contains("https://"));
        }
    }

    #[tokio::test]
    async fn safe_controller_logs_correlate_failed_startup_with_the_download_job() {
        use tracing::instrument::WithSubscriber;
        let sensitive_body =
            format!("{CONTROLLER_TOKEN} {WORKER_TOKEN} https://sensitive.example?secret=value");
        let controller = mock(vec![
            Reply::state("TERMINATED"),
            Reply::new(StatusCode::FORBIDDEN, sensitive_body),
        ])
        .await;
        let worker = mock(vec![]).await;
        let (remote, _dir) = remote(&controller, &worker).await;
        let logs = LogCapture::default();
        let request = serde_json::from_value(json!({
            "url":"https://youtube.com/watch?v=abcdefghijk&private=never-log", "type":"audio", "format":"mp3"
        })).unwrap();
        let job_id = Uuid::new_v4();
        let (progress, _) = watch::channel(DownloadProgress::default());
        // Create the instrumented future while the capture subscriber is active:
        // #[tracing::instrument] constructs its span when the async method is called.
        let subscriber = tracing::Dispatch::new(log_subscriber(logs.clone()));
        let operation = tracing::dispatcher::with_default(&subscriber, || {
            remote.run(&request, job_id, CancellationToken::new(), progress)
        });
        let error = operation.with_subscriber(subscriber).await.unwrap_err();
        assert!(error.message.contains("POST /start returned HTTP 403"));
        let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        for secret in [
            CONTROLLER_TOKEN,
            WORKER_TOKEN,
            "sensitive.example",
            "secret=value",
            "private=never-log",
            "youtube.com",
            "test-access-id",
            "test-access-secret",
            controller.origin.as_str(),
        ] {
            assert!(
                !text.contains(secret),
                "sensitive data appeared in captured tracing output"
            );
        }
        let events: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        for name in [
            "remote_job_started",
            "vm_lifecycle_lock_waiting",
            "vm_lifecycle_lock_acquired",
            "controller_request_started",
            "controller_response_received",
            "vm_state_observed",
            "controller_request_rejected",
            "worker_startup_failed",
            "remote_job_failed",
        ] {
            assert!(
                events.iter().any(|event| event["fields"]["event"] == name),
                "missing expected stage event: {name}"
            );
        }
        let rejected = events
            .iter()
            .find(|event| event["fields"]["event"] == "controller_request_rejected")
            .unwrap();
        assert_eq!(rejected["fields"]["operation"], "start");
        assert_eq!(rejected["fields"]["method"], "POST");
        assert_eq!(rejected["fields"]["path"], "/start");
        assert_eq!(rejected["fields"]["http_status"], 403);
        assert!(rejected["fields"]["elapsed_ms"].is_u64());
        assert_eq!(rejected["span"]["job_id"], job_id.to_string());
        assert_eq!(rejected["span"]["name"], "remote_job");
        assert!(worker.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn malformed_and_unavailable_controller_logs_omit_untrusted_content() {
        use tracing::instrument::WithSubscriber;
        let mut slow = Reply::state("RUNNING");
        slow.delay = Duration::from_secs(5);
        let controller = mock(vec![
            Reply::state(CONTROLLER_TOKEN),
            Reply::new(StatusCode::OK, format!("malformed-{WORKER_TOKEN}")),
            Reply::new(StatusCode::OK, "x".repeat(MAX_CONTROLLER_STATUS_BYTES + 1)),
            slow,
        ])
        .await;
        let worker = mock(vec![]).await;
        let (mut remote, _dir) = remote(&controller, &worker).await;
        Arc::get_mut(&mut remote).unwrap().controller = controller_client()
            .no_proxy()
            .timeout(Duration::from_millis(50))
            .build()
            .unwrap();
        let logs = LogCapture::default();
        async {
            for _ in 0..4 {
                assert!(remote.vm_state().await.is_err());
            }
        }
        .with_subscriber(log_subscriber(logs.clone()))
        .await;
        let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        assert!(!text.contains(CONTROLLER_TOKEN));
        assert!(!text.contains(WORKER_TOKEN));
        assert!(!text.contains(controller.origin.as_str()));
        let events: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        for reason in ["unknown_state", "malformed_json", "oversized_body"] {
            assert!(events
                .iter()
                .any(|event| event["fields"]["reason"] == reason));
        }
        let failed = events
            .iter()
            .find(|event| event["fields"]["event"] == "controller_transport_failed")
            .unwrap();
        assert_eq!(failed["fields"]["error_category"], "timeout");
        assert_eq!(failed["fields"]["method"], "GET");
        assert_eq!(failed["fields"]["path"], "/status");
    }
}
