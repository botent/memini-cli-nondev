//! Small utility helpers shared across the crate.

use std::env;

/// Return the first non-empty environment variable from `keys`, or `None`.
pub fn env_first(keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Ok(value) = env::var(key) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// Normalise a URL by prepending `http://` or `https://` when the scheme is missing.
pub fn normalize_url(raw: &str) -> String {
    if raw.contains("://") {
        return raw.to_string();
    }
    let scheme = if raw.starts_with("localhost") || raw.starts_with("127.") || raw.contains(":80") {
        "http"
    } else {
        "https"
    };
    format!("{scheme}://{raw}")
}
