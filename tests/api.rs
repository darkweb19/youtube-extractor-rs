use async_trait::async_trait;
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use uuid::Uuid;
use youtube_extractor::{
    api::{router, ApiState},
    config::Config,
    download::DownloadProgress,
    error::{AppError, ErrorCode, Result},
    extractor::Extractor,
    jobs::{JobExecutor, JobManager, JobSettings},
    model::{DownloadRequest, VideoInfo},
    storage::StoredOutput,
};
const TOKEN: &str = "test-token-0123456789-test-token-0123456789";
struct Restricted;
#[async_trait]
impl Extractor for Restricted {
    async fn video_info(&self, _: &str) -> Result<VideoInfo> {
        Err(AppError::new(
            ErrorCode::AccessRestricted,
            "YouTube requires verification",
        ))
    }
}
struct NoWork;
#[async_trait]
impl JobExecutor for NoWork {
    async fn run(
        &self,
        _: &DownloadRequest,
        _: Uuid,
        _: CancellationToken,
        _: watch::Sender<DownloadProgress>,
    ) -> Result<StoredOutput> {
        Err(AppError::new(
            ErrorCode::UnsupportedFormat,
            "Controlled test",
        ))
    }
    async fn cleanup(&self, _: Uuid) -> Result<()> {
        Ok(())
    }
}
fn config(root: &Path, rate: u32) -> Config {
    Config {
        bind_addr: "0.0.0.0:8080".parse().unwrap(),
        api_token: Some(TOKEN.into()),
        data_dir: root.into(),
        workers: 1,
        queue_capacity: 1,
        max_download_bytes: 100,
        max_storage_bytes: 1000,
        job_ttl: Duration::from_secs(60),
        max_jobs: 5,
        connect_timeout: Duration::from_secs(1),
        request_timeout: Duration::from_secs(1),
        download_timeout: Duration::from_secs(10),
        ffmpeg_path: "ffmpeg".into(),
        requests_per_minute: rate,
        max_concurrent_requests: 1,
    }
}
async fn app(root: &Path, rate: u32) -> (Router, Arc<JobManager>) {
    let config = Arc::new(config(root, rate));
    let jobs = JobManager::new(
        JobSettings {
            data_dir: root.into(),
            workers: 1,
            queue_capacity: 1,
            max_jobs: 5,
            max_download_bytes: 100,
            max_storage_bytes: 1000,
            ttl: config.job_ttl,
            timeout: config.download_timeout,
        },
        Arc::new(NoWork),
    )
    .await
    .unwrap();
    (
        router(ApiState::new(config, Arc::new(Restricted), jobs.clone())),
        jobs,
    )
}
fn request(method: &str, path: &str, body: &str, authorized: bool) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if authorized {
        builder = builder.header("authorization", format!("Bearer {TOKEN}"));
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

fn frame_request(method: &str, path: &str, body: &str, cookie: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("x-frame-client", "1")
        .header("sec-fetch-site", "same-origin");
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

async fn login(app: &Router) -> String {
    let mut request = frame_request("POST", "/api/v1/session", "", None);
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_owned();
    for flag in [
        "__Secure-frame_session=",
        "Max-Age=31536000",
        "Path=/api",
        "Secure",
        "HttpOnly",
        "SameSite=Strict",
    ] {
        assert!(cookie.contains(flag), "missing {flag}");
    }
    assert!(!cookie.contains("Domain="));
    assert!(!cookie.contains(TOKEN));
    assert!(to_bytes(response.into_body(), 65536)
        .await
        .unwrap()
        .is_empty());
    cookie.split(';').next().unwrap().to_owned()
}

fn signed_cookie(expiry: u64) -> String {
    use ring::hmac;
    use std::fmt::Write;
    let payload = format!("v1.{expiry}.{}", Uuid::new_v4().simple());
    let key = hmac::Key::new(hmac::HMAC_SHA256, TOKEN.as_bytes());
    let tag = hmac::sign(
        &key,
        format!("frame-browser-session-v1\0{payload}").as_bytes(),
    );
    let mut hex = String::new();
    for byte in tag.as_ref() {
        write!(&mut hex, "{byte:02x}").unwrap();
    }
    format!("__Secure-frame_session={payload}.{hex}")
}

#[tokio::test]
async fn browser_session_survives_state_recreation_and_authenticates_all_routes() {
    let temp = tempfile::tempdir().unwrap();
    let (app, jobs) = app(temp.path(), 120).await;
    let cookie = login(&app).await;
    let restored = router(ApiState::new(
        Arc::new(config(temp.path(), 120)),
        Arc::new(Restricted),
        jobs.clone(),
    ));
    let response = restored
        .clone()
        .oneshot(frame_request("GET", "/api/v1/session", "", Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(!text.contains(TOKEN));
    assert!(!text.contains(&cookie));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
        serde_json::json!({"authenticated": true, "saved": true})
    );

    let response = restored
        .clone()
        .oneshot(request("GET", "/api/v1/session", "", true))
        .await
        .unwrap();
    let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
        serde_json::json!({"authenticated": true, "saved": false})
    );

    for (method, path, expected) in [
        (
            "POST",
            "/api/v1/videos/info".to_string(),
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            "/api/v1/downloads".to_string(),
            StatusCode::BAD_REQUEST,
        ),
        (
            "GET",
            "/api/v1/internal/activity".to_string(),
            StatusCode::OK,
        ),
        (
            "GET",
            format!("/api/v1/downloads/{}", Uuid::new_v4()),
            StatusCode::NOT_FOUND,
        ),
        (
            "GET",
            format!("/api/v1/downloads/{}/file", Uuid::new_v4()),
            StatusCode::NOT_FOUND,
        ),
        (
            "DELETE",
            format!("/api/v1/downloads/{}", Uuid::new_v4()),
            StatusCode::NO_CONTENT,
        ),
    ] {
        let response = restored
            .clone()
            .oneshot(frame_request(method, &path, "{}", Some(&cookie)))
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{method} {path}");
        let _ = to_bytes(response.into_body(), 65536).await.unwrap();
    }
    jobs.shutdown().await;
}

#[tokio::test]
async fn browser_session_rejects_expiry_tampering_duplicates_and_token_rotation() {
    let temp = tempfile::tempdir().unwrap();
    let (app, jobs) = app(temp.path(), 120).await;
    let cookie = login(&app).await;
    let mut tampered = cookie.clone().into_bytes();
    let last = tampered.last_mut().unwrap();
    *last = if *last == b'0' { b'1' } else { b'0' };
    let tampered = String::from_utf8(tampered).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    for invalid in [
        tampered,
        signed_cookie(now - 1),
        signed_cookie(now + 31536060),
        format!("{cookie}; {cookie}"),
        format!("__Secure-frame_session={}", "a".repeat(121)),
    ] {
        let response = app
            .clone()
            .oneshot(frame_request("GET", "/api/v1/session", "", Some(&invalid)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let mut duplicate = frame_request("GET", "/api/v1/session", "", Some(&cookie));
    duplicate
        .headers_mut()
        .append("cookie", cookie.parse().unwrap());
    assert_eq!(
        app.clone().oneshot(duplicate).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );

    let mut rotated = config(temp.path(), 120);
    rotated.api_token = Some("rotated-token-0123456789-rotated-token-0123456789".into());
    let rotated = router(ApiState::new(
        Arc::new(rotated),
        Arc::new(Restricted),
        jobs.clone(),
    ));
    let response = rotated
        .oneshot(frame_request("GET", "/api/v1/session", "", Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    jobs.shutdown().await;
}

#[tokio::test]
async fn session_writes_and_cookie_auth_require_a_same_origin_browser_request() {
    let temp = tempfile::tempdir().unwrap();
    let (app, jobs) = app(temp.path(), 120).await;
    let cookie = login(&app).await;

    let no_header = Request::builder()
        .uri("/api/v1/session")
        .header("cookie", &cookie)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(no_header).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    for site in ["cross-site", "same-site"] {
        let mut cross_site = frame_request("GET", "/api/v1/session", "", Some(&cookie));
        cross_site
            .headers_mut()
            .insert("sec-fetch-site", site.parse().unwrap());
        assert_eq!(
            app.clone().oneshot(cross_site).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );

        let mut logout = frame_request("DELETE", "/api/v1/session", "", None);
        logout
            .headers_mut()
            .insert("sec-fetch-site", site.parse().unwrap());
        assert_eq!(
            app.clone().oneshot(logout).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );

        let mut login = frame_request("POST", "/api/v1/session", "", None);
        login
            .headers_mut()
            .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
        login
            .headers_mut()
            .insert("sec-fetch-site", site.parse().unwrap());
        assert_eq!(
            app.clone().oneshot(login).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }

    let mut bad_bearer = frame_request("GET", "/api/v1/session", "", Some(&cookie));
    bad_bearer
        .headers_mut()
        .insert("authorization", "Bearer incorrect".parse().unwrap());
    assert_eq!(
        app.clone().oneshot(bad_bearer).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let cookie_only_post = frame_request("POST", "/api/v1/session", "", Some(&cookie));
    assert_eq!(
        app.clone()
            .oneshot(cookie_only_post)
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let mut login_without_client = request("POST", "/api/v1/session", "", true);
    login_without_client
        .headers_mut()
        .insert("sec-fetch-site", "same-origin".parse().unwrap());
    assert_eq!(
        app.clone()
            .oneshot(login_without_client)
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.clone()
            .oneshot(request("DELETE", "/api/v1/session", "", false))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    // Existing bearer clients remain independent of browser-only CSRF checks.
    let mut bearer = request("GET", "/api/v1/internal/activity", "", true);
    bearer
        .headers_mut()
        .insert("sec-fetch-site", "cross-site".parse().unwrap());
    let response = app.clone().oneshot(bearer).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let _ = to_bytes(response.into_body(), 65536).await.unwrap();

    let logout = app
        .clone()
        .oneshot(frame_request("DELETE", "/api/v1/session", "", None))
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    let clearing = logout.headers()["set-cookie"].to_str().unwrap();
    assert_eq!(
        clearing,
        "__Secure-frame_session=; Max-Age=0; Path=/api; Secure; HttpOnly; SameSite=Strict"
    );
    assert_eq!(logout.headers()["cache-control"], "no-store");
    jobs.shutdown().await;
}

#[tokio::test]
async fn no_token_development_mode_does_not_mint_a_cookie() {
    let temp = tempfile::tempdir().unwrap();
    let (_, jobs) = app(temp.path(), 120).await;
    let mut config = config(temp.path(), 120);
    config.api_token = None;
    let app = router(ApiState::new(
        Arc::new(config),
        Arc::new(Restricted),
        jobs.clone(),
    ));
    let status = app
        .clone()
        .oneshot(frame_request("GET", "/api/v1/session", "", None))
        .await
        .unwrap();
    let body = to_bytes(status.into_body(), 65536).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({"authenticated": true, "saved": false})
    );
    let response = app
        .clone()
        .oneshot(frame_request("POST", "/api/v1/session", "", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(!response.headers().contains_key("set-cookie"));
    let _ = to_bytes(response.into_body(), 65536).await.unwrap();
    jobs.shutdown().await;
}
#[tokio::test]
async fn frontend_assets_are_public_but_api_credentials_remain_protected() {
    let temp = tempfile::tempdir().unwrap();
    let (app, jobs) = app(temp.path(), 120).await;
    for (path, content_type, marker) in [
        ("/", "text/html", "id=\"confirm-dialog\""),
        ("/app.css", "text/css", ".confirm-dialog"),
        ("/app.js", "text/javascript", "/api/v1/downloads"),
    ] {
        let response = app
            .clone()
            .oneshot(request("GET", path, "", false))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert!(response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with(content_type));
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(response.headers()["cache-control"], "no-store");
        if path == "/" {
            assert_eq!(
                response.headers()["content-security-policy"],
                "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; media-src blob:; frame-src https://www.youtube-nocookie.com; base-uri 'none'; form-action 'self'; frame-ancestors https://sujanshrestha.ca; object-src 'none'"
            );
            assert_eq!(
                response.headers()["referrer-policy"],
                "strict-origin-when-cross-origin"
            );
            assert!(!response.headers().contains_key("x-frame-options"));
        }
        let body = to_bytes(response.into_body(), 65536).await.unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text.contains(marker), "{path}");
        assert!(
            !text.contains(TOKEN),
            "{path} must not leak the bearer token"
        );
    }
    // Serving a public HTML shell must not make the API anonymous.
    let response = app
        .oneshot(request("POST", "/api/v1/downloads", "{}", false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    jobs.shutdown().await;
}

#[tokio::test]
async fn every_v1_route_including_files_requires_authentication() {
    let temp = tempfile::tempdir().unwrap();
    let (app, jobs) = app(temp.path(), 120).await;
    for (method, path) in [
        ("POST", "/api/v1/videos/info".to_string()),
        ("POST", "/api/v1/downloads".to_string()),
        ("GET", format!("/api/v1/downloads/{}", Uuid::new_v4())),
        ("DELETE", format!("/api/v1/downloads/{}", Uuid::new_v4())),
        ("GET", format!("/api/v1/downloads/{}/file", Uuid::new_v4())),
    ] {
        let response = app
            .clone()
            .oneshot(request(method, &path, "{}", false))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()["www-authenticate"], "Bearer");
    }
    jobs.shutdown().await;
}
#[tokio::test]
async fn errors_are_json_classified_and_request_validation_is_strict() {
    let temp = tempfile::tempdir().unwrap();
    let (app, jobs) = app(temp.path(), 120).await;
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/videos/info",
            r#"{"url":"https://youtu.be/aqz-KE-bpKQ"}"#,
            true,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let json: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(json["error"]["code"], "ACCESS_RESTRICTED");
    for input in [
        r#"{"url":"http://127.0.0.1/","type":"video","format":"mp4"}"#,
        r#"{"url":"https://youtu.be/aqz-KE-bpKQ","type":"audio","format":"mp4"}"#,
        r#"{"url":"https://youtu.be/aqz-KE-bpKQ","type":"video","format":"mp4","path":"/etc/passwd"}"#,
    ] {
        let response = app
            .clone()
            .oneshot(request("POST", "/api/v1/downloads", input, true))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let _ = to_bytes(response.into_body(), 65536).await.unwrap();
    }
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/downloads/not-a-uuid/file",
            "",
            true,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let _ = to_bytes(response.into_body(), 65536).await.unwrap();
    let response = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/downloads/{}", Uuid::new_v4()),
            "",
            true,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let _ = to_bytes(response.into_body(), 65536).await.unwrap();
    let oversized = format!("{{\"url\":\"{}\"}}", "x".repeat(17 * 1024));
    let response = app
        .clone()
        .oneshot(request("POST", "/api/v1/videos/info", &oversized, true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let _ = to_bytes(response.into_body(), 65536).await.unwrap();
    jobs.shutdown().await;
}
#[tokio::test]
async fn authenticated_rate_limit_is_enforced() {
    let temp = tempfile::tempdir().unwrap();
    let (app, jobs) = app(temp.path(), 3).await;
    let path = format!("/api/v1/downloads/{}", Uuid::new_v4());
    for _ in 0..3 {
        let response = app
            .clone()
            .oneshot(request("GET", &path, "", true))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let _ = to_bytes(response.into_body(), 65536).await.unwrap();
    }
    let response = app
        .clone()
        .oneshot(request("GET", &path, "", true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()["retry-after"], "60");
    jobs.shutdown().await;
}

struct FileExecutor {
    root: std::path::PathBuf,
}
#[async_trait]
impl JobExecutor for FileExecutor {
    async fn run(
        &self,
        _: &DownloadRequest,
        id: Uuid,
        _: CancellationToken,
        _: watch::Sender<DownloadProgress>,
    ) -> Result<StoredOutput> {
        let dir = self.root.join(id.to_string());
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("controlled.mp4");
        tokio::fs::write(&path, vec![0_u8; 256 * 1024])
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
        let _ = tokio::fs::remove_dir_all(self.root.join(id.to_string())).await;
        Ok(())
    }
}

#[tokio::test]
async fn browser_session_can_fetch_and_delete_a_completed_file() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = config(temp.path(), 120);
    config.max_download_bytes = 512 * 1024;
    config.max_storage_bytes = 1024 * 1024;
    let config = Arc::new(config);
    let jobs = JobManager::new(
        JobSettings {
            data_dir: temp.path().into(),
            workers: 1,
            queue_capacity: 1,
            max_jobs: 5,
            max_download_bytes: config.max_download_bytes,
            max_storage_bytes: config.max_storage_bytes,
            ttl: config.job_ttl,
            timeout: config.download_timeout,
        },
        Arc::new(FileExecutor {
            root: temp.path().into(),
        }),
    )
    .await
    .unwrap();
    let request: DownloadRequest = serde_json::from_str(
        r#"{"url":"https://youtu.be/aqz-KE-bpKQ","type":"video","format":"mp4"}"#,
    )
    .unwrap();
    let job = jobs.submit(request).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while jobs.get(job.job_id).await.unwrap().status
            != youtube_extractor::jobs::JobStatus::Completed
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let app = router(ApiState::new(config, Arc::new(Restricted), jobs.clone()));
    let cookie = login(&app).await;
    let file = app
        .clone()
        .oneshot(frame_request(
            "GET",
            &format!("/api/v1/downloads/{}/file", job.job_id),
            "",
            Some(&cookie),
        ))
        .await
        .unwrap();
    assert_eq!(file.status(), StatusCode::OK);
    assert_eq!(file.headers()["content-type"], "video/mp4");
    assert_eq!(
        to_bytes(file.into_body(), 512 * 1024).await.unwrap().len(),
        256 * 1024
    );
    let deleted = app
        .clone()
        .oneshot(frame_request(
            "DELETE",
            &format!("/api/v1/downloads/{}", job.job_id),
            "",
            Some(&cookie),
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    let _ = to_bytes(deleted.into_body(), 65536).await.unwrap();
    let missing = app
        .oneshot(frame_request(
            "GET",
            &format!("/api/v1/downloads/{}/file", job.job_id),
            "",
            Some(&cookie),
        ))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    jobs.shutdown().await;
}

#[tokio::test]
async fn stalled_file_receiver_retains_concurrency_only_until_independent_deadline() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = config(temp.path(), 120);
    config.max_download_bytes = 512 * 1024;
    config.max_storage_bytes = 1024 * 1024;
    config.download_timeout = Duration::from_millis(100);
    let config = Arc::new(config);
    let jobs = JobManager::new(
        JobSettings {
            data_dir: temp.path().into(),
            workers: 1,
            queue_capacity: 1,
            max_jobs: 5,
            max_download_bytes: config.max_download_bytes,
            max_storage_bytes: config.max_storage_bytes,
            ttl: config.job_ttl,
            timeout: Duration::from_secs(2),
        },
        Arc::new(FileExecutor {
            root: temp.path().into(),
        }),
    )
    .await
    .unwrap();
    let request: DownloadRequest = serde_json::from_str(
        r#"{"url":"https://youtu.be/aqz-KE-bpKQ","type":"video","format":"mp4"}"#,
    )
    .unwrap();
    let job = jobs.submit(request).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while jobs.get(job.job_id).await.unwrap().status
            != youtube_extractor::jobs::JobStatus::Completed
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let app = router(ApiState::new(config, Arc::new(Restricted), jobs.clone()));
    let response = app
        .clone()
        .oneshot(crate::request(
            "GET",
            &format!("/api/v1/downloads/{}/file", job.job_id),
            "",
            true,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let blocked = app
        .clone()
        .oneshot(crate::request(
            "GET",
            &format!("/api/v1/downloads/{}", job.job_id),
            "",
            true,
        ))
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::SERVICE_UNAVAILABLE);
    drop(blocked);
    // Keep response unpolled: timeout must run independently of downstream body polling.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let available = app
        .clone()
        .oneshot(crate::request(
            "GET",
            &format!("/api/v1/downloads/{}", job.job_id),
            "",
            true,
        ))
        .await
        .unwrap();
    assert_eq!(available.status(), StatusCode::OK);
    let _ = to_bytes(available.into_body(), 65536).await.unwrap();
    drop(response);
    jobs.shutdown().await;
}
