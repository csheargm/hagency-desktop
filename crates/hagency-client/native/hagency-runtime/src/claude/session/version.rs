//! Version of the qualified native stdio contract, reported before input.
use super::Error;
use serde_json::Value;

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
        Some([2, minor, patch]) if (*minor, *patch) >= (1, 321) => Ok(()),
        _ => Err(Error::UnsupportedVersion),
    }
}
