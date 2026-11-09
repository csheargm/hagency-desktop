//! Closed in-place editing. Creating configurations and amending an admitted
//! resource are separate operations, never caller-controlled flags here.
//!
//! A ceiling-only request that names `expectedCeilingSequence` is a request to
//! amend the monthly maximum of an offered, published or committed resource.
//! It is never applied as a definition edit, and an edit request is never
//! applied as an amendment: the input alone selects the operation.
use super::*;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditLocalResource {
    pub command_id: String,
    pub resource_id: String,
    pub expected_revision: String,
    pub profile: ProfileChange,
    pub ceiling: CeilingChange,
    /// Omission preserves both the stored default and legacy receipt hashes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_policy: Option<hagency_core::execution::ExecutionPolicy>,
    /// Only for a monthly-maximum amendment: the `ceilingAmendment.sequence`
    /// the caller reviewed. Omission keeps an in-place edit and its hashes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_ceiling_sequence: Option<u64>,
}

/// Why a monthly-maximum amendment was refused before any ledger effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CeilingAmendmentRefusal {
    /// Editable in place instead, unknown history, or no monthly maximum.
    NotAvailable,
    /// An offered resource's monthly maximum can change but never be cleared.
    ClearRefused,
    /// The monthly maximum already has this value.
    Unchanged,
    /// Below this month's published capacity, Agent holds or measured use.
    BelowCommitments,
    /// Usage on the resource is unsettled; the maximum can only be raised.
    UsageUnsettled,
}
impl EditLocalResource {
    /// An amendment request: it names the sequence it expects and changes
    /// nothing but the monthly maximum (`validate` enforces the shape).
    pub fn is_ceiling_amendment(&self) -> bool {
        self.expected_ceiling_sequence.is_some()
    }
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.command_id, 128)?;
        identifier(&self.resource_id, 128)?;
        if self.expected_revision.len() != 64
            || !self
                .expected_revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(hagency_core::InvalidInput("invalid resource revision").into());
        }
        if let ProfileChange::Select { model, reasoning } = &self.profile
            && (model.is_empty()
                || model.len() > 256
                || model.chars().any(char::is_control)
                || reasoning.as_ref().is_some_and(|s| {
                    s.is_empty() || s.len() > 128 || s.chars().any(char::is_control)
                }))
        {
            return Err(hagency_core::InvalidInput("invalid resource configuration").into());
        }
        // An amendment keeps the model and the default permissions, and sets
        // a whole monthly maximum of at least one token. A clear request stays
        // well-formed here so the writer can refuse it with its own reason.
        if let Some(sequence) = self.expected_ceiling_sequence
            && (sequence > hagency_core::JSON_SAFE_MAX
                || !matches!(self.profile, ProfileChange::Preserve {})
                || self.execution_policy.is_some()
                || match self.ceiling {
                    CeilingChange::Preserve {} => true,
                    CeilingChange::Clear {} => false,
                    CeilingChange::Monthly { tokens } => u64::from(tokens) == 0,
                })
        {
            return Err(hagency_core::InvalidInput("invalid ceiling amendment").into());
        }
        Ok(())
    }

    fn receipt(&self, context: &HostContext, profile: &str) -> Result<CommandReceipt, Error> {
        CommandReceipt::new(
            context,
            "resource.edit_local",
            &self.command_id,
            &serde_json::json!({"profile":profile,"input":self}),
        )
    }
}

impl Session {
    /// Trusted native read-only provider observation. No generic JSON command
    /// receives this registry or authority; it cannot authorize an edit.
    pub fn resource_observation_registry(
        &self, context: &HostContext,
    ) -> Result<(crate::owner_resources::OwnerResourceRegistry, crate::owner_resources::ProfileAuthority), Error> {
        self.check(context, Permission::ReadResources)?;
        self.check(context, Permission::ReadProviders)?;
        let SessionStore::Current(store) = self.local_store()?;
        let profile=self.current_profile(store)?;
        let registry=store.enrollment_registry(&profile)?;
        self.check(context, Permission::ReadProviders)?;
        Ok((registry,profile))
    }
    /// Current native writer selection, never a serialized mini-app capability.
    pub fn local_resource_editor_registry(
        &self,
        context: &HostContext,
    ) -> Result<
        (
            crate::owner_resources::OwnerResourceRegistry,
            crate::owner_resources::ProfileAuthority,
        ),
        Error,
    > {
        self.check(context, Permission::ConfigureResources)?;
        let SessionStore::Current(store) = self.local_store()?;
        let profile = self.current_profile(store)?;
        let registry = store.enrollment_registry(&profile)?;
        self.check(context, Permission::ConfigureResources)?;
        Ok((registry, profile))
    }
    /// Read only: recover the exact original effect before requesting fresh
    /// provider metadata. A completed edit may since have been offered.
    pub async fn recover_local_resource_edit(
        &self,
        context: &HostContext,
        input: &EditLocalResource,
    ) -> Result<Option<ResourceConfigurationResult>, Error> {
        self.check(context, Permission::ConfigureResources)?;
        input.validate()?;
        let SessionStore::Current(store) = self.local_store()?;
        let receipt = input.receipt(context, store.identity().0)?;
        let result = store
            .recover_local_resource_edit(
                &self.current_profile(store)?,
                input.resource_id.clone(),
                receipt,
            )
            .await?;
        self.check(context, Permission::ConfigureResources)?;
        Ok(result)
    }

    /// Trusted host only, after the native review of this same input. No
    /// provider metadata is needed: the definition and its model are unchanged.
    pub async fn amend_resource_ceiling(
        &self,
        context: &HostContext,
        input: EditLocalResource,
        confirmed_until: std::time::Instant,
    ) -> Result<ResourceConfigurationResult, Error> {
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        let _command = self.scope.commands.lock().await;
        self.check(context, Permission::ConfigureResources)?;
        input.validate()?;
        if !input.is_ceiling_amendment() {
            return Err(hagency_core::InvalidInput("not a ceiling amendment").into());
        }
        let SessionStore::Current(store) = self.local_store()?;
        let receipt = input.receipt(context, store.identity().0)?;
        let result = store
            .amend_resource_ceiling(&self.current_profile(store)?, input, receipt, confirmed_until)
            .await?;
        self.check(context, Permission::ConfigureResources)
            .map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }

    /// Trusted host only. The binding cannot be deserialized from mini-app data.
    pub async fn edit_local_resource(
        &self,
        context: &HostContext,
        input: EditLocalResource,
        binding: crate::owner_resources::ResourceEditBinding,
    ) -> Result<ResourceConfigurationResult, Error> {
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        let _command = self.scope.commands.lock().await;
        self.check(context, Permission::ConfigureResources)?;
        input.validate()?;
        let SessionStore::Current(store) = self.local_store()?;
        let receipt = input.receipt(context, store.identity().0)?;
        let result = store
            .edit_local_resource(&self.current_profile(store)?, input, binding, receipt)
            .await?;
        self.check(context, Permission::ConfigureResources)
            .map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}
