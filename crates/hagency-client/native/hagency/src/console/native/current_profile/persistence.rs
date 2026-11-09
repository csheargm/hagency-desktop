use super::*;
pub(super) fn persist_new(state: &Path, bytes: &[u8]) -> Result<(), NativeError> {
    persist_new_with(state, bytes, || Ok(()))
}
fn persist_new_with(
    state: &Path,
    bytes: &[u8],
    before_publish: impl FnOnce() -> Result<(), NativeError>,
) -> Result<(), NativeError> {
    use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
    use std::io::Write;
    let dir = cap_std::fs::Dir::open_ambient_dir(state, cap_std::ambient_authority())
        .map_err(|_| descriptor_error())?;
    hagency_store::private::check_handle(
        &dir.try_clone()
            .map_err(|_| descriptor_error())?
            .into_std_file(),
    )
    .map_err(|_| descriptor_error())?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| descriptor_error())?;
    let name = format!(
        ".current-profile-{}.tmp",
        random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let mut options = cap_std::fs::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = dir
            .open_with(&name, &options)
            .map_err(|_| descriptor_error())?
            .into_std();
        hagency_store::private::seal_created_file_handle(&file).map_err(|_| descriptor_error())?;
        file.write_all(bytes).map_err(|_| descriptor_error())?;
        file.sync_all().map_err(|_| descriptor_error())?;
        before_publish()?;
        hagency_platform::private_state::publish_new(&dir, &name, INITIALIZER_FILE)
            .map_err(|_| descriptor_error())
    })();
    // A failed publish cannot replace the original name. If publication happened
    // but directory-sync failed, the exact descriptor remains for recovery.
    let _ = dir.remove_file(&name);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn current_profile_atomic_initializer_never_publishes_partial_or_replaces_pending() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("private");
        hagency_store::private::directory(&state).unwrap();
        let path = state.join(INITIALIZER_FILE);
        assert!(
            persist_new_with(&state, b"original", || {
                assert!(!path.exists());
                Err(descriptor_error())
            })
            .is_err()
        );
        assert!(!path.exists());
        // An interrupted unpublished temporary has no initializer authority.
        hagency_store::private::write_new(&state.join(".current-profile-orphan.tmp"), b"partial")
            .unwrap();
        persist_new(&state, b"original").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        assert!(persist_new(&state, b"replacement").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
    }
}
