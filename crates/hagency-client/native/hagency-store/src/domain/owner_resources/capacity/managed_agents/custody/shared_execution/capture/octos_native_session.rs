//! Bind a fresh Octos process's workspace to one authenticated conversation.
//! Octos native state is never resumed: every process accounts a fresh session.
use super::*;
use hagency_runtime::{octos::session::Phase, owned::OwnedOctosSession};
use native_session::{Conversation, scope_digest, scope_key};

pub struct SharedOctosSession {
    registry: Arc<Inner>,
    command: String,
    digest: String,
    settings: String,
    route: Option<SharedSessionRoute>,
    scope: Option<String>,
    attached: bool,
}

#[allow(clippy::too_many_arguments)]
fn prepare(
    registry: &Arc<Inner>, enrollment: &VerifiedManagementEnrollment, root: &SharedRuntimeCommand,
    command: &str, digest: &str, provider: &str, policy: &str,
    route: Option<SharedSessionRoute>, settings: String,
) -> Result<SharedOctosSession, Error> {
    hash(&settings)?;
    if root.runtime()? != "octos" { return Err(Error::LocalAuthority); }
    let scope = if let Some(route) = &route {
        route.validate()?;
        if route.runtime_command_id != command || route.runtime_command_digest != digest
            || route.assignment_id != root.assignment_id { return Err(Error::LocalAuthority); }
        Some(scope_key(root, enrollment, provider, policy, &settings, Conversation::route(route))?)
    } else { None };
    Ok(SharedOctosSession { registry: registry.clone(), command: command.into(), digest: digest.into(),
        settings, route, scope, attached: false })
}
impl SharedRuntimeCustody {
    pub fn prepare_octos_session(&self, route: Option<SharedSessionRoute>, settings: String) -> Result<SharedOctosSession, Error> {
        self.check()?;
        let session = prepare(&self.scope.registry, &self.enrollment, &self.command, &self.command.command_id,
            &self.digest, &self.provider, self.command.execution_policy(false, None)?, route, settings)?;
        self.check()?;
        Ok(session)
    }
}
impl SharedAttemptCustody {
    pub fn prepare_octos_session(&self, route: Option<SharedSessionRoute>, settings: String) -> Result<SharedOctosSession, Error> {
        self.check()?;
        let root = &self.command.original_runtime_command;
        let session = prepare(&self.registry, &self.enrollment, root, &self.command.command_id,
            &self.digest, &self.provider, root.execution_policy(self.command.web_search,
                self.command.tool_policy.as_ref().map(|p| p.mode.as_str()))?, route, settings)?;
        self.check()?;
        Ok(session)
    }
}
#[cfg(feature = "shared-budget-execution")]
impl SharedBudgetAttemptCustody {
    pub fn prepare_octos_session(&self, route: Option<SharedSessionRoute>, settings: String) -> Result<SharedOctosSession, Error> {
        self.check()?;
        let root = &self.command.original_runtime_command;
        let session = prepare(&self.registry, &self.enrollment, root, &self.command.command_id,
            &self.digest, &self.provider, root.execution_policy(self.command.web_search,
                self.command.tool_policy.as_ref().map(|p| p.mode.as_str()))?, route, settings)?;
        self.check()?;
        Ok(session)
    }
}
impl SharedOctosSession {
    pub fn storage_scope(&self) -> &str { self.scope.as_deref().unwrap_or(&self.digest) }

    pub async fn attach(&mut self, a: VerifiedSharedActivity, w: SharedWorkItem, process: &OwnedOctosSession) -> Result<(), Error> {
        a.work(&w)?;
        let source = process.shared_process_source().map_err(|_| Error::LocalAuthority)?;
        if self.attached || !Arc::ptr_eq(&self.registry, a.sink.registry())
            || a.sink.command_id() != self.command || a.sink.command_digest() != self.digest
            || a.sink.original().runtime()? != "octos" || !a.sink.source().same(&source)
            || process.phase() != Phase::Ready || self.route.as_ref().is_some_and(|r| !r.matches_work(&w))
        { return Err(Error::LocalAuthority); }
        let scope = scope_digest(&a, &w, &self.settings)?;
        if self.scope.as_ref().is_some_and(|old| old != &scope) { return Err(Error::LocalAuthority); }
        self.registry.store.call(4096, move |db| {
            work::live(&db.db, &a)?;
            if !work::exact_work(&db.db, a.sink.child(), &w)? { return Err(Error::LocalAuthority); }
            a.check()
        }).await?;
        self.scope = Some(scope);
        self.attached = true;
        Ok(())
    }
}
