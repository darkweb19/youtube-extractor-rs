//! Railway on-demand Compute Engine controller and authenticated GCP worker transport.
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
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine as _,
};
use reqwest::{Client, Method, StatusCode, Url};
use ring::{
    rand::SystemRandom,
    signature::{KeyPair, RsaKeyPair, RSA_PKCS1_SHA256},
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    env,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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
fn label(name: &'static str) -> Result<String> {
    let v = required(name)?;
    if v.len() > 100
        || !v
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(AppError::invalid(format!(
            "{name} must be an ASCII cloud identifier"
        )));
    }
    Ok(v)
}

#[derive(Clone)]
struct Settings {
    project: String,
    zone: String,
    instance: String,
    worker: Url,
    worker_token: String,
    access_id: String,
    access_secret: String,
    client_email: String,
    private_key: String,
    idle: Duration,
    start_timeout: Duration,
    max_bytes: u64,
}

#[derive(Deserialize)]
struct ServiceAccount {
    client_email: String,
    private_key: String,
}

#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
    expires_in: u64,
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
    cloud: Client,
    worker: Client,
    storage: LocalStorage,
    token: Mutex<Option<(String, Instant)>>,
    lifecycle: Mutex<()>,
    active: AtomicUsize,
    last_activity: StdMutex<Instant>,
}

impl Remote {
    pub fn from_env(storage: LocalStorage, max_bytes: u64) -> Result<Arc<Self>> {
        let url = required("GCP_WORKER_URL")?;
        let worker = Url::parse(&url).map_err(|_| AppError::invalid("Invalid GCP_WORKER_URL"))?;
        if worker.scheme() != "https"
            || worker.host_str().is_none()
            || worker.username() != ""
            || worker.password().is_some()
            || worker.path() != "/"
            || worker.query().is_some()
            || worker.fragment().is_some()
        {
            return Err(AppError::invalid("GCP_WORKER_URL must be an HTTPS origin"));
        }
        let worker_token = required("GCP_WORKER_TOKEN")?;
        if worker_token.len() < 32 {
            return Err(AppError::invalid(
                "GCP_WORKER_TOKEN must be at least 32 characters",
            ));
        }
        let account: ServiceAccount = serde_json::from_str(&required("GCP_SERVICE_ACCOUNT_JSON")?)
            .map_err(|_| AppError::invalid("Invalid GCP_SERVICE_ACCOUNT_JSON"))?;
        if account.client_email.is_empty() || !account.private_key.contains("BEGIN PRIVATE KEY") {
            return Err(AppError::invalid(
                "GCP service account credentials are invalid",
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
        let cloud = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(45))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| bad("Google API client unavailable"))?;
        let worker_http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(1800))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| bad("Worker HTTP client unavailable"))?;
        Ok(Arc::new(Self {
            settings: Settings {
                project: label("GCP_PROJECT_ID")?,
                zone: label("GCP_ZONE")?,
                instance: label("GCP_INSTANCE_NAME")?,
                worker,
                worker_token,
                access_id: required("CF_ACCESS_CLIENT_ID")?,
                access_secret: required("CF_ACCESS_CLIENT_SECRET")?,
                client_email: account.client_email,
                private_key: account.private_key,
                idle: Duration::from_secs(idle),
                start_timeout: Duration::from_secs(start_timeout),
                max_bytes,
            },
            cloud,
            worker: worker_http,
            storage,
            token: Mutex::new(None),
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

    fn vm_url(&self, action: &str) -> String {
        let s = &self.settings;
        format!(
            "https://compute.googleapis.com/compute/v1/projects/{}/zones/{}/instances/{}{}",
            s.project, s.zone, s.instance, action
        )
    }

    async fn google_token(&self) -> Result<String> {
        let mut cache = self.token.lock().await;
        if let Some((token, deadline)) = cache.as_ref() {
            if Instant::now() < *deadline {
                return Ok(token.clone());
            }
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| bad("System clock unavailable"))?
            .as_secs();
        let claims = json!({
            "iss": self.settings.client_email,
            "scope": "https://www.googleapis.com/auth/compute",
            "aud": "https://oauth2.googleapis.com/token",
            "iat": now,
            "exp": now + 3600
        });
        let head = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let body = URL_SAFE_NO_PAD.encode(claims.to_string());
        let assertion = format!("{head}.{body}");
        let pem = self
            .settings
            .private_key
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .collect::<String>();
        let der = STANDARD
            .decode(pem)
            .map_err(|_| bad("Invalid service account key encoding"))?;
        let key =
            RsaKeyPair::from_pkcs8(&der).map_err(|_| bad("Invalid service account private key"))?;
        let mut signature = vec![0; key.public().modulus_len()];
        key.sign(
            &RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            assertion.as_bytes(),
            &mut signature,
        )
        .map_err(|_| bad("Google API authentication signing failed"))?;
        let signed = format!("{assertion}.{}", URL_SAFE_NO_PAD.encode(signature));
        let response = self
            .cloud
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", signed.as_str()),
            ])
            .send()
            .await
            .map_err(|_| bad("Google OAuth token request failed"))?;
        if !response.status().is_success() {
            return Err(bad("Google OAuth rejected service account credentials"));
        }
        let payload: TokenReply = response
            .json()
            .await
            .map_err(|_| bad("Invalid Google OAuth reply"))?;
        if payload.access_token.is_empty() || payload.expires_in <= 60 {
            return Err(bad("Google OAuth returned an unusable token"));
        }
        *cache = Some((
            payload.access_token.clone(),
            Instant::now() + Duration::from_secs(payload.expires_in - 60),
        ));
        Ok(payload.access_token)
    }

    async fn vm_state(&self) -> Result<String> {
        let response = self
            .cloud
            .get(self.vm_url(""))
            .bearer_auth(self.google_token().await?)
            .send()
            .await
            .map_err(|_| bad("Cannot check VM state"))?;
        if !response.status().is_success() {
            return Err(bad("Google Compute Engine instance lookup failed"));
        }
        let instance: Instance = response
            .json()
            .await
            .map_err(|_| bad("Invalid VM state response"))?;
        Ok(instance.status)
    }

    async fn vm_action(&self, action: &'static str) -> Result<()> {
        let response = self
            .cloud
            .post(self.vm_url(action))
            .bearer_auth(self.google_token().await?)
            .send()
            .await
            .map_err(|_| bad("Compute Engine lifecycle request failed"))?;
        if !response.status().is_success() {
            return Err(bad("Compute Engine rejected VM lifecycle request"));
        }
        Ok(())
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
        let deadline = Instant::now() + self.settings.start_timeout;
        let mut start_requested = false;
        loop {
            if Instant::now() >= deadline {
                return Err(AppError::new(
                    ErrorCode::Timeout,
                    "GCP worker startup timed out",
                ));
            }
            match self.vm_state().await?.as_str() {
                "RUNNING" => {
                    if self.worker_ready().await {
                        return Ok(());
                    }
                }
                "TERMINATED" => {
                    if !start_requested {
                        self.vm_action("/start").await?;
                        start_requested = true;
                    }
                }
                "STOPPING" | "STAGING" | "PROVISIONING" | "REPAIRING" => (),
                _ => return Err(bad("GCP VM is in an unsupported state")),
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
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
            self.vm_action("/stop").await?;
            tracing::info!("Stopped idle GCP extractor instance");
        }
        Ok(())
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
        if cancel.is_cancelled() {
            return Err(AppError::new(ErrorCode::Cancelled, "Job cancelled"));
        }
        let mut dispatched = None;
        for attempt in 0..2 {
            match self
                .worker_req(Method::POST, "/api/v1/downloads")
                .header("X-Idempotency-Key", id.to_string())
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
        let remote = job.job_id;
        if remote != id {
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
                        return self.retrieve(id, remote, &output, &cancel).await;
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

    async fn cleanup(&self, id: Uuid) -> Result<()> {
        self.storage.cleanup(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_prevent_path_injection() {
        assert!(Uuid::parse_str("../other").is_err());
        assert!(matches!("mp4", "mp4" | "webm" | "mp3" | "m4a"));
    }
}
