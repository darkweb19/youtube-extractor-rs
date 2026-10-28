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

// Group 3: active states map to the controller progress type while preserving
// byte counts; the client derives the internal stage from the typed status.
#[tokio::test]
async fn queued_and_running_polls_update_progress_from_job_view_bytes() {
    let local_id = Uuid::new_v4();
    let worker_id = Uuid::new_v4();
    let dispatch = worker_test_view(worker_id, JobState::Queued, 0, None, None);
    let queued = worker_test_view(worker_id, JobState::Queued, 12, None, None);
    let running = worker_test_view(worker_id, JobState::Running, 64, Some(100), None);
    let failed = worker_test_view(worker_id, JobState::Failed, 100, Some(100), None);
    let controller = mock(vec![]).await;
    let worker = mock(vec![
        worker_test_reply(StatusCode::ACCEPTED, &dispatch),
        worker_test_reply(StatusCode::OK, &queued),
        worker_test_reply(StatusCode::OK, &running),
        worker_test_reply(StatusCode::OK, &failed),
        Reply::new(StatusCode::NO_CONTENT, ""),
    ])
    .await;
    let (client, _dir) = remote(&controller, &worker).await;
    let request = worker_test_request();
    let cancellation = CancellationToken::new();
    let (progress, mut received) = watch::channel(DownloadProgress::default());
    let task = tokio::spawn(async move {
        client
            .run_attempt(&request, local_id, worker_id, &cancellation, &progress)
            .await
    });

    tokio::time::timeout(Duration::from_secs(2), received.changed())
        .await
        .unwrap()
        .unwrap();
    let queued_progress = received.borrow_and_update().clone();
    assert_eq!(queued_progress.stage, "queued");
    assert_eq!(queued_progress.downloaded_bytes, 12);
    assert_eq!(queued_progress.total_bytes, None);

    tokio::time::timeout(Duration::from_secs(5), received.changed())
        .await
        .unwrap()
        .unwrap();
    let running_progress = received.borrow_and_update().clone();
    assert_eq!(running_progress.stage, "running");
    assert_eq!(running_progress.downloaded_bytes, 64);
    assert_eq!(running_progress.total_bytes, Some(100));

    let error = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::DownloadInterrupted);
    let terminal_progress = received.borrow().clone();
    assert_eq!(terminal_progress.stage, "failed");
    assert_eq!(terminal_progress.downloaded_bytes, 100);
}

// Group 4: completed output is transferred and published; failed and cancelled
// worker states become safe local errors and each terminal path requests cleanup.
fn worker_test_output(filename: &str, size_bytes: u64) -> OutputInfo {
    OutputInfo {
        filename: filename.into(),
        mime_type: "audio/mpeg".into(),
        size_bytes,
    }
}

#[tokio::test]
async fn terminal_job_states_transfer_completed_output_and_report_failures() {
    let local_id = Uuid::new_v4();
    let worker_id = Uuid::new_v4();
    let queued = worker_test_view(worker_id, JobState::Queued, 0, None, None);
    let completed = worker_test_view(
        worker_id,
        JobState::Completed,
        7,
        Some(7),
        Some(worker_test_output("result.mp3", 7)),
    );
    let controller = mock(vec![]).await;
    let worker = mock(vec![
        worker_test_reply(StatusCode::ACCEPTED, &queued),
        worker_test_reply(StatusCode::OK, &completed),
        Reply::new(StatusCode::OK, "payload"),
        Reply::new(StatusCode::NO_CONTENT, ""),
    ])
    .await;
    let (client, _dir) = remote(&controller, &worker).await;
    let (progress, received) = watch::channel(DownloadProgress::default());
    let output = client
        .run_attempt(
            &worker_test_request(),
            local_id,
            worker_id,
            &CancellationToken::new(),
            &progress,
        )
        .await
        .unwrap();
    assert_eq!(output.size_bytes, 7);
    assert_eq!(output.filename, format!("{local_id}.mp3"));
    assert_eq!(tokio::fs::read(&output.path).await.unwrap(), b"payload");
    let completed_progress = received.borrow().clone();
    assert_eq!(completed_progress.stage, "completed");
    assert_eq!(completed_progress.downloaded_bytes, 7);
    assert_eq!(completed_progress.total_bytes, Some(7));
    assert_eq!(
        worker.requests.lock().unwrap()[2].path,
        format!("/api/v1/downloads/{worker_id}/file")
    );
    assert_eq!(worker.requests.lock().unwrap()[3].method, Method::DELETE);

    for (state, expected_code) in [
        (JobState::Failed, ErrorCode::DownloadInterrupted),
        (JobState::Cancelled, ErrorCode::Cancelled),
    ] {
        let local_id = Uuid::new_v4();
        let worker_id = Uuid::new_v4();
        let dispatch = worker_test_view(worker_id, JobState::Queued, 0, None, None);
        let terminal = worker_test_view(worker_id, state, 3, Some(10), None);
        let controller = mock(vec![]).await;
        let worker = mock(vec![
            worker_test_reply(StatusCode::ACCEPTED, &dispatch),
            worker_test_reply(StatusCode::OK, &terminal),
            Reply::new(StatusCode::NO_CONTENT, ""),
        ])
        .await;
        let (client, _dir) = remote(&controller, &worker).await;
        let (progress, received) = watch::channel(DownloadProgress::default());
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

        assert_eq!(error.code, expected_code);
        let terminal_progress = received.borrow().clone();
        assert_eq!(
            terminal_progress.stage,
            if state == JobState::Failed {
                "failed"
            } else {
                "cancelled"
            }
        );
        assert_eq!(terminal_progress.downloaded_bytes, 3);
        assert_eq!(terminal_progress.total_bytes, Some(10));
        let requests = worker.requests.lock().unwrap();
        assert_eq!(requests.last().unwrap().method, Method::DELETE);
        assert!(!requests
            .iter()
            .any(|request| request.path.ends_with("/file")));
    }
}

// Group 5: reject invalid and mismatched identifiers at dispatch and poll
// boundaries so a response cannot redirect work to another job.
#[tokio::test]
async fn dispatch_and_poll_reject_malformed_or_mismatched_job_ids() {
    for response_id in [Uuid::nil().to_string(), "not-a-uuid".into()] {
        let local_id = Uuid::new_v4();
        let worker_id = Uuid::new_v4();
        let mut response = worker_test_view(worker_id, JobState::Queued, 0, None, None);
        response.job_id = response_id;
        let controller = mock(vec![]).await;
        let worker = mock(vec![worker_test_reply(StatusCode::ACCEPTED, &response)]).await;
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
        assert_eq!(error.code, ErrorCode::Network);
        assert!(worker
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.method == Method::POST));
    }

    for response_id in [Uuid::nil().to_string(), "not-a-uuid".into()] {
        let requested_id = Uuid::new_v4();
        let mut response = worker_test_view(requested_id, JobState::Running, 3, Some(10), None);
        response.job_id = response_id;
        let controller = mock(vec![]).await;
        let worker = mock(vec![worker_test_reply(StatusCode::OK, &response)]).await;
        let (client, _dir) = remote(&controller, &worker).await;
        let error = client.worker_job(requested_id).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::Network);
        assert!(
            error.message.contains("different job identifier")
                || error.message.contains("Invalid GCP job status")
        );
    }
}
