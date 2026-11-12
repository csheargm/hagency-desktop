//! Host-only native-session archive. Work trees are deliberately excluded:
//! rebuilding an unchanged dependency tree is not part of ending a turn.
use super::{NativeError, hash, remove, store, unavailable};
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use hagency_store::private;
use matrix_sdk_store_encryption::StoreCipher;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

const CHUNK: usize = 32 * 1024;
const MAX_ENTRIES: usize = 8192;
const MAX_CHUNKS: usize = 32768;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    version: u8,
    pub scope: String,
    pub directory: String,
    pub assignment: String,
    pub generation: u64,
    entries: Vec<Entry>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: String,
    directory: bool,
    executable: bool,
    chunks: Vec<String>,
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Block {
    scope: String,
    path: String,
    index: usize,
    bytes: Vec<u8>,
}
fn block_id(cipher: &StoreCipher, block: &Block) -> Result<String, NativeError> {
    let plain = serde_json::to_vec(block).map_err(|_| unavailable())?;
    Ok(cipher
        .hash_key("hagency-shared-claude-native-v1", &plain)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

pub(super) fn cipher(root: &Path) -> Result<StoreCipher, NativeError> {
    let path = root.join("store-key");
    let mut key = match private::read_secret(&path) {
        Ok(bytes) => bytes,
        Err(hagency_store::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            if root.join("cipher-key").exists() || root.join("sealed").exists() {
                return Err(unavailable());
            }
            let mut key = vec![0; 32];
            getrandom::fill(&mut key).map_err(|_| unavailable())?;
            private::write_new(&path, &key).map_err(store)?;
            key
        }
        Err(error) => return Err(store(error)),
    };
    if key.len() != 32 {
        return Err(unavailable());
    }
    let path = root.join("cipher-key");
    let result = if path.exists() {
        StoreCipher::import_with_key(&key, &read(&path, 1024)?).map_err(|_| unavailable())
    } else {
        if root.join("sealed").is_dir()
            && std::fs::read_dir(root.join("sealed"))
                .map_err(|_| unavailable())?
                .next()
                .is_some()
        {
            return Err(unavailable());
        }
        let cipher = StoreCipher::new().map_err(|_| unavailable())?;
        private::write_new(
            &path,
            &cipher.export_with_key(&key).map_err(|_| unavailable())?,
        )
        .map_err(store)?;
        Ok(cipher)
    };
    key.fill(0);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, StoreCipher) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let source = root.join("a".repeat(64));
        private::create_directory_new(&source).unwrap();
        for name in ["home", "config", "work", "tmp"] {
            private::create_directory_new(&source.join(name)).unwrap();
        }
        let archive = root.join("archive");
        let cipher = StoreCipher::new().unwrap();
        (tmp, source, archive, cipher)
    }
    #[test]
    fn shared_claude_archive_is_private_incremental_and_excludes_dependency_tree() {
        let (_tmp, source, archive, cipher) = fixture();
        let scope = "a".repeat(64);
        std::fs::write(source.join("config/session"), vec![b'x'; CHUNK * 2 + 11]).unwrap();
        std::fs::create_dir(source.join("work/node_modules")).unwrap();
        std::fs::write(
            source.join("work/node_modules/unrelated"),
            "not native session state",
        )
        .unwrap();
        seal(&cipher, &source, &scope, "assignment", 1, &archive).unwrap();
        let first = manifest(&cipher, &archive).unwrap().unwrap();
        assert!(first.entries.iter().all(|e| !e.path.starts_with("work")));
        let blocks = first
            .entries
            .iter()
            .flat_map(|e| &e.chunks)
            .map(|id| (id.clone(), std::fs::read(archive.join(id)).unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(blocks.len(), 3);
        seal(&cipher, &source, &scope, "assignment", 1, &archive).unwrap();
        for (id, bytes) in &blocks {
            assert_eq!(
                &std::fs::read(archive.join(id)).unwrap(),
                bytes,
                "unchanged blocks are not encrypted again"
            );
        }
        assert!(restore(&cipher, &source, &scope, "assignment", 1, &archive).unwrap());
        assert_eq!(
            std::fs::read(source.join("config/session")).unwrap(),
            vec![b'x'; CHUNK * 2 + 11]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(source.join("config/session"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(source.join("config"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn shared_claude_archive_never_follows_symlinks_and_failed_seal_keeps_commit() {
        let (tmp, source, archive, cipher) = fixture();
        let scope = "a".repeat(64);
        std::fs::write(source.join("config/session"), "previous native id").unwrap();
        seal(&cipher, &source, &scope, "assignment", 1, &archive).unwrap();
        let committed = std::fs::read(archive.join("manifest")).unwrap();
        let outside = tmp.path().join("owner-secret");
        std::fs::write(&outside, "OWNER_SECRET").unwrap();
        std::os::unix::fs::symlink(&outside, source.join("config/escape")).unwrap();
        assert!(seal(&cipher, &source, &scope, "assignment", 1, &archive).is_err());
        assert_eq!(std::fs::read(archive.join("manifest")).unwrap(), committed);
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "OWNER_SECRET");
        assert!(restore(&cipher, &source, &scope, "assignment", 1, &archive).unwrap());
        assert!(!source.join("config/escape").exists());
        assert_eq!(
            std::fs::read_to_string(source.join("config/session")).unwrap(),
            "previous native id"
        );
        std::fs::hard_link(
            tmp.path().join("owner-secret"),
            source.join("config/hardlink"),
        )
        .unwrap();
        assert!(seal(&cipher, &source, &scope, "assignment", 1, &archive).is_err());
        assert_eq!(std::fs::read(archive.join("manifest")).unwrap(), committed);
    }
    #[test]
    fn shared_claude_archive_refuses_path_escape_corrupt_blocks_and_wrong_scope() {
        let (tmp, source, archive, cipher) = fixture();
        let scope = "a".repeat(64);
        std::fs::write(source.join("config/session"), "native id").unwrap();
        seal(&cipher, &source, &scope, "assignment", 1, &archive).unwrap();
        assert!(restore(&cipher, &source, &"b".repeat(64), "assignment", 1, &archive).is_err());
        let mut m = manifest(&cipher, &archive).unwrap().unwrap();
        let block = m
            .entries
            .iter()
            .flat_map(|e| &e.chunks)
            .next()
            .unwrap()
            .clone();
        let original = std::fs::read(archive.join(&block)).unwrap();
        private::replace(&archive.join(&block), b"corrupt").unwrap();
        assert!(restore(&cipher, &source, &scope, "assignment", 1, &archive).is_err());
        assert!(!source.join("config").exists());
        private::replace(&archive.join(&block), &original).unwrap();
        m.entries.push(Entry {
            path: "../outside".into(),
            directory: false,
            executable: false,
            chunks: vec![block],
        });
        private::replace(
            &archive.join("manifest"),
            &cipher.encrypt_value(&m).unwrap(),
        )
        .unwrap();
        assert!(restore(&cipher, &source, &scope, "assignment", 1, &archive).is_err());
        assert!(!tmp.path().join("outside").exists());
    }
    #[test]
    fn shared_claude_archive_ignores_uncommitted_blocks_and_refuses_missing_committed_data() {
        let (_tmp, source, archive, cipher) = fixture();
        let scope = "a".repeat(64);
        private::directory(&archive).unwrap();
        private::write_new(&archive.join("b".repeat(64)), b"incomplete encrypted block").unwrap();
        assert!(!restore(&cipher, &source, &scope, "assignment", 1, &archive).unwrap());
        std::fs::write(source.join("config/session"), "committed native id").unwrap();
        seal(&cipher, &source, &scope, "assignment", 1, &archive).unwrap();
        let m = manifest(&cipher, &archive).unwrap().unwrap();
        let id = m.entries.iter().flat_map(|e| &e.chunks).next().unwrap();
        std::fs::remove_file(archive.join(id)).unwrap();
        assert!(restore(&cipher, &source, &scope, "assignment", 1, &archive).is_err());
        assert!(!source.join("config").exists());
        assert!(
            archive.join("manifest").exists(),
            "failed restore cannot replace original custody"
        );
    }
    #[test]
    fn shared_claude_archive_rejects_replacing_a_current_block_with_valid_old_ciphertext() {
        let (_tmp, source, archive, cipher) = fixture();
        let scope = "a".repeat(64);
        std::fs::write(source.join("config/session"), "old").unwrap();
        seal(&cipher, &source, &scope, "assignment", 1, &archive).unwrap();
        let old = manifest(&cipher, &archive).unwrap().unwrap();
        let id = old.entries.iter().flat_map(|e| &e.chunks).next().unwrap();
        let old_ciphertext = std::fs::read(archive.join(id)).unwrap();
        std::fs::write(source.join("config/session"), "new").unwrap();
        seal(&cipher, &source, &scope, "assignment", 1, &archive).unwrap();
        let current = manifest(&cipher, &archive).unwrap().unwrap();
        let current_id = current
            .entries
            .iter()
            .flat_map(|e| &e.chunks)
            .next()
            .unwrap();
        assert_ne!(current_id, id);
        private::replace(&archive.join(current_id), &old_ciphertext).unwrap();
        assert!(seal(&cipher, &source, &scope, "assignment", 1, &archive).is_err());
        assert!(restore(&cipher, &source, &scope, "assignment", 1, &archive).is_err());
        assert!(!source.join("config").exists());
    }
}
fn read(path: &Path, limit: usize) -> Result<Vec<u8>, NativeError> {
    let file = private::open(path, false).map_err(store)?;
    if file.metadata().map_err(|_| unavailable())?.len() > limit as u64 {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    if bytes.len() > limit {
        return Err(unavailable());
    }
    Ok(bytes)
}
fn relative(value: &str) -> Result<&Path, NativeError> {
    let path = Path::new(value);
    if value.len() > 4096
        || path
            .components()
            .any(|p| !matches!(p, Component::Normal(_)))
        || !matches!(path.components().next(), Some(Component::Normal(v)) if v == "config" || v == "home")
    {
        return Err(unavailable());
    }
    Ok(path)
}
pub(super) fn manifest(
    cipher: &StoreCipher,
    archive: &Path,
) -> Result<Option<Manifest>, NativeError> {
    let path = archive.join("manifest");
    if !path.try_exists().map_err(|_| unavailable())? {
        return Ok(None);
    }
    let m: Manifest = cipher
        .decrypt_value(&read(&path, 16 * 1024 * 1024)?)
        .map_err(|_| unavailable())?;
    if m.version != 1 || m.generation == 0 || m.entries.len() > MAX_ENTRIES {
        return Err(unavailable());
    }
    hash(&m.scope)?;
    hash(&m.directory)?;
    let mut seen = BTreeSet::new();
    let mut count = 0;
    for entry in &m.entries {
        relative(&entry.path)?;
        if !seen.insert(&entry.path)
            || (entry.directory && (!entry.chunks.is_empty() || entry.executable))
        {
            return Err(unavailable());
        }
        count += entry.chunks.len();
        if count > MAX_CHUNKS {
            return Err(unavailable());
        }
        for chunk in &entry.chunks {
            hash(chunk)?;
        }
    }
    Ok(Some(m))
}
pub(super) fn seal(
    cipher: &StoreCipher,
    root: &Path,
    scope: &str,
    assignment: &str,
    generation: u64,
    archive: &Path,
) -> Result<(), NativeError> {
    private::directory(archive).map_err(store)?;
    let source = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())
        .map_err(|_| unavailable())?;
    let mut m = Manifest {
        version: 1,
        scope: scope.into(),
        directory: root
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(unavailable)?
            .into(),
        assignment: assignment.into(),
        generation,
        entries: Vec::new(),
    };
    hash(&m.directory)?;
    let mut pending = vec![PathBuf::from("home"), PathBuf::from("config")];
    let mut total = 0;
    while let Some(path) = pending.pop() {
        if m.entries.len() >= MAX_ENTRIES {
            return Err(unavailable());
        }
        let meta = source.symlink_metadata(&path).map_err(|_| unavailable())?;
        // Never follow an agent-controlled link, device, socket or FIFO. In
        // particular, archive creation is not an ambient host file-read tool.
        if meta.file_type().is_symlink() || (!meta.is_dir() && !meta.is_file()) {
            return Err(unavailable());
        }
        let name = path.to_str().ok_or_else(unavailable)?.to_owned();
        relative(&name)?;
        let mut entry = Entry {
            path: name.clone(),
            directory: meta.is_dir(),
            executable: false,
            chunks: Vec::new(),
        };
        if meta.is_dir() {
            let dir = source.open_dir(&path).map_err(|_| unavailable())?;
            let mut children = dir
                .entries()
                .map_err(|_| unavailable())?
                .map(|e| e.map(|e| path.join(e.file_name())))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| unavailable())?;
            children.sort();
            pending.extend(children.into_iter().rev());
        } else {
            let mut options = cap_std::fs::OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            let mut file = source
                .open_with(&path, &options)
                .map_err(|_| unavailable())?;
            if !file.metadata().map_err(|_| unavailable())?.is_file() {
                return Err(unavailable());
            }
            #[cfg(unix)]
            {
                use cap_std::fs::{MetadataExt, PermissionsExt};
                // A link inside the scope must not turn the host archiver into
                // a reader for an inode also named outside the scope.
                if file.metadata().map_err(|_| unavailable())?.nlink() != 1 {
                    return Err(unavailable());
                }
                entry.executable = meta.permissions().mode() & 0o100 != 0;
            }
            loop {
                let mut bytes = vec![0; CHUNK];
                let n = file.read(&mut bytes).map_err(|_| unavailable())?;
                if n == 0 {
                    break;
                }
                bytes.truncate(n);
                total += 1;
                if total > MAX_CHUNKS {
                    return Err(unavailable());
                }
                let block = Block {
                    scope: scope.into(),
                    path: name.clone(),
                    index: entry.chunks.len(),
                    bytes,
                };
                let id = block_id(cipher, &block)?;
                let target = archive.join(&id);
                if !target.try_exists().map_err(|_| unavailable())? {
                    private::write_new(
                        &target,
                        &cipher.encrypt_value(&block).map_err(|_| unavailable())?,
                    )
                    .map_err(store)?;
                } else {
                    let retained: Block = cipher
                        .decrypt_value(&read(&target, 1024 * 1024)?)
                        .map_err(|_| unavailable())?;
                    if retained != block {
                        return Err(unavailable());
                    }
                }
                entry.chunks.push(id);
            }
        }
        m.entries.push(entry);
    }
    let encrypted = cipher.encrypt_value(&m).map_err(|_| unavailable())?;
    if encrypted.len() > 16 * 1024 * 1024 {
        return Err(unavailable());
    }
    // All immutable blocks are durable before this atomic commit. A failed or
    // interrupted seal leaves the preceding complete manifest usable.
    private::replace(&archive.join("manifest"), &encrypted).map_err(store)?;
    let live: BTreeSet<_> = m
        .entries
        .iter()
        .flat_map(|e| e.chunks.iter().map(String::as_str))
        .chain(std::iter::once("manifest"))
        .collect();
    for entry in std::fs::read_dir(archive).map_err(|_| unavailable())? {
        let entry = entry.map_err(|_| unavailable())?;
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| live.contains(name))
        {
            remove(&entry.path())?;
        }
    }
    Ok(())
}
pub(super) fn restore(
    cipher: &StoreCipher,
    root: &Path,
    scope: &str,
    assignment: &str,
    generation: u64,
    archive: &Path,
) -> Result<bool, NativeError> {
    let Some(m) = manifest(cipher, archive)? else {
        return Ok(false);
    };
    if m.scope != scope
        || m.assignment != assignment
        || m.generation != generation
        || root.file_name().and_then(|n| n.to_str()) != Some(m.directory.as_str())
    {
        return Err(unavailable());
    }
    // Destination starts empty and private. Neither manifest paths nor stored
    // links can redirect restoration into an owner file or another scope.
    for name in ["home", "config"] {
        remove(&root.join(name))?;
    }
    let work = (|| {
        for entry in m.entries {
            let path = root.join(relative(&entry.path)?);
            if entry.directory {
                private::create_directory_new(&path).map_err(store)?;
                continue;
            }
            let mut file = private::open(&path, true).map_err(store)?;
            for (index, id) in entry.chunks.iter().enumerate() {
                let block: Block = cipher
                    .decrypt_value(&read(&archive.join(id), 1024 * 1024)?)
                    .map_err(|_| unavailable())?;
                if block.scope != scope
                    || block.path != entry.path
                    || block.index != index
                    || block.bytes.is_empty()
                    || block.bytes.len() > CHUNK
                    || block_id(cipher, &block)? != *id
                {
                    return Err(unavailable());
                }
                file.write_all(&block.bytes).map_err(|_| unavailable())?;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(std::fs::Permissions::from_mode(if entry.executable {
                    0o700
                } else {
                    0o600
                }))
                .map_err(|_| unavailable())?;
            }
            file.sync_all().map_err(|_| unavailable())?;
        }
        if !root.join("home").is_dir() || !root.join("config").is_dir() {
            return Err(unavailable());
        }
        Ok(true)
    })();
    if work.is_err() {
        for name in ["home", "config"] {
            let _ = remove(&root.join(name));
        }
    }
    work
}
