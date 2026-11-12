//! Fleet alert reads use the same resource visibility as individual alert reads.
use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebAlertsQuery {
    pub after: Option<String>,
    pub status: Option<String>,
    pub severity: Option<String>,
    /// Original ceiling alerts identify their agent by the resource ID.
    pub agent: Option<String>,
}
impl WebAlertsQuery {
    pub fn validate(&self) -> Result<(), Error> {
        if let Some(id) = &self.after {
            alerts::alert_key(id)?;
        }
        if let Some(id) = &self.agent {
            identifier(id, 128)?;
        }
        if self
            .status
            .as_deref()
            .is_some_and(|s| !crate::ALERT_STATUSES.contains(&s))
            || self
                .severity
                .as_deref()
                .is_some_and(|s| !matches!(s, "info" | "warning" | "critical"))
        {
            return Err(hagency_core::InvalidInput("invalid overview alert filter").into());
        }
        Ok(())
    }
}
#[derive(Debug, Default, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebAlertCounts {
    pub total: u64,
    pub by_status: BTreeMap<String, u64>,
    pub open_by_severity: BTreeMap<String, u64>,
}
#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebAlertsPage {
    pub alerts: Vec<ResourceAlert>,
    /// Whole authorized resource scope, independent of current filters/cursor.
    pub counts: WebAlertCounts,
    pub filtered_count: u64,
    pub next_after: Option<String>,
    pub observed_at_ms: u64,
}

#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebWorkforceLink {
    pub request_id: String,
    pub agent_id: String,
    pub project_id: String,
    pub allocation_id: String,
    pub resource_id: String,
    pub account_id: Option<String>,
    pub model: String,
    pub reasoning: Option<String>,
    pub model_revision_matches: bool,
    pub period_key: String,
    pub allocated_tokens: u64,
    pub retained_tokens: u64,
    /// Old-period unresolved resource liability, separate from current allowance.
    #[serde(default)]
    pub prior_retained_tokens: u64,
    pub known_consumed_tokens: u64,
    pub usage_complete: bool,
    pub process_state: String,
}

#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebManagedUsageRow {
    pub request_id: String,
    pub agent_id: String,
    pub name: String,
    pub capability: String,
    pub manager_mxid: String,
    pub state: String,
    pub project_id: String,
    pub allocation_id: String,
    pub resource_id: String,
    pub period_key: String,
    pub allocated_tokens: u64,
    pub retained_tokens: u64,
    /// Old-period unresolved resource liability, separate from current allowance.
    #[serde(default)]
    pub prior_retained_tokens: u64,
    pub known_consumed_tokens: u64,
    pub usage_complete: bool,
    pub call_count: u64,
}
#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebManagedUsage {
    pub supported: bool,
    pub complete: bool,
    pub items: Vec<WebManagedUsageRow>,
}
impl Session {
    pub async fn web_owned_workforce(
        &self,
        context: &HostContext,
        after: Option<String>,
    ) -> Result<WebManagedUsage, Error> {
        if let Some(after) = &after {
            identifier(after, 128)?;
        }
        self.check(context, Permission::ReadResources)?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .web_managed_usage_page(&self.current_profile(store)?, after, 16)
                    .await?
            }
        };
        self.check(context, Permission::ReadResources)?;
        Ok(result)
    }
    pub async fn web_managed_usage(&self, context: &HostContext) -> Result<WebManagedUsage, Error> {
        self.check(context, Permission::ReadResources)?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .web_managed_usage(&self.current_profile(store)?)
                    .await?
            }
        };
        self.check(context, Permission::ReadResources)?;
        Ok(result)
    }
    pub async fn web_workforce_links(
        &self,
        context: &HostContext,
        ids: Vec<String>,
    ) -> Result<Vec<WebWorkforceLink>, Error> {
        if ids.len() > 16 {
            return Err(Error::Capacity);
        }
        for id in &ids {
            identifier(id, 128)?;
        }
        self.check(context, Permission::ReadResources)?;
        self.check(context, Permission::ReadAccounts)?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .web_workforce_links(&self.current_profile(store)?, ids)
                    .await?
            }
        };
        self.check(context, Permission::ReadResources)?;
        self.check(context, Permission::ReadAccounts)?;
        Ok(result)
    }
    pub async fn web_alerts(
        &self,
        context: &HostContext,
        query: WebAlertsQuery,
    ) -> Result<WebAlertsPage, Error> {
        query.validate()?;
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        self.check(context, Permission::ReadAlerts)?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .web_alerts(&self.current_profile(store)?, query)
                    .await?
            }
        };
        self.check(context, Permission::ReadAlerts)?;
        Ok(result)
    }
}
