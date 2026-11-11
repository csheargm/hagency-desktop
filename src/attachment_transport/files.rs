//! Capability-relative scratch files, owned until confirmed deletion.
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt, OpenOptionsMaybeDirExt};
use cap_std::fs::{Dir, OpenOptions};
use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

fn err(error: hagency_store::Error) -> io::Error {
    io::Error::other(error)
}
fn open_dir(parent: &Dir, name: &Path) -> io::Result<Dir> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .follow(FollowSymlinks::No)
        .maybe_dir(true);
    let file = parent.open_with(name, &options)?.into_std();
    if !file.metadata()?.is_dir() {
        return Err(io::Error::other("directory required"));
    }
    Ok(Dir::from_std_file(file))
}
fn open(parent: &Dir, name: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = parent.open_with(name, &options)?.into_std();
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("regular file required"));
    }
    Ok(file)
}
/// A failed create owns its original handle/parent until unlink is confirmed.
#[derive(Default)]
pub(super) struct CreateOwner {
    pending: std::sync::Mutex<Vec<CreationWitness>>,
    count: std::sync::atomic::AtomicUsize,
}
impl CreateOwner {
    pub fn failures(&self) -> usize {
        self.count.load(std::sync::atomic::Ordering::Acquire)
    }
    fn remove_or_retain(
        &self,
        mut witness: CreationWitness,
        unlink: impl FnOnce(&Dir, &Path) -> io::Result<()>,
    ) {
        if witness.remove_with(unlink).is_err() {
            let mut pending = self.pending.lock().unwrap();
            pending.push(witness);
            self.count.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
    }
    pub fn retry(&self) {
        self.retry_with(|dir, name| dir.remove_file(name));
    }
    pub fn retry_with(&self, mut unlink: impl FnMut(&Dir, &Path) -> io::Result<()>) {
        let pending = std::mem::take(&mut *self.pending.lock().unwrap());
        let mut retained = Vec::new();
        for mut witness in pending {
            if witness.remove_with(|dir, name| unlink(dir, name)).is_err() {
                retained.push(witness);
            } else {
                drop(witness);
                self.count.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            }
        }
        // Counts include in-flight retry custody, including concurrent retry
        // callers with an empty pending list. Reinserting unknowns changes none.
        self.pending.lock().unwrap().extend(retained);
    }
}
struct CreationWitness {
    parent: Dir,
    name: PathBuf,
    original: File,
}
impl CreationWitness {
    fn remove_with(
        &mut self,
        unlink: impl FnOnce(&Dir, &Path) -> io::Result<()>,
    ) -> io::Result<()> {
        let original = file_id(&self.original)?;
        let current = match open(&self.parent, &self.name) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if file_id(&current)? != original {
            return Err(io::Error::other(
                "refused replaced private creation cleanup",
            ));
        }
        drop(current);
        unlink(&self.parent, &self.name)
    }
}
pub(super) struct CreatedFile {
    witness: Option<CreationWitness>,
    owner: std::sync::Arc<CreateOwner>,
}
impl std::ops::Deref for CreatedFile {
    type Target = File;
    fn deref(&self) -> &File {
        &self.witness.as_ref().unwrap().original
    }
}
impl std::ops::DerefMut for CreatedFile {
    fn deref_mut(&mut self) -> &mut File {
        &mut self.witness.as_mut().unwrap().original
    }
}
impl CreatedFile {
    fn finish(mut self) -> File {
        self.witness.take().unwrap().original
    }
    fn discard(mut self, unlink: impl FnOnce(&Dir, &Path) -> io::Result<()>) {
        if let Some(witness) = self.witness.take() {
            self.owner.remove_or_retain(witness, unlink);
        }
    }
}
impl Drop for CreatedFile {
    fn drop(&mut self) {
        if let Some(witness) = self.witness.take() {
            self.owner
                .remove_or_retain(witness, |dir, name| dir.remove_file(name));
        }
    }
}
fn create(
    owner: &std::sync::Arc<CreateOwner>,
    parent: &Dir,
    name: &Path,
) -> io::Result<CreatedFile> {
    create_with(
        owner,
        parent,
        name,
        |file| hagency_store::private::seal_created_file_handle(file).map_err(err),
        |dir, name| dir.remove_file(name),
    )
}
pub(super) fn create_with(
    owner: &std::sync::Arc<CreateOwner>,
    parent: &Dir,
    name: &Path,
    seal: impl FnOnce(&File) -> io::Result<()>,
    unlink: impl FnOnce(&Dir, &Path) -> io::Result<()>,
) -> io::Result<CreatedFile> {
    if owner.failures() != 0 {
        return Err(io::Error::other(
            "Private file creation cleanup remains unconfirmed",
        ));
    }
    // Clone the retained parent BEFORE create_new. Any later setup failure has
    // a real cleanup owner, including ID/descriptor acquisition failures.
    let directory = parent.try_clone()?;
    let mut options = OpenOptions::new();
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
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        use windows::Win32::{
            Foundation::{GENERIC_READ, GENERIC_WRITE},
            Storage::FileSystem::{WRITE_DAC, WRITE_OWNER},
        };
        options.access_mode(GENERIC_READ.0 | GENERIC_WRITE.0 | WRITE_DAC.0 | WRITE_OWNER.0);
    }
    let file = parent.open_with(name, &options)?.into_std();
    let created = CreatedFile {
        witness: Some(CreationWitness {
            parent: directory,
            name: name.to_owned(),
            original: file,
        }),
        owner: owner.clone(),
    };
    if let Err(error) = seal(&created) {
        created.discard(unlink);
        return Err(if owner.failures() == 0 {
            error
        } else {
            io::Error::other(format!(
                "Private file setup failed ({error}); original cleanup unconfirmed"
            ))
        });
    }
    Ok(created)
}
pub(super) struct Root {
    path: PathBuf,
    dir: Dir,
    _lease: File,
    creates: std::sync::Arc<CreateOwner>,
    poisoned: std::sync::atomic::AtomicBool,
    orphan_journals: std::sync::Mutex<Vec<JournalRef>>,
}
impl Root {
    #[cfg(test)]
    pub fn dir_for_test(&self) -> io::Result<Dir> {
        self.dir.try_clone()
    }
    pub fn cleanup_failures(&self) -> usize {
        self.orphan_journals.lock().unwrap().len().max(usize::from(
            self.poisoned.load(std::sync::atomic::Ordering::Acquire),
        ))
    }
    #[cfg(test)]
    pub fn new(cache: &Path) -> io::Result<Self> {
        Self::new_with_owner(cache, std::sync::Arc::default())
    }
    pub fn new_with_owner(cache: &Path, creates: std::sync::Arc<CreateOwner>) -> io::Result<Self> {
        std::fs::create_dir_all(cache)?;
        let path = cache.join("private-attachments-v1");
        hagency_store::private::directory(&path).map_err(err)?;
        let root = Dir::open_ambient_dir(&path, cap_std::ambient_authority())?;
        hagency_store::private::check_handle(&root.try_clone()?.into_std_file()).map_err(err)?;
        for item in root.entries()? {
            let item = item?;
            if !item.file_name().to_string_lossy().starts_with("run_") {
                continue;
            }
            let Ok(dir) = open_dir(&root, Path::new(&item.file_name())) else {
                continue;
            };
            let lease = match open(&dir, Path::new("lease")) {
                Ok(lease) => {
                    if lease.try_lock().is_err() {
                        continue;
                    }
                    Some(lease)
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    if dir
                        .dir_metadata()?
                        .modified()?
                        .into_std()
                        .elapsed()
                        .unwrap_or_default()
                        < Duration::from_secs(1800)
                    {
                        continue;
                    }
                    None
                }
                Err(error) => return Err(error),
            };
            for entry in dir.entries()? {
                let entry = entry?;
                if entry.file_name().to_string_lossy().starts_with("save_") {
                    recover_journal(&dir, Path::new(&entry.file_name()))?;
                }
            }
            // No recursive traversal: only files this format can create.
            for entry in dir.entries()? {
                let entry = entry?;
                let metadata = dir.symlink_metadata(entry.file_name())?;
                if metadata.is_file() && !metadata.file_type().is_symlink() {
                    let id =
                        same_file::Handle::from_file(open(&dir, Path::new(&entry.file_name()))?)?;
                    let current =
                        same_file::Handle::from_file(open(&dir, Path::new(&entry.file_name()))?)?;
                    if id != current {
                        return Err(io::Error::other("scratch identity changed"));
                    }
                    drop((id, current));
                    dir.remove_file(entry.file_name())?;
                }
            }
            drop(lease);
            dir.remove_open_dir()?;
        }
        let name = format!("run_{}", uuid::Uuid::new_v4());
        hagency_store::private::create_directory_new(&path.join(&name)).map_err(err)?;
        let dir = open_dir(&root, Path::new(&name))?;
        let lease = create(&creates, &dir, Path::new("lease"))?;
        lease.try_lock()?;
        Ok(Self {
            path: path.join(name),
            dir,
            _lease: lease.finish(),
            creates,
            poisoned: std::sync::atomic::AtomicBool::new(false),
            orphan_journals: std::sync::Mutex::default(),
        })
    }
    pub fn destination(&self, path: &Path) -> io::Result<(Scratch, File)> {
        self.destination_with(path, |writer, bytes, directory| {
            writer.write_all(bytes)?;
            writer.sync_all()?;
            #[cfg(unix)]
            directory.try_clone()?.into_std_file().sync_all()?;
            #[cfg(not(unix))]
            let _ = directory;
            Ok(())
        })
    }
    pub fn destination_with(
        &self,
        path: &Path,
        write_journal: impl FnOnce(&mut File, &[u8], &Dir) -> io::Result<()>,
    ) -> io::Result<(Scratch, File)> {
        if self.poisoned.load(std::sync::atomic::Ordering::Acquire) {
            return Err(io::Error::other(
                "Attachment recovery requires confirmed cleanup",
            ));
        }
        let parent = std::fs::canonicalize(
            path.parent()
                .ok_or_else(|| io::Error::other("Save destination has no parent"))?,
        )?;
        let dir = open_absolute(&parent)?;
        let name = format!(".hagency-download-{}", uuid::Uuid::new_v4());
        let file = create(&self.creates, &dir, Path::new(&name))?;
        let id = same_file::Handle::from_file(file.try_clone()?)?;
        let mut scratch = Scratch {
            dir,
            path: parent.join(&name),
            name,
            identity: Some(id),
            journal: None,
        };
        let journal = (|| -> io::Result<JournalRef> {
            let data = Journal {
                version: 1,
                parent: encode_path(&parent),
                parent_id: file_id(&scratch.dir.try_clone()?.into_std_file())?,
                file_id: file_id(&file)?,
                name: scratch.name.clone(),
            };
            let bytes = serde_json::to_vec(&data)?;
            if bytes.len() > 64 * 1024 {
                return Err(io::Error::other("Save recovery metadata exceeds its limit"));
            }
            let quota = crate::transfer_budget::temporary_budget()
                .reserve(
                    bytes.len() as u64,
                    crate::transfer_budget::MAX_TEMPORARY_BYTES,
                )
                .map_err(io::Error::other)?;
            let name = format!("save_{}.json", uuid::Uuid::new_v4());
            let writer = create(&self.creates, &self.dir, Path::new(&name))?;
            let mut journal = JournalRef {
                quota: Some(quota),
                dir: self.dir.try_clone()?,
                name,
                identity: Some(same_file::Handle::from_file(writer.try_clone()?)?),
            };
            let mut writer = writer.finish();
            let written = write_journal(&mut writer, &bytes, &self.dir);
            if let Err(error) = written {
                drop(writer);
                if let Err(cleanup) = journal.remove() {
                    self.poisoned
                        .store(true, std::sync::atomic::Ordering::Release);
                    self.orphan_journals.lock().unwrap().push(journal);
                    return Err(io::Error::other(format!(
                        "Save journal setup failed ({error}); cleanup unconfirmed ({cleanup})"
                    )));
                }
                return Err(error);
            }
            Ok(journal)
        })();
        match journal {
            Ok(journal) => {
                scratch.journal = Some(journal);
                Ok((scratch, file.finish()))
            }
            Err(error) => {
                // No payload was written. The still-armed created guard owns
                // exact cleanup; failed unlink transfers its original witness.
                drop(scratch);
                drop(file);
                Err(error)
            }
        }
    }
    pub fn create(&self) -> io::Result<(Scratch, File)> {
        if self.poisoned.load(std::sync::atomic::Ordering::Acquire) {
            return Err(io::Error::other(
                "Attachment recovery requires confirmed cleanup",
            ));
        }
        let name = format!("file_{}", uuid::Uuid::new_v4());
        let file = create(&self.creates, &self.dir, Path::new(&name))?;
        let id = same_file::Handle::from_file(file.try_clone()?)?;
        Ok((
            Scratch {
                dir: self.dir.try_clone()?,
                path: self.path.join(&name),
                name,
                identity: Some(id),
                journal: None,
            },
            file.finish(),
        ))
    }
}
pub(super) struct Scratch {
    dir: Dir,
    path: PathBuf,
    name: String,
    identity: Option<same_file::Handle>,
    journal: Option<JournalRef>,
}
impl Scratch {
    #[cfg(test)]
    pub fn path_for_test(&self) -> PathBuf {
        self.path.clone()
    }
    fn remove_journal(&mut self) -> io::Result<()> {
        if let Some(journal) = self.journal.as_mut() {
            journal.remove()?;
        }
        self.journal.take();
        Ok(())
    }
    pub fn publish(&mut self, destination: &Path) -> io::Result<()> {
        let name = destination
            .file_name()
            .ok_or_else(|| io::Error::other("Save destination has no filename"))?;
        drop(self.open()?);
        self.dir.rename(&self.name, &self.dir, name)?;
        // This is now the user's chosen permanent file. Cleanup must never
        // remove it. The original IO owner releases its temporary reservation.
        self.identity.take();
        self.remove_journal()?;
        Ok(())
    }
    pub fn open(&self) -> io::Result<File> {
        let file = open(&self.dir, Path::new(&self.name))?;
        hagency_store::private::check_handle(&file).map_err(err)?;
        let id = same_file::Handle::from_file(file.try_clone()?)?;
        let alias = same_file::Handle::from_file(
            hagency_store::private::open(&self.path, false).map_err(err)?,
        )?;
        if self.identity.as_ref() != Some(&id) || id != alias {
            return Err(io::Error::other("attachment file identity changed"));
        }
        Ok(file)
    }
    pub fn remove(&mut self) -> io::Result<()> {
        if self.identity.is_none() {
            return self.remove_journal();
        }
        let current = match open(&self.dir, Path::new(&self.name)) {
            Ok(file) => same_file::Handle::from_file(file)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.identity.take();
                return self.remove_journal();
            }
            Err(error) => return Err(error),
        };
        if self.identity.as_ref() != Some(&current) {
            return Err(io::Error::other("refused replaced attachment cleanup"));
        }
        drop(current);
        self.dir.remove_file(&self.name)?;
        self.identity.take();
        self.remove_journal()
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Journal {
    version: u8,
    parent: Vec<u8>,
    parent_id: [u64; 2],
    file_id: [u64; 2],
    name: String,
}
struct JournalRef {
    quota: Option<crate::transfer_budget::Reservation>,
    dir: Dir,
    name: String,
    identity: Option<same_file::Handle>,
}
impl JournalRef {
    fn remove(&mut self) -> io::Result<()> {
        let file = match open(&self.dir, Path::new(&self.name)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.identity.take();
                self.quota.take();
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let current = same_file::Handle::from_file(file)?;
        if self.identity.as_ref() != Some(&current) {
            return Err(io::Error::other("Save journal identity changed"));
        }
        drop(current);
        self.dir.remove_file(&self.name)?;
        self.identity.take();
        self.quota.take();
        Ok(())
    }
}
fn recover_journal(directory: &Dir, name: &Path) -> io::Result<()> {
    let file = open(directory, name)?;
    hagency_store::private::check_handle(&file).map_err(err)?;
    if file.metadata()?.len() > 64 * 1024 {
        return Err(io::Error::other("Save journal exceeds its limit"));
    }
    let id = same_file::Handle::from_file(file.try_clone()?)?;
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
    let journal: Journal = serde_json::from_slice(&bytes)?;
    if journal.version != 1
        || !journal.name.starts_with(".hagency-download-")
        || journal.name.contains(['/', '\\'])
    {
        return Err(io::Error::other("Invalid save journal"));
    }
    let parent = decode_path(journal.parent)?;
    let dir = open_absolute(&parent)?;
    if file_id(&dir.try_clone()?.into_std_file())? != journal.parent_id {
        return Err(io::Error::other(
            "Save parent identity changed; cleanup unconfirmed",
        ));
    }
    match open(&dir, Path::new(&journal.name)) {
        Ok(file) => {
            hagency_store::private::check_handle(&file).map_err(err)?;
            if file_id(&file)? != journal.file_id {
                return Err(io::Error::other(
                    "Save scratch identity changed; cleanup unconfirmed",
                ));
            }
            drop(file);
            dir.remove_file(&journal.name)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let current = same_file::Handle::from_file(open(directory, name)?)?;
    if id != current {
        return Err(io::Error::other("Save journal replaced"));
    }
    drop((id, current));
    directory.remove_file(name)
}
fn open_absolute(path: &Path) -> io::Result<Dir> {
    use std::path::Component;
    if !path.is_absolute() {
        return Err(io::Error::other("Absolute save parent required"));
    }
    let mut anchor = PathBuf::new();
    let mut directory = None;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => anchor.push(prefix.as_os_str()),
            Component::RootDir => {
                anchor.push(component.as_os_str());
                directory = Some(Dir::open_ambient_dir(
                    &anchor,
                    cap_std::ambient_authority(),
                )?);
            }
            Component::Normal(name) => {
                directory = Some(open_dir(
                    directory
                        .as_ref()
                        .ok_or_else(|| io::Error::other("Invalid path root"))?,
                    Path::new(name),
                )?);
            }
            _ => return Err(io::Error::other("Normalized save parent required")),
        }
    }
    directory.ok_or_else(|| io::Error::other("Save parent missing"))
}
#[cfg(unix)]
fn file_id(file: &File) -> io::Result<[u64; 2]> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok([metadata.dev(), metadata.ino()])
}
#[cfg(windows)]
fn file_id(file: &File) -> io::Result<[u64; 2]> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .map_err(io::Error::other)?;
    Ok([
        info.dwVolumeSerialNumber as u64,
        ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
    ])
}
#[cfg(not(any(unix, windows)))]
fn file_id(_file: &File) -> io::Result<[u64; 2]> {
    Err(io::Error::other("Save recovery identity unsupported"))
}
#[cfg(unix)]
fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}
#[cfg(windows)]
fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}
#[cfg(not(any(unix, windows)))]
fn encode_path(path: &Path) -> Vec<u8> {
    path.to_string_lossy().as_bytes().to_vec()
}
#[cfg(unix)]
fn decode_path(bytes: Vec<u8>) -> io::Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Ok(std::ffi::OsString::from_vec(bytes).into())
}
#[cfg(windows)]
fn decode_path(bytes: Vec<u8>) -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    if !bytes.len().is_multiple_of(2) {
        return Err(io::Error::other("Invalid path encoding"));
    }
    let wide = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    Ok(std::ffi::OsString::from_wide(&wide).into())
}
#[cfg(not(any(unix, windows)))]
fn decode_path(_bytes: Vec<u8>) -> io::Result<PathBuf> {
    Err(io::Error::other("Save recovery unsupported"))
}
