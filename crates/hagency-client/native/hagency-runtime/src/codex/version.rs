//! Oldest Codex this host talks to. Nothing older is supported and nothing
//! newer is refused: the resource owner keeps the CLI current.

/// Codex on the day this floor was set (2026-11-10).
pub const MINIMUM: [u32; 3] = [0, 167, 0];
/// What an owner is told to do; names `MINIMUM`.
pub const REQUIRED: &str = "Codex 0.167.0 or newer is required";

/// `initialize` answers `"<client name>/<Codex version> (<system>) …"` in
/// `userAgent`. A pre-release of a version counts as that version.
pub fn supported(user_agent: &str) -> bool {
    parse(user_agent).is_some_and(|found| found >= MINIMUM)
}

fn parse(user_agent: &str) -> Option<[u32; 3]> {
    let (_, rest) = user_agent.split_once('/')?;
    let version = rest.split(' ').next()?;
    let release = version.split(['-', '+']).next()?;
    let mut parts = release.split('.').map(|part| {
        (!part.is_empty() && part.len() <= 9 && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse().ok())
            .flatten()
    });
    let found = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_floor_is_read_from_the_initialize_user_agent() {
        // The reply of codex-cli 0.167.0 to a client named "probe".
        assert!(supported(
            "probe/0.167.0 (Mac OS 26.6.2; arm64) unknown (probe; 0.0.1)"
        ));
        for newer in ["hagency/0.167.1", "hagency/0.168.0-alpha.2 (x)", "hagency/1.0.0 (x)"] {
            assert!(supported(newer), "{newer}");
        }
        for refused in [
            "hagency/0.166.9 (Mac OS 26.6.2; arm64)",
            "hagency/0.153.4",
            "hagency/0.167 (x)",
            "hagency/0.167.0.1 (x)",
            "hagency/v0.167.0 (x)",
            "hagency/ 0.167.0",
            "0.167.0",
            "fixture",
            "",
        ] {
            assert!(!supported(refused), "{refused}");
        }
        assert!(REQUIRED.contains(&MINIMUM.map(|part| part.to_string()).join(".")));
    }
}
