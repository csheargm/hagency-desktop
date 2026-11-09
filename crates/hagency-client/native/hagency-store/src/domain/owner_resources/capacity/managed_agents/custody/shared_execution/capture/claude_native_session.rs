//! A saved native context never substitutes for the next original call's custody.
use super::*;
use hagency_runtime::{claude::session as native, owned::OwnedClaudeSession};
use native_session::{Conversation, previous_checkpoint, scope_digest, scope_key};

#[derive(Clone, PartialEq, Eq)]
struct Previous {
    native: String,
    receipt: String,
}
pub struct SharedClaudeSession {
    registry: Arc<Inner>,
    command: String,
    digest: String,
    settings: String,
    route: Option<SharedSessionRoute>,
    scope: Option<String>,
    previous: Option<Previous>,
    bound: Option<SharedNativeSession>,
}

fn previous(db: &Connection, scope: &str, child: &str) -> Result<Option<Previous>, Error> {
    let Some((native, receipt)) = previous_checkpoint(db, scope)? else {
        return Ok(None);
    };
    identifier(&native, 128)?;
    let id = format!("{}_usage", receipt.observation_id);
    let row: Option<(String, String)> = db.query_row(
        "SELECT evidence,content_digest FROM owner_managed_observations WHERE id=?1 AND child_id=?2 AND target_id=?3 AND kind='usage'",
        params![id, child, receipt.call_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    let Some((raw, digest)) = row else {
        return Ok(None);
    };
    let evidence: Value = serde_json::from_str(&raw)?;
    if canonical::digest(&evidence)? != digest
        || evidence["kind"] != "usage"
        || evidence["target"] != receipt.call_id
        || evidence["observation"]["id"] != id
    {
        return Err(Error::Conflict);
    }
    let observation = &evidence["observation"];
    if observation["terminal"] != true
        || observation["complete_capture"] != true
        || observation["unrepresentable"] != false
    {
        return Ok(None);
    }
    let original = &observation["usage"];
    let usage = &original["claude_runtime_evidence"]["usage"];
    if original["framework"] != "claude"
        || !original["failure"].is_null()
        || usage["coverage"] != "reported_models_result"
        || ["missing", "invalid", "unsupported"]
            .iter()
            .any(|k| usage["diagnostics"][k] != false)
    {
        return Ok(None);
    }
    let mut counts = [0; 4];
    for (index, key) in ["input", "output", "cacheRead", "cacheWrite"]
        .iter()
        .enumerate()
    {
        let Some(n) = original["totals"][key]
            .as_u64()
            .filter(|n| *n <= JSON_SAFE_MAX)
        else {
            return Ok(None);
        };
        if usage["counts"][key].as_u64() != Some(n) {
            return Err(Error::Conflict);
        }
        counts[index] = n;
    }
    if counts
        .iter()
        .try_fold(0_u64, |sum, n| {
            sum.checked_add(*n).filter(|n| *n <= JSON_SAFE_MAX)
        })
        .is_none()
    {
        return Ok(None);
    }
    Ok(Some(Previous {
        native,
        receipt: receipt.observation_id,
    }))
}

#[allow(clippy::too_many_arguments)]
async fn prepare(
    registry: &Arc<Inner>,
    enrollment: &VerifiedManagementEnrollment,
    root: &SharedRuntimeCommand,
    command: &str,
    digest: &str,
    provider: &str,
    policy: &str,
    route: Option<SharedSessionRoute>,
    settings: String,
) -> Result<SharedClaudeSession, Error> {
    hash(&settings)?;
    if root.runtime()? != "claude" {
        return Err(Error::LocalAuthority);
    }
    let mut scope = None;
    if let Some(route) = &route {
        route.validate()?;
        if route.runtime_command_id != command
            || route.runtime_command_digest != digest
            || route.assignment_id != root.assignment_id
        {
            return Err(Error::LocalAuthority);
        }
        // Plaintext V1/V2 still has an immutable complete-history input contract.
        if route.encrypted
            || route.native_context_version == Some(3)
            || !cfg!(feature = "shared-context")
        {
            scope = Some(scope_key(
                root,
                enrollment,
                provider,
                policy,
                &settings,
                Conversation::route(route),
            )?);
        }
    }
    let prior = if let Some(scope) = scope.clone() {
        let child = root.agent_reservation_id.clone();
        registry
            .store
            .call(32 * 1024, move |db| previous(&db.db, &scope, &child))
            .await?
    } else {
        None
    };
    Ok(SharedClaudeSession {
        registry: registry.clone(),
        command: command.into(),
        digest: digest.into(),
        settings,
        route,
        scope,
        previous: prior,
        bound: None,
    })
}

impl SharedRuntimeCustody {
    pub async fn prepare_claude_session(
        &self,
        route: Option<SharedSessionRoute>,
        settings: String,
    ) -> Result<SharedClaudeSession, Error> {
        self.check()?;
        let result = prepare(
            &self.scope.registry,
            &self.enrollment,
            &self.command,
            &self.command.command_id,
            &self.digest,
            &self.provider,
            self.command.execution_policy(false, None)?,
            route,
            settings,
        )
        .await?;
        self.check()?;
        Ok(result)
    }
}
impl SharedAttemptCustody {
    pub async fn prepare_claude_session(
        &self,
        route: Option<SharedSessionRoute>,
        settings: String,
    ) -> Result<SharedClaudeSession, Error> {
        self.check()?;
        let result = prepare(
            &self.registry,
            &self.enrollment,
            &self.command.original_runtime_command,
            &self.command.command_id,
            &self.digest,
            &self.provider,
            self.command.original_runtime_command.execution_policy(
                self.command.web_search,
                self.command.tool_policy.as_ref().map(|p| p.mode.as_str()),
            )?,
            route,
            settings,
        )
        .await?;
        self.check()?;
        Ok(result)
    }
}
#[cfg(feature = "shared-budget-execution")]
impl SharedBudgetAttemptCustody {
    pub async fn prepare_claude_session(
        &self,
        route: Option<SharedSessionRoute>,
        settings: String,
    ) -> Result<SharedClaudeSession, Error> {
        self.check()?;
        let result = prepare(
            &self.registry,
            &self.enrollment,
            &self.command.original_runtime_command,
            &self.command.command_id,
            &self.digest,
            &self.provider,
            self.command.original_runtime_command.execution_policy(
                self.command.web_search,
                self.command.tool_policy.as_ref().map(|p| p.mode.as_str()),
            )?,
            route,
            settings,
        )
        .await?;
        self.check()?;
        Ok(result)
    }
}

impl SharedClaudeSession {
    /// Host-only namespace for native plaintext. A missing route gets the
    /// original command's unique directory and cannot reuse another room.
    pub fn storage_scope(&self) -> &str {
        self.scope.as_deref().unwrap_or(&self.digest)
    }
    pub fn resume_id(&self) -> Option<&str> {
        self.previous.as_ref().map(|p| p.native.as_str())
    }
    pub fn is_resuming(&self) -> bool {
        self.previous.is_some()
    }
    /// Rebuild only before binding a call, from this exact stopped native
    /// process's failed startup before prompt bytes. No usage is rewritten.
    pub fn rebuild_missing(&mut self, process: &OwnedClaudeSession) -> Result<(), Error> {
        if self.bound.is_some()
            || self.previous.is_none()
            || process.pre_prompt_resume_id() != self.resume_id()
            || process
                .shared_resume_id()
                .map_err(|_| Error::LocalAuthority)?
                != self.resume_id()
        {
            return Err(Error::LocalAuthority);
        }
        self.previous = None;
        Ok(())
    }
    /// Only after the original process is qualified and exact work is received
    /// can a planned resume be bound. This fences older contexts before input.
    pub async fn attach(
        &mut self,
        a: VerifiedSharedActivity,
        w: SharedWorkItem,
        process: &OwnedClaudeSession,
    ) -> Result<(), Error> {
        a.work(&w)?;
        let source = process
            .shared_process_source()
            .map_err(|_| Error::LocalAuthority)?;
        if self.bound.is_some()
            || !Arc::ptr_eq(&self.registry, a.sink.registry())
            || a.sink.command_id() != self.command
            || a.sink.command_digest() != self.digest
            || a.sink.original().runtime()? != "claude"
            || !a.sink.source().same(&source)
            || process.phase() != native::Phase::Ready
            || process
                .shared_resume_id()
                .map_err(|_| Error::LocalAuthority)?
                != self.resume_id()
            || self.route.as_ref().is_some_and(|r| !r.matches_work(&w))
        {
            return Err(Error::LocalAuthority);
        }
        let scope = scope_digest(&a, &w, &self.settings)?;
        if self.scope.as_ref().is_some_and(|old| old != &scope) {
            return Err(Error::LocalAuthority);
        }
        let admitted_scope = scope.clone();
        let expected = self.previous.clone();
        let child = a.sink.child().to_owned();
        let process_digest = a.sink.process_digest()?;
        let bound = SharedNativeSession {
            work: w.clone(),
            source,
            process_digest: process_digest.clone(),
            previous: self.resume_id().map(str::to_owned),
        };
        self.registry
            .store
            .call(32 * 1024, move |db| {
                let tx = db
                    .db
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                work::live(&tx, &a)?;
                if !work::exact_work(&tx, a.sink.child(), &w)?
                    || expected.is_some() && previous(&tx, &scope, &child)? != expected
                    || expected.is_some()
                        && !w.encrypted
                        && cfg!(feature = "shared-context")
                        && !context::native_input(&tx, &w)?
                {
                    return Err(Error::LocalAuthority);
                }
                bounded_row(
                    &tx,
                    "owner_shared_native_turns",
                    "work_id",
                    &w.id,
                    1_000_000,
                )?;
                let revision = revision(&tx)?;
                tx.execute(
                    "INSERT INTO owner_shared_native_turns VALUES(?1,?2,?3,?4)",
                    params![w.id, scope, process_digest, revision],
                )?;
                a.check()?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.bound = Some(bound);
        self.scope = Some(admitted_scope);
        Ok(())
    }
    pub fn bind_capture(
        &self,
        sink: &mut SharedCallSink,
        process: &OwnedClaudeSession,
    ) -> Result<(), Error> {
        let bound = self.bound.as_ref().ok_or(Error::LocalAuthority)?;
        if sink.sequence != 1
            || sink.usage.is_some()
            || sink.terminal.is_some()
            || sink.custody.work.id != bound.work.id
            || !bound.source.same(sink.custody.sink.source())
            || !matches!(&sink.source, Source::Claude(source) if process.matches_observation_source(source))
            || self
                .resume_id()
                .is_some_and(|id| process.session_id() != Some(id))
        {
            return Err(Error::LocalAuthority);
        }
        // Resume restores conversation only. Usage comes from this turn's
        // native result deltas, independently of restored modelUsage totals.
        Ok(())
    }
    pub async fn retain(
        self,
        sink: &SharedCallSink,
        process: &OwnedClaudeSession,
    ) -> Result<(), Error> {
        let bound = self.bound.ok_or(Error::LocalAuthority)?;
        if !sink.clean
            || sink.terminal != Some(TurnResult::Completed)
            || sink.custody.work.id != bound.work.id
            || !bound.source.same(sink.custody.sink.source())
            || process.phase() != native::Phase::ResultObserved
            || process.needs_usage_snapshot()
            || !matches!(&sink.source, Source::Claude(source) if process.matches_observation_source(source))
        {
            return Err(Error::LocalAuthority);
        }
        let native = process
            .session_id()
            .ok_or(Error::LocalAuthority)?
            .to_owned();
        identifier(&native, 128)?;
        sink.persist_native_checkpoint(bound.process_digest, native)
            .await
    }
}
