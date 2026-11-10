//! Oldest Claude Code this host talks to, reported before input. Nothing older
//! is supported and nothing newer is refused: the resource owner keeps the CLI
//! current.
use super::Error;
use serde_json::Value;

/// Claude Code on the day this floor was set (2026-11-10).
pub const MINIMUM: [u32; 3] = [2, 1, 322];

pub(super) fn check(value: &Value) -> Result<(), Error> {
    let version = value
        .as_str()
        .filter(|v| v.len() <= 32)
        .ok_or(Error::UnsupportedVersion)?;
    let parts: Option<Vec<u32>> = version
        .split('.')
        .map(|part| {
            (!part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part == "0" || !part.starts_with('0')))
            .then(|| part.parse().ok())
            .flatten()
        })
        .collect();
    match parts.as_deref() {
        Some(&[major, minor, patch]) if [major, minor, patch] >= MINIMUM => Ok(()),
        _ => Err(Error::UnsupportedVersion),
    }
}
