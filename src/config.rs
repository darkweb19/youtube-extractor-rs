use std::{
    env, fmt,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    str::FromStr,
    time::Duration,
};

use crate::error::{AppError, Result};

/// Runtime resource budgets. Debug intentionally redacts the bearer token.
#[derive(Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub api_token: Option<String>,
    pub data_dir: PathBuf,
    pub workers: usize,
    pub queue_capacity: usize,
    pub max_download_bytes: u64,
    pub max_storage_bytes: u64,
    pub job_ttl: Duration,
    pub max_jobs: usize,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub download_timeout: Duration,
    pub ffmpeg_path: PathBuf,
    pub requests_per_minute: u32,
    pub max_concurrent_requests: usize,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("api_token", &self.api_token.as_ref().map(|_| "[REDACTED]"))
            .field("data_dir", &self.data_dir)
            .field("workers", &self.workers)
            .field("queue_capacity", &self.queue_capacity)
            .field("max_download_bytes", &self.max_download_bytes)
            .field("max_storage_bytes", &self.max_storage_bytes)
            .field("job_ttl", &self.job_ttl)
            .field("max_jobs", &self.max_jobs)
            .finish_non_exhaustive()
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|key| env::var(key).ok())
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        fn number<T: FromStr>(
            lookup: &impl Fn(&str) -> Option<String>,
            name: &str,
            default: T,
        ) -> Result<T> {
            match lookup(name) {
                Some(value) => value.parse().map_err(|_| {
                    AppError::invalid(format!("{name} must be a valid positive integer"))
                }),
                None => Ok(default),
            }
        }

        let api_token = lookup("API_TOKEN").filter(|v| !v.is_empty());
        if api_token.is_none() && lookup("ALLOW_UNAUTHENTICATED").as_deref() != Some("true") {
            return Err(AppError::invalid(
                "API_TOKEN is required; ALLOW_UNAUTHENTICATED=true is only for local development",
            ));
        }
        let config = Self {
            bind_addr: SocketAddr::new(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                number(&lookup, "PORT", 8080_u16)?,
            ),
            api_token,
            data_dir: PathBuf::from(lookup("DATA_DIR").unwrap_or_else(|| "data".into())),
            workers: number(&lookup, "WORKERS", 2)?,
            queue_capacity: number(&lookup, "QUEUE_CAPACITY", 16)?,
            max_download_bytes: number(&lookup, "MAX_DOWNLOAD_BYTES", 1024_u64 * 1024 * 1024)?,
            max_storage_bytes: number(&lookup, "MAX_STORAGE_BYTES", 8_u64 * 1024 * 1024 * 1024)?,
            job_ttl: Duration::from_secs(number(&lookup, "JOB_TTL_SECS", 3600)?),
            max_jobs: number(&lookup, "MAX_JOBS", 256)?,
            connect_timeout: Duration::from_secs(number(&lookup, "CONNECT_TIMEOUT_SECS", 10)?),
            request_timeout: Duration::from_secs(number(&lookup, "REQUEST_TIMEOUT_SECS", 30)?),
            download_timeout: Duration::from_secs(number(&lookup, "DOWNLOAD_TIMEOUT_SECS", 1800)?),
            ffmpeg_path: PathBuf::from(lookup("FFMPEG_PATH").unwrap_or_else(|| "ffmpeg".into())),
            requests_per_minute: number(&lookup, "REQUESTS_PER_MINUTE", 120)?,
            max_concurrent_requests: number(&lookup, "MAX_CONCURRENT_REQUESTS", 32)?,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if let Some(token) = &self.api_token {
            if token.len() < 32
                || token.len() > 4096
                || !token.bytes().all(|byte| byte.is_ascii_graphic())
            {
                return Err(AppError::invalid(
                    "API_TOKEN must contain 32 to 4096 visible ASCII bytes",
                ));
            }
        }
        if self.bind_addr.port() == 0 {
            return Err(AppError::invalid("PORT must be between 1 and 65535"));
        }
        if !(1..=64).contains(&self.workers) || !(1..=4096).contains(&self.queue_capacity) {
            return Err(AppError::invalid(
                "WORKERS must be 1..64 and QUEUE_CAPACITY must be 1..4096",
            ));
        }
        if self.max_jobs < self.workers.saturating_add(self.queue_capacity)
            || self.max_jobs > 100_000
        {
            return Err(AppError::invalid(
                "MAX_JOBS must cover workers plus queue capacity and be at most 100000",
            ));
        }
        if self.max_download_bytes == 0
            || self.job_ttl.is_zero()
            || self.connect_timeout.is_zero()
            || self.request_timeout.is_zero()
            || self.download_timeout.is_zero()
            || self.requests_per_minute == 0
            || self.max_concurrent_requests == 0
            || self.max_concurrent_requests > 4096
        {
            return Err(AppError::invalid("Resource sizes, timeouts, and request limits must be positive (request concurrency at most 4096)"));
        }
        if self.max_storage_bytes < self.max_download_bytes {
            return Err(AppError::invalid(
                "MAX_STORAGE_BYTES must cover at least one MAX_DOWNLOAD_BYTES job reservation",
            ));
        }
        // Upper limits prevent duration-to-millisecond and timestamp overflows.
        let max_timeout = Duration::from_secs(7 * 24 * 3600);
        if self.job_ttl > max_timeout
            || self.connect_timeout > max_timeout
            || self.request_timeout > max_timeout
            || self.download_timeout > max_timeout
        {
            return Err(AppError::invalid(
                "Timeouts and job retention must be at most seven days",
            ));
        }
        if self.data_dir.as_os_str().is_empty() || self.ffmpeg_path.as_os_str().is_empty() {
            return Err(AppError::invalid(
                "DATA_DIR and FFMPEG_PATH cannot be empty",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn parse(values: &[(&str, &str)]) -> Result<Config> {
        let values: HashMap<_, _> = values.iter().copied().collect();
        Config::from_lookup(|key| values.get(key).map(|value| value.to_string()))
    }

    #[test]
    fn authentication_is_required_unless_explicitly_disabled() {
        assert!(parse(&[]).is_err());
        assert!(parse(&[("ALLOW_UNAUTHENTICATED", "1")]).is_err());
        assert!(parse(&[("ALLOW_UNAUTHENTICATED", "true")]).is_ok());
        assert!(parse(&[("API_TOKEN", "weak")]).is_err());
    }

    #[test]
    fn resource_limits_cannot_be_zero_or_unbounded() {
        assert!(parse(&[("ALLOW_UNAUTHENTICATED", "true"), ("WORKERS", "0")]).is_err());
        assert!(parse(&[("ALLOW_UNAUTHENTICATED", "true"), ("MAX_JOBS", "2")]).is_err());
        assert!(parse(&[("ALLOW_UNAUTHENTICATED", "true"), ("PORT", "0")]).is_err());
        assert!(parse(&[
            ("ALLOW_UNAUTHENTICATED", "true"),
            ("MAX_STORAGE_BYTES", "1")
        ])
        .is_err());
        assert!(parse(&[
            ("ALLOW_UNAUTHENTICATED", "true"),
            ("JOB_TTL_SECS", "18446744073709551615")
        ])
        .is_err());
    }

    #[test]
    fn token_is_redacted_from_debug() {
        let token = "private-token-value-private-token-value";
        let config = parse(&[("API_TOKEN", token)]).unwrap();
        assert!(!format!("{config:?}").contains(token));
    }
}
