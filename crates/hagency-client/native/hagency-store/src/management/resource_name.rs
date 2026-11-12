//! Local display metadata only. Effective configuration/amendment stays separate.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceLocalName {
    pub name: String,
    pub revision: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenameResource {
    pub command_id: String,
    pub resource_id: String,
    pub expected_name_revision: u64,
    pub name: String,
}
impl RenameResource {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.command_id, 128)?;
        identifier(&self.resource_id, 128)?;
        if self.expected_name_revision > 9_007_199_254_740_990
            || self.name.len() > 320
            || self.name.chars().count() > 80
            || self.name.trim() != self.name
            || self
                .name
                .chars()
                .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
        {
            return Err(hagency_core::InvalidInput("invalid local resource name").into());
        }
        Ok(())
    }
}

/// Exact original effect receipt, not a claim that this name is still current.
/// Read the resource detail again for its current name/revision.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceNameResult {
    pub resource_id: String,
    pub local_name: ResourceLocalName,
}

impl Session {
    pub async fn rename_resource(
        &self,
        context: &HostContext,
        input: RenameResource,
    ) -> Result<ResourceNameResult, Error> {
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        let _command = self.scope.commands.lock().await;
        self.check(context, Permission::RenameResources)?;
        input.validate()?;

        {
            let result = match self.local_store()? {

                SessionStore::Current(store) => {
                    // Include exact native profile in receipt comparison. It is
                    // never supplied by the script or inferred from a visible ID.
                    let receipt = CommandReceipt::new(
                        context,
                        "resource.local_name",
                        &input.command_id,
                        &serde_json::json!({"profile":store.identity().0,"input":input}),
                    )?;
                    store
                        .rename_resource(&self.current_profile(store)?, input, receipt)
                        .await?
                }
            };
            self.check(context, Permission::RenameResources)
                .map_err(|_| Error::OutcomeUnknown)?;
            Ok(result)
        }
    }
}
