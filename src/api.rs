//! One trusted service tenant. Every v1 route, including file delivery, requires the shared bearer token.
use crate::{
    config::Config,
    error::{AppError, ErrorCode, Result},
    extractor::Extractor,
    jobs::{JobManager, JobRecord, JobStatus},
    model::{DownloadRequest, JobError, JobProgress, JobState, JobView, OutputInfo},
};
use axum::{
    body::Body,
    extract::{rejection::JsonRejection, DefaultBodyLimit, Path, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use serde::Deserialize;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

struct Window {
    started: Instant,
    used: u32,
}
#[derive(Clone)]
pub struct ApiState {
    pub config: Arc<Config>,
    pub extractor: Arc<dyn Extractor>,
    pub jobs: Arc<JobManager>,
    gate: Arc<Semaphore>,
    rate: Arc<Mutex<Window>>,
    readiness: Arc<tokio::sync::Mutex<Option<(Instant, bool)>>>,
}
impl ApiState {
    pub fn new(config: Arc<Config>, extractor: Arc<dyn Extractor>, jobs: Arc<JobManager>) -> Self {
        Self {
            gate: Arc::new(Semaphore::new(config.max_concurrent_requests)),
            config,
            extractor,
            jobs,
            rate: Arc::new(Mutex::new(Window {
                started: Instant::now(),
                used: 0,
            })),
            readiness: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }
}
pub fn router(state: ApiState) -> Router {
    let protected = Router::new()
        .route("/api/v1/videos/info", post(video_info))
        .route("/api/v1/downloads", post(create_download))
        .route("/api/v1/internal/activity", get(worker_activity))
        .route(
            "/api/v1/downloads/{id}",
            get(get_download).delete(delete_download),
        )
        .route("/api/v1/downloads/{id}/file", get(download_file))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .route_layer(middleware::from_fn_with_state(state.clone(), protect));
    Router::new()
        .route("/", get(frontend_page))
        .route("/app.css", get(frontend_css))
        .route("/app.js", get(frontend_js))
        .route("/health", get(health))
        .merge(protected)
        .with_state(state)
}
// The operator UI is served without a JavaScript build tool, and does not
// embed or expose the server's API_TOKEN. All v1 API calls remain bearer-protected.
async fn frontend_page() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "strict-origin-when-cross-origin"),
            (header::CONTENT_SECURITY_POLICY, "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; media-src blob:; frame-src https://www.youtube-nocookie.com; base-uri 'none'; form-action 'self'; frame-ancestors 'none'; object-src 'none'"),
        ],
        include_str!("../web/index.html"),
    )
}
async fn frontend_css() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        include_str!("../web/app.css"),
    )
}
async fn frontend_js() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        include_str!("../web/app.js"),
    )
}
async fn protect(State(state): State<ApiState>, request: Request, next: Next) -> Response {
    if let Some(expected) = &state.config.api_token {
        let provided = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .unwrap_or("");
        if !bool::from(expected.as_bytes().ct_eq(provided.as_bytes())) {
            return AppError::new(ErrorCode::Unauthorized, "A valid bearer token is required")
                .into_response();
        }
    }
    {
        let mut rate = match state.rate.lock() {
            Ok(rate) => rate,
            Err(_) => return AppError::internal("Rate limiter unavailable").into_response(),
        };
        if rate.started.elapsed() >= Duration::from_secs(60) {
            rate.started = Instant::now();
            rate.used = 0;
        }
        if rate.used >= state.config.requests_per_minute {
            let mut response =
                AppError::new(ErrorCode::RateLimited, "Service request rate exceeded")
                    .into_response();
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
            return response;
        }
        rate.used += 1;
    }
    let Ok(permit) = state.gate.clone().try_acquire_owned() else {
        return AppError::new(ErrorCode::QueueFull, "Concurrent request limit reached")
            .into_response();
    };
    match tokio::time::timeout(state.config.request_timeout, next.run(request)).await {
        Ok(mut response) => {
            let body = std::mem::replace(response.body_mut(), Body::empty());
            let (sender, receiver) = tokio::sync::mpsc::channel(1);
            let deadline = tokio::time::Instant::now() + state.config.download_timeout;
            // A bounded producer owns file leases and the concurrency permit.
            // Its independent deadline fires even when Hyper stops polling a stalled socket.
            tokio::spawn(async move {
                let _permit = permit;
                let mut stream = body.into_data_stream();
                loop {
                    let chunk = tokio::select! {
                        _ = tokio::time::sleep_until(deadline) => break,
                        _ = sender.closed() => break,
                        chunk = stream.next() => chunk,
                    };
                    let Some(chunk) = chunk else { break };
                    tokio::select! {
                        _ = tokio::time::sleep_until(deadline) => break,
                        result = sender.send(chunk) => { if result.is_err() { break; } }
                    }
                }
            });
            *response.body_mut() = Body::from_stream(futures_util::stream::unfold(
                receiver,
                |mut receiver| async move { receiver.recv().await.map(|chunk| (chunk, receiver)) },
            ));
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response.headers_mut().insert(
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            );
            response
        }
        Err(_) => AppError::new(ErrorCode::Timeout, "API request timed out").into_response(),
    }
}
async fn health(State(state): State<ApiState>) -> Response {
    let Ok(mut cached) = state.readiness.try_lock() else {
        return health_response(false);
    };
    if let Some((checked, ready)) = *cached {
        if checked.elapsed() < Duration::from_secs(5) {
            return health_response(ready);
        }
    }
    let check = async {
        state.config.validate()?;
        let probe = state
            .config
            .data_dir
            .join(format!(".health-{}", Uuid::new_v4()));
        tokio::fs::write(&probe, b"ready")
            .await
            .map_err(|_| AppError::new(ErrorCode::Storage, "Data directory is not writable"))?;
        tokio::fs::remove_file(&probe)
            .await
            .map_err(|_| AppError::new(ErrorCode::Storage, "Data directory cleanup failed"))?;
        let probe = std::env::var_os("FFPROBE_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "ffprobe".into());
        for binary in [&state.config.ffmpeg_path, &probe] {
            let output = tokio::process::Command::new(binary)
                .arg("-version")
                .kill_on_drop(true)
                .output()
                .await
                .map_err(|_| AppError::new(ErrorCode::Internal, "FFmpeg tools are unavailable"))?;
            if !output.status.success() {
                return Err(AppError::new(
                    ErrorCode::Internal,
                    "FFmpeg tool readiness check failed",
                ));
            }
        }
        if std::env::var("YTDLP_PROXY_REQUIRED").as_deref() == Ok("true") {
            let proxy = std::env::var("YTDLP_PROXY")
                .map_err(|_| AppError::internal("Worker proxy is not configured"))?;
            let url = url::Url::parse(&proxy)
                .map_err(|_| AppError::internal("Worker proxy URL is invalid"))?;
            if url.scheme() != "socks5" || url.host_str() != Some("127.0.0.1") {
                return Err(AppError::internal("Worker proxy must be local SOCKS5"));
            }
            let port = url.port().ok_or_else(|| AppError::internal("Proxy port is missing"))?;
            tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(("127.0.0.1", port)))
                .await
                .map_err(|_| AppError::internal("WARP proxy is unavailable"))?
                .map_err(|_| AppError::internal("WARP proxy is unavailable"))?;
            let status = tokio::process::Command::new("warp-cli")
                .arg("status")
                .output()
                .await
                .map_err(|_| AppError::internal("WARP status is unavailable"))?;
            let status_text = String::from_utf8_lossy(&status.stdout);
            if !status.status.success() || !status_text.contains("Connected") || status_text.contains("Disconnected") {
                return Err(AppError::internal("WARP is not connected"));
            }
        }
        Ok::<_, AppError>(())
    };
    let ready = matches!(
        tokio::time::timeout(Duration::from_secs(5), check).await,
        Ok(Ok(()))
    );
    *cached = Some((Instant::now(), ready));
    health_response(ready)
}
fn health_response(ready: bool) -> Response {
    if ready {
        (StatusCode::OK, Json(serde_json::json!({"status":"ok","storage":"local","durability":"requires_persistent_volume"}))).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"status":"unhealthy"})),
        )
            .into_response()
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InfoRequest {
    url: String,
}
fn json_error(error: JsonRejection) -> AppError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        AppError::new(ErrorCode::DownloadTooLarge, "Request body exceeds 16 KiB")
    } else {
        AppError::invalid("Invalid JSON request body")
    }
}
async fn video_info(
    State(state): State<ApiState>,
    body: std::result::Result<Json<InfoRequest>, JsonRejection>,
) -> Result<Json<crate::model::VideoInfo>> {
    let Json(body) = body.map_err(json_error)?;
    if body.url.len() > 2048 {
        return Err(AppError::invalid("URL must be at most 2048 bytes"));
    }
    Ok(Json(state.extractor.video_info(&body.url).await?))
}
async fn worker_activity(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let active = state.jobs.has_activity().await;
    Json(serde_json::json!({"busy": active}))
}
async fn create_download(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: std::result::Result<Json<DownloadRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<JobView>)> {
    let Json(request) = body.map_err(json_error)?;
    request.validate()?;
    crate::extractor::normalize_url(&request.url)?;
    let job = if let Some(header) = headers.get("X-Idempotency-Key") {
        let id = header.to_str().ok().and_then(|v| Uuid::parse_str(v).ok())
            .ok_or_else(|| AppError::invalid("Invalid X-Idempotency-Key"))?;
        state.jobs.submit_with_id(request, Some(id)).await?
    } else {
        state.jobs.submit(request).await?
    };
    Ok((StatusCode::ACCEPTED, Json(view(job))))
}
fn parse_id(id: &str) -> Result<Uuid> {
    Uuid::parse_str(id).map_err(|_| AppError::invalid("Job identifier must be a UUID"))
}
async fn get_download(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<JobView>> {
    Ok(Json(view(state.jobs.get(parse_id(&id)?).await?)))
}
async fn delete_download(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    state.jobs.cancel(parse_id(&id)?).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn download_file(State(state): State<ApiState>, Path(id): Path<String>) -> Result<Response> {
    let (file, output, lease) = state.jobs.open_completed(parse_id(&id)?).await?;
    let filename: String = output
        .filename
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(128)
        .collect();
    let mut response = Body::from_stream(ReaderStream::new(file).map(move |chunk| {
        let _lease = &lease;
        chunk
    }))
    .into_response();
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
            .map_err(|_| AppError::internal("Invalid output filename"))?,
    );
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&output.content_type)
            .map_err(|_| AppError::internal("Invalid output media type"))?,
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&output.size_bytes.to_string())
            .map_err(|_| AppError::internal("Invalid output size"))?,
    );
    Ok(response)
}
fn view(record: JobRecord) -> JobView {
    let status = match record.status {
        JobStatus::Queued => JobState::Queued,
        JobStatus::Running => JobState::Running,
        JobStatus::Completed => JobState::Completed,
        JobStatus::Failed => JobState::Failed,
        JobStatus::Cancelled => JobState::Cancelled,
    };
    let timestamp =
        chrono::DateTime::from_timestamp(record.created_at as i64, 0).unwrap_or_default();
    let percent =
        record.progress.total_bytes.filter(|n| *n > 0).map(|n| {
            ((record.progress.downloaded_bytes as f64 / n as f64 * 100.0).min(100.0)) as f32
        });
    JobView {
        job_id: record.job_id.to_string(),
        status,
        created_at: timestamp,
        updated_at: chrono::DateTime::from_timestamp(record.updated_at as i64, 0)
            .unwrap_or_default(),
        progress: JobProgress {
            downloaded_bytes: record.progress.downloaded_bytes,
            total_bytes: record.progress.total_bytes,
            percent,
        },
        output: record
            .output
            .filter(|_| status == JobState::Completed)
            .map(|output| OutputInfo {
                filename: output.filename,
                mime_type: output.content_type,
                size_bytes: output.size_bytes,
            }),
        error: record.error.map(JobError::from),
        download_url: (status == JobState::Completed)
            .then(|| format!("/api/v1/downloads/{}/file", record.job_id)),
    }
}
