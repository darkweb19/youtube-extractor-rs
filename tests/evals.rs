//! Versioned scenario corpus; all upstream metadata comes from a synthetic executable.
use futures_util::FutureExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashSet, path::PathBuf, time::Duration};
use youtube_extractor::{
    error::{AppError, ErrorCode},
    extractor::{normalize_url, Extractor},
    media::Ffmpeg,
    model::DownloadRequest,
    storage::LocalStorage,
    ytdlp::YtDlp,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    source: String,
    scenario: Scenario,
    expect: Expectation,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Scenario {
    Url {
        url: String,
    },
    Request {
        request: Value,
    },
    Metadata {
        url: String,
        stdout: String,
        #[serde(default)]
        exit_code: u8,
    },
}

#[derive(Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
enum Expectation {
    Success { contains: Value },
    Error { code: ErrorCode },
}

fn contains(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Object(actual), Value::Object(expected)) => expected
            .iter()
            .all(|(key, value)| actual.get(key).is_some_and(|item| contains(item, value))),
        (Value::Array(actual), Value::Array(expected)) => {
            actual.len() == expected.len()
                && actual
                    .iter()
                    .zip(expected)
                    .all(|(item, value)| contains(item, value))
        }
        _ => actual == expected,
    }
}

fn grade(result: Result<Value, AppError>, expected: &Expectation) -> Result<(), String> {
    match (result, expected) {
        (Ok(actual), Expectation::Success { contains: expected })
            if contains(&actual, expected) =>
        {
            Ok(())
        }
        (Err(error), Expectation::Error { code }) if error.code == *code => Ok(()),
        (Ok(_), Expectation::Error { .. }) => Err("expected a rejection, received success".into()),
        (Err(error), Expectation::Success { .. }) => {
            Err(format!("expected success, received {:?}", error.code))
        }
        (Err(error), Expectation::Error { code }) => Err(format!(
            "expected error {code:?}, received {:?}",
            error.code
        )),
        _ => Err("public response does not match the expected fields".into()),
    }
}

async fn evaluate(scenario: &Scenario) -> Result<Value, AppError> {
    match scenario {
        Scenario::Url { url } => Ok(json!({"id": normalize_url(url)?})),
        Scenario::Request { request } => {
            let request: DownloadRequest = serde_json::from_value(request.clone())
                .map_err(|_| AppError::invalid("Request schema rejected"))?;
            request.validate()?;
            let id = normalize_url(&request.url)?;
            Ok(json!({"id": id, "request": request}))
        }
        Scenario::Metadata {
            url,
            stdout,
            exit_code,
        } => metadata(url, stdout, *exit_code).await,
    }
}

#[cfg(unix)]
async fn metadata(url: &str, stdout: &str, exit_code: u8) -> Result<Value, AppError> {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().expect("create isolated eval workspace");
    let response = temp.path().join("response.json");
    let arguments = temp.path().join("arguments");
    let executable = temp.path().join("synthetic-yt-dlp");
    std::fs::write(&response, stdout).expect("write synthetic response");
    let quote =
        |path: &std::path::Path| format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\ncat {}\nexit {exit_code}\n",
        quote(&arguments),
        quote(&response)
    );
    std::fs::write(&executable, script).expect("write synthetic executable");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
        .expect("mark synthetic executable executable");
    let storage = LocalStorage::new(temp.path().join("jobs")).await?;
    let worker = YtDlp::new(
        executable,
        storage,
        Ffmpeg::new("ffmpeg".into(), "ffprobe".into(), Duration::from_secs(3)),
        Duration::from_secs(3),
        1024 * 1024,
    );
    let result = worker.video_info(url).await;
    if let Ok(id) = normalize_url(url) {
        let args =
            std::fs::read_to_string(&arguments).expect("synthetic metadata worker was invoked");
        let args: Vec<_> = args.lines().collect();
        assert!(args.contains(&"--ignore-config"));
        assert!(args.contains(&"--dump-single-json"));
        assert!(args.ends_with(&["--", &format!("https://www.youtube.com/watch?v={id}")]));
    } else {
        assert!(
            !arguments.exists(),
            "invalid input spawned the metadata worker"
        );
    }
    let public = match result {
        Ok(info) => serde_json::to_value(info).expect("serialize public metadata"),
        Err(error) => {
            assert!(!error.message.contains("private-signed-sentinel"));
            return Err(error);
        }
    };
    assert!(!public.to_string().contains("private-signed-sentinel"));
    for format in public["formats"].as_array().expect("public formats array") {
        assert!(format.get("url").is_none());
        assert!(format.get("signature_cipher").is_none());
    }
    Ok(public)
}

#[cfg(not(unix))]
async fn metadata(_url: &str, _stdout: &str, _exit_code: u8) -> Result<Value, AppError> {
    panic!("metadata scenario evals require a Unix shell; run on Linux or macOS")
}

#[tokio::test]
async fn evaluate_scenarios() {
    let cases: Vec<Case> = serde_json::from_str(include_str!("../evals/cases.json"))
        .expect("valid eval corpus schema");
    assert!(!cases.is_empty(), "corpus must not be empty");
    let mut seen = HashSet::new();
    for case in &cases {
        assert!(
            !case.id.is_empty() && seen.insert(case.id.clone()),
            "unique nonempty case IDs"
        );
        assert!(!case.source.is_empty(), "cases need provenance and intent");
        if let Expectation::Success { contains } = &case.expect {
            assert!(contains.as_object().is_some_and(|value| !value.is_empty()));
        }
    }
    let selected: Option<Vec<String>> = std::env::var("EXTRACTOR_EVAL_CASES")
        .ok()
        .map(|value| serde_json::from_str(&value).expect("valid selected case IDs"));
    if let Some(selected) = &selected {
        assert!(!selected.is_empty(), "selected case set cannot be empty");
        assert!(
            selected.iter().all(|id| seen.contains(id)),
            "unknown case IDs"
        );
    }
    let mut results = Vec::new();
    let mut failures = Vec::new();
    for case in cases
        .iter()
        .filter(|case| selected.as_ref().is_none_or(|ids| ids.contains(&case.id)))
    {
        let graded = match std::panic::AssertUnwindSafe(evaluate(&case.scenario))
            .catch_unwind()
            .await
        {
            Ok(result) => grade(result, &case.expect),
            Err(_) => Err("scenario assertion failed; inspect cargo.log".into()),
        };
        let detail = graded
            .as_ref()
            .err()
            .cloned()
            .unwrap_or_else(|| "scenario checks passed".into());
        if graded.is_err() {
            failures.push(format!("{}: {detail}", case.id));
        }
        results.push(json!({"id": case.id, "source": case.source,
            "status": if graded.is_ok() {"passed"} else {"failed"}, "detail": detail}));
    }
    assert!(!results.is_empty(), "no cases evaluated");
    if let Some(path) = std::env::var_os("EXTRACTOR_EVAL_REPORT") {
        std::fs::write(
            PathBuf::from(path),
            serde_json::to_vec_pretty(&results).unwrap(),
        )
        .expect("write eval report");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn grader_requires_the_expected_outcome_and_all_public_fields() {
    let success = Expectation::Success {
        contains: json!({"formats": [{"itag": 140}]}),
    };
    assert!(grade(
        Ok(json!({"formats": [{"itag": 140, "container":"m4a"}]})),
        &success
    )
    .is_ok());
    assert!(grade(Ok(json!({"formats": []})), &success).is_err());
    assert!(grade(Ok(json!({"formats": [{"itag": 136}]})), &success).is_err());
    assert!(grade(Err(AppError::invalid("rejected")), &success).is_err());
    let rejection = Expectation::Error {
        code: ErrorCode::PlayerChanged,
    };
    assert!(grade(Err(AppError::invalid("rejected")), &rejection).is_err());
    assert!(grade(Ok(json!({})), &rejection).is_err());
}
