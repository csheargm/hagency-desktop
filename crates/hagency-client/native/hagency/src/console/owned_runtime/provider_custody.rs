//! A successful runtime-task join is insufficient when provider cleanup was lost.
use super::*;
use crate::console::lifecycle::{DrainFailure, TaskCustody};

/// A retained native session belongs to one authenticated conversation. Room
/// membership alone never selects somebody else's session or approval scope.
#[derive(PartialEq, Eq)]
pub(super) struct SessionKey {
    owner: String,
    scope: Scope,
    generation: i64,
    profile: codex::Profile,
    reference: String,
    host_files: bool,
}
impl SessionKey {
    pub fn new(
        owner: &str,
        dispatch: &inbox::Dispatch,
        profile: &codex::Profile,
        config: &StartConfig,
    ) -> Self {
        Self {
            owner: owner.into(),
            scope: dispatch.scope(),
            generation: dispatch.binding_generation,
            profile: profile.clone(),
            reference: config.credential_ref.clone(),
            host_files: config.host_files,
        }
    }
}
/// Only idle, settled providers enter this bounded LRU. Eviction and host stop
/// use the same whole-process-tree custody as a cancelled active call. Durable
/// native context remains on disk when an idle process is evicted.
#[derive(Default)]
pub(super) struct Sessions(std::collections::VecDeque<(SessionKey, Provider)>);
impl Sessions {
    pub fn take(&mut self, key: &SessionKey) -> Option<Provider> {
        let index = self.0.iter().position(|(saved, _)| saved == key)?;
        self.0.remove(index).map(|(_, provider)| provider)
    }
    pub fn retain(&mut self, key: SessionKey, mut provider: Provider) {
        provider.session.park();
        if self.0.len() == 8 {
            self.0.pop_front();
        }
        self.0.push_back((key, provider));
    }
}

// Tests retain the same native guardian as production; a Rust test harness is
// not a guardian executable. CI supplies this freshly built offline helper.
fn guardian() -> codex::Result<Option<std::path::PathBuf>> {
    #[cfg(test)]
    {
        std::env::var_os("HAGENCY_TEST_RUNTIME_PROBE")
            .map(std::path::PathBuf::from)
            .map(Some)
            .ok_or(codex::Error::Profile(
                "explicit supervised runtime probe required",
            ))
    }
    #[cfg(not(test))]
    {
        Ok(None)
    }
}
pub(super) async fn spawn(
    profile: &codex::Profile,
    reference: &str,
) -> codex::Result<runtime::Process> {
    runtime::spawn_with_guardian(profile, reference, guardian()?.as_deref()).await
}
pub(super) async fn spawn_for_scope(
    profile: &codex::Profile,
    reference: &str,
    ledger: &mut Ledger,
    scope: &Scope,
) -> codex::Result<runtime::Process> {
    runtime::spawn_for_scope_with_guardian(
        profile,
        reference,
        ledger,
        scope,
        guardian()?.as_deref(),
    )
    .await
}

pub(super) struct Provider {
    process: Option<runtime::Process>,
    tasks: TaskCustody,
}
impl Provider {
    pub async fn capture(
        tasks: &TaskCustody,
        future: impl std::future::Future<Output = codex::Result<runtime::Process>>,
    ) -> codex::Result<Self> {
        let creation = tasks.checked().map_err(|_| codex::Error::Unknown)?;
        // A cancelled/failed low-level constructor may already have launched
        // an inspection process. Until it supplies exit evidence, retain an
        // explicit unknown rather than interpreting its missing return as zero.
        let process = future.await?;
        creation.complete();
        Ok(Self {
            process: Some(process),
            tasks: tasks.clone(),
        })
    }
    pub async fn stop(&mut self) -> codex::Result<()> {
        let result = self
            .process
            .as_mut()
            .expect("provider custody")
            .stop()
            .await;
        if result.is_err() {
            self.tasks.unknown();
        }
        result
    }
}
impl std::ops::Deref for Provider {
    type Target = runtime::Process;
    fn deref(&self) -> &Self::Target {
        self.process.as_ref().expect("provider custody")
    }
}
impl std::ops::DerefMut for Provider {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.process.as_mut().expect("provider custody")
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        let Some(mut process) = self.process.take() else {
            return;
        };
        if tokio::runtime::Handle::try_current().is_err() {
            self.tasks.unknown();
            return;
        }
        self.tasks
            .cleanup(async move { process.stop().await.map_err(|_| DrainFailure::Unknown) });
    }
}
