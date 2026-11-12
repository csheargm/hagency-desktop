//! Current plaintext protocol for the real-process custody fixtures. These
//! helpers use the public V3 admission APIs and preserve the original evidence.
use super::*;

async fn input(
    registry: &OwnerResourceRegistry,
    activity: VerifiedSharedActivity,
    work: &SharedWorkItem,
) -> Result<SharedPreparedInput, Error> {
    let work_id = work.id.clone();
    let binding: Option<(String, u64)> = registry.0.store.call(1024, move |db| {
        let row: Option<(String, String)> = db.db.query_row(
            "SELECT binding_digest,binding_payload FROM owner_shared_demand_bindings WHERE work_id=?1",
            [&work_id], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        row.map(|(digest, payload)| {
            let binding: SharedDemandBinding = serde_json::from_str(&payload)?;
            Ok((digest, binding.control_revision))
        }).transpose()
    }).await?;
    let context = SharedThreadContext {
        version: 3,
        input_profile: INPUT_PROFILE_V3.into(),
        work_id: work.id.clone(),
        work_digest: digest(work),
        assignment_id: work.assignment_id.clone(),
        runtime_command_digest: work.runtime_command_digest.clone(),
        demand_binding_digest: binding.as_ref().map(|binding| binding.0.clone()),
        control_revision: binding.map(|binding| binding.1),
        room_id: work.room_id.clone(),
        thread_root: work.thread_root.clone(),
        event_id: work.event_id.clone(),
        received_at_ms: work.created_at_ms,
        context_ordinal: 1,
        coverage: COVERAGE_NATIVE_HISTORY.into(),
        omitted_earlier: true,
        history: vec![],
    };
    use sha2::{Digest, Sha256};
    let record = SharedContextRecord {
        context_digest: digest(&context),
        input_digest: format!(
            "{:x}",
            Sha256::digest(render_context(&context, work)?.as_bytes())
        ),
        context,
    };
    registry
        .prepare_shared_input(
            activity,
            work.clone(),
            SharedWorkInputMode {
                mode: "thread_context_v1".into(),
                context_digest: None,
                input_digest: None,
            },
            Some(record),
        )
        .await
}

pub(super) trait CurrentSharedInputFixture {
    async fn prepare_current_call(
        &self,
        activity: impl Fn() -> VerifiedSharedActivity,
        work: SharedWorkItem,
    ) -> Result<ManagedCallHistory, Error>;
    async fn begin_current_start(
        &self,
        activity: impl Fn() -> VerifiedSharedActivity,
        work: SharedWorkItem,
        call: String,
    ) -> Result<SharedStartAttempt, Error>;
    async fn issue_current_call(
        &self,
        attempt: SharedStartAttempt,
        start: SharedWorkStart,
    ) -> Result<SharedCallCustody, Error>;
}

impl CurrentSharedInputFixture for OwnerResourceRegistry {
    async fn prepare_current_call(
        &self,
        activity: impl Fn() -> VerifiedSharedActivity,
        work: SharedWorkItem,
    ) -> Result<ManagedCallHistory, Error> {
        let input = input(self, activity(), &work).await?;
        self.prepare_shared_input_call(activity(), work, input)
            .await
    }

    async fn begin_current_start(
        &self,
        activity: impl Fn() -> VerifiedSharedActivity,
        work: SharedWorkItem,
        call: String,
    ) -> Result<SharedStartAttempt, Error> {
        let input = input(self, activity(), &work).await?;
        self.begin_shared_input_start(activity(), work, call, input)
            .await
    }

    async fn issue_current_call(
        &self,
        attempt: SharedStartAttempt,
        start: SharedWorkStart,
    ) -> Result<SharedCallCustody, Error> {
        let work_id = start.work.id.clone();
        let (context_digest, input_digest) = self
            .0
            .store
            .call(1024, move |db| {
                Ok(db.db.query_row(
                    "SELECT digest,input_digest FROM owner_shared_contexts WHERE work_id=?1",
                    [&work_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?)
            })
            .await?;
        self.issue_shared_context_call(
            attempt,
            SharedContextStart {
                start,
                context_digest,
                input_digest,
            },
        )
        .await
    }
}
