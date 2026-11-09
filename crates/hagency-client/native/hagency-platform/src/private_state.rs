#[cfg(windows)]
#[allow(unsafe_code)] // Audited retained-object synchronous Windows IO boundary.
mod windows_directory;
#[cfg(windows)]
pub use windows_directory::WindowsDirectorySync;

/// OS-private state failure, independent of any ledger or authorization layer.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("filesystem error")]
    Io(#[from] std::io::Error),
    #[error("state must be owner-private regular files and directories")]
    Private,
    #[error("private state on this platform is unavailable")]
    PlatformUnavailable,
}
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

pub fn directory(path: &Path) -> Result<(), Error> {
    if !path.exists() {
        match create_directory_new(path) {
            // Another owner task may create the same account directory between
            // the existence check and mkdir. Accept it only after applying all
            // ordinary ownership, permissions and no-symlink checks.
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                check_directory(path)
            }
            result => result,
        }
    } else {
        check_directory(path)
    }
}

/// Create one new private directory under an already provisioned parent.
/// Existing entries return the original AlreadyExists error without modification.
/// Windows assigns the current user and protected DACL during atomic creation.
pub fn create_directory_new(path: &Path) -> Result<(), Error> {
    #[cfg(windows)]
    windows::create_directory_new(path)?;
    #[cfg(unix)]
    fs::DirBuilder::new().mode(0o700).create(path)?;
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        return Err(Error::PlatformUnavailable);
    }
    #[cfg(any(unix, windows))]
    check_directory(path)
}

fn check_directory(path: &Path) -> Result<(), Error> {
    #[cfg(windows)]
    {
        windows::check_directory(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(Error::PlatformUnavailable)
    }
    #[cfg(unix)]
    {
        let meta = fs::symlink_metadata(path)?;
        if !meta.is_dir()
            || meta.file_type().is_symlink()
            || meta.mode() & 0o077 != 0
            || meta.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(Error::Private);
        }
        Ok(())
    }
}

fn check(file: &File, path: &Path, sqlite_journal: bool) -> Result<(), Error> {
    #[cfg(not(windows))]
    let _ = sqlite_journal;
    let meta = file.metadata()?;
    let path_meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || path_meta.file_type().is_symlink() {
        return Err(Error::Private);
    }
    #[cfg(unix)]
    if meta.mode() & 0o077 != 0
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.nlink() != 1
        || meta.ino() != path_meta.ino()
        || meta.dev() != path_meta.dev()
    {
        return Err(Error::Private);
    }
    #[cfg(windows)]
    windows::check_with_policy(file, path, sqlite_journal)?;
    #[cfg(not(any(unix, windows)))]
    return Err(Error::PlatformUnavailable);
    #[cfg(any(unix, windows))]
    Ok(())
}

/// Validate the actual retained handle, without resolving any ambient path.
/// This checks current owner permissions, not physical namespace provisioning or
/// protection against a malicious process sharing the same OS identity.
pub fn check_handle(file: &File) -> Result<(), Error> {
    let meta = file.metadata()?;
    if !(meta.is_file() || meta.is_dir()) || meta.file_type().is_symlink() {
        return Err(Error::Private);
    }
    #[cfg(unix)]
    if meta.mode() & 0o077 != 0
        || meta.uid() != rustix::process::geteuid().as_raw()
        || (meta.is_file() && meta.nlink() != 1)
    {
        return Err(Error::Private);
    }
    #[cfg(windows)]
    windows::check_handle(file, false)?;
    #[cfg(not(any(unix, windows)))]
    return Err(Error::PlatformUnavailable);
    #[cfg(any(unix, windows))]
    Ok(())
}

/// Sync the original directory, not an ambient path or Linux O_PATH descriptor.
/// This is the same retained-object adapter used by managed account publication.
pub fn sync_directory(dir: &cap_std::fs::Dir) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use cap_fs_ext::{
            FollowSymlinks, OpenOptionsFollowExt, OpenOptionsMaybeDirExt, OpenOptionsSyncExt,
        };
        let mut options = cap_std::fs::OpenOptions::new();
        options
            .read(true)
            .follow(FollowSymlinks::No)
            .maybe_dir(true)
            .nonblock(true);
        let file = dir.open_with(".", &options)?.into_std();
        check_handle(&file)?;
        if !crate::same_directory(&file, &dir.try_clone()?.into_std_file())? {
            return Err(Error::Private);
        }
        file.sync_all()?;
        Ok(())
    }
    #[cfg(windows)]
    {
        WindowsDirectorySync::open(dir)?
            .ok_or(Error::PlatformUnavailable)?
            .sync()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = dir;
        Err(Error::PlatformUnavailable)
    }
}

/// Creation-only adapter for the retained handle returned by successful
/// create_new. Call BEFORE any bytes are written, never for an existing object.
/// Windows assigns the current SID and a protected private DACL, then applies
/// the same strict checker. The host must prove create_new; length is not proof.
pub fn seal_created_file_handle(file: &File) -> Result<(), Error> {
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() != 0 {
        return Err(Error::Private);
    }
    #[cfg(windows)]
    windows::seal_created_file_handle(file)?;
    check_handle(file)?;
    if file.metadata()?.len() != 0 {
        return Err(Error::Private);
    }
    Ok(())
}

pub fn open(path: &Path, create: bool) -> Result<File, Error> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    #[cfg(windows)]
    if create {
        let file = windows::create_file(path)?;
        check(&file, path, false)?;
        return Ok(file);
    }
    let file = if create {
        options.create_new(true).open(path)?
    } else {
        options.open(path)?
    };
    check(&file, path, false)?;
    Ok(file)
}

/// SQLite auxiliary files inherit the private directory ACL. In an elevated
/// Windows process their owner can be Builtin Administrators; never accept a
/// different unprivileged owner or any additional ACL principal.
pub fn open_journal(path: &Path) -> Result<File, Error> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    check(&file, path, true)?;
    Ok(file)
}

/// Create-only so init cannot replace a live token. Existing state is never imported.
pub fn write_new(path: &Path, value: &[u8]) -> Result<(), Error> {
    let mut file = open(path, true)?;
    file.write_all(value)?;
    file.sync_all()?;
    #[cfg(unix)]
    File::open(path.parent().ok_or(Error::Private)?)?.sync_all()?;
    Ok(())
}

/// Replace an existing private file's contents atomically, or create it.
///
/// Unlike `write_new` (create-only, so `init` can never clobber live state),
/// this is the rotation path: the new bytes are written to a sibling temporary
/// file, synced, and renamed over the target. A crash therefore leaves either
/// the previous file or the new one — never a truncated credential that no
/// caller would accept. The replacement must already satisfy the same private
/// gate as the file it replaces, so a rotation cannot widen permissions.
pub fn replace(path: &Path, value: &[u8]) -> Result<(), Error> {
    let directory = path.parent().ok_or(Error::Private)?;
    // A distinct free name in the SAME directory, so the rename below is a
    // same-filesystem atomic replace rather than a copy.
    let mut suffix = [0u8; 8];
    getrandom::fill(&mut suffix).map_err(|_| Error::Private)?;
    let temporary = directory.join(format!(".{}.tmp", hex(&suffix)));
    {
        let mut file = open(&temporary, true)?;
        file.write_all(value)?;
        file.sync_all()?;
    }
    // `open` already checked the temporary's mode/owner/regularity; re-check
    // the destination after the swap, so a rotation that somehow landed a
    // non-private file reports failure instead of leaving it live.
    fs::rename(&temporary, path)?;
    let replaced = open(path, false)?;
    drop(replaced);
    #[cfg(unix)]
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn read_secret(path: &Path) -> Result<Vec<u8>, Error> {
    let file = open(path, false)?;
    if file.metadata()?.len() > 512 {
        return Err(Error::Private);
    }
    let mut bytes = Vec::new();
    file.take(513).read_to_end(&mut bytes)?;
    if bytes.len() > 512 {
        return Err(Error::Private);
    }
    Ok(bytes)
}

#[cfg(windows)]
#[allow(unsafe_code)] // Audited Windows FFI boundary; the rest of this crate denies unsafe.
mod windows;

/// The OS profile itself may have system/admin ACL entries. Validate its owner,
/// without treating it as an application-private directory.
#[cfg(windows)]
pub(crate) fn check_os_profile_owner(file: &File) -> Result<(), Error> {
    windows::check_os_profile_owner(file)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn native_private_retained_directory_sync() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("original");
        directory(&path).unwrap();
        let retained =
            cap_std::fs::Dir::open_ambient_dir(&path, cap_std::ambient_authority()).unwrap();
        // Reproduce the invalid descriptor on Linux; never accept it as sync.
        #[cfg(target_os = "linux")]
        assert_eq!(
            retained
                .try_clone()
                .unwrap()
                .into_std_file()
                .sync_all()
                .unwrap_err()
                .raw_os_error(),
            Some(9)
        );
        sync_directory(&retained).unwrap();
        let moved = temporary.path().join("held");
        fs::rename(&path, &moved).unwrap();
        directory(&path).unwrap();
        // A public ambient replacement must neither be opened nor grant sync.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        sync_directory(&retained).unwrap();
        fs::set_permissions(&moved, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(sync_directory(&retained), Err(Error::Private)));
    }

    #[test]
    fn native_private_directory_creation() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("private");
        directory(&parent).unwrap();
        let fresh = parent.join("fresh");
        create_directory_new(&fresh).unwrap();
        directory(&fresh).unwrap();
        let before = fs::metadata(&fresh).unwrap();
        assert_eq!(before.mode() & 0o777, 0o700);
        write_new(&fresh.join("original"), b"original bytes").unwrap();
        assert!(matches!(create_directory_new(&fresh), Err(Error::Io(error))
            if error.kind() == std::io::ErrorKind::AlreadyExists));
        let after = fs::metadata(&fresh).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        assert_eq!(
            read_secret(&fresh.join("original")).unwrap(),
            b"original bytes"
        );

        fs::set_permissions(&fresh, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(create_directory_new(&fresh), Err(Error::Io(error))
            if error.kind() == std::io::ErrorKind::AlreadyExists));
        assert_eq!(fs::metadata(&fresh).unwrap().mode() & 0o777, 0o755);
        assert!(matches!(directory(&fresh), Err(Error::Private)));
        let link = parent.join("link");
        std::os::unix::fs::symlink(&fresh, &link).unwrap();
        assert!(create_directory_new(&link).is_err());
        assert!(directory(&link).is_err());
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let file = parent.join("file");
        write_new(&file, b"existing file").unwrap();
        assert!(create_directory_new(&file).is_err());
        assert_eq!(read_secret(&file).unwrap(), b"existing file");
        assert!(create_directory_new(&parent.join("missing/child")).is_err());
        assert!(!parent.join("missing").exists());

        let contested = parent.join("contested");
        let barrier = std::sync::Barrier::new(2);
        let outcomes = std::thread::scope(|scope| {
            let create = || {
                barrier.wait();
                create_directory_new(&contested)
            };
            let first = scope.spawn(create);
            let second = scope.spawn(create);
            [first.join().unwrap(), second.join().unwrap()]
        });
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|result| matches!(result,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists))
                .count(),
            1
        );
        directory(&contested).unwrap();
    }

    #[test]
    fn private_storage_rejects_public_access() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("state");
        directory(&root).unwrap();
        let token = root.join("operator.token");
        write_new(&token, b"fixture-secret").unwrap();
        fs::set_permissions(&token, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(read_secret(&token), Err(Error::Private)));
        assert!(matches!(
            check_handle(&File::open(&token).unwrap()),
            Err(Error::Private)
        ));
        fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&token, root.join("hard-link")).unwrap();
        assert!(matches!(read_secret(&token), Err(Error::Private)));
        fs::remove_file(root.join("hard-link")).unwrap();
        std::os::unix::fs::symlink(&token, root.join("symbolic-link")).unwrap();
        assert!(matches!(
            read_secret(&root.join("symbolic-link")),
            Err(Error::Private)
        ));
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(directory(&root), Err(Error::Private)));
    }
}

/// Publish a synced owner-private temporary file without replacing any existing
/// name. Current profile initialization uses this filesystem primitive; it
/// does not supply resource or account authority.
pub fn publish_new(dir: &cap_std::fs::Dir, temporary: &str, name: &str) -> Result<(), Error> {
    use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
    for value in [temporary, name] {
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
            || value == "."
            || value == ".."
        {
            return Err(Error::Private);
        }
    }
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let original = dir.open_with(temporary, &options)?.into_std();
    check_handle(&original)?;
    if !original.metadata()?.is_file() {
        return Err(Error::Private);
    }
    original.sync_all()?;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    rustix::fs::renameat_with(
        dir,
        temporary,
        dir,
        name,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)?;
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Err(Error::PlatformUnavailable);
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        sync_directory(dir)?;
        let current = dir.open_with(name, &options)?.into_std();
        check_handle(&current)?;
        if !crate::same_file(&original, &current)? {
            return Err(Error::Private);
        }
        Ok(())
    }
}
