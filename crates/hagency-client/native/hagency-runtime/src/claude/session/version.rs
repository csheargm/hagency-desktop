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
    // Three numbers, then whatever the build adds: a fourth number, build
    // metadata, a pre-release tag. Only the tag makes a version older, and
    // only when the three numbers are the floor itself.
    let mut numbers = [0u32; 3];
    let mut rest = version;
    for (index, number) in numbers.iter_mut().enumerate() {
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let part = &rest[..end];
        if part.is_empty() || (part != "0" && part.starts_with('0')) {
            return Err(Error::UnsupportedVersion);
        }
        *number = part.parse().map_err(|_| Error::UnsupportedVersion)?;
        rest = &rest[end..];
        if index < 2 {
            rest = rest.strip_prefix('.').ok_or(Error::UnsupportedVersion)?;
        }
    }
    let prerelease = rest.starts_with('-');
    if numbers > MINIMUM || (numbers == MINIMUM && !prerelease) {
        Ok(())
    } else {
        Err(Error::UnsupportedVersion)
    }
}
