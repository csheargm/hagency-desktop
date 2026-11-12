//! Public resource observations, not an amendment or contribution authority.
use super::{HostContext, Permission, ResourceSummary, Session};
use crate::Error;
use hagency_core::{
    allocation::Ceiling,
    project::identifier,
    qualification::{ConfigurationChoice, Tier},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceDetailQuery {
    pub id: String,
}
impl ResourceDetailQuery {
    /// Called before host initialization as well as at the native boundary.
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.id, 128)?;
        Ok(())
    }
}

#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceDetail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar: Option<hagency_core::recurring_quota::Calendar>,
    #[serde(deserialize_with = "crate::management::wire_static::one")]
    pub protocol: crate::management::wire_static::Label,
    pub observed_at_ms: u64,
    pub resource: ResourceSummary,
    pub ceiling: Option<Ceiling>,
    /// Default for newly provisioned agents; not a rewrite of existing policies.
    pub execution_policy: hagency_core::execution::ExecutionPolicy,
    /// Embedded model qualification, not a live provider/runtime check.
    pub choices: Vec<ConfigurationChoice>,
    pub model_tier: Option<Tier>,
    pub model_family: Option<String>,
    pub editing: ResourceEditing,
    /// Whether the monthly maximum can be amended where `editing` refuses an
    /// in-place change because the resource was offered, published or
    /// retains commitments. `ceiling` above is already the amended maximum.
    pub ceiling_amendment: CeilingAmendment,
}

/// Current readback for a monthly-maximum amendment, never authority: the
/// native writer repeats every check when it records the amendment.
#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CeilingAmendment {
    pub allowed: bool,
    /// Set exactly when `allowed` is false.
    pub reason: Option<CeilingAmendmentBlocker>,
    /// Amendments recorded so far (0: the definition's own maximum). An
    /// amendment request echoes it as `expectedCeilingSequence`.
    pub sequence: u64,
    /// The lowest monthly maximum an amendment may set now: this month's
    /// published capacity, Agent holds and measured use. `None` while usage
    /// is unsettled (the maximum can then only be raised) or when not allowed.
    pub minimum_tokens: Option<u64>,
    /// The live grant's per-request limit when exactly one grant of the
    /// resource is live. An amendment never changes it.
    pub per_request_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CeilingAmendmentBlocker {
    /// Never offered, published or committed: change the definition (or
    /// create a configuration) instead of amending it.
    NotOffered,
    ContributionHistoryNotProven,
    /// There is no monthly maximum to amend.
    MonthlyCeilingRequired,
    ReadOnly,
}

#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceEditing {
    pub allowed: bool,
    /// This is current readback, never authority for a subsequent mutation.
    pub reason: Option<ResourceEditBlocker>,
}

#[derive(Debug, Serialize, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceEditBlocker {
    ContributionHistoryNotProven,
    PreviouslyOffered,
    Published,
    RetainedCommitments,
    ManagedNamespaceRequired,
    AccountUnavailable,
    ReadOnly,
}

impl Session {
    pub async fn resource_detail(
        &self,
        context: &HostContext,
        query: ResourceDetailQuery,
    ) -> Result<ResourceDetail, Error> {
        self.check(context, Permission::ReadResources)?;
        query.validate()?;
        let mut result = match self.local_store()? {

            super::SessionStore::Current(store) => {
                store
                    .detail(&self.current_profile(store)?, query.id)
                    .await?
            }
        };
        self.check(context, Permission::ReadResources)?;
        if (result.editing.allowed || result.ceiling_amendment.allowed)
            && self.check(context, Permission::ConfigureResources).is_err()
        {
            if result.editing.allowed {
                result.editing = ResourceEditing {
                    allowed: false,
                    reason: Some(ResourceEditBlocker::ReadOnly),
                };
            }
            if result.ceiling_amendment.allowed {
                result.ceiling_amendment.allowed = false;
                result.ceiling_amendment.reason = Some(CeilingAmendmentBlocker::ReadOnly);
                result.ceiling_amendment.minimum_tokens = None;
            }
        }
        Ok(result)
    }
}
