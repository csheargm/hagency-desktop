//! Owner-local policies and crash-safe token accounting. No server approval or model secrets.
//! A host must authenticate the owner, validate server binding records and enforce tool
//! interception before using this library. Each call ID denotes ONE provider invocation:
//! a retry which can charge again requires a fresh call ID under the original requester.
pub mod codex;
pub mod inbox;
pub mod questions;
pub mod reconciliation;
#[cfg(unix)]
pub mod room_files;
pub mod runtime;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid input: {0}")]
    Invalid(&'static str),
    #[error("owner or scope mismatch")]
    Unauthorized,
    #[error("foreign or legacy database")]
    ForeignDatabase,
    #[error("budget is exhausted")]
    Budget,
    #[error("request denied or requires owner confirmation")]
    Denied,
    #[error("provider execution outcome is unknown")]
    Unknown,
    #[error("installation resource ownership unavailable")]
    Installation(#[from] hagency_platform::legacy_writer::Error),
    #[error("idempotency or revision conflict")]
    Conflict,
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
const APP_ID: i64 = 0x48414c32;
const MAX: u64 = 9_007_199_254_740_991;
const UNREPORTED_ZERO: &str = "unreported-zero-v1";
const UNREPORTED_USAGE: &str = "unreported-v2";

/// Missing usage has no measured categories and cannot settle a reservation.
fn unreported_usage() -> Usage {
    Usage {
        input: 0,
        output: 0,
        cached_input: 0,
        cache_write_input: None,
        reasoning_output: 0,
        accounting_version: UNREPORTED_USAGE.into(),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub agent: String,
    pub binding: String,
    pub room: String,
    pub requester: String,
    pub thread: String,
}
impl Scope {
    fn validate(&self) -> Result<()> {
        for value in [
            &self.agent,
            &self.binding,
            &self.room,
            &self.requester,
            &self.thread,
        ] {
            key(value)?;
        }
        if !self.room.starts_with('!')
            || !self.requester.starts_with('@')
            || !self.requester.contains(':')
        {
            return Err(Error::Invalid("Matrix identity"));
        }
        Ok(())
    }
    pub fn context_key(&self, owner: &str) -> Result<String> {
        json(&(
            owner,
            &self.agent,
            &self.binding,
            &self.room,
            &self.requester,
            &self.thread,
        ))
    }
    fn layers(&self) -> Result<[String; 3]> {
        Ok([
            json(&("agent", &self.agent))?,
            json(&("room", &self.agent, &self.binding))?,
            json(&("requester", &self.agent, &self.binding, &self.requester))?,
        ])
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Period {
    Lifetime,
    UtcDay,
    UtcMonth,
}
impl Period {
    fn window(self, now: i64) -> Result<String> {
        if now < 0 {
            return Err(Error::Invalid("time"));
        }
        let t =
            time::OffsetDateTime::from_unix_timestamp(now).map_err(|_| Error::Invalid("time"))?;
        Ok(match self {
            Self::Lifetime => "lifetime".into(),
            Self::UtcDay => format!("day:{}-{}-{}", t.year(), u8::from(t.month()), t.day()),
            Self::UtcMonth => format!("month:{}-{}", t.year(), u8::from(t.month())),
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Limit {
    /// Retained wire encoding for an absent limit; equivalent to `Unlimited`.
    Unset,
    Unlimited,
    Tokens(u64),
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Budget {
    pub limit: Limit,
    pub period: Period,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RequestPolicy {
    Allow,
    Deny,
    AskOwner,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ToolPolicy {
    Deny,
    AskOwner,
    AllowWithRules {
        tools: Vec<String>,
        directories: Vec<String>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub budget: Budget,
    pub requests: RequestPolicy,
    pub high_risk: ToolPolicy,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cached_input: u64,
    /// Included in `input`, just like cached_input. Historical records did
    /// not retain this category, so absence must remain unknown, not zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_input: Option<u64>,
    pub reasoning_output: u64,
    pub accounting_version: String,
}
impl Usage {
    pub fn is_reported(&self) -> bool {
        !matches!(
            self.accounting_version.as_str(),
            UNREPORTED_ZERO | UNREPORTED_USAGE
        )
    }
    /// Provider-measured categories for analytics. Reservations and historical
    /// records without cache-write evidence must never become measured zeros.
    pub fn category_counts(&self) -> (Option<u64>, Option<u64>, Option<u64>, Option<u64>) {
        if !self.is_reported() {
            return (None, None, None, None);
        }
        let fresh = self.cache_write_input.and_then(|write| {
            self.input
                .checked_sub(self.cached_input)?
                .checked_sub(write)
        });
        (
            fresh,
            Some(self.cached_input),
            self.cache_write_input,
            Some(self.output),
        )
    }
    fn total(&self) -> Result<u64> {
        key(&self.accounting_version)?;
        if self.cached_input > self.input
            || self.reasoning_output > self.output
            || self
                .cache_write_input
                .is_some_and(|write| write > self.input - self.cached_input)
        {
            return Err(Error::Invalid("usage subcategory exceeds total"));
        }
        bounded(
            self.input
                .checked_add(self.output)
                .ok_or(Error::Invalid("overflow"))?,
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToolProposal {
    pub scope: Scope,
    pub dispatch: String,
    pub tool: String,
    pub arguments: serde_json::Value,
    pub canonical_directory: String,
    pub risk: String,
    pub policy_revision: [i64; 3],
    pub expires: i64,
}
/// A reference to a local OS credential store entry; NEVER store the actual model token.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelProfile {
    pub model: String,
    pub credential_ref: String,
    pub workspace_root: String,
    #[serde(default)]
    pub reasoning_effort: String,
    #[serde(default = "default_web_search")]
    pub web_search: bool,
}
pub const fn default_web_search() -> bool {
    true
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PolicyVersion {
    pub revision: i64,
    pub policy: Policy,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutstandingCall {
    pub id: String,
    pub scope: Scope,
    pub reserved: u64,
    pub state: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reservation {
    New,
    AlreadyPending,
    AlreadySettled(Usage),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Snapshot {
    scope: String,
    window: String,
}
#[derive(Clone, Debug)]
pub enum Layer {
    Agent,
    Room,
    Requester,
}

pub struct Ledger {
    db: Connection,
    owner: String,
    _legacy_writer: hagency_platform::legacy_writer::LegacyWriterAdmission,
}
impl Ledger {
    /// Open only the new schema. Identity must come from the local authenticated session.
    pub fn open(path: impl AsRef<Path>, owner: &str) -> Result<Self> {
        key(owner)?;
        if !owner.starts_with('@') || !owner.contains(':') {
            return Err(Error::Invalid("owner Matrix ID"));
        }
        #[cfg(test)]
        hagency_platform::legacy_writer::test_support::initialize_process_root()?;
        let legacy_writer =
            hagency_platform::legacy_writer::LegacyWriterAdmission::acquire_for_legacy_startup()?;
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        // Serialize the decision as well as DDL: a waiter must see the schema
        // committed by the first opener rather than its own pre-lock snapshot.
        db.execute_batch("BEGIN IMMEDIATE")?;
        let app: i64 = db.pragma_query_value(None, "application_id", |r| r.get(0))?;
        let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let count: i64 = db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )?;
        if app == 0 && version == 0 && count == 0 {
            db.execute_batch(include_str!("schema.sql"))?;
            db.execute("INSERT INTO identity VALUES (?)", [owner])?;
            db.pragma_update(None, "application_id", APP_ID)?;
            db.pragma_update(None, "user_version", 1)?;
        } else if app != APP_ID || version != 1 {
            return Err(Error::ForeignDatabase);
        }
        let stored: String = db.query_row("SELECT owner FROM identity", [], |r| r.get(0))?;
        if stored != owner {
            return Err(Error::Unauthorized);
        }
        let has_effort: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('model_profiles') WHERE name='reasoning_effort')",
            [], |r| r.get(0),
        )?;
        if !has_effort {
            db.execute_batch(
                "ALTER TABLE model_profiles ADD COLUMN reasoning_effort TEXT NOT NULL DEFAULT ''",
            )?;
        }
        let has_web_search: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('model_profiles') WHERE name='web_search')", [], |r|r.get(0))?;
        if !has_web_search {
            db.execute_batch("ALTER TABLE model_profiles ADD COLUMN web_search INTEGER NOT NULL DEFAULT 1 CHECK(web_search IN (0,1))")?;
        }
        db.execute_batch("CREATE TABLE IF NOT EXISTS external_sessions (scope TEXT NOT NULL, profile TEXT NOT NULL, session TEXT NOT NULL, counters TEXT NOT NULL DEFAULT '', PRIMARY KEY(scope,profile))")?;
        db.execute_batch("COMMIT")?;
        db.pragma_update(None, "foreign_keys", true)?;
        Ok(Self {
            db,
            owner: owner.into(),
            _legacy_writer: legacy_writer,
        })
    }
    /// Bind fresh local data permanently to the authenticated server/issuer/sub/MXID
    /// digest. A copied database from another profile is rejected even if its MXID matches.
    /// Existing populated unscoped data is never adopted or migrated.
    pub fn open_scoped(path: impl AsRef<Path>, owner: &str, profile: &str) -> Result<Self> {
        if profile.len() != 64
            || !profile
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Invalid("profile identity"));
        }
        let mut ledger = Self::open(path, owner)?;
        let tx = ledger
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='profile_identity')", [], |r|r.get(0))?;
        if !exists {
            return Err(Error::ForeignDatabase);
        }
        let stored: Option<String> = tx
            .query_row(
                "SELECT digest FROM profile_identity WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        match stored {
            Some(stored) if stored != profile => return Err(Error::Unauthorized),
            Some(_) => {}
            None => {
                let tables = tx.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT IN ('identity','profile_identity','inbox_limits')")?.query_map([], |r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
                for table in tables {
                    let query = format!(
                        "SELECT EXISTS(SELECT 1 FROM \"{}\")",
                        table.replace('"', "\"\"")
                    );
                    if tx.query_row(&query, [], |r| r.get::<_, bool>(0))? {
                        return Err(Error::ForeignDatabase);
                    }
                }
                tx.execute(
                    "INSERT INTO profile_identity(singleton,digest) VALUES(1,?)",
                    [profile],
                )?;
            }
        }
        tx.commit()?;
        Ok(ledger)
    }
    fn owner(&self, owner: &str) -> Result<()> {
        if owner == self.owner {
            Ok(())
        } else {
            Err(Error::Unauthorized)
        }
    }
    /// Only accept a binding already verified against the server's owner-scoped API.
    pub fn register_binding(&mut self, owner: &str, scope: &Scope) -> Result<()> {
        self.owner(owner)?;
        scope.validate()?;
        self.db.execute(
            "INSERT INTO bindings VALUES (?,?,?) ON CONFLICT(id) DO NOTHING",
            (&scope.binding, &scope.agent, &scope.room),
        )?;
        verify(&self.db, scope)
    }
    pub fn set_policy(
        &mut self,
        owner: &str,
        scope: &Scope,
        layer: Layer,
        expected_revision: i64,
        policy: &Policy,
    ) -> Result<i64> {
        self.owner(owner)?;
        verify(&self.db, scope)?;
        let index = match layer {
            Layer::Agent => 0,
            Layer::Room => 1,
            Layer::Requester => 2,
        };
        self.write_policy(&scope.layers()?[index], expected_revision, policy)
    }
    /// The host must verify this agent against its authenticated owner API first.
    /// Uses the same key as runtime accounting, without inventing a Room binding.
    pub fn set_agent_policy(
        &mut self,
        owner: &str,
        agent: &str,
        expected_revision: i64,
        policy: &Policy,
    ) -> Result<i64> {
        self.owner(owner)?;
        key(agent)?;
        self.write_policy(&json(&("agent", agent))?, expected_revision, policy)
    }
    fn write_policy(&mut self, k: &str, expected_revision: i64, policy: &Policy) -> Result<i64> {
        if let Limit::Tokens(n) = policy.budget.limit {
            bounded(n)?;
        }
        if expected_revision < 0 || expected_revision == i64::MAX {
            return Err(Error::Invalid("revision"));
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: i64 = tx
            .query_row("SELECT revision FROM policies WHERE scope=?", [&k], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(0);
        if current != expected_revision {
            return Err(Error::Conflict);
        }
        tx.execute("INSERT INTO policies VALUES (?,?,?) ON CONFLICT(scope) DO UPDATE SET revision=excluded.revision,config=excluded.config",(&k,current+1,json(policy)?))?;
        tx.commit()?;
        Ok(current + 1)
    }
    pub fn agent_policy(&self, owner: &str, agent: &str) -> Result<PolicyVersion> {
        self.owner(owner)?;
        key(agent)?;
        let k = json(&("agent", agent))?;
        let revision = self
            .db
            .query_row("SELECT revision FROM policies WHERE scope=?", [&k], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(0);
        Ok(PolicyVersion {
            revision,
            policy: policy(&self.db, &k, 0)?,
        })
    }
    pub fn agent_account(
        &self,
        owner: &str,
        agent: &str,
        period: Period,
        now: i64,
    ) -> Result<(u64, u64)> {
        self.owner(owner)?;
        key(agent)?;
        Ok(self
            .db
            .query_row(
                "SELECT spent,held FROM accounts WHERE scope=? AND window=?",
                (json(&("agent", agent))?, period.window(now)?),
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .unwrap_or((0, 0)))
    }
    pub fn policy_snapshot(&self, scope: &Scope) -> Result<[PolicyVersion; 3]> {
        verify(&self.db, scope)?;
        let layers = scope.layers()?;
        let mut versions = Vec::with_capacity(3);
        for (index, k) in layers.iter().enumerate() {
            let revision = self
                .db
                .query_row("SELECT revision FROM policies WHERE scope=?", [k], |r| {
                    r.get(0)
                })
                .optional()?
                .unwrap_or(0);
            versions.push(PolicyVersion {
                revision,
                policy: policy(&self.db, k, index)?,
            });
        }
        versions.try_into().map_err(|_| Error::Conflict)
    }
    /// Owner-only reconciliation view; contains no messages or model credentials.
    pub fn outstanding_calls(&self, owner: &str) -> Result<Vec<OutstandingCall>> {
        self.owner(owner)?;
        let mut stmt = self.db.prepare(
            "SELECT id,scope,reserved,state FROM calls WHERE state!='settled' ORDER BY id",
        )?;
        let records = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut calls = vec![];
        for record in records {
            let (id, scope, reserved, state) = record?;
            calls.push(OutstandingCall {
                id,
                scope: serde_json::from_str(&scope)?,
                reserved,
                state,
            });
        }
        Ok(calls)
    }
    /// Reserve a verifiable maximum input+output count BEFORE starting the provider.
    /// `request_confirmed` is intentionally absent: owner confirmations are scoped records.
    pub fn reserve(
        &mut self,
        scope: &Scope,
        call: &str,
        dispatch: &str,
        maximum: u64,
        now: i64,
    ) -> Result<Reservation> {
        if maximum == 0 { return Err(Error::Invalid("zero reservation")); }
        self.begin_call(scope, call, dispatch, Some(maximum), now)
    }
    /// Admit a new task from its known consumption, without freezing an estimate.
    /// Native reasoning/tool loops may exceed the budget; final receipts charge
    /// their full usage. Unknown calls remain pending measurement with zero held.
    pub fn begin_postpaid(
        &mut self,
        scope: &Scope,
        call: &str,
        dispatch: &str,
        now: i64,
    ) -> Result<Reservation> {
        self.begin_call(scope, call, dispatch, None, now)
    }
    fn begin_call(
        &mut self,
        scope: &Scope,
        call: &str,
        dispatch: &str,
        estimate: Option<u64>,
        now: i64,
    ) -> Result<Reservation> {
        let maximum = estimate.unwrap_or(0);
        verify(&self.db, scope)?;
        key(call)?;
        if call.starts_with("bound-breach:") {
            return Err(Error::Invalid("reserved call ID"));
        }
        key(dispatch)?;
        bounded(maximum)?;
        // Zero was never accepted by reserve(). It identifies a postpaid call
        // without rewriting old reservation digests or saved commands.
        let digest = call_digest(scope, dispatch, maximum)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<(String, String, Option<String>)> = tx
            .query_row(
                "SELECT digest,state,usage FROM calls WHERE id=?",
                [call],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((old, state, usage)) = previous {
            if old != digest {
                return Err(Error::Conflict);
            }
            return match state.as_str() {
                "pending" => Ok(Reservation::AlreadyPending),
                "settled" => Ok(Reservation::AlreadySettled(serde_json::from_str(
                    &usage.ok_or(Error::Conflict)?,
                )?)),
                _ => Err(Error::Unknown),
            };
        }
        let prior: Option<(String, String)> = tx
            .query_row(
                "SELECT scope,policies FROM dispatches WHERE id=?",
                [dispatch],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let original: Option<Vec<(i64, Policy)>> = match prior {
            Some((stored, policies)) => {
                if stored != json(scope)? {
                    return Err(Error::Unauthorized);
                }
                Some(serde_json::from_str(&policies)?)
            }
            None => None,
        };
        let continuing_postpaid: bool = estimate.is_none() && tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM calls WHERE digest=? AND reserved=0)",
            [&digest], |row| row.get(0),
        )?;
        let mut captured = vec![];
        let mut snapshots = vec![];
        for (index, k) in scope.layers()?.iter().enumerate() {
            let mut policy = policy(&tx, k, index)?;
            let revision: i64 = tx
                .query_row("SELECT revision FROM policies WHERE scope=?", [k], |r| {
                    r.get(0)
                })
                .optional()?
                .unwrap_or(0);
            captured.push((revision, policy.clone()));
            if let Some(original) = &original {
                let (old_revision, old) = original.get(index).ok_or(Error::Conflict)?;
                if old.budget.period != policy.budget.period {
                    return Err(Error::Conflict);
                }
                policy.budget.limit = intersect(&old.budget.limit, &policy.budget.limit);
                match old.requests {
                    RequestPolicy::Deny => return Err(Error::Denied),
                    RequestPolicy::AskOwner => {
                        let approval = hash(&("request", scope, dispatch, k, old_revision))?;
                        let ok:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM approvals WHERE digest=? AND consumed=0 AND expires>?)",(&approval,now),|r|r.get(0))?;
                        if !ok {
                            return Err(Error::Denied);
                        }
                    }
                    RequestPolicy::Allow => {}
                }
            }
            match policy.requests {
                RequestPolicy::Deny => return Err(Error::Denied),
                RequestPolicy::AskOwner => {
                    let revision: i64 =
                        tx.query_row("SELECT revision FROM policies WHERE scope=?", [k], |r| {
                            r.get(0)
                        })?;
                    let approval = hash(&("request", scope, dispatch, k, revision))?;
                    let ok: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM approvals WHERE digest=? AND consumed=0 AND expires>?)",(&approval,now),|r|r.get(0))?;
                    if !ok {
                        return Err(Error::Denied);
                    }
                    // Confirmation covers the dispatch, including its later tool-loop model calls.
                }
                RequestPolicy::Allow => {}
            }
            let window = policy.budget.period.window(now)?;
            let (spent, held): (u64, u64) = tx
                .query_row(
                    "SELECT spent,held FROM accounts WHERE scope=? AND window=?",
                    (k, &window),
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .unwrap_or((0, 0));
            if let Some(maximum) = estimate {
                let sum = bounded(spent.checked_add(held)
                    .and_then(|n| n.checked_add(maximum)).ok_or(Error::Budget)?)?;
                if matches!(policy.budget.limit, Limit::Tokens(n) if sum > n) {
                    return Err(Error::Budget);
                }
            } else if !continuing_postpaid
                && matches!(policy.budget.limit, Limit::Tokens(n) if spent >= n)
            {
                // Only a new task is checked. Other unfinished work and legacy
                // unknown estimates are not fictitious consumption. Their
                // original records remain available for later reconciliation.
                return Err(Error::Budget);
            }
            tx.execute("INSERT INTO accounts VALUES (?,?,0,?) ON CONFLICT(scope,window) DO UPDATE SET held=held+excluded.held",(k,&window,maximum))?;
            snapshots.push(Snapshot {
                scope: k.clone(),
                window,
            });
        }
        tx.execute(
            "INSERT INTO dispatches VALUES (?,?,?) ON CONFLICT(id) DO NOTHING",
            (dispatch, json(scope)?, json(&captured)?),
        )?;
        tx.execute(
            "INSERT INTO calls VALUES (?,?,?,?,?,?,'pending',NULL)",
            (
                call,
                &scope.binding,
                json(scope)?,
                digest,
                maximum,
                json(&snapshots)?,
            ),
        )?;
        tx.commit()?;
        Ok(Reservation::New)
    }
    pub fn settle(&mut self, scope: &Scope, call: &str, usage: &Usage) -> Result<()> {
        verify(&self.db, scope)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        settle_transaction(&tx, scope, call, usage)?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn settle_codex(
        &mut self,
        scope: &Scope,
        call: &str,
        usage: &Usage,
        context: &str,
        counters: &str,
    ) -> Result<()> {
        verify(&self.db, scope)?;
        let base = scope.context_key(&self.owner)?;
        let valid = context == base;
        #[cfg(unix)]
        let valid = valid || context == format!("{}:{base}", crate::codex::FILE_CAPABILITY_VERSION);
        if !valid {
            return Err(Error::Unauthorized);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        settle_transaction(&tx, scope, call, usage)?;
        tx.execute("INSERT INTO codex_usage VALUES (?,?) ON CONFLICT(scope) DO UPDATE SET counters=excluded.counters",(context,counters))?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn settle_external(
        &mut self,
        scope: &Scope,
        call: &str,
        usage: &Usage,
        profile: &str,
        session: &str,
        counters: &str,
    ) -> Result<()> {
        verify(&self.db, scope)?;
        key(profile)?;
        key(session)?;
        let context = scope.context_key(&self.owner)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        settle_transaction(&tx, scope, call, usage)?;
        let counters = if !usage.is_reported() && counters.is_empty() {
            "null"
        } else {
            counters
        };
        tx.execute("INSERT INTO external_sessions VALUES (?,?,?,?) ON CONFLICT(scope,profile) DO UPDATE SET session=excluded.session,counters=excluded.counters",(context,profile,session,counters))?;
        tx.commit()?;
        Ok(())
    }
    /// Retain the native session as soon as the admitted call observes its
    /// identity. A crash before the result must not discard the conversation.
    pub(crate) fn bind_external_session(
        &mut self,
        scope: &Scope,
        call: &str,
        profile: &str,
        session: &str,
        initial: &Usage,
    ) -> Result<()> {
        verify(&self.db, scope)?;
        key(profile)?;
        key(session)?;
        let context = scope.context_key(&self.owner)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM calls WHERE id=? AND scope=? AND state='pending')",
            (call, json(scope)?),
            |r| r.get(0),
        )?;
        if !active {
            return Err(Error::Unauthorized);
        }
        let prior: Option<String> = tx
            .query_row(
                "SELECT session FROM external_sessions WHERE scope=? AND profile=?",
                (&context, profile),
                |r| r.get(0),
            )
            .optional()?;
        if prior.as_deref().is_some_and(|id| id != session) {
            return Err(Error::Conflict);
        }
        tx.execute(
            "INSERT INTO external_sessions VALUES (?,?,?,?) ON CONFLICT(scope,profile) DO NOTHING",
            (&context, profile, session, json(initial)?),
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn mark_unknown(&mut self, scope: &Scope, call: &str) -> Result<()> {
        verify(&self.db, scope)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pending = tx.query_row(
            "SELECT state!='settled' FROM calls WHERE id=? AND scope=?",
            (call, json(scope)?),
            |r| r.get::<_, bool>(0),
        )?;
        if pending {
            settle_transaction(&tx, scope, call, &unreported_usage())?;
            mark_usage_gap(&tx, scope, &self.owner)?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Exclusive execution-device restart: keep interrupted usage unresolved.
    /// Estimates remain held until a measured or explicitly reconciled receipt.
    pub fn recover_interrupted(&mut self, owner: &str) -> Result<usize> {
        self.owner(owner)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = recover_pending_calls(&tx, owner, None)?;
        tx.commit()?;
        Ok(changed)
    }
    pub fn account(
        &self,
        scope: &Scope,
        layer: Layer,
        period: Period,
        now: i64,
    ) -> Result<(u64, u64)> {
        verify(&self.db, scope)?;
        let index = match layer {
            Layer::Agent => 0,
            Layer::Room => 1,
            Layer::Requester => 2,
        };
        Ok(self
            .db
            .query_row(
                "SELECT spent,held FROM accounts WHERE scope=? AND window=?",
                (&scope.layers()?[index], period.window(now)?),
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .unwrap_or((0, 0)))
    }
    pub fn set_model_profile(
        &mut self,
        owner: &str,
        scope: &Scope,
        profile: &ModelProfile,
    ) -> Result<()> {
        self.owner(owner)?;
        verify(&self.db, scope)?;
        self.write_model_profile(&scope.agent, profile)
    }
    pub fn set_agent_model_profile(
        &mut self,
        owner: &str,
        agent: &str,
        profile: &ModelProfile,
    ) -> Result<()> {
        self.owner(owner)?;
        key(agent)?;
        self.write_model_profile(agent, profile)
    }
    /// Compare the complete private model/workspace snapshot under the writer lock.
    /// A stale editor cannot overwrite a later provider or workspace choice.
    pub fn set_agent_model_profile_checked(
        &mut self,
        owner: &str,
        agent: &str,
        expected_digest: &str,
        profile: &ModelProfile,
    ) -> Result<()> {
        self.owner(owner)?;
        key(agent)?;
        if expected_digest.len() != 64
            || !expected_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Invalid("model digest"));
        }
        validate_model_profile(profile)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read_model_profile_on(&tx, agent)?;
        if model_profile_digest(current.as_ref())? != expected_digest {
            return Err(Error::Conflict);
        }
        write_model_profile_on(&tx, agent, profile)?;
        tx.commit()?;
        Ok(())
    }
    fn write_model_profile(&mut self, agent: &str, profile: &ModelProfile) -> Result<()> {
        validate_model_profile(profile)?;
        write_model_profile_on(&self.db, agent, profile)
    }
    pub fn model_profile(&self, scope: &Scope) -> Result<Option<ModelProfile>> {
        verify(&self.db, scope)?;
        self.read_model_profile(&scope.agent)
    }
    pub fn agent_model_profile(&self, owner: &str, agent: &str) -> Result<Option<ModelProfile>> {
        self.owner(owner)?;
        key(agent)?;
        self.read_model_profile(agent)
    }
    fn read_model_profile(&self, agent: &str) -> Result<Option<ModelProfile>> {
        read_model_profile_on(&self.db, agent)
    }
    pub fn context_session(&self, scope: &Scope) -> Result<Option<String>> {
        verify(&self.db, scope)?;
        Ok(self
            .db
            .query_row(
                "SELECT model_session FROM contexts WHERE scope=?",
                [scope.context_key(&self.owner)?],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn set_context_session(&mut self, scope: &Scope, session: &str) -> Result<()> {
        verify(&self.db, scope)?;
        key(session)?;
        self.db.execute("INSERT INTO contexts VALUES (?,?) ON CONFLICT(scope) DO UPDATE SET model_session=excluded.model_session",(scope.context_key(&self.owner)?,session))?;
        Ok(())
    }
    pub fn request_disposition(&self, scope: &Scope, dispatch: &str) -> Result<RequestPolicy> {
        verify(&self.db, scope)?;
        key(dispatch)?;
        let prior: Option<(String, String)> = self
            .db
            .query_row(
                "SELECT scope,policies FROM dispatches WHERE id=?",
                [dispatch],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let mut ask = false;
        if let Some((stored, policies)) = prior {
            if stored != json(scope)? {
                return Err(Error::Unauthorized);
            }
            for (_, p) in serde_json::from_str::<Vec<(i64, Policy)>>(&policies)? {
                match p.requests {
                    RequestPolicy::Deny => return Ok(RequestPolicy::Deny),
                    RequestPolicy::AskOwner => ask = true,
                    _ => {}
                }
            }
        }
        for version in self.policy_snapshot(scope)? {
            match version.policy.requests {
                RequestPolicy::Deny => return Ok(RequestPolicy::Deny),
                RequestPolicy::AskOwner => ask = true,
                _ => {}
            }
        }
        Ok(if ask {
            RequestPolicy::AskOwner
        } else {
            RequestPolicy::Allow
        })
    }
    #[allow(clippy::too_many_arguments)]
    pub fn approve_request_exact(
        &mut self,
        owner: &str,
        scope: &Scope,
        dispatch: &str,
        revisions: [i64; 3],
        expires: i64,
        now: i64,
    ) -> Result<()> {
        self.approve_request_bound(owner, scope, dispatch, Some(revisions), expires, now)
    }
    pub fn approve_request(
        &mut self,
        owner: &str,
        scope: &Scope,
        dispatch: &str,
        expires: i64,
        now: i64,
    ) -> Result<()> {
        self.approve_request_bound(owner, scope, dispatch, None, expires, now)
    }
    #[allow(clippy::too_many_arguments)]
    fn approve_request_bound(
        &mut self,
        owner: &str,
        scope: &Scope,
        dispatch: &str,
        expected: Option<[i64; 3]>,
        expires: i64,
        now: i64,
    ) -> Result<()> {
        self.owner(owner)?;
        verify(&self.db, scope)?;
        key(dispatch)?;
        if now < 0 || expires <= now || expires - now > 3600 {
            return Err(Error::Denied);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (index, k) in scope.layers()?.into_iter().enumerate() {
            let record: Option<(i64, String)> = tx
                .query_row(
                    "SELECT revision,config FROM policies WHERE scope=?",
                    [&k],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if expected.is_some_and(|v| v[index] != record.as_ref().map(|v| v.0).unwrap_or(0)) {
                return Err(Error::Conflict);
            }
            if let Some((revision, config)) = record {
                let p: Policy = serde_json::from_str(&config)?;
                if matches!(p.requests, RequestPolicy::Deny) {
                    return Err(Error::Denied);
                }
                if matches!(p.requests, RequestPolicy::AskOwner) {
                    let digest = hash(&("request", scope, dispatch, &k, revision))?;
                    tx.execute(
                        "INSERT INTO approvals VALUES (?,?,?,?,0) ON CONFLICT(digest) DO NOTHING",
                        (digest, &scope.binding, json(&(scope, dispatch))?, expires),
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
    /// Read the effective current+dispatch-initial policy without consuming confirmation.
    pub fn tool_disposition(&mut self, proposal: &ToolProposal, now: i64) -> Result<ToolPolicy> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let effective = validate_tool(&tx, proposal, now)?;
        tx.commit()?;
        Ok(effective.high_risk)
    }
    pub fn approve_tool(
        &mut self,
        owner: &str,
        proposal: &ToolProposal,
        now: i64,
    ) -> Result<String> {
        self.owner(owner)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_tool(&tx, proposal, now)?;
        let digest = hash(proposal)?;
        tx.execute(
            "INSERT INTO approvals VALUES (?,?,?,?,0) ON CONFLICT(digest) DO NOTHING",
            (
                &digest,
                &proposal.scope.binding,
                json(proposal)?,
                proposal.expires,
            ),
        )?;
        tx.commit()?;
        Ok(digest)
    }
    pub fn authorize_tool(&mut self, proposal: &ToolProposal, now: i64) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let p = validate_tool(&tx, proposal, now)?;
        let result = match p.high_risk {
            ToolPolicy::Deny => Err(Error::Denied),
            ToolPolicy::AllowWithRules { tools, directories }
                if tools.contains(&proposal.tool)
                    && directories.contains(&proposal.canonical_directory) =>
            {
                Ok(())
            }
            ToolPolicy::AskOwner => {
                let changed = tx.execute(
                    "UPDATE approvals SET consumed=1 WHERE digest=? AND consumed=0 AND expires>?",
                    (hash(proposal)?, now),
                )?;
                if changed == 1 {
                    Ok(())
                } else {
                    Err(Error::Denied)
                }
            }
            _ => Err(Error::Denied),
        };
        result?;
        tx.commit()?;
        Ok(())
    }
}
fn recover_pending_calls(
    tx: &rusqlite::Transaction,
    owner: &str,
    binding: Option<&str>,
) -> Result<usize> {
    let interrupted = tx
        .prepare("SELECT scope FROM calls WHERE state='pending' AND (? IS NULL OR binding=?)")?
        .query_map((binding, binding), |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for encoded in &interrupted {
        mark_usage_gap(tx, &serde_json::from_str(encoded)?, owner)?;
    }
    Ok(tx.execute(
        "UPDATE calls SET state='unknown' WHERE state='pending' AND (? IS NULL OR binding=?)",
        (binding, binding),
    )?)
}

// Preserve conversation history, while marking cumulative accounting as missing.
// The next reported total establishes a new baseline without charging old usage.
fn mark_usage_gap(tx: &rusqlite::Transaction, scope: &Scope, owner: &str) -> Result<()> {
    let context = scope.context_key(owner)?;
    #[cfg(unix)]
    let contexts = [
        context.clone(),
        format!("{}:{context}", crate::codex::FILE_CAPABILITY_VERSION),
    ];
    #[cfg(not(unix))]
    let contexts = [context.clone()];
    for context in contexts {
        tx.execute("INSERT INTO codex_usage VALUES (?,'null') ON CONFLICT(scope) DO UPDATE SET counters='null'", [&context])?;
    }
    tx.execute(
        "UPDATE external_sessions SET counters='null' WHERE scope=?",
        [&context],
    )?;
    Ok(())
}

fn settle_transaction(
    tx: &rusqlite::Transaction,
    scope: &Scope,
    call: &str,
    usage: &Usage,
) -> Result<()> {
    let total = usage.total()?;
    let (stored, reserved, snapshots, state, old): (String, u64, String, String, Option<String>) =
        tx.query_row(
            "SELECT scope,reserved,snapshots,state,usage FROM calls WHERE id=?",
            [call],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
    if stored != json(scope)? {
        return Err(Error::Unauthorized);
    }
    if !usage.is_reported() {
        if state != "settled" {
            tx.execute(
                "UPDATE calls SET state='unknown',usage=NULL WHERE id=?",
                [call],
            )?;
        }
        return Ok(());
    }
    if state == "settled" && old.as_deref() == Some(&json(usage)?) {
        return Ok(());
    }
    // A later genuine usage receipt may replace the provisional zero once.
    let provisional_zero = old
        .as_deref()
        .and_then(|s| serde_json::from_str::<Usage>(s).ok())
        .is_some_and(|u| u.accounting_version == UNREPORTED_ZERO);
    if state == "settled" && !provisional_zero {
        return Err(Error::Conflict);
    }
    let release = if state == "settled" { 0 } else { reserved };
    // Estimated reservations are not provider caps. Charge actual usage honestly.
    for s in serde_json::from_str::<Vec<Snapshot>>(&snapshots)? {
        let spent: u64 = tx.query_row(
            "SELECT spent FROM accounts WHERE scope=? AND window=?",
            (&s.scope, &s.window),
            |r| r.get(0),
        )?;
        bounded(
            spent
                .checked_add(total)
                .ok_or(Error::Invalid("usage overflow"))?,
        )?;
        tx.execute(
            "UPDATE accounts SET held=held-?,spent=spent+? WHERE scope=? AND window=?",
            (release, total, &s.scope, &s.window),
        )?;
    }
    tx.execute(
        "UPDATE calls SET state='settled',usage=? WHERE id=?",
        (json(usage)?, call),
    )?;

    Ok(())
}

fn validate_tool(db: &Connection, proposal: &ToolProposal, now: i64) -> Result<Policy> {
    verify(db, &proposal.scope)?;
    if now < 0 || proposal.expires <= now || proposal.expires - now > 3600 {
        return Err(Error::Denied);
    }
    for v in [
        &proposal.dispatch,
        &proposal.tool,
        &proposal.canonical_directory,
        &proposal.risk,
    ] {
        key(v)?;
    }
    let original: Option<(String, String)> = db
        .query_row(
            "SELECT scope,policies FROM dispatches WHERE id=?",
            [&proposal.dispatch],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (scope_json, original) = original.ok_or(Error::Denied)?;
    if scope_json != json(&proposal.scope)? {
        return Err(Error::Denied);
    }
    let original: Vec<(i64, Policy)> = serde_json::from_str(&original)?;
    let mut effective = Policy {
        budget: Budget {
            limit: Limit::Unlimited,
            period: Period::Lifetime,
        },
        requests: RequestPolicy::Allow,
        high_risk: ToolPolicy::AllowWithRules {
            tools: vec![proposal.tool.clone()],
            directories: vec![proposal.canonical_directory.clone()],
        },
    };
    for (index, k) in proposal.scope.layers()?.iter().enumerate() {
        let record: Option<(i64, String)> = db
            .query_row(
                "SELECT revision,config FROM policies WHERE scope=?",
                [k],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (revision, p) = match record {
            Some((r, c)) => (r, serde_json::from_str::<Policy>(&c)?),
            None => (0, policy(db, k, index)?),
        };
        if revision != proposal.policy_revision[index] || matches!(p.requests, RequestPolicy::Deny)
        {
            return Err(Error::Denied);
        }
        let old = &original.get(index).ok_or(Error::Conflict)?.1;
        for tool_policy in [&old.high_risk, &p.high_risk] {
            match tool_policy {
                ToolPolicy::Deny => return Err(Error::Denied),
                ToolPolicy::AskOwner => effective.high_risk = ToolPolicy::AskOwner,
                ToolPolicy::AllowWithRules { tools, directories } => {
                    if !tools.contains(&proposal.tool)
                        || !directories.contains(&proposal.canonical_directory)
                    {
                        return Err(Error::Denied);
                    }
                }
            }
        }
    }
    // A tool/directory rule is not a grant of broader native permissions.
    // These callbacks must consume the owner's exact one-use decision even
    // when ordinary calls to that tool are allowed by a saved rule.
    if proposal.risk == "permission_expansion" {
        effective.high_risk = ToolPolicy::AskOwner;
    }
    Ok(effective)
}
fn intersect(a: &Limit, b: &Limit) -> Limit {
    match (a, b) {
        (Limit::Tokens(a), Limit::Tokens(b)) => Limit::Tokens((*a).min(*b)),
        (Limit::Tokens(n), _) | (_, Limit::Tokens(n)) => Limit::Tokens(*n),
        _ => Limit::Unlimited,
    }
}
fn key(v: &str) -> Result<()> {
    if v.is_empty() || v.len() > 1024 || v.chars().any(char::is_control) {
        Err(Error::Invalid("identity/key"))
    } else {
        Ok(())
    }
}
fn bounded(n: u64) -> Result<u64> {
    if n > MAX {
        Err(Error::Invalid("unsafe integer"))
    } else {
        Ok(n)
    }
}
fn json<T: Serialize>(v: &T) -> Result<String> {
    Ok(serde_json::to_string(v)?)
}
fn hash<T: Serialize>(v: &T) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(json(v)?)))
}
fn call_digest(scope: &Scope, dispatch: &str, reserved: u64) -> Result<String> {
    if reserved == 0 {
        hash(&("postpaid-v1", scope, dispatch))
    } else {
        hash(&(scope, dispatch, reserved))
    }
}
fn verify(db: &Connection, scope: &Scope) -> Result<()> {
    scope.validate()?;
    let record: Option<(String, String)> = db
        .query_row(
            "SELECT agent,room FROM bindings WHERE id=?",
            [&scope.binding],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if record.as_ref() != Some(&(scope.agent.clone(), scope.room.clone())) {
        Err(Error::Unauthorized)
    } else {
        Ok(())
    }
}
fn policy(db: &Connection, k: &str, _index: usize) -> Result<Policy> {
    let record: Option<String> = db
        .query_row("SELECT config FROM policies WHERE scope=?", [k], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(match record {
        Some(s) => serde_json::from_str(&s)?,
        None => Policy {
            budget: Budget {
                limit: Limit::Unlimited,
                period: Period::Lifetime,
            },
            requests: RequestPolicy::Allow,
            high_risk: ToolPolicy::Deny,
        },
    })
}
#[cfg(test)]
mod tests;

/// Opaque native CAS digest; no credential reference or workspace is projected.
pub fn model_profile_digest(profile: Option<&ModelProfile>) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&profile)?)
    ))
}
fn validate_model_profile(profile: &ModelProfile) -> Result<()> {
    if !profile.reasoning_effort.is_empty()
        && ![
            "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
        ]
        .contains(&profile.reasoning_effort.as_str())
    {
        return Err(Error::Invalid("reasoning effort"));
    }
    for value in [
        &profile.model,
        &profile.credential_ref,
        &profile.workspace_root,
    ] {
        key(value)?;
    }
    if !(profile.credential_ref.starts_with("keychain:")
        || runtime::managed_reference(&profile.credential_ref).is_some()
        || profile
            .credential_ref
            .strip_prefix("codex-managed:shared-home:")
            .is_some_and(|hash| {
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            }))
        || !Path::new(&profile.workspace_root).is_absolute()
    {
        return Err(Error::Invalid("local credential reference or workspace"));
    }
    Ok(())
}
fn write_model_profile_on(db: &Connection, agent: &str, profile: &ModelProfile) -> Result<()> {
    db.execute("INSERT INTO model_profiles (agent,model,credential_ref,workspace_root,reasoning_effort,web_search) VALUES (?,?,?,?,?,?) ON CONFLICT(agent) DO UPDATE SET model=excluded.model,credential_ref=excluded.credential_ref,workspace_root=excluded.workspace_root,reasoning_effort=excluded.reasoning_effort,web_search=excluded.web_search",(agent,&profile.model,&profile.credential_ref,&profile.workspace_root,&profile.reasoning_effort,profile.web_search))?;
    Ok(())
}
fn read_model_profile_on(db: &Connection, agent: &str) -> Result<Option<ModelProfile>> {
    Ok(db
            .query_row(
                "SELECT model,credential_ref,workspace_root,reasoning_effort,web_search FROM model_profiles WHERE agent=?",
                [agent],
                |r| {
                    Ok(ModelProfile {
                        model: r.get(0)?,
                        credential_ref: r.get(1)?,
                        workspace_root: r.get(2)?,
                        reasoning_effort: r.get(3)?,
                        web_search: r.get(4)?,
                    })
                },
            )
            .optional()?)
}
