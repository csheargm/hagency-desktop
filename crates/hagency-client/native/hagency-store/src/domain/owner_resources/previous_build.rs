//! Start-fresh rule for a canonical resource database left by an earlier
//! Hagency build. This build cannot continue that build's profile plans, so
//! their rows block set-up for good. The product decision is to delete such a
//! database at start, without asking, instead of leaving it to a script.
//!
//! Nothing is deleted while the database is open anywhere, and nothing is
//! deleted once a profile initialized by this build uses it.
use super::*;

const DATABASE: &str = "domain.sqlite3";
/// The database, SQLite's companions, and the earlier build's refusal marker.
/// The directory, its locks and the owner-root records stay.
const FILES: [&str; 5] = [
    DATABASE,
    "domain.sqlite3-wal",
    "domain.sqlite3-shm",
    "domain.sqlite3-journal",
    "legacy-resource-refusal.json",
];
const PLAN_LIMIT: usize = 1024 * 1024;

fn missing(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
}

/// Whether some profile in this database was initialized by this build. A
/// plan that cannot be read counts as in use: doubt never deletes.
fn in_use_by_this_build(db: &rusqlite::Connection) -> Result<bool, Error> {
    let table: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='owner_resource_adoptions')",
        [],
        |r| r.get(0),
    )?;
    if !table {
        return Ok(false);
    }
    let mut plans = db.prepare(&format!(
        "SELECT CASE WHEN length(CAST(plan AS BLOB))<={PLAN_LIMIT} THEN plan END FROM owner_resource_adoptions"
    ))?;
    let mut rows = plans.query([])?;
    while let Some(row) = rows.next()? {
        let Some(plan) = row.get::<_, Option<String>>(0)? else {
            return Ok(true);
        };
        match serde_json::from_str::<serde_json::Value>(&plan) {
            Ok(plan) if plan["kind"] != "current_profile" => {}
            _ => return Ok(true),
        }
    }
    Ok(false)
}

fn discard(root: &Path) -> Result<bool, Error> {
    match std::fs::symlink_metadata(root) {
        Err(e) if missing(&e) => return Ok(true),
        Err(e) => return Err(Error::Io(e)),
        Ok(_) => {}
    }
    crate::private::directory(root)?;
    let root = root.canonicalize()?;
    // Held to the end, so no open in this process can start meanwhile.
    let registries = REGISTRIES.lock().map_err(|_| Error::Unavailable)?;
    if registries.get(&root).and_then(Weak::upgrade).is_some() {
        return Err(Error::Locked);
    }
    // The lock every opener of this database holds for as long as it is open.
    let lock_path = root.join("domain.lock");
    let lock = match crate::private::open(&lock_path, true) {
        Ok(lock) => lock,
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            crate::private::open(&lock_path, false)?
        }
        Err(e) => return Err(e),
    };
    lock.try_lock().map_err(|_| Error::Locked)?;
    let file = root.join(DATABASE);
    match std::fs::symlink_metadata(&file) {
        Err(e) if missing(&e) => {}
        Err(e) => return Err(Error::Io(e)),
        Ok(_) => {
            crate::private::open(&file, false)?;
            let db = rusqlite::Connection::open_with_flags(
                &file,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            if in_use_by_this_build(&db)? {
                return Ok(false);
            }
        }
    }
    for name in FILES {
        match std::fs::remove_file(root.join(name)) {
            Ok(()) => {}
            Err(e) if missing(&e) => {}
            Err(e) => return Err(Error::Io(e)),
        }
    }
    drop(lock);
    drop(registries);
    Ok(true)
}

impl OwnerResourceRegistry {
    /// Deletes the OS owner's canonical resource database when no profile of
    /// this build uses it. `Ok(true)`: the database is gone and the next open
    /// starts an empty one. `Ok(false)`: a profile of this build uses it and
    /// it was kept. `Err(Locked)`: it is open in this or another process and
    /// was kept. Call from a blocking task.
    pub fn discard_previous_build() -> Result<bool, Error> {
        discard(&hagency_platform::owner_home()?.join(".hagency-owner-resources"))
    }
    /// Isolated fixture only. Absent from production/default builds.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn discard_previous_build_for_tests(root: &Path) -> Result<bool, Error> {
        discard(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn registry_with_plan(root: &Path, plan: &'static str) -> OwnerResourceRegistry {
        let registry = OwnerResourceRegistry::open_root(root).unwrap();
        registry
            .0
            .store
            .call(4096, move |db| {
                db.db.execute(
                    "INSERT INTO owner_resource_adoptions(id,profile,plan_digest,plan,phase) VALUES('plan',?1,?2,?3,0)",
                    params!["a".repeat(64), "b".repeat(64), plan],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        registry
    }
    async fn close(registry: OwnerResourceRegistry) {
        registry.shutdown().await.unwrap();
        drop(registry);
    }
    fn present(root: &Path) -> Vec<&'static str> {
        FILES
            .into_iter()
            .filter(|name| root.join(name).exists())
            .collect()
    }

    #[tokio::test]
    async fn a_database_from_an_earlier_build_is_removed_and_the_next_open_starts_empty() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("owner");
        let old = registry_with_plan(&root, r#"{"version":1,"kind":"empty_device"}"#).await;
        let old_id = old.id().to_owned();
        close(old).await;
        crate::private::write_new(&root.join("legacy-resource-refusal.json"), b"{}").unwrap();
        assert!(present(&root).contains(&DATABASE));
        assert!(discard(&root).unwrap());
        assert_eq!(present(&root), Vec::<&str>::new());
        // The directory and its lock are kept; only the database is new.
        assert!(root.join("domain.lock").exists());
        let fresh = OwnerResourceRegistry::open_root(&root).unwrap();
        assert_ne!(fresh.id(), old_id);
        let plans: i64 = fresh
            .0
            .store
            .call(4096, |db| {
                Ok(db
                    .db
                    .query_row("SELECT COUNT(*) FROM owner_resource_adoptions", [], |r| r.get(0))?)
            })
            .await
            .unwrap();
        assert_eq!(plans, 0);
        close(fresh).await;
    }

    #[tokio::test]
    async fn a_database_a_profile_of_this_build_uses_is_kept() {
        for plan in [
            r#"{"version":1,"kind":"current_profile"}"#,
            // Doubt never deletes.
            "not json",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("owner");
            close(registry_with_plan(&root, plan).await).await;
            assert!(!discard(&root).unwrap(), "{plan}");
            assert!(present(&root).contains(&DATABASE), "{plan}");
        }
    }

    #[tokio::test]
    async fn nothing_is_removed_while_the_database_is_open() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("owner");
        let open = registry_with_plan(&root, r#"{"version":1,"kind":"empty_device"}"#).await;
        assert!(matches!(discard(&root), Err(Error::Locked)));
        assert!(present(&root).contains(&DATABASE));
        close(open).await;
        assert!(discard(&root).unwrap());
    }

    #[test]
    fn a_missing_or_empty_directory_needs_no_discard() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("owner");
        assert!(discard(&root).unwrap());
        assert!(!root.exists());
        crate::private::directory(&root).unwrap();
        assert!(discard(&root).unwrap());
    }
}
