use async_trait::async_trait;
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use uuid::Uuid;
use youtube_extractor::{
    api::{self, ApiState},
    config::Config,
    download::DownloadProgress,
    error::{AppError, ErrorCode, Result},
    extractor::Extractor,
    jobs::{JobExecutor, JobManager, JobSettings, JobStatus},
    model::{DownloadRequest, VideoInfo},
    storage::StoredOutput,
    web::{self, BrowserSettings},
};
const TOKEN: &str = "test-token-0123456789-test-token-0123456789";
const INPUT: &str =
    r#"{"url":"https://youtu.be/aqz-KE-bpKQ","type":"video","format":"mp4","quality":"720p"}"#;
struct Controlled(PathBuf);
#[async_trait]
impl JobExecutor for Controlled {
    async fn run(
        &self,
        _: &DownloadRequest,
        id: Uuid,
        _: CancellationToken,
        _: watch::Sender<DownloadProgress>,
    ) -> Result<StoredOutput> {
        let dir = self.0.join(id.to_string());
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("controlled.mp4");
        tokio::fs::write(&path, vec![42u8; 256 * 1024])
            .await
            .unwrap();
        Ok(StoredOutput {
            path,
            size_bytes: 256 * 1024,
            filename: "controlled.mp4".into(),
            content_type: "video/mp4".into(),
        })
    }
    async fn cleanup(&self, id: Uuid) -> Result<()> {
        let _ = tokio::fs::remove_dir_all(self.0.join(id.to_string())).await;
        Ok(())
    }
}
#[async_trait]
impl Extractor for Controlled {
    async fn video_info(&self, _: &str) -> Result<VideoInfo> {
        Err(AppError::new(
            ErrorCode::UnsupportedFormat,
            "Controlled test",
        ))
    }
}
async fn app(root: &Path, rate: u32, concurrency: usize) -> (Router, Router, Arc<JobManager>) {
    app_with_settings(root, rate, concurrency, BrowserSettings::default()).await
}
async fn app_with_settings(
    root: &Path,
    rate: u32,
    concurrency: usize,
    settings: BrowserSettings,
) -> (Router, Router, Arc<JobManager>) {
    let config = Arc::new(Config {
        bind_addr: "0.0.0.0:8080".parse().unwrap(),
        api_token: Some(TOKEN.into()),
        data_dir: root.into(),
        workers: 1,
        queue_capacity: 4,
        max_download_bytes: 512 * 1024,
        max_storage_bytes: 8 * 1024 * 1024,
        job_ttl: Duration::from_secs(60),
        max_jobs: 8,
        connect_timeout: Duration::from_secs(1),
        request_timeout: Duration::from_secs(1),
        download_timeout: Duration::from_millis(150),
        ffmpeg_path: "ffmpeg".into(),
        requests_per_minute: rate,
        max_concurrent_requests: concurrency,
    });
    let executor = Arc::new(Controlled(root.into()));
    let jobs = JobManager::new(
        JobSettings {
            data_dir: root.into(),
            workers: 1,
            queue_capacity: 4,
            max_jobs: 8,
            max_download_bytes: config.max_download_bytes,
            max_storage_bytes: config.max_storage_bytes,
            ttl: config.job_ttl,
            timeout: Duration::from_secs(2),
        },
        executor.clone(),
    )
    .await
    .unwrap();
    let state = ApiState::new(config, executor, jobs.clone());
    (
        web::router_with_settings(state.clone(), settings),
        api::router(state),
        jobs,
    )
}
fn req(
    method: &str,
    path: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    body: &str,
) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost")
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    if let Some(csrf) = csrf {
        request = request.header("x-csrf-token", csrf);
    }
    request.body(Body::from(body.to_string())).unwrap()
}
async fn json(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
}
async fn session(app: &Router) -> (String, String) {
    let response = app
        .clone()
        .oneshot(req("GET", "/ui/session", None, None, ""))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Strict")
            && cookie.contains("Path=/ui")
    );
    let csrf = json(response).await["csrf_token"]
        .as_str()
        .unwrap()
        .to_string();
    (cookie.split(';').next().unwrap().to_string(), csrf)
}
async fn submit(app: &Router, cookie: &str, csrf: &str) -> String {
    let response = app
        .clone()
        .oneshot(req(
            "POST",
            "/ui/downloads",
            Some(cookie),
            Some(csrf),
            INPUT,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    json(response).await["job_id"].as_str().unwrap().to_string()
}
async fn complete(jobs: &JobManager, id: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while jobs.get(Uuid::parse_str(id).unwrap()).await.unwrap().status != JobStatus::Completed {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn assets_are_public_and_private_api_keeps_bearer_authentication() {
    let temp = tempfile::tempdir().unwrap();
    let (app, private, jobs) = app(temp.path(), 120, 4).await;
    for (path, mime) in [
        ("/", "text/html"),
        ("/assets/app.js", "text/javascript"),
        ("/assets/styles.css", "text/css"),
        ("/assets/url.mjs", "text/javascript"),
    ] {
        let response = app
            .clone()
            .oneshot(req("GET", path, None, None, ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with(mime));
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'"));
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains(TOKEN));
    }
    for (method, path) in [
        ("POST", "/api/v1/downloads"),
        (
            "GET",
            "/api/v1/downloads/00000000-0000-0000-0000-000000000000/file",
        ),
        (
            "HEAD",
            "/api/v1/downloads/00000000-0000-0000-0000-000000000000/file",
        ),
    ] {
        let response = private
            .clone()
            .oneshot(req(method, path, None, None, INPUT))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    jobs.shutdown().await;
}
#[tokio::test]
async fn csrf_and_origin_are_required_and_ownership_never_exposes_private_jobs() {
    let temp = tempfile::tempdir().unwrap();
    let (app, _, jobs) = app(temp.path(), 120, 4).await;
    let (a, csrf) = session(&app).await;
    let (b, _) = session(&app).await;
    let response = app
        .clone()
        .oneshot(req("POST", "/ui/downloads", Some(&a), None, INPUT))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let mut request = req("POST", "/ui/downloads", Some(&a), Some(&csrf), INPUT);
    request
        .headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    assert_eq!(
        app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let mut request = req("POST", "/ui/downloads", Some(&a), Some(&csrf), INPUT);
    request
        .headers_mut()
        .insert("sec-fetch-site", "same-site".parse().unwrap());
    assert_eq!(
        app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let id = submit(&app, &a, &csrf).await;
    complete(&jobs, &id).await;
    for suffix in ["", "/file", "/media"] {
        let response = app
            .clone()
            .oneshot(req(
                "GET",
                &format!("/ui/downloads/{id}{suffix}"),
                Some(&b),
                None,
                "",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let _ = json(response).await;
    }
    let private = jobs
        .submit(serde_json::from_str(INPUT).unwrap())
        .await
        .unwrap();
    for method in ["GET", "HEAD"] {
        for suffix in ["", "/file", "/media"] {
            let response = app
                .clone()
                .oneshot(req(
                    method,
                    &format!("/ui/downloads/{}{suffix}", private.job_id),
                    Some(&a),
                    None,
                    "",
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            let _ = to_bytes(response.into_body(), 4096).await.unwrap();
        }
    }
    let response = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/ui/downloads/{id}"),
            Some(&a),
            None,
            "",
        ))
        .await
        .unwrap();
    let body = json(response).await;
    assert_eq!(body["download_url"], format!("/ui/downloads/{id}/file"));
    assert_eq!(body["media_url"], format!("/ui/downloads/{id}/media"));
    let response = app
        .clone()
        .oneshot(req(
            "DELETE",
            &format!("/ui/downloads/{id}"),
            Some(&a),
            Some(&csrf),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let _ = to_bytes(response.into_body(), 1024).await.unwrap();
    jobs.shutdown().await;
}
#[tokio::test]
async fn media_supports_head_single_byte_ranges_and_stream_deadlines() {
    let temp = tempfile::tempdir().unwrap();
    let (app, _, jobs) = app(temp.path(), 120, 1).await;
    let (cookie, csrf) = session(&app).await;
    let id = submit(&app, &cookie, &csrf).await;
    complete(&jobs, &id).await;
    let path = format!("/ui/downloads/{id}/media");
    let response = app
        .clone()
        .oneshot(req("HEAD", &path, Some(&cookie), None, ""))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-length"],
        (256 * 1024).to_string()
    );
    assert_eq!(response.headers()["accept-ranges"], "bytes");
    assert!(response.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .starts_with("inline"));
    assert!(to_bytes(response.into_body(), 1024)
        .await
        .unwrap()
        .is_empty());
    for (range, expected, size) in [
        ("bytes=1-3", "bytes 1-3/262144", 3),
        ("bytes=-2", "bytes 262142-262143/262144", 2),
        ("bytes=262142-", "bytes 262142-262143/262144", 2),
    ] {
        let mut request = req("GET", &path, Some(&cookie), None, "");
        request
            .headers_mut()
            .insert("range", range.parse().unwrap());
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()["content-range"], expected);
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.unwrap().len(),
            size
        );
    }
    for range in [
        "bytes=262144-",
        "bytes=0-1,4-8",
        "bytes=-0",
        "bytes=9-3",
        "bytes=x-2",
    ] {
        let mut request = req("GET", &path, Some(&cookie), None, "");
        request
            .headers_mut()
            .insert("range", range.parse().unwrap());
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(response.headers()["content-range"], "bytes */262144");
        let _ = json(response).await;
    }
    let response = app
        .clone()
        .oneshot(req("GET", &path, Some(&cookie), None, ""))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let blocked = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/ui/downloads/{id}"),
            Some(&cookie),
            None,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::SERVICE_UNAVAILABLE);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let available = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("/ui/downloads/{id}"),
            Some(&cookie),
            None,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(available.status(), StatusCode::OK);
    let _ = json(available).await;
    drop(response);
    jobs.shutdown().await;
}
#[tokio::test]
async fn public_requests_share_bounded_rate_limit_and_validate_payloads() {
    let temp = tempfile::tempdir().unwrap();
    let (app, _, jobs) = app(temp.path(), 3, 4).await;
    let (cookie, csrf) = session(&app).await;
    for input in [
        r#"{"url":"https://example.org/","type":"video","format":"mp4"}"#,
        r#"{"url":"https://youtu.be/aqz-KE-bpKQ","type":"audio","format":"mp3"}"#,
    ] {
        let response = app
            .clone()
            .oneshot(req(
                "POST",
                "/ui/downloads",
                Some(&cookie),
                Some(&csrf),
                input,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let _ = json(response).await;
    }
    let response = app
        .clone()
        .oneshot(req("GET", "/ui/session", Some(&cookie), None, ""))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()["retry-after"], "60");
    jobs.shutdown().await;
}

#[tokio::test]
async fn browser_session_is_stable_and_secure_origin_configuration_is_exact() {
    let temp = tempfile::tempdir().unwrap();
    let (app, _, jobs) = app_with_settings(
        temp.path(),
        120,
        4,
        BrowserSettings {
            secure_cookies: true,
            public_origin: Some("https://video.example.com".into()),
        },
    )
    .await;
    let (cookie, csrf) = session(&app).await;
    let mut request = req("GET", "/ui/session", Some(&cookie), None, "");
    request
        .headers_mut()
        .insert("origin", "https://video.example.com".parse().unwrap());
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let set_cookie = response.headers()["set-cookie"].to_str().unwrap();
    assert!(set_cookie.contains("; Secure"));
    assert_eq!(set_cookie.split(';').next().unwrap(), cookie);
    assert_eq!(json(response).await["csrf_token"], csrf);
    let mut request = req("POST", "/ui/downloads", Some(&cookie), Some(&csrf), INPUT);
    request.headers_mut().insert(
        "origin",
        "https://video.example.com.evil.test".parse().unwrap(),
    );
    assert_eq!(
        app.oneshot(request).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    jobs.shutdown().await;

    for origin in [
        "https://video.example.com/path",
        "https://user:pass@video.example.com",
        "http://video.example.com",
        "*",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let (app, _, jobs) = app_with_settings(
            temp.path(),
            120,
            4,
            BrowserSettings {
                secure_cookies: true,
                public_origin: Some(origin.into()),
            },
        )
        .await;
        let response = app
            .oneshot(req("GET", "/ui/session", None, None, ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(json(response).await["error"]["message"]
            .as_str()
            .unwrap()
            .contains("UI_PUBLIC_ORIGIN"));
        jobs.shutdown().await;
    }
}

#[tokio::test]
async fn malformed_oversized_and_unsupported_browser_requests_have_json_errors() {
    let temp = tempfile::tempdir().unwrap();
    let (app, _, jobs) = app(temp.path(), 120, 4).await;
    let (cookie, csrf) = session(&app).await;
    let response = app
        .clone()
        .oneshot(req("POST", "/ui/downloads", None, Some(&csrf), INPUT))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let _ = json(response).await;
    for (payload, expected) in [
        ("{broken".to_string(), StatusCode::BAD_REQUEST),
        ("x".repeat(17 * 1024), StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let response = app
            .clone()
            .oneshot(req(
                "POST",
                "/ui/downloads",
                Some(&cookie),
                Some(&csrf),
                &payload,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        assert!(json(response).await["error"]["message"].is_string());
    }
    let response = app
        .clone()
        .oneshot(req(
            "OPTIONS",
            "/ui/downloads",
            Some(&cookie),
            Some(&csrf),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert!(response
        .headers()
        .get("access-control-allow-origin")
        .is_none());
    assert!(json(response).await["error"]["message"].is_string());
    let id = submit(&app, &cookie, &csrf).await;
    let response = app
        .clone()
        .oneshot(req(
            "DELETE",
            &format!("/ui/downloads/{id}"),
            Some(&cookie),
            None,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let _ = json(response).await;
    jobs.shutdown().await;
}

#[tokio::test]
async fn removed_jobs_release_ownership_capacity_for_concurrent_submissions() {
    let temp = tempfile::tempdir().unwrap();
    let (app, _, jobs) = app(temp.path(), 120, 8).await;
    let (cookie, csrf) = session(&app).await;
    // Job retention can remove records independently of the browser DELETE
    // route. Fill the ownership table with those now-absent records first.
    for _ in 0..8 {
        let id = submit(&app, &cookie, &csrf).await;
        complete(&jobs, &id).await;
        jobs.cancel(Uuid::parse_str(&id).unwrap()).await.unwrap();
    }
    let requests = (0..4).map(|_| {
        app.clone().oneshot(req(
            "POST",
            "/ui/downloads",
            Some(&cookie),
            Some(&csrf),
            INPUT,
        ))
    });
    for response in futures_util::future::join_all(requests).await {
        let response = response.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let _ = json(response).await;
    }
    jobs.shutdown().await;
}
