//! Hosting data written by an earlier Hagency build. This build has no
//! migration for it, so it blocked hosting for good. The product decision is
//! to discard it at start and begin as a new installation, without asking.
//!
//! What goes: the canonical resource database (through the store, which
//! refuses while it is open or once a profile of this build uses it), the
//! profile's journals that are bound to that database, and the initializer
//! record. What stays: every directory and lock, the owner-root records, the
//! Matrix session and its keys, provider sign-ins, and all work files.
use super::*;

/// The earlier builds' largest record.
const LIMIT: usize = 16 * 1024;
/// Formats written before this build's version 4.
const EARLIER: std::ops::Range<u64> = 1..4;
/// Bindings to the discarded database. Replayed into a new one, they would
/// recreate what the discard removed.
const JOURNALS: [&str; 4] = [
    "owner-grant-intents-v1.json",
    "owner-grant-intents-v1.identity",
    "management-enrollments-v1.json",
    "management-enrollments-v1.identity",
];

pub(super) struct Record {
    registry: String,
    profile: String,
}

/// `Some` only for a readable record as an earlier build wrote it: an earlier
/// format version, the database it was bound to, and its profile. A damaged,
/// oversized, incomplete or unreadable file is not one and stays an error.
pub(super) fn record(state: &Path) -> Option<Record> {
    let file = hagency_store::private::open(&state.join(INITIALIZER_FILE), false).ok()?;
    let mut bytes = Vec::new();
    file.take(LIMIT as u64 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > LIMIT {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let value = value.as_object()?;
    if !EARLIER.contains(&value.get("version")?.as_u64()?) {
        return None;
    }
    Some(Record {
        registry: value
            .get("registryId")?
            .as_str()
            .filter(|r| !r.is_empty() && r.len() <= 128)?
            .to_owned(),
        profile: value
            .get("profile")?
            .as_str()
            .filter(|p| p.len() == 64 && p.bytes().all(|b| b.is_ascii_hexdigit()))?
            .to_owned(),
    })
}

fn remove(path: &Path) -> Result<(), NativeError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(descriptor_error()),
    }
}

/// `database` deletes the canonical database and says whether it is gone.
/// `current_registry` names the database that stayed.
///
/// The record goes last, so an interrupted discard runs again at next start.
/// When the database stays because a profile of this build uses it, the
/// record is removed only if it names a different, already discarded
/// database. Otherwise the earlier rows of this profile are still in the
/// database that stayed, and this profile remains unavailable.
pub(super) fn discard(
    state: &Path,
    record: Record,
    database: impl FnOnce() -> Result<bool, hagency_store::Error>,
    current_registry: impl FnOnce() -> Result<String, hagency_store::Error>,
) -> Result<(), NativeError> {
    let removed = database().map_err(|error| {
        tracing::warn!(%error, "Hosting data from an earlier Hagency version could not be discarded yet");
        descriptor_error()
    })?;
    if !removed && record.registry == current_registry().map_err(|_| descriptor_error())? {
        tracing::warn!(
            "Hosting data from an earlier Hagency version shares a resource database with a current profile; it was kept"
        );
        return Err(descriptor_error());
    }
    let owner = state
        .join("owned-agent-owners")
        .join(format!("owner_{}", record.profile));
    for name in JOURNALS {
        remove(&owner.join(name))?;
    }
    remove(&state.join(INITIALIZER_FILE))?;
    tracing::warn!(
        database_removed = removed,
        "Discarded hosting data written by an earlier Hagency version; hosting starts as a new installation"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    struct Fixture {
        _temp: tempfile::TempDir,
        state: std::path::PathBuf,
        canonical: std::path::PathBuf,
        owner: std::path::PathBuf,
    }
    fn earlier(version: u64, registry: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "version": version,
            "registryId": registry,
            "migrationId": "empty_test",
            "origin": "https://server.test/",
            "owner": "@owner:test",
            "profile": PROFILE,
            "originalDevice": "device",
            "emptyPrepare": {"anything": "an earlier build wrote"},
        }))
        .unwrap()
    }
    /// An owner root as an earlier build left it, next to a canonical
    /// database that no profile of this build uses.
    async fn fixture(record: &[u8]) -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let state = base.join("state");
        let owner = state
            .join("owned-agent-owners")
            .join(format!("owner_{PROFILE}"));
        for directory in [&state, &state.join("owned-agent-owners"), &owner] {
            hagency_store::private::directory(directory).unwrap();
        }
        hagency_store::private::write_new(&state.join(INITIALIZER_FILE), record).unwrap();
        for name in JOURNALS.into_iter().chain(["profile-identity.json"]) {
            hagency_store::private::write_new(&owner.join(name), b"{}").unwrap();
        }
        std::fs::create_dir_all(owner.join("managed-claude/scopes/work")).unwrap();
        std::fs::write(owner.join("managed-claude/scopes/work/notes"), "work files").unwrap();
        let canonical = base.join("canonical");
        let registry = OwnerResourceRegistry::isolated_for_tests(&canonical).unwrap();
        registry.shutdown().await.unwrap();
        drop(registry);
        Fixture {
            _temp: temp,
            state,
            canonical,
            owner,
        }
    }
    fn database(f: &Fixture) -> impl FnOnce() -> Result<bool, hagency_store::Error> + use<> {
        let canonical = f.canonical.clone();
        move || OwnerResourceRegistry::discard_previous_build_for_tests(&canonical)
    }
    fn unused() -> Result<String, hagency_store::Error> {
        panic!("the database was removed, so no registry is consulted")
    }

    #[tokio::test]
    async fn an_earlier_builds_hosting_data_is_discarded_and_the_owner_starts_as_new() {
        for version in [1, 2, 3] {
            let f = fixture(&earlier(version, "old-registry")).await;
            assert!(read_descriptor(&f.state).is_err());
            let found = record(&f.state).expect("an earlier format");
            discard(&f.state, found, database(&f), unused).unwrap();
            // No record: the host reports that set-up is required.
            assert!(read_descriptor(&f.state).unwrap().is_none());
            assert!(!f.canonical.join("domain.sqlite3").exists());
            for name in JOURNALS {
                assert!(!f.owner.join(name).exists(), "{name}");
            }
            // Identity, locks and work files are not hosting data.
            assert!(f.owner.join("profile-identity.json").exists());
            assert!(f.canonical.join("domain.lock").exists());
            assert_eq!(
                std::fs::read_to_string(f.owner.join("managed-claude/scopes/work/notes")).unwrap(),
                "work files"
            );
        }
    }

    #[tokio::test]
    async fn only_a_readable_record_of_an_earlier_format_is_discarded() {
        let mut current = serde_json::from_slice::<serde_json::Value>(&earlier(4, "r")).unwrap();
        current.as_object_mut().unwrap().remove("emptyPrepare");
        current["currentProfile"] = true.into();
        let mut oversized = earlier(3, "r");
        oversized.extend(std::iter::repeat_n(b' ', LIMIT));
        // An earlier build always named its database and its profile.
        let without = |key: &str| {
            let mut record = serde_json::from_slice::<serde_json::Value>(&earlier(3, "r")).unwrap();
            record.as_object_mut().unwrap().remove(key);
            serde_json::to_vec(&record).unwrap()
        };
        let mut other_profile = serde_json::from_slice::<serde_json::Value>(&earlier(3, "r")).unwrap();
        other_profile["profile"] = "../elsewhere".into();
        for kept in [
            without("registryId"),
            without("profile"),
            earlier(3, ""),
            serde_json::to_vec(&other_profile).unwrap(),
            serde_json::to_vec(&current).unwrap(),
            earlier(0, "r"),
            earlier(5, "r"),
            br#"{"version":"3"}"#.to_vec(),
            br#"[3]"#.to_vec(),
            b"damaged".to_vec(),
            oversized,
        ] {
            let f = fixture(&kept).await;
            assert!(record(&f.state).is_none());
            assert!(f.state.join(INITIALIZER_FILE).exists());
            assert!(f.canonical.join("domain.sqlite3").exists());
        }
    }

    #[tokio::test]
    async fn a_database_that_stays_keeps_the_record_that_still_points_at_it() {
        let f = fixture(&earlier(3, "shared-registry")).await;
        let found = record(&f.state).unwrap();
        // The store kept the database: a profile of this build uses it.
        let kept = discard(&f.state, found, || Ok(false), || Ok("shared-registry".into()));
        assert_eq!(kept.unwrap_err().code, "current_profile_initializer_unavailable");
        assert!(f.state.join(INITIALIZER_FILE).exists());
        assert!(f.owner.join(JOURNALS[0]).exists());
        // The record names a database that an earlier discard already removed.
        let found = record(&f.state).unwrap();
        discard(&f.state, found, || Ok(false), || Ok("new-registry".into())).unwrap();
        assert!(read_descriptor(&f.state).unwrap().is_none());
        assert!(!f.owner.join(JOURNALS[0]).exists());
        assert!(f.canonical.join("domain.sqlite3").exists());
    }

    #[tokio::test]
    async fn a_database_that_is_open_elsewhere_postpones_the_discard() {
        let f = fixture(&earlier(3, "old-registry")).await;
        let found = record(&f.state).unwrap();
        let busy = discard(&f.state, found, || Err(hagency_store::Error::Locked), unused);
        assert_eq!(busy.unwrap_err().code, "current_profile_initializer_unavailable");
        assert!(f.state.join(INITIALIZER_FILE).exists());
        assert!(f.owner.join(JOURNALS[0]).exists());
        assert!(f.canonical.join("domain.sqlite3").exists());
    }
}
