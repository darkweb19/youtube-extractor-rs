// Regression tests for the authenticated GCP worker's public job contract.
// These tests share the parent test module's local HTTP mock.

use super::*;

use crate::model::{JobError, JobProgress};

fn worker_test_view(
    id: Uuid,
    status: JobState,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    output: Option<OutputInfo>,
) -> JobView {
    let now = chrono::Utc::now();
    JobView {
        job_id: id.to_string(),
        status,
        created_at: now,
        updated_at: now,
        progress: JobProgress {
            downloaded_bytes,
            total_bytes,
            percent: total_bytes
                .filter(|total| *total > 0)
                .map(|total| (downloaded_bytes as f64 / total as f64 * 100.0).min(100.0) as f32),
        },
        error: (status == JobState::Failed).then(|| JobError {
            code: ErrorCode::DownloadInterrupted,
            message: "worker extraction failed".into(),
        }),
        output,
        download_url: None,
    }
}

fn worker_test_reply(status: StatusCode, view: &JobView) -> Reply {
    Reply::new(status, serde_json::to_string(view).unwrap())
}

// Group 1: serialize the real public model and confirm poll deserialization
// preserves its timestamps and progress fields without requiring a stage.
#[tokio::test]
async fn worker_poll_deserializes_the_public_job_view_contract() {
    let id = Uuid::new_v4();
    let view = worker_test_view(id, JobState::Running, 64, Some(100), None);
    let wire = serde_json::to_value(&view).unwrap();
    assert_eq!(wire["status"], "running");
    assert_eq!(wire["progress"]["downloaded_bytes"], 64);
    assert_eq!(wire["progress"]["total_bytes"], 100);
    assert_eq!(wire["progress"]["percent"], 64.0);
    assert!(wire["progress"].get("stage").is_none());

    let controller = mock(vec![]).await;
    let worker = mock(vec![worker_test_reply(StatusCode::OK, &view)]).await;
    let (client, _dir) = remote(&controller, &worker).await;
    let decoded = client.worker_job(id).await.unwrap();

    assert_eq!(decoded.job_id, id.to_string());
    assert_eq!(decoded.status, JobState::Running);
    assert_eq!(decoded.created_at, view.created_at);
    assert_eq!(decoded.updated_at, view.updated_at);
    assert_eq!(decoded.progress.downloaded_bytes, 64);
    assert_eq!(decoded.progress.total_bytes, Some(100));
    assert_eq!(decoded.progress.percent, Some(64.0));
}

// Group 2: 202 is an accepted dispatch, with worker credentials and the stable
// idempotency key attached to the request.
fn worker_test_request() -> DownloadRequest {
    use crate::model::{DownloadKind, OutputFormat};

    DownloadRequest {
        url: "https://youtube.com/watch?v=dQw4w9WgXcQ".into(),
        kind: DownloadKind::Audio,
        format: OutputFormat::Mp3,
        quality: None,
        audio_bitrate_kbps: None,
    }
}

#[tokio::test]
async fn dispatch_accepts_http_202_with_worker_auth_and_idempotency_key() {
    let local_id = Uuid::new_v4();
    let worker_id = Uuid::new_v4();
    let accepted = worker_test_view(worker_id, JobState::Queued, 0, None, None);
    let failed = worker_test_view(worker_id, JobState::Failed, 0, None, None);
    let controller = mock(vec![]).await;
    let worker = mock(vec![
        worker_test_reply(StatusCode::ACCEPTED, &accepted),
        worker_test_reply(StatusCode::OK, &failed),
        Reply::new(StatusCode::NO_CONTENT, ""),
    ])
    .await;
    let (client, _dir) = remote(&controller, &worker).await;
    let (progress, _) = watch::channel(DownloadProgress::default());

    let error = client
        .run_attempt(
            &worker_test_request(),
            local_id,
            worker_id,
            &CancellationToken::new(),
            &progress,
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::DownloadInterrupted);
    let requests = worker.requests.lock().unwrap();
    assert_eq!(requests[0].method, Method::POST);
    assert_eq!(requests[0].path, "/api/v1/downloads");
    assert_eq!(
        requests[0].headers["Authorization"],
        format!("Bearer {WORKER_TOKEN}")
    );
    assert_eq!(requests[0].headers["CF-Access-Client-Id"], "test-access-id");
    assert_eq!(
        requests[0].headers["CF-Access-Client-Secret"],
        "test-access-secret"
    );
    assert_eq!(
        requests[0].headers["X-Idempotency-Key"],
        worker_id.to_string()
    );
    assert_eq!(requests[1].path, format!("/api/v1/downloads/{worker_id}"));
    assert_eq!(requests[2].method, Method::DELETE);
}
