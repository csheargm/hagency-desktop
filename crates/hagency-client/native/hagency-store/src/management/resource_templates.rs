//! Retained resource agent definitions. These are reusable drafts, not approvals.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceTemplate {
    pub id: String,
    pub name: String,
    pub role: String,
    pub enabled: bool,
    pub created_at: u64,
    pub status: String,
    pub active_engagements: u64,
}
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTemplatePage {
    pub resource_id: String,
    pub resource_name: String,
    pub revision: String,
    pub items: Vec<ResourceTemplate>,
    pub next_after: Option<String>,
    pub roles: Vec<String>,
    pub can_create: bool,
}
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTemplateDetail {
    pub resource_id: String,
    pub revision: String,
    pub definition: ResourceTemplate,
    pub roles: Vec<String>,
    pub can_edit_identity: bool,
    pub can_toggle: bool,
    pub can_delete: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateCreateInput {
    pub resource_id: String,
    pub expected_revision: String,
    pub name: String,
    pub role: String,
    pub enabled: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateUpdateInput {
    pub resource_id: String,
    pub id: String,
    pub expected_revision: String,
    pub name: String,
    pub role: String,
    pub enabled: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateDeleteInput {
    pub resource_id: String,
    pub id: String,
    pub expected_revision: String,
}
fn version(resource: &str, revision: &str) -> Result<(), Error> {
    identifier(resource, 128)?;
    if revision.len() != 64
        || !revision
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(hagency_core::InvalidInput("invalid template revision").into());
    }
    Ok(())
}
fn fields(name: &str, role: &str) -> Result<(), Error> {
    if name.is_empty()
        || name.len() > 64
        || !name.as_bytes()[0].is_ascii_lowercase()
        || !name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'))
        || !hagency_core::qualification::roles().any(|r| r == role)
    {
        return Err(hagency_core::InvalidInput("invalid resource template").into());
    }
    Ok(())
}
impl TemplateCreateInput {
    pub fn validate(&self) -> Result<(), Error> {
        version(&self.resource_id, &self.expected_revision)?;
        fields(&self.name, &self.role)
    }
}
impl TemplateUpdateInput {
    pub fn validate(&self) -> Result<(), Error> {
        version(&self.resource_id, &self.expected_revision)?;
        identifier(&self.id, 128)?;
        fields(&self.name, &self.role)
    }
}
impl TemplateDeleteInput {
    pub fn validate(&self) -> Result<(), Error> {
        version(&self.resource_id, &self.expected_revision)?;
        identifier(&self.id, 128)?;
        Ok(())
    }
}
/// Delete retains the reviewed old fields, even after the definition is removed.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceTemplateChange {
    pub operation: String,
    pub name: String,
    pub role: String,
    pub enabled: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceTemplateCommand {
    pub command_id: String,
    pub resource_id: String,
    pub definition_id: String,
    pub expected_revision: String,
    pub change: ResourceTemplateChange,
}
impl ResourceTemplateCommand {
    pub fn operation(&self) -> Result<IntentOperation, Error> {
        match self.change.operation.as_str() {
            "create" => Ok(IntentOperation::TemplateCreate),
            "update" => Ok(IntentOperation::TemplateUpdate),
            "delete" => Ok(IntentOperation::TemplateDelete),
            _ => Err(hagency_core::InvalidInput("invalid template operation").into()),
        }
    }
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.command_id, 128)?;
        identifier(&self.definition_id, 128)?;
        version(&self.resource_id, &self.expected_revision)?;
        self.operation()?;
        fields(&self.change.name, &self.change.role)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceTemplateResult {
    pub command_id: String,
    pub resource_id: String,
    pub definition_id: String,
    pub deleted: bool,
    pub revision: String,
}
impl Session {
    /// A native session gate is retained into the serialized canonical writer.
    pub fn resource_template_access(
        &self,
        context: &HostContext,
        permission: Permission,
    ) -> Result<(), Error> {
        if !matches!(
            permission,
            Permission::ReadResourceTemplates | Permission::ManageResourceTemplates
        ) {
            return Err(Error::LocalAuthority);
        }
        self.check(context, permission)?;
        let SessionStore::Current(store) = self.local_store()?;
        {
            store.enrollment_registry(&self.current_profile(store)?)?;
            return self.check(context, permission);
        }
    }
    fn template_profile(
        &self,
        context: &HostContext,
        permission: Permission,
        store: &crate::owner_resources::adoption::CurrentManagementStore,
    ) -> Result<crate::owner_resources::ProfileAuthority, Error> {
        self.resource_template_access(context, permission)?;
        let authority = self.authority();
        let context = context.clone();
        self.current_profile(store)?
            .with_native_gate(move || authority.authorize(&context, permission).is_ok())
    }
    pub async fn resource_templates(
        &self,
        context: &HostContext,
        resource_id: String,
        after: Option<String>,
    ) -> Result<ResourceTemplatePage, Error> {
        self.resource_template_access(context, Permission::ReadResourceTemplates)?;
        identifier(&resource_id, 128)?;
        if let Some(id) = &after {
            identifier(id, 128)?;
        }

        {
            let SessionStore::Current(store) = self.local_store()?;
            let mut result = store
                .resource_templates(
                    &self.template_profile(context, Permission::ReadResourceTemplates, store)?,
                    resource_id,
                    after,
                )
                .await?;
            result.can_create &= self
                .check(context, Permission::ManageResourceTemplates)
                .is_ok();
            self.check(context, Permission::ReadResourceTemplates)?;
            Ok(result)
        }
    }
    pub async fn resource_template(
        &self,
        context: &HostContext,
        resource_id: String,
        id: String,
    ) -> Result<ResourceTemplateDetail, Error> {
        self.resource_template_access(context, Permission::ReadResourceTemplates)?;
        identifier(&resource_id, 128)?;
        identifier(&id, 128)?;

        {
            let SessionStore::Current(store) = self.local_store()?;
            let mut result = store
                .resource_template(
                    &self.template_profile(context, Permission::ReadResourceTemplates, store)?,
                    resource_id,
                    id,
                )
                .await?;
            let write = self
                .check(context, Permission::ManageResourceTemplates)
                .is_ok();
            result.can_edit_identity &= write;
            result.can_toggle &= write;
            result.can_delete &= write;
            self.check(context, Permission::ReadResourceTemplates)?;
            Ok(result)
        }
    }
    pub async fn apply_resource_template(
        &self,
        context: &HostContext,
        input: ResourceTemplateCommand,
    ) -> Result<ResourceTemplateResult, Error> {
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        let _command = self.scope.commands.lock().await;
        self.resource_template_access(context, Permission::ManageResourceTemplates)?;
        input.validate()?;

        {
            let SessionStore::Current(store) = self.local_store()?;
            let receipt = CommandReceipt::new(
                context,
                "resource.template",
                &input.command_id,
                &serde_json::json!({"profile":store.identity().0,"input":input}),
            )?;
            let result = store
                .apply_resource_template(
                    &self.template_profile(context, Permission::ManageResourceTemplates, store)?,
                    input,
                    receipt,
                )
                .await?;
            self.check(context, Permission::ManageResourceTemplates)
                .map_err(|_| Error::OutcomeUnknown)?;
            Ok(result)
        }
    }
    /// Native original-ID cancellation of a saved template change. It shares
    /// the apply receipt key, so whichever commits first owns the command: a
    /// cancelled change can never be applied later, and an already applied
    /// change is returned (`Some`) instead of being contradicted.
    pub async fn cancel_resource_template(
        &self,
        context: &HostContext,
        input: ResourceTemplateCommand,
    ) -> Result<Option<ResourceTemplateResult>, Error> {
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        let _command = self.scope.commands.lock().await;
        self.resource_template_access(context, Permission::ManageResourceTemplates)?;
        input.validate()?;

        {
            let SessionStore::Current(store) = self.local_store()?;
            let receipt = CommandReceipt::new(
                context,
                "resource.template",
                &input.command_id,
                &serde_json::json!({"profile":store.identity().0,"input":input}),
            )?;
            let result = store
                .cancel_resource_template(
                    &self.template_profile(context, Permission::ManageResourceTemplates, store)?,
                    input,
                    receipt,
                )
                .await?;
            self.check(context, Permission::ManageResourceTemplates)
                .map_err(|_| Error::OutcomeUnknown)?;
            Ok(result)
        }
    }
}
