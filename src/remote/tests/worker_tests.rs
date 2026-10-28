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

// Group 6: malformed JSON and invalid model field types fail with fixed errors
// that do not expose raw worker response contents.
#[tokio::test]
async fn malformed_and_wrong_type_worker_payloads_fail_safely() {
    let id = Uuid::new_v4();
    let model_value = || {
        serde_json::to_value(worker_test_view(
            id,
            JobState::Running,
            4,
            Some(10),
            Some(worker_test_output("result.mp3", 7)),
        ))
        .unwrap()
    };

    let mut wrong_status = model_value();
    wrong_status["status"] = json!("pending-secret");
    let mut wrong_progress = model_value();
    wrong_progress["progress"]["downloaded_bytes"] = json!("four-secret");
    let mut wrong_total = model_value();
    wrong_total["progress"]["total_bytes"] = json!("ten-secret");
    let mut wrong_percent = model_value();
    wrong_percent["progress"]["percent"] = json!("forty-secret");
    let mut wrong_output_size = model_value();
    wrong_output_size["output"]["size_bytes"] = json!("seven-secret");
    let mut missing_downloaded_bytes = model_value();
    missing_downloaded_bytes["progress"]
        .as_object_mut()
        .unwrap()
        .remove("downloaded_bytes");
    let mut missing_status = model_value();
    missing_status.as_object_mut().unwrap().remove("status");
    let mut numeric_job_id = model_value();
    numeric_job_id["job_id"] = json!(12345);

    let invalid_bodies = vec![
        "malformed-secret {".to_string(),
        serde_json::to_string(&wrong_status).unwrap(),
        serde_json::to_string(&wrong_progress).unwrap(),
        serde_json::to_string(&wrong_total).unwrap(),
        serde_json::to_string(&wrong_percent).unwrap(),
        serde_json::to_string(&wrong_output_size).unwrap(),
        serde_json::to_string(&missing_downloaded_bytes).unwrap(),
        serde_json::to_string(&missing_status).unwrap(),
        serde_json::to_string(&numeric_job_id).unwrap(),
    ];
    for body in invalid_bodies.iter() {
        let controller = mock(vec![]).await;
        let worker = mock(vec![Reply::new(StatusCode::OK, body.clone())]).await;
        let (client, _dir) = remote(&controller, &worker).await;
        let error = client.worker_job(id).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::Network);
        assert!(!error.message.contains("secret"));
    }

    for body in invalid_bodies {
        let local_id = Uuid::new_v4();
        // Keep the UUID valid so this test proves schema rejection, rather
        // than succeeding only because the response identifies another job.
        let worker_id = id;
        let controller = mock(vec![]).await;
        let worker = mock(vec![Reply::new(StatusCode::ACCEPTED, body)]).await;
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
        assert_eq!(error.message, "Invalid GCP dispatch reply");
        assert!(!error.message.contains("secret"));
        assert_eq!(worker.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn worker_decode_diagnostics_keep_untrusted_payloads_out_of_logs() {
    use tracing::instrument::WithSubscriber;

    let id = Uuid::new_v4();
    let mut invalid: Value =
        serde_json::to_value(worker_test_view(id, JobState::Running, 4, Some(10), None)).unwrap();
    invalid["status"] = json!("pending-secret-marker");
    invalid["download_url"] = json!("https://sensitive.example/private?token=secret-marker");
    let body = serde_json::to_string(&invalid).unwrap();

    let controller = mock(vec![]).await;
    let worker = mock(vec![Reply::new(StatusCode::OK, body.clone())]).await;
    let (client, _dir) = remote(&controller, &worker).await;
    let logs = LogCapture::default();
    let subscriber = tracing::Dispatch::new(log_subscriber(logs.clone()));
    let operation = tracing::dispatcher::with_default(&subscriber, || client.worker_job(id));
    assert!(operation.with_subscriber(subscriber).await.is_err());
    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(!text.contains("pending-secret-marker"));
    assert!(!text.contains("sensitive.example"));
    assert!(!text.contains("secret-marker"));
    let events: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let rejection = events
        .iter()
        .find(|event| event["fields"]["event"] == "worker_job_poll_invalid")
        .unwrap();
    assert_eq!(rejection["fields"]["reason"], "invalid_response_body");

    let controller = mock(vec![]).await;
    let worker = mock(vec![Reply::new(StatusCode::ACCEPTED, body)]).await;
    let (client, _dir) = remote(&controller, &worker).await;
    let logs = LogCapture::default();
    let subscriber = tracing::Dispatch::new(log_subscriber(logs.clone()));
    let request = worker_test_request();
    let cancellation = CancellationToken::new();
    let (progress, _) = watch::channel(DownloadProgress::default());
    let operation = tracing::dispatcher::with_default(&subscriber, || {
        client.run_attempt(&request, id, id, &cancellation, &progress)
    });
    assert!(operation.with_subscriber(subscriber).await.is_err());
    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(!text.contains("pending-secret-marker"));
    assert!(!text.contains("sensitive.example"));
    let events: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let rejection = events
        .iter()
        .find(|event| event["fields"]["event"] == "worker_dispatch_invalid")
        .unwrap();
    assert_eq!(rejection["fields"]["reason"], "invalid_response_body");
}

async fn worker_with_truncated_poll(body: String) -> Mock {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let requests = Arc::new(StdMutex::new(Vec::new()));
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request_bytes = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = stream.read(&mut chunk).await.unwrap();
            if read == 0 {
                break;
            }
            request_bytes.extend_from_slice(&chunk[..read]);
            if let Some(end) = request_bytes
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
            {
                request_bytes.truncate(end + 4);
                break;
            }
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len() + 20,
            body
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        let _ = stream.shutdown().await;
    });
    Mock {
        origin,
        requests,
        task,
    }
}

#[tokio::test]
async fn truncated_worker_body_logs_fixed_reason_without_body_content() {
    use tracing::instrument::WithSubscriber;

    let id = Uuid::new_v4();
    let worker = worker_with_truncated_poll(format!(
        r#"{{"job_id":"{id}","private":"body-secret-marker"}}"#
    ))
    .await;
    let controller = mock(vec![]).await;
    let (client, _dir) = remote(&controller, &worker).await;
    let logs = LogCapture::default();
    let subscriber = tracing::Dispatch::new(log_subscriber(logs.clone()));
    let operation = tracing::dispatcher::with_default(&subscriber, || client.worker_job(id));
    let error = operation.with_subscriber(subscriber).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Network);
    assert!(!error.message.contains("body-secret-marker"));
    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(!text.contains("body-secret-marker"));
    let events: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let rejection = events
        .iter()
        .find(|event| event["fields"]["event"] == "worker_job_poll_invalid")
        .unwrap();
    assert_eq!(rejection["fields"]["reason"], "invalid_response_body");
}

// Group 7: keep uncertain-delivery retry, queue-full handling, cancellation
// cleanup, and output validation from crossing the worker trust boundary.
#[tokio::test]
async fn uncertain_dispatch_retries_with_the_same_idempotency_key() {
    let local_id = Uuid::new_v4();
    let worker_id = Uuid::new_v4();
    let accepted = worker_test_view(worker_id, JobState::Queued, 0, None, None);
    let failed = worker_test_view(worker_id, JobState::Failed, 0, None, None);
    let mut timed_out = worker_test_reply(StatusCode::ACCEPTED, &accepted);
    timed_out.delay = Duration::from_secs(2);
    let controller = mock(vec![]).await;
    let worker = mock(vec![
        timed_out,
        worker_test_reply(StatusCode::ACCEPTED, &accepted),
        worker_test_reply(StatusCode::OK, &failed),
        Reply::new(StatusCode::NO_CONTENT, ""),
    ])
    .await;
    let (client, _dir) = remote(&controller, &worker).await;
    let request = worker_test_request();
    let cancellation = CancellationToken::new();
    let (progress, _) = watch::channel(DownloadProgress::default());

    let error = tokio::time::timeout(
        Duration::from_secs(10),
        client.run_attempt(&request, local_id, worker_id, &cancellation, &progress),
    )
    .await
    .unwrap()
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::DownloadInterrupted);
    let requests = worker.requests.lock().unwrap();
    let dispatches: Vec<_> = requests
        .iter()
        .filter(|request| request.method == Method::POST)
        .collect();
    assert_eq!(dispatches.len(), 2);
    assert_eq!(
        dispatches[0].headers["X-Idempotency-Key"],
        worker_id.to_string()
    );
    assert_eq!(
        dispatches[1].headers["X-Idempotency-Key"],
        worker_id.to_string()
    );
}

#[tokio::test]
async fn cancellation_and_invalid_outputs_cleanup_without_streaming_bad_files() {
    let local_id = Uuid::new_v4();
    let worker_id = Uuid::new_v4();
    let accepted = worker_test_view(worker_id, JobState::Queued, 0, None, None);
    let controller = mock(vec![]).await;
    let mut delayed_poll = worker_test_reply(StatusCode::OK, &accepted);
    delayed_poll.delay = Duration::from_secs(5);
    let worker = mock(vec![
        worker_test_reply(StatusCode::ACCEPTED, &accepted),
        delayed_poll,
        Reply::new(StatusCode::NO_CONTENT, ""),
    ])
    .await;
    let (client, _dir) = remote(&controller, &worker).await;
    let cancellation = CancellationToken::new();
    let run_remote = client.clone();
    let run_cancellation = cancellation.clone();
    let request = worker_test_request();
    let (progress, _) = watch::channel(DownloadProgress::default());
    let attempt = tokio::spawn(async move {
        run_remote
            .run_attempt(&request, local_id, worker_id, &run_cancellation, &progress)
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let poll_started = worker
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.method == Method::GET);
            if poll_started {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    cancellation.cancel();
    let error = tokio::time::timeout(Duration::from_secs(2), attempt)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Cancelled);
    {
        let requests = worker.requests.lock().unwrap();
        assert_eq!(requests[0].method, Method::POST);
        assert_eq!(requests[1].method, Method::GET);
        assert_eq!(requests[2].method, Method::DELETE);
    }

    let local_id = Uuid::new_v4();
    let worker_id = Uuid::new_v4();
    let dispatch = worker_test_view(worker_id, JobState::Queued, 0, None, None);
    let controller = mock(vec![]).await;
    let worker = mock(vec![
        worker_test_reply(StatusCode::ACCEPTED, &dispatch),
        Reply::new(StatusCode::INTERNAL_SERVER_ERROR, "poll-body-secret"),
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
    assert_eq!(error.code, ErrorCode::Network);
    assert!(!error.message.contains("poll-body-secret"));
    {
        let requests = worker.requests.lock().unwrap();
        assert_eq!(requests[0].method, Method::POST);
        assert_eq!(requests[1].method, Method::GET);
        assert_eq!(requests[2].method, Method::DELETE);
    }

    for (filename, size_bytes, expected_code) in [
        ("payload.exe", 10, ErrorCode::Network),
        ("empty.mp3", 0, ErrorCode::DownloadTooLarge),
        ("limit.mp3", 1024, ErrorCode::DownloadTooLarge),
    ] {
        let local_id = Uuid::new_v4();
        let worker_id = Uuid::new_v4();
        let dispatch = worker_test_view(worker_id, JobState::Queued, 0, None, None);
        let completed = worker_test_view(
            worker_id,
            JobState::Completed,
            size_bytes,
            Some(size_bytes),
            Some(worker_test_output(filename, size_bytes)),
        );
        let controller = mock(vec![]).await;
        let worker = mock(vec![
            worker_test_reply(StatusCode::ACCEPTED, &dispatch),
            worker_test_reply(StatusCode::OK, &completed),
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
        assert_eq!(error.code, expected_code);
        assert!(worker
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| !request.path.ends_with("/file")));
    }

    let local_id = Uuid::new_v4();
    let worker_id = Uuid::new_v4();
    let controller = mock(vec![]).await;
    let worker = mock(vec![Reply::new(
        StatusCode::TOO_MANY_REQUESTS,
        "queue-secret",
    )])
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
    assert_eq!(error.code, ErrorCode::QueueFull);
    assert!(!error.message.contains("queue-secret"));
    assert_eq!(worker.requests.lock().unwrap().len(), 1);
}
