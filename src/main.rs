use std::{future::IntoFuture, path::PathBuf, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use youtube_extractor::{
    api::{self, ApiState},
    config::Config,
    extractor::Extractor,
    jobs::{JobExecutor, JobManager, JobSettings},
    media::Ffmpeg,
    remote::Remote,
    storage::LocalStorage,
    ytdlp::YtDlp,
};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .json()
        .init();
    if let Err(error) = run().await {
        tracing::error!(code=?error.code, message=%error.message, "Service stopped");
        std::process::exit(1);
    }
}
async fn run() -> youtube_extractor::error::Result<()> {
    let config = Arc::new(Config::from_env()?);
    let storage = LocalStorage::new(config.data_dir.clone()).await?;
    let probe = std::env::var_os("FFPROBE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| "ffprobe".into());
    let ffmpeg = Ffmpeg::new(config.ffmpeg_path.clone(), probe, config.download_timeout);
    let mode = std::env::var("EXTRACTOR_MODE").unwrap_or_else(|_| "local".into());
    let (extractor, executor): (Arc<dyn Extractor>, Arc<dyn JobExecutor>) = if mode == "remote" {
        let remote = Remote::from_env(storage.clone(), config.max_download_bytes)?;
        remote.start_idle_watch();
        (remote.clone(), remote)
    } else if mode == "local" {
        if std::env::var("YTDLP_PROXY_REQUIRED").as_deref() == Ok("true")
            && !std::env::var("YTDLP_PROXY").is_ok_and(|v| v.starts_with("socks5://127.0.0.1:"))
        {
            return Err(youtube_extractor::error::AppError::invalid(
                "GCP worker mode requires a local WARP SOCKS5 proxy",
            ));
        }
        let local = Arc::new(YtDlp::new(
            std::env::var_os("YTDLP_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| "yt-dlp".into()),
            storage,
            ffmpeg,
            config.download_timeout,
            config.max_download_bytes,
        ));
        (local.clone(), local)
    } else {
        return Err(youtube_extractor::error::AppError::invalid(
            "EXTRACTOR_MODE must be local or remote",
        ));
    };
    let jobs = JobManager::new(
        JobSettings {
            data_dir: config.data_dir.clone(),
            workers: config.workers,
            queue_capacity: config.queue_capacity,
            max_jobs: config.max_jobs,
            max_download_bytes: config.max_download_bytes,
            max_storage_bytes: config.max_storage_bytes,
            ttl: config.job_ttl,
            timeout: config.download_timeout,
        },
        executor,
    )
    .await?;
    let app = api::router(ApiState::new(config.clone(), extractor, jobs.clone()));
    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .map_err(|_| {
            youtube_extractor::error::AppError::internal("Unable to bind service listener")
        })?;
    tracing::info!(address=%config.bind_addr, "HTTP service ready");
    let shutdown_jobs = jobs.clone();
    let shutdown_started = CancellationToken::new();
    let signal_started = shutdown_started.clone();
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        shutdown_signal().await;
        signal_started.cancel();
        shutdown_jobs.begin_shutdown().await;
    });
    let drain_deadline = async {
        shutdown_started.cancelled().await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    };
    let result = tokio::select! {
        result = server.into_future() => result,
        _ = drain_deadline => {
            tracing::warn!("HTTP shutdown drain deadline reached; closing remaining connections");
            Ok(())
        }
    };
    jobs.shutdown().await;
    result.map_err(|_| youtube_extractor::error::AppError::internal("HTTP server failed"))
}
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("signal registration");
        tokio::select! { _ = tokio::signal::ctrl_c() => (), _ = terminate.recv() => () }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("Graceful shutdown requested");
    // Shutdown waits for cooperative media cancellation before the runtime exits.
}
