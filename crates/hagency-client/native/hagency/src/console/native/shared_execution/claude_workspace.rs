//! Native sessions are encrypted while idle; each authorized child gets only
//! its own conversation scope. Project work trees remain stable across turns.
use super::{NativeError, failure, hash, store};
use matrix_sdk_store_encryption::StoreCipher;
use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
mod archive;
fn unavailable() -> NativeError {
    failure(409, "shared_claude_workspace_unavailable")
}
/// The server also answers `shared_assignment_authority_changed` for a paused
/// Agent and for a lapsed reservation. Both can be resumed, so that code does
/// not remove a conversation's saved session or work folder.
pub(super) fn scope_revoked(error: &NativeError) -> bool {
    matches!(
        error.code.as_str(),
        "provider_account_changed"
            | "shared_room_authority_changed"
            | "shared_room_binding_changed"
            | "shared_execution_enrollment_changed"
    )
}
#[derive(Default)]
struct State {
    closed: bool,
    roots: BTreeMap<PathBuf, Arc<Root>>,
    scopes: BTreeMap<PathBuf, Arc<Scope>>,
}
struct Root {
    path: PathBuf,
    cipher: StoreCipher,
    _lock: File,
}
struct Scope {
    root: Arc<Root>,
    path: PathBuf,
    assignment: String,
    generation: u64,
    state: Mutex<ScopeState>,
}
struct ScopeState {
    key: String,
    active: bool,
    retired: bool,
}
#[derive(Clone, Default)]
pub(super) struct Workspaces(Arc<Mutex<State>>);
pub(super) struct Lease {
    scope: Arc<Scope>,
}
fn remove(path: &Path) -> Result<(), NativeError> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {
            std::fs::remove_dir_all(path).map_err(|_| unavailable())
        }
        Ok(_) => std::fs::remove_file(path).map_err(|_| unavailable()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(unavailable()),
    }
}
fn clear_plaintext(path: &Path) -> Result<(), NativeError> {
    for name in ["config", "home", "tmp"] {
        remove(&path.join(name))?;
    }
    Ok(())
}
impl Root {
    fn open(root: &Path) -> Result<Self, NativeError> {
        hagency_store::private::directory(root).map_err(store)?;
        if root.canonicalize().ok().as_deref() != Some(root) {
            return Err(unavailable());
        }
        let lock_path = root.join("owner.lock");
        let lock = match hagency_store::private::open(&lock_path, false) {
            Ok(lock) => lock,
            Err(hagency_store::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                match hagency_store::private::open(&lock_path, true) {
                    Ok(lock) => lock,
                    Err(hagency_store::Error::Io(e))
                        if e.kind() == std::io::ErrorKind::AlreadyExists =>
                    {
                        hagency_store::private::open(&lock_path, false).map_err(store)?
                    }
                    Err(error) => return Err(store(error)),
                }
            }
            Err(error) => return Err(store(error)),
        };
        lock.try_lock().map_err(|_| unavailable())?;
        let cipher = archive::cipher(root)?;
        for name in ["scopes", "sealed"] {
            hagency_store::private::directory(&root.join(name)).map_err(store)?;
        }
        // Only ephemeral native files are swept. Completed encrypted sessions
        // and normal work trees survive daemon shutdown and normal restart.
        for entry in std::fs::read_dir(root.join("scopes")).map_err(|_| unavailable())? {
            let entry = entry.map_err(|_| unavailable())?;
            if entry.file_type().map_err(|_| unavailable())?.is_symlink() {
                remove(&entry.path())?;
                continue;
            }
            hash(entry.file_name().to_str().ok_or_else(unavailable)?)?;
            clear_plaintext(&entry.path())?;
        }
        Ok(Self {
            path: root.into(),
            cipher,
            _lock: lock,
        })
    }
    /// Removes what an earlier generation of this assignment left behind.
    fn superseded(
        &self,
        assignment: &str,
        generation: u64,
        active: &[PathBuf],
    ) -> Result<(), NativeError> {
        for entry in std::fs::read_dir(self.path.join("sealed")).map_err(|_| unavailable())? {
            let entry = entry.map_err(|_| unavailable())?;
            let name = entry.file_name();
            let key = name.to_str().ok_or_else(unavailable)?;
            hash(key)?;
            let Some(m) = archive::manifest(&self.cipher, &entry.path())? else {
                remove(&entry.path())?;
                continue;
            };
            if m.scope != key {
                return Err(unavailable());
            }
            let path = self.path.join("scopes").join(&m.directory);
            if active.contains(&path) {
                continue;
            }
            if m.assignment == assignment && m.generation != generation {
                remove(&entry.path())?;
                remove(&path)?;
            }
        }
        Ok(())
    }
}
impl Scope {
    fn archive(&self, key: &str) -> PathBuf {
        self.root.path.join("sealed").join(key)
    }
    fn retire(&self) -> Result<(), NativeError> {
        let mut state = self.state.lock().map_err(|_| unavailable())?;
        state.retired = true;
        if !state.active {
            remove(&self.archive(&state.key))?;
            remove(&self.path)?;
        }
        Ok(())
    }
    fn prepare(&self, key: &str) -> Result<(), NativeError> {
        hagency_store::private::directory(&self.path).map_err(store)?;
        hagency_store::private::directory(&self.path.join("work")).map_err(store)?;
        clear_plaintext(&self.path)?;
        if !archive::restore(
            &self.root.cipher,
            &self.path,
            key,
            &self.assignment,
            self.generation,
            &self.archive(key),
        )? {
            for name in ["home", "config"] {
                hagency_store::private::create_directory_new(&self.path.join(name))
                    .map_err(store)?;
            }
        }
        hagency_store::private::create_directory_new(&self.path.join("tmp")).map_err(store)?;
        Ok(())
    }
}
impl Workspaces {
    pub(super) fn sweep_existing(&self, state_root: &Path) -> Result<(), NativeError> {
        let owners = state_root.join("owned-agent-owners");
        if !owners.try_exists().map_err(|_| unavailable())? {
            return Ok(());
        }
        hagency_store::private::directory(&owners).map_err(store)?;
        let mut state = self.0.lock().map_err(|_| unavailable())?;
        if state.closed {
            return Err(unavailable());
        }
        for entry in std::fs::read_dir(owners).map_err(|_| unavailable())? {
            let entry = entry.map_err(|_| unavailable())?;
            let name = entry.file_name();
            let Some(key) = name.to_str().and_then(|name| name.strip_prefix("owner_")) else {
                continue;
            };
            hash(key)?;
            if !entry.file_type().map_err(|_| unavailable())?.is_dir() {
                return Err(unavailable());
            }
            hagency_store::private::directory(&entry.path()).map_err(store)?;
            let root = entry.path().join("managed-claude");
            if root.try_exists().map_err(|_| unavailable())? && !state.roots.contains_key(&root) {
                state
                    .roots
                    .insert(root.clone(), Arc::new(Root::open(&root)?));
            }
        }
        Ok(())
    }
    pub(super) fn open(
        &self,
        root: &Path,
        key: &str,
        assignment: &str,
        generation: u64,
    ) -> Result<Lease, NativeError> {
        hash(key)?;
        if generation == 0 {
            return Err(unavailable());
        }
        let mut state = self.0.lock().map_err(|_| unavailable())?;
        if state.closed {
            return Err(unavailable());
        }
        let root = match state.roots.get(root) {
            Some(v) => v.clone(),
            None => {
                let owned = Arc::new(Root::open(root)?);
                state.roots.insert(root.into(), owned.clone());
                owned
            }
        };
        for old in state
            .scopes
            .values()
            .filter(|s| s.assignment == assignment && s.generation != generation)
        {
            old.retire()?;
        }
        let active = state
            .scopes
            .values()
            .filter_map(|s| {
                s.state
                    .lock()
                    .ok()
                    .filter(|s| s.active)
                    .map(|_| s.path.clone())
            })
            .collect::<Vec<_>>();
        root.superseded(assignment, generation, &active)?;
        let directory = match archive::manifest(&root.cipher, &root.path.join("sealed").join(key))?
        {
            Some(m)
                if m.scope == key && m.assignment == assignment && m.generation == generation =>
            {
                m.directory
            }
            Some(_) => return Err(unavailable()),
            None => key.into(),
        };
        let path = root.path.join("scopes").join(directory);
        let retired = state
            .scopes
            .get(&path)
            .map(|scope| {
                scope
                    .state
                    .lock()
                    .map(|s| s.retired && !s.active)
                    .map_err(|_| unavailable())
            })
            .transpose()?
            .unwrap_or(false);
        if retired {
            state.scopes.remove(&path);
        }
        if let Some(scope) = state.scopes.get(&path) {
            let mut status = scope.state.lock().map_err(|_| unavailable())?;
            if status.active
                || status.retired
                || status.key != key
                || scope.assignment != assignment
                || scope.generation != generation
            {
                return Err(unavailable());
            }
            scope.prepare(key)?;
            status.active = true;
            return Ok(Lease {
                scope: scope.clone(),
            });
        }
        let scope = Arc::new(Scope {
            root,
            path: path.clone(),
            assignment: assignment.into(),
            generation,
            state: Mutex::new(ScopeState {
                key: key.into(),
                active: true,
                retired: false,
            }),
        });
        scope.prepare(key)?;
        state.scopes.insert(path, scope.clone());
        Ok(Lease { scope })
    }
    pub(super) fn close(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.closed = true;
        }
    }
}
impl Lease {
    pub(super) fn path(&self) -> &Path {
        &self.scope.path
    }
    pub(super) fn retire(&self) {
        let _ = self.scope.retire();
    }
    /// The first bootstrap learns its exact room only after durable work
    /// admission. Preserve its physical cwd; the encrypted host manifest binds
    /// that directory to the complete authenticated conversation scope.
    pub(super) fn bind_admitted_scope(&self, key: &str) -> Result<(), NativeError> {
        hash(key)?;
        let mut state = self.scope.state.lock().map_err(|_| unavailable())?;
        if !state.active || state.retired {
            return Err(unavailable());
        }
        if state.key == key {
            return Ok(());
        }
        if self
            .scope
            .archive(&state.key)
            .try_exists()
            .map_err(|_| unavailable())?
            || self
                .scope
                .archive(key)
                .try_exists()
                .map_err(|_| unavailable())?
        {
            return Err(unavailable());
        }
        state.key = key.into();
        Ok(())
    }
    /// Call only after the retained process owner proved the entire tree stopped.
    pub(super) fn seal_stopped(&self) -> Result<(), NativeError> {
        let state = self.scope.state.lock().map_err(|_| unavailable())?;
        if state.retired {
            return Ok(());
        }
        archive::seal(
            &self.scope.root.cipher,
            &self.scope.path,
            &state.key,
            &self.scope.assignment,
            self.scope.generation,
            &self.scope.archive(&state.key),
        )?;
        clear_plaintext(&self.scope.path)
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.scope.state.lock() {
            state.active = false;
            if state.retired {
                let _ = remove(&self.scope.archive(&state.key));
                let _ = remove(&self.scope.path);
            } else {
                let _ = clear_plaintext(&self.scope.path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf, Workspaces) {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap().join("native");
        (tmp, base, Workspaces::default())
    }
    #[test]
    fn shared_claude_plaintext_is_scoped_and_removed_on_authority_loss() {
        let (_tmp, base, workspaces) = fixture();
        let a = workspaces
            .open(&base, &"a".repeat(64), "assignment", 1)
            .unwrap();
        let b = workspaces
            .open(&base, &"b".repeat(64), "other-assignment", 1)
            .unwrap();
        let path = a.path().to_path_buf();
        std::fs::write(path.join("config/session"), "encrypted-room-plaintext").unwrap();
        a.retire();
        assert!(path.exists()); // The running scope retains custody until stop.
        drop(a);
        assert!(!path.exists());
        assert!(b.path().exists());
        workspaces.close();
        let other = b.path().to_path_buf();
        drop(b);
        assert!(!other.join("config").exists());
        assert!(other.join("work").exists());
    }
    #[test]
    fn shared_claude_generation_and_orphan_sweep_never_adopt_old_text() {
        let (_tmp, base, workspaces) = fixture();
        let old = workspaces
            .open(&base, &"a".repeat(64), "assignment", 1)
            .unwrap();
        let old_path = old.path().to_path_buf();
        std::fs::write(old_path.join("config/session"), "old generation native id").unwrap();
        old.seal_stopped().unwrap();
        drop(old);
        drop(workspaces);
        let workspaces = Workspaces::default();
        let current = workspaces
            .open(&base, &"b".repeat(64), "assignment", 2)
            .unwrap();
        assert!(!old_path.exists());
        assert!(!base.join("sealed").join("a".repeat(64)).exists());
        assert!(
            Workspaces::default()
                .open(&base, &"c".repeat(64), "other", 1)
                .is_err()
        );
        drop(current);
        drop(workspaces);
        let orphan = base.join("scopes").join("d".repeat(64)).join("config");
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::write(orphan.join("text"), "orphan").unwrap();
        let clean = Workspaces::default();
        let lease = clean.open(&base, &"c".repeat(64), "assignment", 2).unwrap();
        assert!(!orphan.exists());
        assert!(lease.path().exists());
    }
    #[test]
    fn shared_claude_archive_preserves_native_session_across_normal_host_restart() {
        let (_tmp, base, workspaces) = fixture();
        let key = "a".repeat(64);
        let session = workspaces.open(&base, &key, "assignment", 1).unwrap();
        let native_id = "01234567-89ab-cdef-0123-456789abcdef";
        let path = session.path().to_path_buf();
        std::fs::write(
            path.join("config/session.jsonl"),
            format!("{{\"sessionId\":\"{native_id}\",\"message\":\"PRIVATE_CHAT\"}}\n"),
        )
        .unwrap();
        std::fs::write(path.join("work/project"), "normal workspace").unwrap();
        session.seal_stopped().unwrap();
        assert!(!path.join("config").exists());
        workspaces.close();
        drop(session);
        drop(workspaces);
        let reopened = Workspaces::default();
        let restored = reopened.open(&base, &key, "assignment", 1).unwrap();
        assert_eq!(restored.path(), path);
        assert!(
            std::fs::read_to_string(path.join("config/session.jsonl"))
                .unwrap()
                .contains(native_id)
        );
        assert_eq!(
            std::fs::read_to_string(path.join("work/project")).unwrap(),
            "normal workspace"
        );
        restored.retire();
        drop(restored);
        assert!(!path.exists());
        assert!(!base.join("sealed").join(key).exists());
    }
    #[test]
    fn shared_claude_bootstrap_binds_the_admitted_room_without_moving_native_cwd() {
        let (_tmp, base, workspaces) = fixture();
        let original = "a".repeat(64);
        let full = "b".repeat(64);
        let lease = workspaces.open(&base, &original, "assignment", 1).unwrap();
        let path = lease.path().to_owned();
        std::fs::write(path.join("config/native-id"), "same-native-session").unwrap();
        lease.bind_admitted_scope(&full).unwrap();
        lease.seal_stopped().unwrap();
        assert!(!base.join("sealed").join(original).exists());
        drop(lease);
        drop(workspaces);
        let reopened = Workspaces::default();
        let next = reopened.open(&base, &full, "assignment", 1).unwrap();
        assert_eq!(next.path(), path);
        assert_eq!(
            std::fs::read_to_string(path.join("config/native-id")).unwrap(),
            "same-native-session"
        );
    }
    #[test]
    fn shared_claude_startup_sweeps_orphan_text_before_any_scope_is_authorized() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let base = root
            .join("owned-agent-owners")
            .join(format!("owner_{}", "a".repeat(64)))
            .join("managed-claude");
        hagency_store::private::directory(base.parent().unwrap().parent().unwrap()).unwrap();
        hagency_store::private::directory(base.parent().unwrap()).unwrap();
        let owner = Workspaces::default();
        let lease = owner.open(&base, &"b".repeat(64), "assignment", 1).unwrap();
        std::fs::write(lease.path().join("config/session"), "committed native id").unwrap();
        lease.seal_stopped().unwrap();
        let path = lease.path().to_owned();
        drop(lease);
        drop(owner);
        hagency_store::private::directory(&path.join("config")).unwrap();
        std::fs::write(path.join("config/orphan"), "crash plaintext").unwrap();
        let reopened = Workspaces::default();
        reopened.sweep_existing(&root).unwrap();
        assert!(!path.join("config").exists());
        assert!(
            base.join("sealed")
                .join("b".repeat(64))
                .join("manifest")
                .exists()
        );
    }
    #[test]
    fn shared_claude_scope_revocation_does_not_confuse_stop_or_expiry_with_member_loss() {
        for code in [
            "shared_room_authority_changed",
            "shared_room_binding_changed",
            "provider_account_changed",
            "shared_execution_enrollment_changed",
        ] {
            assert!(scope_revoked(&failure(409, code)));
        }
        for code in [
            // What the server answers for a paused Agent or a lapsed reservation.
            "shared_assignment_authority_changed",
            "shared_assignment_not_running",
            "shared_budget_exhausted",
            "shared_capacity_exhausted",
            "shared_execution_closed",
            "management_scope_changed",
            "shared_activity_expired_or_changed",
            "shared_claude_version_unsupported",
            "server_unavailable",
            "shared_execution_expired",
        ] {
            assert!(!scope_revoked(&failure(409, code)));
        }
    }
}
