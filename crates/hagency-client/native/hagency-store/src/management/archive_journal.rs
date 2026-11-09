//! Bounded private command custody on an already fenced source database.
//! No source migration, resource API, arbitrary SQL or allocation authority.
use super::{intents::JournalAction, *};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, OpenOptions};
use rusqlite::{
    OpenFlags,
    config::DbConfig,
    hooks::{AuthAction, AuthContext, Authorization},
};
use std::{
    fs::File,
    path::{Component, Path, PathBuf},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};

/// A distinct identity rejected by normal domain and personal-ledger openers.
/// Only a future reviewed migration may write it; this module has no writer API.
pub const MANAGEMENT_ARCHIVE_APPLICATION_ID: i32 = 0x48415231;
const MAX_OUTPUT: usize = intents::MAX_INTENT_OUTPUT;
const DEADLINE: Duration = Duration::from_secs(2);

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    version: u16,
    kind: String,
    migration_id: String,
    registry_id: String,
    profile_digest: String,
    owner_mxid: String,
    inventory_digest: String,
    source_schema: i32,
}

/// Exact native-saved migration binding. Not Deserialize; never UI input.
#[derive(Clone)]
pub struct ArchiveJournalBinding(Manifest);
impl ArchiveJournalBinding {
    #[allow(clippy::too_many_arguments)] // Exact trusted source provenance, not an untyped map.
    pub fn new(
        migration_id: String,
        registry_id: String,
        profile_digest: String,
        owner_mxid: String,
        inventory_digest: String,
        source_schema: i32,
    ) -> Result<Self, Error> {
        identifier(&migration_id, 128)?;
        for digest in [&registry_id, &profile_digest, &inventory_digest] {
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            {
                return Err(Error::LocalAuthority);
            }
        }
        palpo_hagency_contract::MatrixUserId::try_from(owner_mxid.clone())
            .map_err(|_| Error::LocalAuthority)?;
        if !matches!(source_schema, 71..=111) {
            return Err(Error::Schema);
        }
        Ok(Self(Manifest {
            version: 1,
            kind: "management".into(),
            migration_id,
            registry_id,
            profile_digest,
            owner_mxid,
            inventory_digest,
            source_schema,
        }))
    }
}

struct Archive {
    db: Connection,
    files: Files,
    _ownership: File,
}
struct Files {
    path: PathBuf,
    dir: Dir,
    retained: Vec<(&'static str, File)>,
}
impl Files {
    fn directory(path: &Path) -> Result<Dir, Error> {
        if !path.is_absolute() {
            return Err(Error::Private);
        }
        let mut root = PathBuf::new();
        let mut names = Vec::new();
        for part in path.components() {
            match part {
                Component::RootDir | Component::Prefix(_) => root.push(part.as_os_str()),
                Component::Normal(name) => names.push(name.to_owned()),
                _ => return Err(Error::Private),
            }
        }
        let mut dir = Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
        for name in names {
            dir = dir.open_dir_nofollow(name)?;
        }
        crate::private::check_handle(&dir.try_clone()?.into_std_file())?;
        Ok(dir)
    }
    fn file(dir: &Dir, name: &str) -> Result<File, Error> {
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let file = dir.open_with(name, &options)?.into_std();
        crate::private::check_handle(&file)?;
        if !file.metadata()?.is_file() {
            return Err(Error::Private);
        }
        Ok(file)
    }
    fn check(&self) -> Result<(), Error> {
        let current = Self::directory(&self.path)?;
        if !hagency_platform::same_directory(
            &self.dir.try_clone()?.into_std_file(),
            &current.try_clone()?.into_std_file(),
        )? {
            return Err(Error::Private);
        }
        for (name, file) in &self.retained {
            crate::private::check_handle(file)?;
            if !hagency_platform::same_file(file, &Self::file(&current, name)?)? {
                return Err(Error::Private);
            }
        }
        Ok(())
    }
}
impl Archive {
    fn open(path: &Path, binding: &ArchiveJournalBinding) -> Result<Self, Error> {
        let deadline = Instant::now() + DEADLINE;
        let dir = Files::directory(path)?;
        let ownership = Files::file(&dir, "domain.lock")?;
        ownership.try_lock().map_err(|_| Error::Locked)?;
        let mut files = Files {
            path: path.into(),
            dir,
            retained: vec![("domain.lock", ownership.try_clone()?)],
        };
        for name in ["domain.sqlite3", "domain.sqlite3-wal", "domain.sqlite3-shm"] {
            files.retained.push((name, Files::file(&files.dir, name)?));
        }
        if files.dir.symlink_metadata("domain.sqlite3-journal").is_ok() {
            return Err(Error::Quarantined);
        }
        // A fenced source must already have its committed WAL and shared-memory
        // companions. No CREATE, normal repository opener, recovery or DDL.
        let mut db = Connection::open_with_flags(
            path.join("domain.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.busy_timeout(Duration::from_millis(100))?;
        db.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
        db.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
        validate_bounded(&mut db, binding, deadline)?;
        db.pragma_update(None, "synchronous", "FULL")?;
        files.check()?;
        if Instant::now() >= deadline {
            return Err(Error::Busy);
        }
        db.authorizer(Some(authorize_sql))?;
        Ok(Self {
            db,
            files,
            _ownership: ownership,
        })
    }
    fn execute(&mut self, command: JournalCommand) -> Result<Vec<PendingIntent>, Error> {
        self.files.check()?;
        let authority = command.authority.clone();
        let deadline = command.deadline;
        self.db.progress_handler(
            1000,
            Some(move || !authority.is_live() || Instant::now() >= deadline),
        )?;
        let result = execute_intent(&mut self.db, command);
        self.db.progress_handler(0, None::<fn() -> bool>)?;
        if result.is_ok() && self.files.check().is_err() {
            return Err(Error::OutcomeUnknown);
        }
        result
    }
}

fn validate_bounded(
    db: &mut Connection,
    binding: &ArchiveJournalBinding,
    deadline: Instant,
) -> Result<(), Error> {
    if Instant::now() >= deadline {
        return Err(Error::Busy);
    }
    // Install before any manifest/shape/count/payload validation. A busy timeout
    // only bounds lock waits; this also bounds an existing source's query work.
    db.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
    let result = validate(db, binding);
    db.progress_handler(0, None::<fn() -> bool>)?;
    if Instant::now() >= deadline {
        Err(Error::Busy)
    } else {
        result
    }
}

fn validate(db: &mut Connection, binding: &ArchiveJournalBinding) -> Result<(), Error> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
    let app: i32 = tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
    let version: i32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let mode: String = tx.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
    if app != MANAGEMENT_ARCHIVE_APPLICATION_ID
        || version != binding.0.source_schema
        || mode != "wal"
    {
        return Err(Error::Schema);
    }
    for (table, columns) in [
        (
            "management_archive_identity",
            &["singleton", "manifest"][..],
        ),
        ("management_identity", &["singleton", "profile_digest"][..]),
        (
            "management_commands",
            &["actor_scope", "command_id", "operation", "digest", "result"][..],
        ),
        (
            "management_intents",
            &[
                "actor_scope",
                "command_id",
                "operation",
                "digest",
                "input",
                "reply",
                "state",
            ][..],
        ),
    ] {
        let ordinary: bool = tx
            .query_row(
                "SELECT type='table' FROM pragma_table_list WHERE schema='main' AND name=?1",
                [table],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false);
        if !ordinary {
            return Err(Error::Schema);
        }
        let actual = tx
            .prepare("SELECT name FROM pragma_table_info(?1) LIMIT 17")?
            .query_map([table], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if actual.len() != columns.len()
            || columns
                .iter()
                .any(|column| !actual.iter().any(|v| v == column))
        {
            return Err(Error::Schema);
        }
    }
    let manifest: Option<String> = tx.query_row("SELECT CASE WHEN length(CAST(manifest AS BLOB))<=4096 THEN manifest END FROM management_archive_identity WHERE singleton=1",[],|r|r.get(0))?;
    let stored: Manifest = serde_json::from_str(&manifest.ok_or(Error::Capacity)?)?;
    if stored != binding.0 {
        return Err(Error::LocalAuthority);
    }
    let profile: String = tx.query_row(
        "SELECT profile_digest FROM management_identity WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if profile != binding.0.profile_digest {
        return Err(Error::LocalAuthority);
    }
    for table in ["management_identity", "management_archive_identity"] {
        let count: i64 =
            tx.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
        if count != 1 {
            return Err(Error::Schema);
        }
    }
    for table in ["management_commands", "management_intents"] {
        let count: i64 = tx.query_row(
            &format!("SELECT COUNT(*) FROM (SELECT 1 FROM {table} LIMIT 100001)"),
            [],
            |r| r.get(0),
        )?;
        if count > 100_000 {
            return Err(Error::Capacity);
        }
    }
    let oversized: bool = tx.query_row(&format!("SELECT EXISTS(SELECT 1 FROM management_intents WHERE length(actor_scope)>64 OR length(command_id)>128 OR length(operation)>64 OR length(digest)<>64 OR length(CAST(input AS BLOB))>{} OR length(CAST(reply AS BLOB))>65536) OR EXISTS(SELECT 1 FROM management_commands WHERE length(actor_scope)>64 OR length(command_id)>128 OR length(operation)>64 OR length(digest)<>64 OR length(CAST(result AS BLOB))>65536)", intents::INPUT_LIMIT_SQL),[],|r|r.get(0))?;
    if oversized {
        return Err(Error::Capacity);
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn authorize_sql(context: AuthContext<'_>) -> Authorization {
    if context.accessor.is_some() || context.database_name.is_some_and(|v| v != "main") {
        return Authorization::Deny;
    }
    let allowed = match context.action {
        AuthAction::Select | AuthAction::Transaction { .. } => true,
        AuthAction::Read { table_name, .. } => {
            matches!(table_name, "management_intents" | "management_commands")
        }
        AuthAction::Insert { table_name } => table_name == "management_intents",
        AuthAction::Update {
            table_name,
            column_name,
        } => {
            table_name == "management_intents" && matches!(column_name, "state" | "input" | "reply")
        }
        AuthAction::Function { function_name } => matches!(function_name, "count" | "length"),
        _ => false,
    };
    if allowed {
        Authorization::Allow
    } else {
        Authorization::Deny
    }
}

enum Job {
    #[cfg(any(test, feature = "resource-adoption"))]
    AdoptionCustody {
        request: Box<crate::owner_resources::adoption::CustodyRequest>,
        reply: oneshot::Sender<
            Result<crate::owner_resources::adoption::VerifiedAdoptionJournal, Error>,
        >,
    },
    #[cfg(any(test, feature = "resource-adoption"))]
    EmptyDeviceCustody {
        request: Box<crate::owner_resources::adoption::EmptyDeviceCustodyRequest>,
        reply: oneshot::Sender<
            Result<crate::owner_resources::adoption::VerifiedEmptyDeviceJournal, Error>,
        >,
    },
    Command {
        command: Box<JournalCommand>,
        reply: oneshot::Sender<Result<Vec<PendingIntent>, Error>>,
        _bytes: OwnedSemaphorePermit,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
    #[cfg(test)]
    Pause {
        entered: oneshot::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    },
}
#[derive(Clone)]
pub struct PrivateManagementJournal {
    tx: mpsc::Sender<Job>,
    bytes: Arc<Semaphore>,
    owner: String,
    profile: String,
    #[cfg(any(test, feature = "resource-adoption"))]
    binding: ArchiveJournalBinding,
}
impl PrivateManagementJournal {
    /// Blocking initialization of an existing fenced source. The trusted host
    /// retains its original root ownership; this retains the domain writer lock.
    /// Does not create a source, fence one, migrate schema or recover execution.
    pub fn open_existing(directory: &Path, binding: ArchiveJournalBinding) -> Result<Self, Error> {
        let mut archive = Archive::open(directory, &binding)?;
        let (tx, mut rx) = mpsc::channel::<Job>(8);
        std::thread::Builder::new()
            .name("hagency-private-journal".into())
            .spawn(move || {
                while let Some(job) = rx.blocking_recv() {
                    match job {
                        #[cfg(any(test, feature = "resource-adoption"))]
                        Job::AdoptionCustody { request, reply } => {
                            if !reply.is_closed() {
                                let deadline = request.expires();
                                let result = (|| {
                                    request.check()?;
                                    archive.files.check()?;
                                    archive.db.progress_handler(
                                        1000,
                                        Some(move || Instant::now() >= deadline),
                                    )?;
                                    let result = request.execute(&archive.db);
                                    archive.db.progress_handler(0, None::<fn() -> bool>)?;
                                    archive.files.check()?;
                                    request.check()?;
                                    if Instant::now() >= deadline {
                                        return Err(Error::Busy);
                                    }
                                    result
                                })();
                                let _ = reply.send(result);
                            }
                        }
                        #[cfg(any(test, feature = "resource-adoption"))]
                        Job::EmptyDeviceCustody { request, reply } => {
                            if !reply.is_closed() {
                                let deadline = request.expires();
                                let result = (|| {
                                    request.check()?;
                                    archive.files.check()?;
                                    archive.db.progress_handler(
                                        1000,
                                        Some(move || Instant::now() >= deadline),
                                    )?;
                                    let result = request.execute(&archive.db);
                                    archive.db.progress_handler(0, None::<fn() -> bool>)?;
                                    archive.files.check()?;
                                    request.check()?;
                                    if Instant::now() >= deadline {
                                        return Err(Error::Busy);
                                    }
                                    result
                                })();
                                let _ = reply.send(result);
                            }
                        }
                        Job::Command {
                            command,
                            reply,
                            _bytes,
                        } => {
                            if !reply.is_closed() {
                                let result = archive.execute(*command).and_then(|result| {
                                    if serde_json::to_vec(&result)?.len() > MAX_OUTPUT {
                                        Err(Error::Capacity)
                                    } else {
                                        Ok(result)
                                    }
                                });
                                let _ = reply.send(result);
                            }
                        }
                        Job::Shutdown { reply } => {
                            drop(archive);
                            drop(rx);
                            let _ = reply.send(());
                            return;
                        }
                        #[cfg(test)]
                        Job::Pause { entered, release } => {
                            let _ = entered.send(());
                            let _ = release.recv();
                        }
                    }
                }
            })?;
        Ok(Self {
            tx,
            bytes: Arc::new(Semaphore::new(MAX_OUTPUT)),
            owner: binding.0.owner_mxid.clone(),
            profile: binding.0.profile_digest.clone(),
            #[cfg(any(test, feature = "resource-adoption"))]
            binding,
        })
    }
    #[cfg(any(test, feature = "resource-adoption"))]
    pub(crate) async fn adoption_custody(
        &self,
        binding: ArchiveJournalBinding,
        request: crate::owner_resources::adoption::CustodyRequest,
    ) -> Result<crate::owner_resources::adoption::VerifiedAdoptionJournal, Error> {
        if self.binding.0 != binding.0 {
            return Err(Error::LocalAuthority);
        }
        request.check()?;
        let deadline = request.expires();
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(Job::AdoptionCustody {
                request: Box::new(request),
                reply,
            })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => Error::Busy,
                mpsc::error::TrySendError::Closed(_) => Error::Unavailable,
            })?;
        tokio::time::timeout_at(deadline.into(), rx)
            .await
            .map_err(|_| Error::OutcomeUnknown)?
            .map_err(|_| Error::OutcomeUnknown)?
    }
    #[cfg(any(test, feature = "resource-adoption"))]
    pub(crate) async fn empty_device_custody(
        &self,
        binding: ArchiveJournalBinding,
        request: crate::owner_resources::adoption::EmptyDeviceCustodyRequest,
    ) -> Result<crate::owner_resources::adoption::VerifiedEmptyDeviceJournal, Error> {
        if self.binding.0 != binding.0 {
            return Err(Error::LocalAuthority);
        }
        request.check()?;
        let deadline = request.expires();
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(Job::EmptyDeviceCustody {
                request: Box::new(request),
                reply,
            })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => Error::Busy,
                mpsc::error::TrySendError::Closed(_) => Error::Unavailable,
            })?;
        tokio::time::timeout_at(deadline.into(), rx)
            .await
            .map_err(|_| Error::OutcomeUnknown)?
            .map_err(|_| Error::OutcomeUnknown)?
    }
    #[cfg(feature = "resource-adoption")]
    pub(crate) fn require_binding(&self, binding: &ArchiveJournalBinding) -> Result<(), Error> {
        if self.binding.0 != binding.0 || !self.writer_open() {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
    #[cfg(feature = "resource-adoption")]
    pub(crate) async fn adopted_command(
        &self,
        binding: &ArchiveJournalBinding,
        command: JournalCommand,
    ) -> Result<Vec<PendingIntent>, Error> {
        self.require_binding(binding)?;
        if command.authority.scope.context.account != self.owner || !command.authority.is_live() {
            return Err(Error::LocalAuthority);
        }
        let result = self.command(command).await?;
        self.require_binding(binding)
            .map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
    pub fn session(
        &self,
        authority: ManagementAuthority,
        authenticated_profile: &str,
    ) -> Result<ArchiveJournalSession, Error> {
        if authenticated_profile != self.profile
            || authority.scope.context.account != self.owner
            || !authority.is_live()
        {
            return Err(Error::LocalAuthority);
        }
        Ok(ArchiveJournalSession {
            journal: self.clone(),
            authority,
        })
    }
    pub fn writer_open(&self) -> bool {
        !self.tx.is_closed()
    }
    pub async fn shutdown(&self) -> Result<(), Error> {
        let (reply, rx) = oneshot::channel();
        tokio::time::timeout(DEADLINE, self.tx.send(Job::Shutdown { reply }))
            .await
            .map_err(|_| Error::OutcomeUnknown)?
            .map_err(|_| Error::Unavailable)?;
        tokio::time::timeout(DEADLINE, rx)
            .await
            .map_err(|_| Error::OutcomeUnknown)?
            .map_err(|_| Error::OutcomeUnknown)
    }
    async fn command(&self, command: JournalCommand) -> Result<Vec<PendingIntent>, Error> {
        let deadline = command.deadline;
        let bytes = self
            .bytes
            .clone()
            .try_acquire_many_owned(command.weight())
            .map_err(|_| Error::Busy)?;
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(Job::Command {
                command: Box::new(command),
                reply,
                _bytes: bytes,
            })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => Error::Busy,
                mpsc::error::TrySendError::Closed(_) => Error::Unavailable,
            })?;
        tokio::time::timeout_at(deadline.into(), rx)
            .await
            .map_err(|_| Error::OutcomeUnknown)?
            .map_err(|_| Error::OutcomeUnknown)?
    }
}

#[derive(Clone)]
pub struct ArchiveJournalSession {
    journal: PrivateManagementJournal,
    authority: ManagementAuthority,
}
impl ArchiveJournalSession {
    async fn journal(
        &self,
        context: &HostContext,
        action: JournalAction,
        permission: Permission,
        mutation: bool,
    ) -> Result<Vec<PendingIntent>, Error> {
        let _pending = self
            .authority
            .scope
            .pending
            .try_acquire()
            .map_err(|_| Error::Busy)?;
        self.authority.authorize(context, permission)?;
        let result = self
            .journal
            .command(JournalCommand::new(
                self.authority.clone(),
                context.clone(),
                action,
            ))
            .await?;
        self.authority
            .authorize(context, permission)
            .map_err(|error| {
                if mutation {
                    Error::OutcomeUnknown
                } else {
                    error
                }
            })?;
        Ok(result)
    }
    pub async fn prepare_intent(
        &self,
        context: &HostContext,
        intent: Intent,
    ) -> Result<PendingIntent, Error> {
        let permission = intent.permission();
        self.journal(context, JournalAction::Prepare(intent), permission, true)
            .await?
            .pop()
            .ok_or(Error::Schema)
    }
    pub async fn record_intent_reply(
        &self,
        context: &HostContext,
        intent: Intent,
        reply: IntentReply,
    ) -> Result<(), Error> {
        if serde_json::to_vec(&reply)?.len() > 64 * 1024 {
            return Err(Error::Capacity);
        }
        let permission = intent.permission();
        self.journal(
            context,
            JournalAction::Reply(intent, reply),
            permission,
            true,
        )
        .await?;
        Ok(())
    }
    pub async fn pending_intents(
        &self,
        context: &HostContext,
    ) -> Result<Vec<PendingIntent>, Error> {
        self.journal(context, JournalAction::Read, Permission::ReadIntents, false)
            .await
    }
    /// One saved command by its exact ID, as `Session::intent`.
    pub async fn intent(
        &self,
        context: &HostContext,
        command_id: String,
    ) -> Result<Option<PendingIntent>, Error> {
        identifier(&command_id, 128)?;
        Ok(self
            .journal(
                context,
                JournalAction::Lookup(command_id),
                Permission::ReadIntents,
                false,
            )
            .await?
            .pop())
    }
    /// Descriptive absence proof only. Scrubbed/acknowledged prior grant
    /// custody cannot establish which grant it belonged to, so fails closed.
    pub async fn require_unused_grant_publication(
        &self,
        context: &HostContext,
        grant_id: String,
    ) -> Result<(), Error> {
        identifier(&grant_id, 128)?;
        self.journal(
            context,
            JournalAction::RequireUnusedGrantPublication(grant_id),
            Permission::ReadIntents,
            false,
        )
        .await?;
        Ok(())
    }
    pub async fn acknowledge_intent(
        &self,
        context: &HostContext,
        command_id: String,
    ) -> Result<(), Error> {
        identifier(&command_id, 128)?;
        self.journal(
            context,
            JournalAction::Acknowledge(command_id),
            Permission::AcknowledgeIntents,
            true,
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
