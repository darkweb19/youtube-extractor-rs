//! Restricted, bounded outbound HTTP. Signed media URLs are never included in errors.
use crate::error::{AppError, ErrorCode, Result};
use reqwest::{header, redirect::Policy, Client, Response};
use std::{
    net::{IpAddr, SocketAddr},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use url::Url;

#[derive(Clone, Copy, Debug)]
pub enum Destination {
    Youtube,
    Media,
}

#[derive(Clone)]
pub struct SafeHttpClient {
    connect_timeout: Duration,
    request_timeout: Duration,
    proxy: Option<reqwest::Proxy>,
    certificates: Vec<reqwest::Certificate>,
    #[cfg(test)]
    test_origin: Option<Url>,
}

impl SafeHttpClient {
    pub fn new(connect_timeout: Duration, request_timeout: Duration) -> Result<Self> {
        let proxy_url = ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
            .iter()
            .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()));
        let proxy = proxy_url
            .map(|p| {
                reqwest::Proxy::all(p).map_err(|_| {
                    AppError::new(ErrorCode::Network, "Invalid outbound proxy configuration")
                })
            })
            .transpose()?;
        let mut certificates = Vec::new();
        // Reqwest's rustls roots do not automatically load an injected session CA.
        for name in ["SSL_CERT_FILE", "REQUESTS_CA_BUNDLE", "CODEX_PROXY_CERT"] {
            if let Some(path) = std::env::var_os(name).filter(|p| !p.is_empty()) {
                let pem = std::fs::read(path).map_err(|_| {
                    AppError::new(
                        ErrorCode::Network,
                        "Cannot read configured TLS trust bundle",
                    )
                })?;
                certificates.extend(reqwest::Certificate::from_pem_bundle(&pem).map_err(|_| {
                    AppError::new(ErrorCode::Network, "Invalid configured TLS trust bundle")
                })?);
            }
        }
        Ok(Self {
            connect_timeout,
            request_timeout,
            proxy,
            certificates,
            #[cfg(test)]
            test_origin: None,
        })
    }

    #[cfg(test)]
    pub fn for_test_origin(origin: &str) -> Self {
        let origin = Url::parse(origin).expect("test origin URL");
        assert_eq!(origin.scheme(), "http");
        assert!(origin
            .host_str()
            .and_then(|h| h.parse::<IpAddr>().ok())
            .is_some_and(|i| i.is_loopback()));
        Self {
            connect_timeout: Duration::from_secs(2),
            request_timeout: Duration::from_secs(10),
            proxy: None,
            certificates: vec![],
            test_origin: Some(origin),
        }
    }

    pub async fn get(&self, url: &str, destination: Destination) -> Result<Response> {
        self.request(url, destination, None).await
    }

    pub async fn get_with_range(&self, url: &str, start: u64) -> Result<Response> {
        self.request(url, Destination::Media, (start > 0).then_some(start))
            .await
    }

    async fn request(
        &self,
        input: &str,
        destination: Destination,
        range_start: Option<u64>,
    ) -> Result<Response> {
        let mut url = Url::parse(input).map_err(|_| AppError::invalid("Invalid upstream URL"))?;
        for attempt in 0..=5 {
            #[cfg(test)]
            let is_test = self
                .test_origin
                .as_ref()
                .is_some_and(|u| u.origin() == url.origin());
            #[cfg(not(test))]
            let is_test = false;
            if !is_test {
                validate_request_destination(&url, destination, attempt > 0)?;
            }
            let mut builder = Client::builder()
                .redirect(Policy::none())
                .referer(false)
                .connect_timeout(self.connect_timeout)
                .read_timeout(self.request_timeout)
                .user_agent("youtube-extractor-rs/0.1 (public media metadata service)");
            if matches!(destination, Destination::Youtube) {
                builder = builder.timeout(self.request_timeout);
            }
            for certificate in &self.certificates {
                builder = builder.add_root_certificate(certificate.clone());
            }
            if let Some(proxy) = &self.proxy {
                // The deployment's approved egress proxy owns DNS and destination enforcement.
                // Force its use even if NO_PROXY was inherited; never fall back to direct egress.
                builder = builder.proxy(proxy.clone());
            } else {
                builder = builder.no_proxy();
                if !is_test {
                    let host = url
                        .host_str()
                        .ok_or_else(|| AppError::invalid("Missing upstream host"))?;
                    let addrs: Vec<SocketAddr> = tokio::time::timeout(
                        self.connect_timeout,
                        tokio::net::lookup_host((host, 443)),
                    )
                    .await
                    .map_err(|_| AppError::new(ErrorCode::Dns, "DNS lookup timed out"))?
                    .map_err(|_| AppError::new(ErrorCode::Dns, "DNS lookup failed"))?
                    .collect();
                    if addrs.is_empty() || addrs.iter().any(|a| !is_public_ip(a.ip())) {
                        return Err(AppError::new(
                            ErrorCode::Dns,
                            "Upstream DNS did not resolve exclusively to public addresses",
                        ));
                    }
                    // Pin this connection to precisely the validated set, preventing DNS rebinding.
                    builder = builder.resolve_to_addrs(host, &addrs);
                }
            }
            let client = builder.build().map_err(|_| {
                AppError::new(ErrorCode::Network, "Cannot construct upstream HTTP client")
            })?;
            let mut request = client.get(url.clone());
            if let Some(start) = range_start {
                request = request.header(header::RANGE, format!("bytes={start}-"));
            }
            let response = tokio::time::timeout(self.request_timeout, request.send())
                .await
                .map_err(|_| {
                    AppError::new(ErrorCode::Timeout, "Upstream response headers timed out")
                })?
                .map_err(map_reqwest_error)?;
            if response.status().is_redirection() {
                if attempt == 5 {
                    return Err(AppError::new(
                        ErrorCode::Network,
                        "Upstream redirect limit reached",
                    ));
                }
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|h| h.to_str().ok())
                    .ok_or_else(|| {
                        AppError::new(ErrorCode::Network, "Invalid upstream redirect")
                    })?;
                url = url.join(location).map_err(|_| {
                    AppError::new(ErrorCode::Network, "Invalid upstream redirect URL")
                })?;
                continue;
            }
            let status = response.status().as_u16();
            // Proxy policy denials must never be presented as YouTube access restrictions.
            if status >= 400
                && (response.headers().contains_key("proxy-status")
                    || response.headers().contains_key("x-block-reason"))
            {
                return Err(AppError::new(
                    ErrorCode::Network,
                    "Outbound proxy rejected or could not complete the request",
                ));
            }
            if !(200..300).contains(&status) {
                return Err(status_error(status, &url));
            }
            return Ok(response);
        }
        unreachable!()
    }

    pub async fn get_bytes(
        &self,
        url: &str,
        destination: Destination,
        max_bytes: usize,
    ) -> Result<Vec<u8>> {
        let mut response = self.get(url, destination).await?;
        if response
            .content_length()
            .is_some_and(|l| l > max_bytes as u64)
        {
            return Err(AppError::new(
                ErrorCode::PlayerChanged,
                "Player response exceeds configured size limit",
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(map_reqwest_error)? {
            if chunk.len() > max_bytes.saturating_sub(bytes.len()) {
                return Err(AppError::new(
                    ErrorCode::PlayerChanged,
                    "Player response exceeds configured size limit",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

pub fn map_reqwest_error(error: reqwest::Error) -> AppError {
    if error.is_timeout() {
        AppError::new(ErrorCode::Timeout, "Upstream request timed out")
    } else {
        AppError::new(
            ErrorCode::Network,
            "Upstream connection or TLS request failed",
        )
    }
}

pub fn validate_media_url(input: &str) -> Result<Url> {
    let url = Url::parse(input)
        .map_err(|_| AppError::new(ErrorCode::UnsupportedFormat, "Invalid media URL"))?;
    validate_request_destination(&url, Destination::Media, false)?;
    Ok(url)
}

pub fn validate_destination(url: &Url, destination: Destination) -> Result<()> {
    let host = url.host_str().unwrap_or("");
    let allowed = match destination {
        Destination::Youtube => matches!(host, "youtube.com" | "www.youtube.com" | "m.youtube.com"),
        Destination::Media => {
            host.ends_with(".googlevideo.com") && host.len() > ".googlevideo.com".len()
        }
    };
    if !is_plain_https_endpoint(url) || !allowed {
        let (code, message) = match destination {
            Destination::Youtube => (
                ErrorCode::Network,
                "YouTube metadata endpoint is not permitted by this service's outbound policy",
            ),
            Destination::Media => (
                ErrorCode::UnsupportedFormat,
                "YouTube returned a media URL outside the permitted HTTPS media hosts",
            ),
        };
        return Err(AppError::new(code, message));
    }
    Ok(())
}

fn is_plain_https_endpoint(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none()
}

/// Recognize access gates only to explain a refusal, never to authorize another host.
fn access_gate(url: &Url) -> Option<&'static str> {
    if !is_plain_https_endpoint(url) {
        return None;
    }
    match url.host_str()? {
        "consent.youtube.com" | "consent.google.com" => Some(
            "YouTube requires interactive consent before serving this video. This service cannot complete consent checks",
        ),
        "accounts.google.com" => Some(
            "YouTube requires sign-in before serving this video. This service cannot access videos requiring authentication",
        ),
        "sorry.google.com" => Some(
            "YouTube requires a browser verification challenge from this deployment. This service cannot complete verification challenges",
        ),
        "www.google.com" if url.path() == "/sorry" || url.path().starts_with("/sorry/") => Some(
            "YouTube requires a browser verification challenge from this deployment. This service cannot complete verification challenges",
        ),
        "youtube.com" | "www.youtube.com" | "m.youtube.com"
            if url.path() == "/signin" || url.path().starts_with("/signin/") => Some(
            "YouTube requires sign-in before serving this video. This service cannot access videos requiring authentication",
        ),
        _ => None,
    }
}

fn validate_request_destination(
    url: &Url,
    destination: Destination,
    redirected: bool,
) -> Result<()> {
    let error = if redirected {
        if let Some(message) = access_gate(url) {
            Some(AppError::new(ErrorCode::AccessRestricted, message))
        } else {
            validate_destination(url, destination).err().map(|_| {
                let message = match destination {
                    Destination::Youtube => "YouTube redirected metadata to an endpoint this service does not permit. Check deployment logs for the rejected host",
                    Destination::Media => "YouTube redirected media to an endpoint this service does not permit. Check deployment logs for the rejected host",
                };
                AppError::new(ErrorCode::Network, message)
            })
        }
    } else {
        validate_destination(url, destination).err()
    };
    if let Some(error) = error {
        // Never log paths, queries, Location headers, or signed media URLs.
        tracing::warn!(
            stage = match destination { Destination::Youtube => "metadata", Destination::Media => "media" },
            phase = if redirected { "redirect" } else { "initial" },
            destination_host = url.host_str().unwrap_or("missing"),
            error_code = ?error.code,
            "Rejected upstream destination"
        );
        return Err(error);
    }
    Ok(())
}

fn status_error(status: u16, url: &Url) -> AppError {
    let expired = url
        .query_pairs()
        .find(|(k, _)| k == "expire")
        .and_then(|(_, v)| v.parse::<u64>().ok())
        .is_some_and(|expiry| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .is_ok_and(|now| expiry <= now.as_secs())
        });
    match status {
        401 | 403 if expired => AppError::new(
            ErrorCode::ExpiredUrl,
            "Media URL has expired; request fresh metadata",
        ),
        401 | 403 => AppError::new(
            ErrorCode::AccessRestricted,
            "Upstream denied access to this content",
        ),
        404 | 410 => AppError::new(ErrorCode::NotFound, "Upstream content is unavailable"),
        429 => AppError::new(
            ErrorCode::UpstreamRateLimited,
            "Upstream rate limit reached",
        ),
        _ => AppError::new(
            ErrorCode::Network,
            format!("Upstream returned HTTP {status}"),
        ),
    }
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 0 || b == 168))
                || (a == 198 && (b == 18 || b == 19))
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            (s[0] & 0xe000) == 0x2000
                && !(s[0] == 0x2001 && s[1] < 0x200)
                && !(s[0] == 0x2001 && s[1] == 0xdb8)
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn fixture(response: impl Into<String>) -> (String, tokio::task::JoinHandle<()>) {
        fixture_chain(vec![response.into()]).await
    }

    async fn fixture_chain(responses: Vec<String>) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            for response in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 4096];
                let mut used = 0;
                while !request[..used].windows(4).any(|part| part == b"\r\n\r\n") {
                    assert!(used < request.len(), "fixture request headers too large");
                    let received = stream.read(&mut request[used..]).await.unwrap();
                    assert!(
                        received > 0,
                        "fixture request closed before complete headers"
                    );
                    used += received;
                }
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (origin, task)
    }

    #[tokio::test]
    async fn redirects_are_revalidated_and_cannot_leave_test_origin() {
        let (origin,task) = fixture("HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
        let error = SafeHttpClient::for_test_origin(&origin)
            .get(&origin, Destination::Media)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Network);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn redirect_chain_rejects_destination_before_any_connection() {
        let denied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let responses = vec![
            "HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
            format!("HTTP/1.1 302 Found\r\nLocation: http://{}/private?signature=DO_NOT_EXPOSE\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", denied.local_addr().unwrap()),
        ];
        let (origin, task) = fixture_chain(responses).await;
        let error = SafeHttpClient::for_test_origin(&origin)
            .get(&origin, Destination::Youtube)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Network);
        assert!(error.message.contains("redirected metadata"));
        assert!(!error.message.contains("DO_NOT_EXPOSE"));
        task.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), denied.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn interactive_redirects_are_classified_without_following_them() {
        for (target, expected) in [
            (
                "https://consent.youtube.com/m?continue=DO_NOT_EXPOSE",
                "consent",
            ),
            (
                "https://consent.google.com/m?continue=DO_NOT_EXPOSE",
                "consent",
            ),
            (
                "https://accounts.google.com/ServiceLogin?continue=DO_NOT_EXPOSE",
                "sign-in",
            ),
            (
                "https://www.google.com/sorry/index?continue=DO_NOT_EXPOSE",
                "verification challenge",
            ),
            (
                "https://sorry.google.com/?continue=DO_NOT_EXPOSE",
                "verification challenge",
            ),
            (
                "https://www.youtube.com/signin?continue=DO_NOT_EXPOSE",
                "sign-in",
            ),
        ] {
            let (origin, task) = fixture(format!("HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")).await;
            let error = SafeHttpClient::for_test_origin(&origin)
                .get(&origin, Destination::Youtube)
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::AccessRestricted, "{target}");
            assert!(error.message.contains(expected));
            assert!(!error.message.contains("DO_NOT_EXPOSE"));
            task.await.unwrap();
        }
    }

    #[test]
    fn access_gate_recognition_never_permits_additional_hosts_or_unsafe_urls() {
        for target in [
            "https://consent.youtube.com.evil.invalid/m",
            "https://consent.youtube.com./m",
            "https://accounts.google.com.evil.invalid/ServiceLogin",
            "https://www.google.com/sorry-looking",
            "https://www.google.com/search",
            "http://consent.youtube.com/m",
            "https://consent.youtube.com:444/m",
            "https://user@consent.youtube.com/m",
            "https://user:password@accounts.google.com/ServiceLogin",
            "https://consent.youtube.com/m#fragment",
        ] {
            let url = Url::parse(target).unwrap();
            assert!(access_gate(&url).is_none(), "{target}");
            assert_eq!(
                validate_request_destination(&url, Destination::Youtube, true)
                    .unwrap_err()
                    .code,
                ErrorCode::Network,
                "{target}"
            );
        }
        for target in [
            "https://consent.youtube.com/m",
            "https://accounts.google.com/ServiceLogin",
        ] {
            let url = Url::parse(target).unwrap();
            assert!(validate_destination(&url, Destination::Youtube).is_err());
            assert!(validate_destination(&url, Destination::Media).is_err());
        }
    }

    #[test]
    fn rejected_destination_diagnostics_include_only_stage_host_and_classification() {
        use std::sync::{Arc, Mutex};
        struct Writer(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let output = Arc::new(Mutex::new(Vec::new()));
        let captured = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || Writer(captured.clone()))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let url =
                Url::parse("https://consent.youtube.com/PRIVATE_PATH?signature=PRIVATE_SIGNATURE")
                    .unwrap();
            let error = validate_request_destination(&url, Destination::Youtube, true).unwrap_err();
            assert!(!error.message.contains("PRIVATE_"));
        });
        let output = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(output.contains("metadata"));
        assert!(output.contains("redirect"));
        assert!(output.contains("consent.youtube.com"));
        assert!(output.contains("AccessRestricted"));
        assert!(!output.contains("PRIVATE_"));
        assert!(!output.contains("signature"));
    }

    #[tokio::test]
    async fn body_limit_applies_without_content_length_and_statuses_are_typed() {
        let (origin,task) = fixture("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\ntest\r\n0\r\n\r\n").await;
        let error = SafeHttpClient::for_test_origin(&origin)
            .get_bytes(&origin, Destination::Youtube, 3)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::PlayerChanged);
        task.await.unwrap();
        for (response,code) in [("HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",ErrorCode::UpstreamRateLimited), ("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",ErrorCode::AccessRestricted), ("HTTP/1.1 403 Forbidden\r\nx-block-reason: policy\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",ErrorCode::Network)] {
            let (origin,task) = fixture(response).await;
            assert_eq!(SafeHttpClient::for_test_origin(&origin).get(&origin,Destination::Youtube).await.unwrap_err().code,code);
            task.await.unwrap();
        }
    }

    #[test]
    fn destinations_require_exact_host_boundaries_and_https() {
        for bad in [
            "https://googlevideo.com.evil.invalid/x",
            "http://r1.googlevideo.com/x",
            "https://user@r1.googlevideo.com/x",
            "https://r1.googlevideo.com:444/x",
            "https://127.0.0.1/x",
            "https://r1.googlevideo.com./x",
            "https://googlevideo.com/x",
            "https://r1.googlevideo.com/x#secret",
        ] {
            assert!(validate_media_url(bad).is_err(), "{bad}");
        }
        assert!(
            validate_media_url("https://rr1---sn-test.googlevideo.com/videoplayback?expire=1")
                .is_ok()
        );
    }
    #[test]
    fn dns_rejects_private_reserved_and_mapped_addresses() {
        for bad in [
            "127.0.0.1",
            "0.0.0.0",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "169.254.169.254",
            "198.18.0.1",
            "192.0.2.1",
            "203.0.113.1",
            "224.0.0.1",
            "::1",
            "::ffff:8.8.8.8",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2002:7f00:1::",
        ] {
            assert!(!is_public_ip(bad.parse().unwrap()), "{bad}");
        }
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
        assert!(is_public_ip("2607:f8b0:4000::1".parse().unwrap()));
    }
}
