//! Cooperative OS-owner writer custody. Only the retained private OS file lock
//! controls exclusion; obsolete migration records have no runtime authority.
use crate::private_state as private;
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, OpenOptions};
use std::{
    fs::File,
    path::{Path, PathBuf},
};

const LOCK: &str = "resource-authority.lock";
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("filesystem error")]
    Io(#[from] std::io::Error),
    #[error("owner-private state required")]
    Private,
    #[error("installation resource writer is busy")]
    Locked,
    #[error("installation writer locking is unavailable on this platform")]
    Unavailable,
    #[error("test installation root is missing or cannot be changed")]
    TestRoot,
}
impl From<private::Error> for Error {
    fn from(error: private::Error) -> Self {
        match error {
            private::Error::Io(error) => Self::Io(error),
            private::Error::Private => Self::Private,
            private::Error::PlatformUnavailable => Self::Unavailable,
        }
    }
}
/// Supported desktop Unix implementation. This is not runtime test evidence.
/// Mobile passwd namespaces and Windows lifetime startup are not enabled.
pub const fn supported_writer_guard_platform() -> bool {
    cfg!(any(
        target_os = "macos",
        all(target_os = "linux", not(target_env = "ohos"))
    ))
}
fn root() -> Result<PathBuf, Error> {
    #[cfg(feature = "test-support")]
    {
        test_support::selected_root()
    }
    #[cfg(not(feature = "test-support"))]
    {
        if !supported_writer_guard_platform() {
            return Err(Error::Unavailable);
        }
        Ok(crate::owner_home()?.join(".hagency-owner-resources"))
    }
}
struct Files {
    root: PathBuf,
    directory: Dir,
    lock: File,
}
impl Files {
    fn open(root: &Path) -> Result<Self, Error> {
        if !root.is_absolute() {
            return Err(Error::Private);
        }
        private::directory(root)?;
        let parent = Dir::open_ambient_dir(
            root.parent().ok_or(Error::Private)?,
            cap_std::ambient_authority(),
        )?;
        let directory = parent.open_dir_nofollow(root.file_name().ok_or(Error::Private)?)?;
        private::check_handle(&directory.try_clone()?.into_std_file())?;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .follow(FollowSymlinks::No)
            .nonblock(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = match directory.open_with(LOCK, options.clone().create_new(true)) {
            Ok(file) => {
                let file = file.into_std();
                private::seal_created_file_handle(&file)?;
                file.sync_all()?;
                private::sync_directory(&directory)?;
                file
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                directory.open_with(LOCK, &options)?.into_std()
            }
            Err(error) => return Err(error.into()),
        };
        private::check_handle(&lock)?;
        if !lock.metadata()?.is_file() || lock.metadata()?.len() != 0 {
            return Err(Error::Private);
        }
        let files = Self {
            root: root.into(),
            directory,
            lock,
        };
        files.check()?;
        Ok(files)
    }
    fn check(&self) -> Result<(), Error> {
        let parent = Dir::open_ambient_dir(
            self.root.parent().ok_or(Error::Private)?,
            cap_std::ambient_authority(),
        )?;
        let current = parent.open_dir_nofollow(self.root.file_name().ok_or(Error::Private)?)?;
        if !crate::same_directory(
            &self.directory.try_clone()?.into_std_file(),
            &current.try_clone()?.into_std_file(),
        )? {
            return Err(Error::Private);
        }
        private::check_handle(&current.try_clone()?.into_std_file())?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let current_lock = current.open_with(LOCK, &options)?.into_std();
        private::check_handle(&current_lock)?;
        if !crate::same_file(&self.lock, &current_lock)? {
            return Err(Error::Private);
        }
        Ok(())
    }
}

/// A shared lock retained until the actual writer connection/effect is gone.
/// There is no expiry: native authorization remains an independent constraint.
pub struct LegacyWriterGuard {
    _files: Files,
}
impl LegacyWriterGuard {
    pub fn acquire() -> Result<Self, Error> {
        Self::at(&root()?)
    }
    fn at(root: &Path) -> Result<Self, Error> {
        let files = Files::open(root)?;
        files.lock.try_lock_shared().map_err(|_| Error::Locked)?;
        files.check()?;
        Ok(Self { _files: files })
    }
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn isolated_for_tests(root: &Path) -> Result<Self, Error> {
        Self::at(root)
    }
}
/// Current startup custody; unsupported platforms do not claim OS lock coverage.
pub enum LegacyWriterAdmission {
    Covered(LegacyWriterGuard),
    UncoveredPlatform,
}
impl LegacyWriterAdmission {
    pub fn acquire() -> Result<Self, Error> {
        if !supported_writer_guard_platform() && !cfg!(feature = "test-support") {
            return Ok(Self::UncoveredPlatform);
        }
        LegacyWriterGuard::acquire().map(Self::Covered)
    }
    pub fn is_covered(&self) -> bool {
        matches!(self, Self::Covered(_))
    }
}
/// Raw exclusive file custody only. The caller must separately enforce native
/// permission, finite expiry and registry identity.
pub struct ExclusiveWriterGuard {
    files: Files,
}
impl ExclusiveWriterGuard {
    pub fn acquire() -> Result<Self, Error> {
        Self::at(&root()?)
    }
    fn at(root: &Path) -> Result<Self, Error> {
        let files = Files::open(root)?;
        files.lock.try_lock().map_err(|_| Error::Locked)?;
        files.check()?;
        Ok(Self { files })
    }
    pub fn check(&self) -> Result<(), Error> {
        self.files.check()
    }
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn isolated_for_tests(root: &Path) -> Result<Self, Error> {
        Self::at(root)
    }
}

/// Explicit process-fixed synthetic root, available only in test builds that
/// opt into the test-support feature. No environment variable selects it.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    struct Selection {
        root: Option<PathBuf>,
        attempted: bool,
    }
    static SELECTED: Mutex<Selection> = Mutex::new(Selection {
        root: None,
        attempted: false,
    });
    pub fn initialize(root: &Path) -> Result<(), Error> {
        let root = root.canonicalize()?;
        let directory = Dir::open_ambient_dir(&root, cap_std::ambient_authority())?;
        private::check_handle(&directory.into_std_file())?;
        let mut selected = SELECTED.lock().map_err(|_| Error::TestRoot)?;
        if selected.root.as_ref() == Some(&root) {
            return Ok(());
        }
        if selected.attempted || selected.root.is_some() {
            return Err(Error::TestRoot);
        }
        selected.root = Some(root);
        Ok(())
    }
    /// Return only the explicit process-fixed test root; never create/fallback.
    pub fn configured_root() -> Result<PathBuf, Error> {
        selected_root()
    }
    pub(super) fn selected_root() -> Result<PathBuf, Error> {
        let mut selected = SELECTED.lock().map_err(|_| Error::TestRoot)?;
        selected.attempted = true;
        selected.root.clone().ok_or(Error::TestRoot)
    }
    /// Explicit fixture helper. The private temporary directory lives for the
    /// test process, including all worker threads and concurrently running tests.
    pub fn initialize_process_root() -> Result<PathBuf, Error> {
        static TEMP: OnceLock<tempfile::TempDir> = OnceLock::new();
        if TEMP.get().is_none() {
            let created = tempfile::tempdir()?;
            let _ = TEMP.set(created);
        }
        let path = TEMP.get().ok_or(Error::TestRoot)?.path().join("authority");
        private::directory(&path)?;
        initialize(&path)?;
        Ok(path)
    }
    /// Create a fixture only after explicitly selecting the synthetic writer
    /// authority. This cannot fall back to an OS-owner directory.
    pub fn initialized_tempdir() -> std::io::Result<tempfile::TempDir> {
        initialize_process_root().map_err(std::io::Error::other)?;
        tempfile::tempdir()
    }
}

#[cfg(test)]
mod tests;
