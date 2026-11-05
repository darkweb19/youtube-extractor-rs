//! Stateless browser credentials, scoped separately from bearer authentication.
use axum::http::{header, HeaderMap, HeaderValue};
use ring::hmac;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const COOKIE_NAME: &str = "__Secure-frame_session";
const DOMAIN: &str = "frame-browser-session-v1\0";
const MAX_AGE: u64 = 365 * 24 * 60 * 60;
const MAX_VALUE_LEN: usize = 120;

/// The custom header requires a same-origin script (cross-origin use would need
/// a CORS preflight, which this service does not allow). Fetch Metadata, when
/// supplied by a browser, also rules out same-site sibling and cross-site pages.
pub(crate) fn browser_request(headers: &HeaderMap) -> bool {
    let mut client = headers.get_all("x-frame-client").iter();
    if client.next().and_then(|v| v.to_str().ok()) != Some("1") || client.next().is_some() {
        return false;
    }
    let mut site = headers.get_all("sec-fetch-site").iter();
    let allowed = match site.next() {
        None => true,
        Some(value) => matches!(value.to_str(), Ok("same-origin" | "none")),
    };
    allowed && site.next().is_none()
}

pub(crate) fn new_cookie(token: &str) -> Option<HeaderValue> {
    let expiry = now()?.checked_add(MAX_AGE)?;
    let payload = format!("v1.{expiry}.{}", Uuid::new_v4().simple());
    let key = hmac::Key::new(hmac::HMAC_SHA256, token.as_bytes());
    let tag = hmac::sign(&key, format!("{DOMAIN}{payload}").as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in tag.as_ref() {
        use std::fmt::Write;
        write!(&mut hex, "{byte:02x}").ok()?;
    }
    HeaderValue::from_str(&format!(
        "{COOKIE_NAME}={payload}.{hex}; Max-Age={MAX_AGE}; Path=/api; Secure; HttpOnly; SameSite=Strict"
    ))
    .ok()
}

pub(crate) fn clear_cookie() -> HeaderValue {
    HeaderValue::from_static(
        "__Secure-frame_session=; Max-Age=0; Path=/api; Secure; HttpOnly; SameSite=Strict",
    )
}

pub(crate) fn valid_cookie(headers: &HeaderMap, token: &str) -> bool {
    let mut found = None;
    for header in headers.get_all(header::COOKIE) {
        let Ok(header) = header.to_str() else {
            return false;
        };
        for part in header.split(';') {
            let part = part.trim();
            let Some((name, value)) = part.split_once('=') else {
                if part == COOKIE_NAME {
                    return false;
                }
                continue;
            };
            if name.trim() == COOKIE_NAME {
                if found.is_some() || value.len() > MAX_VALUE_LEN {
                    return false;
                }
                found = Some(value);
            }
        }
    }
    let Some(value) = found else { return false };
    let Some((payload, signature)) = value.rsplit_once('.') else {
        return false;
    };
    if signature.len() != 64 {
        return false;
    }
    let mut tag = [0_u8; 32];
    for (index, pair) in signature.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let Some(high) = hex_digit(pair[0]) else {
            return false;
        };
        let Some(low) = hex_digit(pair[1]) else {
            return false;
        };
        tag[index] = (high << 4) | low;
    }
    let key = hmac::Key::new(hmac::HMAC_SHA256, token.as_bytes());
    if hmac::verify(&key, format!("{DOMAIN}{payload}").as_bytes(), &tag).is_err() {
        return false;
    }
    let mut fields = payload.split('.');
    let (Some("v1"), Some(expiry), Some(nonce), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return false;
    };
    if expiry.is_empty() || expiry.len() > 20 || !expiry.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let Ok(expiry) = expiry.parse::<u64>() else {
        return false;
    };
    if nonce.len() != 32 || !nonce.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    let Ok(nonce) = Uuid::parse_str(nonce) else {
        return false;
    };
    if nonce.get_version_num() != 4 || nonce.get_variant() != uuid::Variant::RFC4122 {
        return false;
    }
    let Some(now) = now() else { return false };
    expiry > now && now.checked_add(MAX_AGE).is_some_and(|max| expiry <= max)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn now() -> Option<u64> {
    Some(SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs())
}
