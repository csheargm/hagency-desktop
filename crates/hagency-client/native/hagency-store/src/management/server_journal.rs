//! Private current-operation receipts bound to one authenticated profile.
use super::*;
use std::path::Path;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};

const DEADLINE: Duration = Duration::from_secs(2);
const SCHEMA: &str = concat!(
    include_str!("../migrations/070-management-commands.sql"),
    include_str!("../migrations/071-management-intents.sql"),
    "CREATE TABLE server_journal_identity(singleton INTEGER PRIMARY KEY CHECK(singleton=1), owner TEXT NOT NULL);"
);

enum Job {
    Command(
        Box<JournalCommand>,
        oneshot::Sender<Result<Vec<PendingIntent>, Error>>,
        OwnedSemaphorePermit,
    ),
    Shutdown(oneshot::Sender<()>),
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context(account: &str) -> HostContext {
        HostContext::new(account.into(), 1, "b".repeat(64), 1, "device".into(), 1).unwrap()
    }
    fn session(journal: &ServerManagementJournal, permissions: &[Permission]) -> Session {
        Session::new_server(
            journal.clone(),
            context("@admin:test"),
            permissions.iter().copied(),
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
    }
    #[tokio::test]
    async fn server_journal_preserves_original_requests_across_restart_and_denies_local_resources()
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server");
        let profile = "a".repeat(64);
        let owner = context("@admin:test");
        let journal = ServerManagementJournal::open(&path, &profile, "@admin:test").unwrap();
        let access = session(
            &journal,
            &[
                Permission::ReadConnections,
                Permission::ReadResources,
                Permission::ReadIntents,
                Permission::RequestProjects,
            ],
        );
        access
            .authorize(&owner, Permission::ReadConnections)
            .unwrap();
        assert!(matches!(
            access.resources(&owner, ResourceQuery::default()).await,
            Err(Error::ResourceSetupRequired)
        ));
        let original = Intent::new(
            IntentOperation::ProjectRequest,
            "original".into(),
            &serde_json::json!({"commandId":"original","requestedTokens":17}),
        )
        .unwrap();
        assert!(
            access
                .prepare_intent(&owner, original.clone())
                .await
                .unwrap()
                .is_first_attempt()
        );
        drop(access);
        journal.shutdown().await.unwrap();
        assert!(!path.join("domain.sqlite3").exists());
        assert!(matches!(
            ServerManagementJournal::open(&path, &profile, "@other:test"),
            Err(Error::LocalAuthority)
        ));
        assert!(matches!(
            ServerManagementJournal::open(&path, &"c".repeat(64), "@admin:test"),
            Err(Error::LocalAuthority)
        ));
        let journal = ServerManagementJournal::open(&path, &profile, "@admin:test").unwrap();
        let access = session(
            &journal,
            &[Permission::ReadIntents, Permission::RequestProjects],
        );
        let pending = access.pending_intents(&owner).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].command_id, "original");
        assert!(
            !access
                .prepare_intent(&owner, original.clone())
                .await
                .unwrap()
                .is_first_attempt()
        );
        let changed = Intent::new(
            IntentOperation::ProjectRequest,
            "original".into(),
            &serde_json::json!({"commandId":"original","requestedTokens":19}),
        )
        .unwrap();
        assert!(matches!(
            access.prepare_intent(&owner, changed).await,
            Err(Error::Conflict)
        ));
        access
            .record_intent_reply(
                &owner,
                original.clone(),
                IntentReply::Success(serde_json::json!({"ok":true})),
            )
            .await
            .unwrap();
        assert!(
            access
                .prepare_intent(&owner, original)
                .await
                .unwrap()
                .reply
                .is_some()
        );
        access.retire();
        assert!(matches!(
            access.pending_intents(&owner).await,
            Err(Error::LocalAuthority)
        ));
        journal.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn server_journal_requires_current_owner_permission_and_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let journal = ServerManagementJournal::open(
            &dir.path().join("server"),
            &"a".repeat(64),
            "@admin:test",
        )
        .unwrap();
        assert!(matches!(
            Session::new_server(
                journal.clone(),
                context("@other:test"),
                [],
                Instant::now() + Duration::from_secs(30)
            ),
            Err(Error::LocalAuthority)
        ));
        assert!(matches!(
            Session::new_server(journal.clone(), context("@admin:test"), [], Instant::now()),
            Err(Error::LocalAuthority)
        ));
        let access = session(&journal, &[Permission::ReadIntents]);
        let original = Intent::new(
            IntentOperation::ProjectRequest,
            "denied".into(),
            &serde_json::json!({"commandId":"denied"}),
        )
        .unwrap();
        assert!(matches!(
            access
                .prepare_intent(&context("@admin:test"), original)
                .await,
            Err(Error::LocalAuthority)
        ));
        assert!(
            access
                .pending_intents(&context("@admin:test"))
                .await
                .unwrap()
                .is_empty()
        );
        journal.shutdown().await.unwrap();
    }

}

#[derive(Clone)]
pub struct ServerManagementJournal {
    tx: mpsc::Sender<Job>,
    bytes: Arc<Semaphore>,
    owner: String,
    profile: String,
}

impl ServerManagementJournal {
    /// Blocking open on a native worker, scoped to an authenticated profile.
    pub fn open(directory: &Path, profile: &str, owner: &str) -> Result<Self, Error> {
        if profile.len() != 64
            || !profile
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(Error::LocalAuthority);
        }
        palpo_hagency_contract::MatrixUserId::try_from(owner.to_owned())
            .map_err(|_| Error::LocalAuthority)?;
        let mut database = crate::database::open(
            directory,
            crate::database::Schema {
                name: "server-management.sqlite3",
                lock: "server-management.lock",
                application_id: 0x48415331,
                version: 1,
                migrations: &[],
                sql: SCHEMA,
                verify: &[
                    "SELECT profile_digest FROM management_identity LIMIT 0",
                    "SELECT owner FROM server_journal_identity LIMIT 0",
                    "SELECT actor_scope,command_id,operation,digest,input,reply,state FROM management_intents LIMIT 0",
                ],
            },
        )?;
        let tx = database
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT profile_digest FROM management_identity WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            let existing_owner: String = tx.query_row(
                "SELECT owner FROM server_journal_identity WHERE singleton=1",
                [],
                |r| r.get(0),
            )?;
            if existing != profile || existing_owner != owner {
                return Err(Error::LocalAuthority);
            }
        } else {
            let populated: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM management_intents) OR EXISTS(SELECT 1 FROM management_commands) OR EXISTS(SELECT 1 FROM server_journal_identity)", [], |r| r.get(0))?;
            if populated {
                return Err(Error::LocalAuthority);
            }
            tx.execute("INSERT INTO management_identity VALUES(1,?1)", [profile])?;
            tx.execute("INSERT INTO server_journal_identity VALUES(1,?1)", [owner])?;
        }
        tx.commit()?;
        Self::start(database, profile, owner)
    }
    fn start(
        mut database: crate::database::Database,
        profile: &str,
        owner: &str,
    ) -> Result<Self, Error> {
        database
            .connection
            .authorizer(Some(super::journal_sql::authorize_sql))?;
        let (tx, mut rx) = mpsc::channel::<Job>(8);
        std::thread::Builder::new()
            .name("hagency-server-journal".into())
            .spawn(move || {
                while let Some(job) = rx.blocking_recv() {
                    match job {
                        Job::Command(command, reply, _bytes) => {
                            let authority = command.authority.clone();
                            let deadline = command.deadline;
                            let result = (|| {
                                database.connection.progress_handler(
                                    1000,
                                    Some(move || {
                                        !authority.is_live() || Instant::now() >= deadline
                                    }),
                                )?;
                                let result = execute_intent(&mut database.connection, *command);
                                database
                                    .connection
                                    .progress_handler(0, None::<fn() -> bool>)?;
                                result
                            })();
                            let _ = reply.send(result);
                        }
                        Job::Shutdown(reply) => {
                            drop(database);
                            let _ = reply.send(());
                            return;
                        }
                    }
                }
            })?;
        Ok(Self {
            tx,
            bytes: Arc::new(Semaphore::new(8 * 1024 * 1024)),
            owner: owner.into(),
            profile: profile.into(),
        })
    }
    pub(crate) fn profile(&self) -> &str { &self.profile }
    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }
    pub fn is_live(&self) -> bool {
        !self.tx.is_closed()
    }
    pub(crate) async fn command(
        &self,
        command: JournalCommand,
    ) -> Result<Vec<PendingIntent>, Error> {
        if command.authority.scope.context.account != self.owner || !command.authority.is_live() {
            return Err(Error::LocalAuthority);
        }
        let deadline = command.deadline;
        let bytes = self
            .bytes
            .clone()
            .try_acquire_many_owned(command.weight())
            .map_err(|_| Error::Busy)?;
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(Job::Command(Box::new(command), reply, bytes))
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => Error::Busy,
                mpsc::error::TrySendError::Closed(_) => Error::Unavailable,
            })?;
        tokio::time::timeout_at(deadline.into(), rx)
            .await
            .map_err(|_| Error::OutcomeUnknown)?
            .map_err(|_| Error::OutcomeUnknown)?
    }
    pub async fn shutdown(&self) -> Result<(), Error> {
        let (reply, rx) = oneshot::channel();
        tokio::time::timeout(DEADLINE, self.tx.send(Job::Shutdown(reply)))
            .await
            .map_err(|_| Error::OutcomeUnknown)?
            .map_err(|_| Error::Unavailable)?;
        tokio::time::timeout(DEADLINE, rx)
            .await
            .map_err(|_| Error::OutcomeUnknown)?
            .map_err(|_| Error::OutcomeUnknown)
    }
}
