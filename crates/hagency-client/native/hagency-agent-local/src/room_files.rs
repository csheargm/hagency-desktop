//! Host-executed file capabilities. No shell, network, provider credential or ambient
//! model-selected root. Host must supply a verified owner directory and fresh server
//! scope before opening. Private creation uses atomic no-replace publication;
//! shared updates use an explicitly authorized root and content-version checks.
use crate::{Ledger, Scope, ToolPolicy, ToolProposal};
use rustix::fs::{self, Mode, OFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

const BYTES: usize = 128 * 1024;
const LIST: usize = 256;
const INTERNAL: &str = ".hagency-file-control";

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("invalid relative file request")]
    Invalid,
    #[error("private file capability or scope check failed")]
    Boundary,
    #[error("owner approval is required")]
    NeedsOwner,
    #[error("file request denied")]
    Denied,
    #[error("file or result capacity exceeded")]
    Capacity,
    #[error("existing file replacement is unsupported")]
    UnsupportedReplace,
    #[error("existing or changed result cannot be overwritten")]
    Conflict,
    #[error("publication outcome cannot be proved; do not retry blindly")]
    Unknown,
    #[error("file operation unavailable")]
    Io,
}
type Result<T> = std::result::Result<T, FileError>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    List {
        path: String,
    },
    Read {
        path: String,
    },
    Create {
        path: String,
        content: String,
        nonce: String,
    },
    Replace {
        path: String,
    },
    MakeDirectory {
        path: String,
    },
    Update {
        path: String,
        content: String,
        expected_sha256: String,
        nonce: String,
    },
}
impl Operation {
    fn path(&self) -> &str {
        match self {
            Self::List { path }
            | Self::Read { path }
            | Self::Create { path, .. }
            | Self::Replace { path }
            | Self::Update { path, .. }
            | Self::MakeDirectory { path } => path,
        }
    }
    fn tool(&self) -> &'static str {
        match self {
            Self::List { .. } => "room.list",
            Self::Read { .. } => "room.read",
            Self::Create { .. } => "room.create",
            Self::Replace { .. } => "room.replace",
            Self::Update { .. } => "room.update",
            Self::MakeDirectory { .. } => "room.mkdir",
        }
    }
}

/// Never accept this value back from a browser/model as an authorized proposal.
/// The host creates it, displays its exact proposal, and later uses the same value.
pub struct PreparedCall {
    proposal: ToolProposal,
    operation: Operation,
    digest: String,
}
impl PreparedCall {
    pub fn proposal(&self) -> &ToolProposal {
        &self.proposal
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileResult {
    List {
        entries: Vec<String>,
    },
    Read {
        content: String,
        sha256: String,
    },
    DirectoryCreated {
        path: String,
    },
    Updated {
        path: String,
        sha256: String,
    },
    Created {
        path: String,
        nonce: String,
        sha256: String,
        replayed: bool,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    digest: String,
    path: String,
    nonce: String,
    sha256: String,
    dev: u64,
    ino: u64,
    modified: u128,
    changed: i128,
}

pub struct Workspace {
    root: File,
    control: File,
    _lock: File,
    canonical: PathBuf,
    owner: String,
    binding: (String, String, String),
    shared: bool,
    shared_lock: Option<File>,
}

/// A chat-only provider's stable room directory beneath the owner's configured
/// workspace. This grants no host file capability and never relocates existing
/// files. An existing flat workspace remains untouched beside this new child.
pub fn chat_directory(base: &Path, owner: &str, scope: &Scope) -> Result<PathBuf> {
    if !base.is_absolute() || base.canonicalize().ok().as_deref() != Some(base) {
        return Err(FileError::Boundary);
    }
    scope.validate().map_err(|_| FileError::Invalid)?;
    crate::key(owner).map_err(|_| FileError::Invalid)?;
    let mut directory_handle = File::from(
        fs::open("/", directory_flags(), Mode::empty()).map_err(|_| FileError::Io)?,
    );
    for part in base.components() {
        match part {
            Component::RootDir => {},
            Component::Normal(name) => directory_handle = directory(&directory_handle, name)?,
            _ => return Err(FileError::Boundary),
        }
    }
    let work = child_directory(&directory_handle, ".hagency-room-workspaces")?;
    let key = digest(&serde_json::to_vec(&(owner, &scope.agent, &scope.binding, &scope.room))
        .map_err(|_| FileError::Invalid)?);
    let name = format!("binding_{key}");
    let _room = child_directory(&work, &name)?;
    Ok(base.join(".hagency-room-workspaces").join(name))
}

impl Workspace {
    /// Generates the binding root beneath a host-owned owner_<sha256> directory.
    /// Symlink ancestors, group/world access and provider/home roots are refused.
    pub fn open(owner_directory: &Path, owner: &str, scope: &Scope) -> Result<Self> {
        if !owner_directory.is_absolute() {
            return Err(FileError::Boundary);
        }
        scope.validate().map_err(|_| FileError::Invalid)?;
        let name = owner_directory
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or(FileError::Boundary)?;
        if !name.strip_prefix("owner_").is_some_and(hex64) {
            return Err(FileError::Boundary);
        }
        let mut fd =
            File::from(fs::open("/", directory_flags(), Mode::empty()).map_err(|_| FileError::Io)?);
        for part in owner_directory.components() {
            match part {
                Component::RootDir => {}
                Component::Normal(name) => {
                    fd = directory(&fd, name)?;
                }
                _ => return Err(FileError::Boundary),
            }
        }
        private_directory(&fd)?;
        let work = child_directory(&fd, "room-workspaces")?;
        let key = digest(
            &serde_json::to_vec(&(owner, &scope.agent, &scope.binding, &scope.room))
                .map_err(|_| FileError::Invalid)?,
        );
        let root = child_directory(&work, &format!("binding_{key}"))?;
        let control = child_directory(&root, INTERNAL)?;
        let lock = File::from(
            fs::openat(
                &control,
                "lock",
                OFlags::RDWR
                    | OFlags::CREATE
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC
                    | OFlags::NONBLOCK,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(|_| FileError::Boundary)?,
        );
        regular(&lock)?;
        lock.try_lock().map_err(|_| FileError::Boundary)?;
        let canonical = owner_directory
            .join("room-workspaces")
            .join(format!("binding_{key}"));
        Ok(Self {
            root,
            control,
            _lock: lock,
            canonical,
            owner: owner.into(),
            shared: false,
            shared_lock: None,
            binding: (
                scope.agent.clone(),
                scope.binding.clone(),
                scope.room.clone(),
            ),
        })
    }
    /// Open a separately authorized existing project without following symlinks.
    pub fn open_shared(owner_directory: &Path, project: &Path, owner: &str, scope: &Scope) -> Result<Self> {
        if !project.is_absolute() || project.canonicalize().ok().as_deref() != Some(project) {
            return Err(FileError::Boundary);
        }
        let mut root = File::from(fs::open("/", directory_flags(), Mode::empty()).map_err(|_| FileError::Io)?);
        for part in project.components() {
            match part {
                Component::RootDir => {},
                Component::Normal(name) => root = directory(&root, name)?,
                _ => return Err(FileError::Boundary),
            }
        }
        shared_directory(&root)?;
        let mut owner_fd = File::from(fs::open("/", directory_flags(), Mode::empty()).map_err(|_| FileError::Io)?);
        for part in owner_directory.components() {
            match part {
                Component::RootDir => {},
                Component::Normal(name) => owner_fd = directory(&owner_fd, name)?,
                _ => return Err(FileError::Boundary),
            }
        }
        private_directory(&owner_fd)?;
        let locks = child_directory(&owner_fd, "shared-workspace-locks")?;
        let name = digest(project.to_str().ok_or(FileError::Boundary)?.as_bytes());
        let lock = File::from(fs::openat(&locks, name.as_str(),
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::RUSR | Mode::WUSR).map_err(|_| FileError::Boundary)?);
        regular(&lock)?;
        let mut workspace = Self::open(owner_directory, owner, scope)?;
        workspace.shared_lock = Some(lock);
        workspace.root = root;
        workspace.canonical = project.to_path_buf();
        workspace.shared = true;
        Ok(workspace)
    }
    pub fn is_shared(&self) -> bool {
        self.shared
    }
    pub fn shared_pin(&self) -> Result<String> {
        use std::os::unix::fs::MetadataExt;
        if !self.shared { return Err(FileError::Boundary); }
        let metadata = self.root.metadata().map_err(|_| FileError::Boundary)?;
        if self.canonical.canonicalize().ok().as_ref() != Some(&self.canonical) { return Err(FileError::Boundary); }
        let current = std::fs::metadata(&self.canonical).map_err(|_| FileError::Boundary)?;
        if (metadata.dev(), metadata.ino()) != (current.dev(), current.ino()) { return Err(FileError::Boundary); }
        let value = serde_json::json!({"path":self.canonical,"device":metadata.dev(),"inode":metadata.ino()});
        Ok(digest(&serde_json::to_vec(&value).map_err(|_| FileError::Invalid)?))
    }
    pub fn canonical_directory(&self) -> &Path {
        &self.canonical
    }
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &self,
        ledger: &Ledger,
        scope: &Scope,
        dispatch: &str,
        codex_thread: &str,
        turn: &str,
        call: &str,
        operation: Operation,
        now: i64,
    ) -> Result<PreparedCall> {
        self.check_scope(scope)?;
        if matches!(operation, Operation::Replace { .. }) || (!self.shared && matches!(operation, Operation::Update { .. } | Operation::MakeDirectory { .. })) {
            return Err(FileError::UnsupportedReplace);
        }
        relative(
            operation.path(),
            matches!(operation, Operation::List { .. }),
        )?;
        for key in [dispatch, codex_thread, turn, call] {
            crate::key(key).map_err(|_| FileError::Invalid)?;
        }
        if !(0..=i64::MAX - 300).contains(&now) {
            return Err(FileError::Invalid);
        }
        if let Operation::Create { content, nonce, .. } = &operation {
            if content.len() > BYTES {
                return Err(FileError::Capacity);
            }
            if !hex64(nonce) {
                return Err(FileError::Invalid);
            }
        }
        if let Operation::Update { content, expected_sha256, nonce, .. } = &operation {
            if content.len() > BYTES { return Err(FileError::Capacity); }
            if !hex64(expected_sha256) || !hex64(nonce) { return Err(FileError::Invalid); }
        }
        let proposal = ToolProposal {
            scope: scope.clone(),
            dispatch: dispatch.into(),
            tool: operation.tool().into(),
            arguments: serde_json::json!({"codexThreadId":codex_thread,"turnId":turn,"callId":call,"file":operation}),
            canonical_directory: self.canonical.to_str().ok_or(FileError::Boundary)?.into(),
            risk: "high".into(),
            policy_revision: ledger
                .policy_snapshot(scope)
                .map_err(|_| FileError::Denied)?
                .map(|v| v.revision),
            expires: now + 300,
        };
        let digest = digest(&serde_json::to_vec(&proposal).map_err(|_| FileError::Invalid)?);
        Ok(PreparedCall {
            proposal,
            operation,
            digest,
        })
    }
    /// Host must recheck server owner/device/binding liveness immediately before
    /// invoking this method. Local policy is re-evaluated here, before any IO.
    pub fn execute(
        &mut self,
        ledger: &mut Ledger,
        prepared: &PreparedCall,
        now: i64,
    ) -> Result<FileResult> {
        self.check_scope(&prepared.proposal.scope)?;
        if ledger.owner != self.owner
            || prepared.proposal.canonical_directory != self.canonical.to_string_lossy()
        {
            return Err(FileError::Boundary);
        }
        if self.shared { shared_directory(&self.root)?; self.shared_pin()?; } else { private_directory(&self.root)?; }
        private_directory(&self.control)?;
        regular(&self._lock)?;
        let disposition = ledger
            .tool_disposition(&prepared.proposal, now)
            .map_err(|_| FileError::Denied)?;
        if let Operation::Create {
            path,
            content,
            nonce,
        } = &prepared.operation
            && let Some(result) = self.known_create(prepared, path, content, nonce)?
        {
            return Ok(result);
        }
        ledger
            .authorize_tool(&prepared.proposal, now)
            .map_err(|_| {
                if matches!(disposition, ToolPolicy::AskOwner) {
                    FileError::NeedsOwner
                } else {
                    FileError::Denied
                }
            })?;
        match &prepared.operation {
            Operation::List { path } => {
                let directory = self.walk(path)?;
                let mut entries = Vec::new();
                let iterator = fs::Dir::read_from(&directory).map_err(|_| FileError::Io)?;
                let mut inspected = 0;
                for entry in iterator {
                    inspected += 1;
                    if inspected > LIST + 3 {
                        return Err(FileError::Capacity);
                    }
                    let entry = entry.map_err(|_| FileError::Io)?;
                    let name = entry
                        .file_name()
                        .to_str()
                        .map_err(|_| FileError::Boundary)?;
                    if [".", "..", INTERNAL].contains(&name) || name.starts_with(".hagency-") {
                        continue;
                    }
                    // Refuse symlink/device/hardlinked entries, rather than giving
                    // a model the impression that they are usable capabilities.
                    let stat = fs::statat(&directory, name, fs::AtFlags::SYMLINK_NOFOLLOW)
                        .map_err(|_| FileError::Boundary)?;
                    let ty = fs::FileType::from_raw_mode(stat.st_mode);
                    if ty == fs::FileType::RegularFile && stat.st_nlink == 1
                        || ty == fs::FileType::Directory
                    {
                        entries.push(name.to_owned());
                    } else if !self.shared {
                        return Err(FileError::Boundary);
                    }
                    if entries.len() > LIST {
                        return Err(FileError::Capacity);
                    }
                }
                entries.sort();
                Ok(FileResult::List { entries })
            }
            Operation::Read { path } => {
                let (parent, name) = self.parent(path)?;
                let file = if self.shared { read_shared_file(&parent, &name)? } else { read_file(&parent, &name)? };
                let bytes = read_bounded(file, BYTES)?;
                let sha256 = digest(&bytes);
                let content = String::from_utf8(bytes).map_err(|_| FileError::Invalid)?;
                Ok(FileResult::Read { content, sha256 })
            }
            Operation::Create {
                path,
                content,
                nonce,
            } => self.create(prepared, path, content, nonce),
            Operation::Replace { .. } => Err(FileError::UnsupportedReplace),
            Operation::MakeDirectory { path } => {
                let (parent, name) = self.parent(path)?;
                fs::mkdirat(&parent, name.as_str(), Mode::RUSR | Mode::WUSR | Mode::XUSR).map_err(|_| FileError::Conflict)?;
                parent.sync_all().map_err(|_| FileError::Unknown)?;
                Ok(FileResult::DirectoryCreated { path: path.clone() })
            },
            Operation::Update { path, content, expected_sha256, nonce } => {
                let lock = self.shared_lock.as_ref().ok_or(FileError::Boundary)?;
                lock.try_lock().map_err(|_| FileError::Conflict)?;
                let result = self.update(prepared, path, content, expected_sha256, nonce);
                lock.unlock().map_err(|_| FileError::Unknown)?;
                result
            },
        }
    }
    fn check_scope(&self, scope: &Scope) -> Result<()> {
        if self.binding
            != (
                scope.agent.clone(),
                scope.binding.clone(),
                scope.room.clone(),
            )
        {
            return Err(FileError::Boundary);
        }
        Ok(())
    }
    fn walk(&self, path: &str) -> Result<File> {
        let parts = relative(path, true)?;
        let mut dir = self.root.try_clone().map_err(|_| FileError::Io)?;
        for part in parts {
            dir = directory(&dir, part.as_ref())?;
            if self.shared { shared_directory(&dir)?; } else { private_directory(&dir)?; }
        }
        Ok(dir)
    }
    fn parent(&self, path: &str) -> Result<(File, String)> {
        let parts = relative(path, false)?;
        let (name, parents) = parts.split_last().ok_or(FileError::Invalid)?;
        let mut dir = self.root.try_clone().map_err(|_| FileError::Io)?;
        for part in parents {
            dir = directory(&dir, part.as_ref())?;
            if self.shared { shared_directory(&dir)?; } else { private_directory(&dir)?; }
        }
        Ok((dir, (*name).to_owned()))
    }
    fn update(&self, prepared: &PreparedCall, path: &str, content: &str, expected_sha256: &str, nonce: &str) -> Result<FileResult> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if !self.shared { return Err(FileError::UnsupportedReplace); }
        let (parent, name) = self.parent(path)?;
        let receipt_name = format!("update-{}", prepared.digest);
        match read_file(&self.control, &receipt_name) {
            Ok(_) => return Err(FileError::Unknown),
            Err(FileError::Io) => {},
            Err(_) => return Err(FileError::Unknown),
        }
        let existing = read_shared_file(&parent, &name)?;
        let before = existing.metadata().map_err(|_| FileError::Boundary)?;
        if digest(&read_bounded(existing, BYTES)?) != expected_sha256 { return Err(FileError::Conflict); }
        let mut receipt = File::from(fs::openat(&self.control, receipt_name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR).map_err(|_| FileError::Unknown)?);
        receipt.write_all(b"pending").map_err(|_| FileError::Unknown)?;
        receipt.sync_all().map_err(|_| FileError::Unknown)?;
        self.control.sync_all().map_err(|_| FileError::Unknown)?;
        let temporary = format!(".hagency-stage-{nonce}");
        let mut file = File::from(fs::openat(&parent, &temporary,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR).map_err(|_| FileError::Unknown)?);
        file.set_permissions(std::fs::Permissions::from_mode(before.mode() & 0o777)).map_err(|_| FileError::Unknown)?;
        file.write_all(content.as_bytes()).map_err(|_| FileError::Unknown)?;
        file.sync_all().map_err(|_| FileError::Unknown)?;
        let current = read_shared_file(&parent, &name)?;
        let metadata = current.metadata().map_err(|_| FileError::Unknown)?;
        if (metadata.dev(), metadata.ino(), metadata.mtime(), metadata.mtime_nsec(), metadata.ctime(), metadata.ctime_nsec())
            != (before.dev(), before.ino(), before.mtime(), before.mtime_nsec(), before.ctime(), before.ctime_nsec())
            || digest(&read_bounded(current, BYTES)?) != expected_sha256 { return Err(FileError::Conflict); }
        fs::renameat(&parent, &temporary, &parent, &name).map_err(|_| FileError::Unknown)?;
        parent.sync_all().map_err(|_| FileError::Unknown)?;
        let published = read_shared_file(&parent, &name).map_err(|_| FileError::Unknown)?;
        let actual = published.metadata().map_err(|_| FileError::Unknown)?;
        let written = file.metadata().map_err(|_| FileError::Unknown)?;
        let sha256 = digest(content.as_bytes());
        if (actual.dev(), actual.ino()) != (written.dev(), written.ino())
            || digest(&read_bounded(published, BYTES)?) != sha256 { return Err(FileError::Unknown); }
        Ok(FileResult::Updated { path: path.into(), sha256 })
    }
    fn known_create(
        &self,
        prepared: &PreparedCall,
        path: &str,
        content: &str,
        nonce: &str,
    ) -> Result<Option<FileResult>> {
        let (parent, name) = self.parent(path)?;
        let receipt_name = format!("receipt-{}", prepared.digest);
        let sha256 = digest(content.as_bytes());
        // Durable proof, never merely matching contents, permits a known retry.
        match read_file(&self.control, &receipt_name) {
            Ok(file) => {
                let receipt: Receipt = serde_json::from_slice(&read_bounded(file, 8192)?)
                    .map_err(|_| FileError::Unknown)?;
                let file = read_file(&parent, &name).map_err(|_| FileError::Unknown)?;
                let (dev, ino, modified, changed) = identity(&file)?;
                if receipt.digest != prepared.digest
                    || receipt.path != path
                    || receipt.nonce != nonce
                    || receipt.sha256 != sha256
                    || (dev, ino, modified, changed)
                        != (receipt.dev, receipt.ino, receipt.modified, receipt.changed)
                    || digest(&read_bounded(file, BYTES)?) != sha256
                {
                    return Err(FileError::Unknown);
                }
                return Ok(Some(FileResult::Created {
                    path: path.into(),
                    nonce: nonce.into(),
                    sha256,
                    replayed: true,
                }));
            }
            Err(FileError::Io) => {}
            Err(_) => return Err(FileError::Unknown),
        }
        Ok(None)
    }
    fn create(
        &self,
        prepared: &PreparedCall,
        path: &str,
        content: &str,
        nonce: &str,
    ) -> Result<FileResult> {
        let (parent, name) = self.parent(path)?;
        let receipt_name = format!("receipt-{}", prepared.digest);
        let sha256 = digest(content.as_bytes());
        let temporary = format!(".hagency-stage-{nonce}");
        let mut file = File::from(
            fs::openat(
                &parent,
                &temporary,
                OFlags::WRONLY
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC
                    | OFlags::NONBLOCK,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(|_| FileError::Conflict)?,
        );
        regular(&file)?;
        file.write_all(content.as_bytes())
            .map_err(|_| FileError::Unknown)?;
        file.sync_all().map_err(|_| FileError::Unknown)?;
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        fs::renameat_with(
            &parent,
            &temporary,
            &parent,
            &name,
            fs::RenameFlags::NOREPLACE,
        )
        .map_err(|_| FileError::Conflict)?;
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        return Err(FileError::Io);
        parent.sync_all().map_err(|_| FileError::Unknown)?;
        let (dev, ino, modified, changed) = identity(&file)?;
        let published = read_file(&parent, &name).map_err(|_| FileError::Unknown)?;
        if identity(&published)? != (dev, ino, modified, changed)
            || digest(&read_bounded(published, BYTES)?) != sha256
        {
            return Err(FileError::Unknown);
        }
        let receipt = Receipt {
            digest: prepared.digest.clone(),
            path: path.into(),
            nonce: nonce.into(),
            sha256: sha256.clone(),
            dev,
            ino,
            modified,
            changed,
        };
        let bytes = serde_json::to_vec(&receipt).map_err(|_| FileError::Unknown)?;
        let mut proof = File::from(
            fs::openat(
                &self.control,
                &receipt_name,
                OFlags::WRONLY
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC
                    | OFlags::NONBLOCK,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(|_| FileError::Unknown)?,
        );
        proof.write_all(&bytes).map_err(|_| FileError::Unknown)?;
        proof.sync_all().map_err(|_| FileError::Unknown)?;
        self.control.sync_all().map_err(|_| FileError::Unknown)?;
        Ok(FileResult::Created {
            path: path.into(),
            nonce: nonce.into(),
            sha256,
            replayed: false,
        })
    }
}

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK
}
fn directory(parent: &File, name: &std::ffi::OsStr) -> Result<File> {
    Ok(File::from(
        fs::openat(parent, name, directory_flags(), Mode::empty())
            .map_err(|_| FileError::Boundary)?,
    ))
}
fn child_directory(parent: &File, name: &str) -> Result<File> {
    match fs::mkdirat(parent, name, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
        Ok(()) => parent.sync_all().map_err(|_| FileError::Io)?,
        Err(rustix::io::Errno::EXIST) => {}
        Err(_) => return Err(FileError::Io),
    }
    let child = directory(parent, name.as_ref())?;
    private_directory(&child)?;
    Ok(child)
}
fn private_directory(file: &File) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata().map_err(|_| FileError::Boundary)?;
    if !m.is_dir() || m.mode() & 0o077 != 0 || m.uid() != rustix::process::geteuid().as_raw() {
        return Err(FileError::Boundary);
    }
    Ok(())
}
fn shared_directory(file: &File) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata().map_err(|_| FileError::Boundary)?;
    if !m.is_dir() || m.mode() & 0o022 != 0 || m.uid() != rustix::process::geteuid().as_raw() {
        return Err(FileError::Boundary);
    }
    Ok(())
}
fn read_shared_file(parent: &File, name: &str) -> Result<File> {
    use std::os::unix::fs::MetadataExt;
    let file = File::from(fs::openat(parent, name, OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK, Mode::empty()).map_err(|_| FileError::Boundary)?);
    let m = file.metadata().map_err(|_| FileError::Boundary)?;
    if !m.is_file() || m.nlink() != 1 || m.mode() & 0o022 != 0 || m.uid() != rustix::process::geteuid().as_raw() {
        return Err(FileError::Boundary);
    }
    Ok(file)
}
fn regular(file: &File) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata().map_err(|_| FileError::Boundary)?;
    if !m.is_file()
        || m.nlink() != 1
        || m.mode() & 0o077 != 0
        || m.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(FileError::Boundary);
    }
    Ok(())
}
fn read_file(parent: &File, name: &str) -> Result<File> {
    let file = File::from(
        fs::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|e| {
            if e == rustix::io::Errno::NOENT {
                FileError::Io
            } else {
                FileError::Boundary
            }
        })?,
    );
    regular(&file)?;
    Ok(file)
}
fn read_bounded(file: File, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| FileError::Io)?;
    if bytes.len() > limit {
        return Err(FileError::Capacity);
    }
    Ok(bytes)
}
fn identity(file: &File) -> Result<(u64, u64, u128, i128)> {
    use std::os::unix::fs::MetadataExt;
    regular(file)?;
    let m = file.metadata().map_err(|_| FileError::Unknown)?;
    let modified = m
        .modified()
        .map_err(|_| FileError::Unknown)?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| FileError::Unknown)?
        .as_nanos();
    Ok((
        m.dev(),
        m.ino(),
        modified,
        i128::from(m.ctime()) * 1_000_000_000 + i128::from(m.ctime_nsec()),
    ))
}
fn relative(path: &str, empty: bool) -> Result<Vec<&str>> {
    if path.is_empty() && empty {
        return Ok(Vec::new());
    }
    if path.is_empty()
        || path.len() > 1024
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains('\0')
    {
        return Err(FileError::Invalid);
    }
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() > 16
        || parts.iter().any(|p| {
            p.is_empty() || [".", ".."].contains(p) || p.starts_with(".hagency-") || p.len() > 255
        })
    {
        return Err(FileError::Invalid);
    }
    Ok(parts)
}
fn hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests;
