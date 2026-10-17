//! Public browser workflow. Opaque, bounded sessions own only browser-created jobs.
//! The private API bearer token never enters this module or any browser response.
use crate::{
    api::{self, ApiState},
    error::{AppError, ErrorCode, Result},
    model::{DownloadKind, DownloadRequest, JobView, OutputFormat},
};
use axum::{
    body::Body,
    extract::{rejection::JsonRejection, DefaultBodyLimit, Path, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use futures_util::StreamExt;
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

const COOKIE: &str = "extractor_session";
const SESSION_TTL: Duration = Duration::from_secs(3600);
const MAX_SESSIONS: usize = 4096;
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-src https://www.youtube-nocookie.com; media-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'; object-src 'none'";

#[derive(Clone, Default)]
pub struct BrowserSettings {
    pub secure_cookies: bool,
    /// Exact HTTPS origin for deployments whose proxy rewrites Host.
    pub public_origin: Option<String>,
}
impl BrowserSettings {
    fn from_env() -> Self {
        Self {
            secure_cookies: std::env::var("UI_SECURE_COOKIES").as_deref() == Ok("true"),
            public_origin: std::env::var("UI_PUBLIC_ORIGIN").ok(),
        }
    }
}
struct Session {
    csrf: String,
    touched: Instant,
    pending: usize,
}
#[derive(Default)]
struct Sessions {
    sessions: HashMap<Uuid, Session>,
    owners: HashMap<Uuid, Uuid>,
}
impl Sessions {
    fn cleanup(&mut self) {
        // A submission may outlive the request; keep its session until the
        // reservation is committed or dropped, rather than orphaning its job.
        self.sessions
            .retain(|_, session| session.pending > 0 || session.touched.elapsed() < SESSION_TTL);
        self.owners
            .retain(|_, owner| self.sessions.contains_key(owner));
    }
}
/// Reserve ownership before awaiting job persistence. The guard releases its
/// slot on every failure/cancellation path, so concurrent creates stay bounded.
struct OwnershipReservation {
    sessions: Arc<Mutex<Sessions>>,
    owner: Uuid,
    pending: bool,
}
impl OwnershipReservation {
    fn acquire(store: &Arc<Mutex<Sessions>>, owner: Uuid, limit: usize) -> Result<Self> {
        let mut sessions = store
            .lock()
            .map_err(|_| AppError::internal("Browser sessions unavailable"))?;
        sessions.cleanup();
        let pending: usize = sessions
            .sessions
            .values()
            .map(|session| session.pending)
            .sum();
        if sessions.owners.len().saturating_add(pending) >= limit {
            return Err(AppError::new(
                ErrorCode::QueueFull,
                "Browser job capacity reached. Try again later.",
            ));
        }
        sessions
            .sessions
            .get_mut(&owner)
            .ok_or_else(expired)?
            .pending += 1;
        Ok(Self {
            sessions: store.clone(),
            owner,
            pending: true,
        })
    }
    fn commit(mut self, id: Uuid) -> Result<()> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| AppError::internal("Browser sessions unavailable"))?;
        let session = sessions.sessions.get_mut(&self.owner).ok_or_else(expired)?;
        session.pending -= 1;
        session.touched = Instant::now();
        sessions.owners.insert(id, self.owner);
        self.pending = false;
        Ok(())
    }
}
impl Drop for OwnershipReservation {
    fn drop(&mut self) {
        if self.pending {
            if let Ok(mut sessions) = self.sessions.lock() {
                if let Some(session) = sessions.sessions.get_mut(&self.owner) {
                    session.pending = session.pending.saturating_sub(1);
                }
            }
        }
    }
}
#[derive(Clone)]
struct WebState {
    api: ApiState,
    settings: BrowserSettings,
    sessions: Arc<Mutex<Sessions>>,
}

pub fn router(api: ApiState) -> Router {
    if std::env::var("FRONTEND_ENABLED").as_deref() == Ok("false") {
        return Router::new();
    }
    router_with_settings(api, BrowserSettings::from_env())
}
/// Explicit settings keep tests and embedders independent of process environment.
pub fn router_with_settings(api: ApiState, settings: BrowserSettings) -> Router {
    let state = WebState {
        api,
        settings,
        sessions: Arc::new(Mutex::new(Sessions::default())),
    };
    let browser = Router::new()
        .route("/ui/downloads", post(create_download))
        .route(
            "/ui/downloads/{id}",
            get(get_download).delete(delete_download),
        )
        .route("/ui/downloads/{id}/media", get(media_file))
        .route("/ui/downloads/{id}/file", get(download_file))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .route_layer(middleware::from_fn_with_state(state.clone(), browser_guard));
    let session = Router::new()
        .route("/ui/session", get(session))
        .route_layer(middleware::from_fn_with_state(state.clone(), session_guard));
    Router::new()
        .route("/", get(index))
        .route("/assets/styles.css", get(styles))
        .route("/assets/app.js", get(script))
        .route("/assets/url.mjs", get(url_module))
        .merge(browser)
        .merge(session)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}
async fn method_not_allowed() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(serde_json::json!({
            "error": { "code": "INVALID_REQUEST", "message": "This request method is not supported" }
        })),
    )
}
async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    for (name, value) in [
        (header::CONTENT_SECURITY_POLICY, CSP),
        (header::CACHE_CONTROL, "no-store"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "strict-origin-when-cross-origin"),
        (header::X_FRAME_OPTIONS, "DENY"),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    response.headers_mut().insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    response
}
async fn index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../web/index.html"),
    )
}
async fn styles() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../web/styles.css"),
    )
}
async fn script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../web/app.js"),
    )
}
async fn url_module() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../web/url.mjs"),
    )
}

fn denied(message: &str) -> AppError {
    AppError::new(ErrorCode::AccessRestricted, message)
}
fn expired() -> AppError {
    AppError::new(
        ErrorCode::Unauthorized,
        "Your browser session expired. Refresh the page and try again.",
    )
}
fn cookie_id(headers: &HeaderMap) -> Option<Uuid> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|item| {
            item.trim()
                .strip_prefix(&format!("{COOKIE}="))
                .and_then(|id| Uuid::parse_str(id).ok())
        })
}
fn same_origin(state: &WebState, headers: &HeaderMap) -> Result<()> {
    if let Some(origin) = &state.settings.public_origin {
        let valid = url::Url::parse(origin).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https")
                && (!state.settings.secure_cookies || url.scheme() == "https")
                && url.username().is_empty()
                && url.password().is_none()
                && url.origin().ascii_serialization() == *origin
        });
        if !valid {
            return Err(AppError::internal("UI_PUBLIC_ORIGIN must be an exact HTTP(S) origin; HTTPS is required with secure cookies"));
        }
    }
    if let Some(site) = headers.get("sec-fetch-site") {
        if site != "same-origin" && site != "none" {
            return Err(denied("Cross-origin browser requests are not allowed"));
        }
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let origin = origin
            .to_str()
            .map_err(|_| denied("Invalid request origin"))?;
        let expected = match &state.settings.public_origin {
            Some(origin) => origin.clone(),
            None => {
                let host = headers
                    .get(header::HOST)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| denied("Request origin could not be verified"))?;
                format!(
                    "{}://{host}",
                    if state.settings.secure_cookies {
                        "https"
                    } else {
                        "http"
                    }
                )
            }
        };
        if origin != expected {
            return Err(denied("Request origin does not match this service"));
        }
    }
    Ok(())
}
async fn session_guard(State(state): State<WebState>, request: Request, next: Next) -> Response {
    if let Err(error) = same_origin(&state, request.headers()) {
        return error.into_response();
    }
    api::admit(state.api, request, next).await
}
async fn browser_guard(
    State(state): State<WebState>,
    mut request: Request,
    next: Next,
) -> Response {
    let id = {
        let validate = || -> Result<Uuid> {
            same_origin(&state, request.headers())?;
            let id = cookie_id(request.headers()).ok_or_else(expired)?;
            let mut sessions = state
                .sessions
                .lock()
                .map_err(|_| AppError::internal("Browser sessions unavailable"))?;
            sessions.cleanup();
            let session = sessions.sessions.get_mut(&id).ok_or_else(expired)?;
            if request.method() != Method::GET && request.method() != Method::HEAD {
                let csrf = request
                    .headers()
                    .get("x-csrf-token")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                if !bool::from(session.csrf.as_bytes().ct_eq(csrf.as_bytes())) {
                    return Err(denied(
                        "Invalid browser request token. Refresh the page and try again.",
                    ));
                }
            }
            session.touched = Instant::now();
            Ok(id)
        };
        match validate() {
            Ok(id) => id,
            Err(error) => return error.into_response(),
        }
    };
    request.extensions_mut().insert(id);
    api::admit(state.api, request, next).await
}
async fn session(State(state): State<WebState>, headers: HeaderMap) -> Result<Response> {
    let mut sessions = state
        .sessions
        .lock()
        .map_err(|_| AppError::internal("Browser sessions unavailable"))?;
    sessions.cleanup();
    let id = cookie_id(&headers).filter(|id| sessions.sessions.contains_key(id));
    let id = match id {
        Some(id) => id,
        None => {
            if sessions.sessions.len() >= MAX_SESSIONS {
                return Err(AppError::new(
                    ErrorCode::QueueFull,
                    "Browser session capacity reached. Try again later.",
                ));
            }
            let id = Uuid::new_v4();
            sessions.sessions.insert(
                id,
                Session {
                    csrf: Uuid::new_v4().to_string(),
                    touched: Instant::now(),
                    pending: 0,
                },
            );
            id
        }
    };
    let session = sessions.sessions.get_mut(&id).ok_or_else(expired)?;
    session.touched = Instant::now();
    let mut response = Json(serde_json::json!({"csrf_token": session.csrf})).into_response();
    let secure = if state.settings.secure_cookies {
        "; Secure"
    } else {
        ""
    };
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{COOKIE}={id}; Path=/ui; HttpOnly; SameSite=Strict{secure}"
        ))
        .map_err(|_| AppError::internal("Browser session unavailable"))?,
    );
    Ok(response)
}
fn owned(state: &WebState, session: Uuid, id: &str) -> Result<Uuid> {
    let id = api::parse_id(id)?;
    let sessions = state
        .sessions
        .lock()
        .map_err(|_| AppError::internal("Browser sessions unavailable"))?;
    if sessions.owners.get(&id) != Some(&session) {
        return Err(AppError::new(
            ErrorCode::NotFound,
            "Job not found in this browser session",
        ));
    }
    Ok(id)
}
#[derive(Serialize)]
struct BrowserJob {
    #[serde(flatten)]
    job: JobView,
    media_url: Option<String>,
}
fn browser_view(record: crate::jobs::JobRecord) -> BrowserJob {
    let mut job = api::view(record);
    let media_url = job
        .download_url
        .as_ref()
        .map(|_| format!("/ui/downloads/{}/media", job.job_id));
    job.download_url = job
        .download_url
        .as_ref()
        .map(|_| format!("/ui/downloads/{}/file", job.job_id));
    BrowserJob { job, media_url }
}
async fn create_download(
    State(state): State<WebState>,
    Extension(session): Extension<Uuid>,
    body: std::result::Result<Json<DownloadRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<BrowserJob>)> {
    let Json(request) = body.map_err(api::json_error)?;
    if request.kind != DownloadKind::Video || request.format != OutputFormat::Mp4 {
        return Err(AppError::invalid(
            "The browser generator supports MP4 video only",
        ));
    }
    request.validate()?;
    crate::extractor::normalize_url(&request.url)?;
    let reservation =
        match OwnershipReservation::acquire(&state.sessions, session, state.api.config.max_jobs) {
            Ok(reservation) => reservation,
            Err(error) if error.code == ErrorCode::QueueFull => {
                // A queued job can outlive a single execution timeout. Retain its
                // ownership until JobManager actually removes it; reclaim stale
                // ownership only when capacity is needed, without holding a mutex
                // across an await or accepting concurrent unreserved submissions.
                let ids: Vec<_> = state
                    .sessions
                    .lock()
                    .map_err(|_| AppError::internal("Browser sessions unavailable"))?
                    .owners
                    .keys()
                    .copied()
                    .collect();
                for id in ids {
                    if let Err(error) = state.api.jobs.get(id).await {
                        if error.code == ErrorCode::NotFound {
                            state
                                .sessions
                                .lock()
                                .map_err(|_| AppError::internal("Browser sessions unavailable"))?
                                .owners
                                .remove(&id);
                        } else {
                            return Err(error);
                        }
                    }
                }
                OwnershipReservation::acquire(&state.sessions, session, state.api.config.max_jobs)?
            }
            Err(error) => return Err(error),
        };
    let record = state.api.jobs.submit(request).await?;
    reservation.commit(record.job_id)?;
    Ok((StatusCode::ACCEPTED, Json(browser_view(record))))
}
async fn get_download(
    State(state): State<WebState>,
    Extension(session): Extension<Uuid>,
    Path(id): Path<String>,
) -> Result<Json<BrowserJob>> {
    let id = owned(&state, session, &id)?;
    Ok(Json(browser_view(state.api.jobs.get(id).await?)))
}
async fn delete_download(
    State(state): State<WebState>,
    Extension(session): Extension<Uuid>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let id = owned(&state, session, &id)?;
    state.api.jobs.cancel(id).await?;
    state
        .sessions
        .lock()
        .map_err(|_| AppError::internal("Browser sessions unavailable"))?
        .owners
        .remove(&id);
    Ok(StatusCode::NO_CONTENT)
}
async fn media_file(
    State(state): State<WebState>,
    Extension(session): Extension<Uuid>,
    Path(id): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response> {
    serve_file(state, session, id, method, headers, false).await
}
async fn download_file(
    State(state): State<WebState>,
    Extension(session): Extension<Uuid>,
    Path(id): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response> {
    serve_file(state, session, id, method, headers, true).await
}
fn range(headers: &HeaderMap, size: u64) -> std::result::Result<Option<(u64, u64)>, ()> {
    let Some(value) = headers.get(header::RANGE) else {
        return Ok(None);
    };
    if headers.get_all(header::RANGE).iter().count() != 1 || size == 0 {
        return Err(());
    }
    let value = value
        .to_str()
        .map_err(|_| ())?
        .strip_prefix("bytes=")
        .ok_or(())?;
    let (start, end) = value.split_once('-').ok_or(())?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if start.is_empty() {
        if !digits(end) {
            return Err(());
        }
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        return Ok(Some((size.saturating_sub(suffix), size - 1)));
    }
    if !digits(start) || (!end.is_empty() && !digits(end)) {
        return Err(());
    }
    let start = start.parse::<u64>().map_err(|_| ())?;
    let end = if end.is_empty() {
        size - 1
    } else {
        end.parse::<u64>().map_err(|_| ())?.min(size - 1)
    };
    if start >= size || start > end {
        return Err(());
    }
    Ok(Some((start, end)))
}
async fn serve_file(
    state: WebState,
    session: Uuid,
    id: String,
    method: Method,
    headers: HeaderMap,
    attachment: bool,
) -> Result<Response> {
    let id = owned(&state, session, &id)?;
    let (mut file, output, lease) = state.api.jobs.open_completed(id).await?;
    let size = output.size_bytes;
    if file
        .metadata()
        .await
        .map_err(|_| AppError::new(ErrorCode::Storage, "Media file unavailable"))?
        .len()
        != size
    {
        return Err(AppError::new(ErrorCode::Storage, "Media file size changed"));
    }
    let range = match range(&headers, size) {
        Ok(range) => range,
        Err(()) => {
            let mut response = (StatusCode::RANGE_NOT_SATISFIABLE, Json(serde_json::json!({"error":{"code":"INVALID_REQUEST","message":"Requested byte range is not satisfiable"}}))).into_response();
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{size}"))
                    .map_err(|_| AppError::internal("Invalid media range"))?,
            );
            return Ok(response);
        }
    };
    let (start, length) = range.map_or((0, size), |(start, end)| (start, end - start + 1));
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|_| AppError::new(ErrorCode::Storage, "Media file unavailable"))?;
        Body::from_stream(ReaderStream::new(file.take(length)).map(move |chunk| {
            let _lease = &lease;
            chunk
        }))
    };
    let mut response = body.into_response();
    if let Some((start, end)) = range {
        *response.status_mut() = StatusCode::PARTIAL_CONTENT;
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{size}"))
                .map_err(|_| AppError::internal("Invalid media range"))?,
        );
    }
    for (name, value) in [
        (header::CONTENT_TYPE, "video/mp4".to_string()),
        (header::CONTENT_LENGTH, length.to_string()),
        (
            header::CONTENT_DISPOSITION,
            format!(
                "{}; filename=\"video-{id}.mp4\"",
                if attachment { "attachment" } else { "inline" }
            ),
        ),
        (header::ACCEPT_RANGES, "bytes".to_string()),
    ] {
        response.headers_mut().insert(
            name,
            HeaderValue::from_str(&value)
                .map_err(|_| AppError::internal("Invalid media response"))?,
        );
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    fn sessions() -> (Arc<Mutex<Sessions>>, Uuid) {
        let owner = Uuid::new_v4();
        let mut sessions = Sessions::default();
        sessions.sessions.insert(
            owner,
            Session {
                csrf: Uuid::new_v4().to_string(),
                touched: Instant::now(),
                pending: 0,
            },
        );
        (Arc::new(Mutex::new(sessions)), owner)
    }

    #[test]
    fn concurrent_submissions_cannot_exceed_ownership_capacity() {
        let (sessions, owner) = sessions();
        let ready = Arc::new(Barrier::new(17));
        let release = Arc::new(Barrier::new(17));
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let (sessions, ready, release) = (sessions.clone(), ready.clone(), release.clone());
                scope.spawn(move || {
                    let reservation = OwnershipReservation::acquire(&sessions, owner, 4);
                    ready.wait();
                    release.wait();
                    drop(reservation);
                });
            }
            ready.wait();
            assert_eq!(sessions.lock().unwrap().sessions[&owner].pending, 4);
            release.wait();
        });
        assert_eq!(sessions.lock().unwrap().sessions[&owner].pending, 0);
        let reservation = OwnershipReservation::acquire(&sessions, owner, 4).unwrap();
        let id = Uuid::new_v4();
        reservation.commit(id).unwrap();
        assert_eq!(sessions.lock().unwrap().owners.get(&id), Some(&owner));
    }

    #[test]
    fn pending_submission_survives_expiration_and_drop_releases_capacity() {
        let (sessions, owner) = sessions();
        let reservation = OwnershipReservation::acquire(&sessions, owner, 1).unwrap();
        {
            let mut sessions = sessions.lock().unwrap();
            sessions.sessions.get_mut(&owner).unwrap().touched = Instant::now() - SESSION_TTL;
            sessions.cleanup();
            assert!(sessions.sessions.contains_key(&owner));
        }
        assert!(OwnershipReservation::acquire(&sessions, owner, 1).is_err());
        drop(reservation);
        let mut sessions = sessions.lock().unwrap();
        sessions.cleanup();
        assert!(sessions.sessions.is_empty());
        assert!(sessions.owners.is_empty());
    }
}
