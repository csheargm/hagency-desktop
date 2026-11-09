//! A new configuration on an existing account. This cannot enroll an account,
//! change account quota, amend the source or allocate a remote contribution.
use super::*;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateResourceConfiguration {
    pub command_id: String,
    pub source_resource_id: String,
    pub expected_revision: String,
    pub profile: ProfileChange,
    pub ceiling: CeilingChange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_policy: Option<hagency_core::execution::ExecutionPolicy>,
}

impl CreateResourceConfiguration {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.command_id, 128)?;
        identifier(&self.source_resource_id, 128)?;
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
        Ok(())
    }

    fn receipt(&self, context: &HostContext, profile: &str) -> Result<CommandReceipt, Error> {
        CommandReceipt::new(
            context,
            "resource.create_configuration",
            &self.command_id,
            &serde_json::json!({"profile":profile,"input":self}),
        )
    }
}

impl Session {
    /// Original-result lookup precedes current source revision, model and login
    /// checks. It cannot create another resource or renew retired authority.
    pub async fn recover_resource_configuration(
        &self,
        context: &HostContext,
        input: &CreateResourceConfiguration,
    ) -> Result<Option<ResourceConfigurationResult>, Error> {
        self.check(context, Permission::ConfigureResources)?;
        input.validate()?;
        let SessionStore::Current(store) = self.local_store()?;
        let receipt = input.receipt(context, store.identity().0)?;
        let result = store
            .recover_resource_configuration(
                &self.current_profile(store)?,
                input.source_resource_id.clone(),
                receipt,
            )
            .await?;
        self.check(context, Permission::ConfigureResources)?;
        Ok(result)
    }

    /// Trusted native confirmation only. The deadline accompanies the original
    /// confirmation and is checked inside the serialized canonical transaction.
    pub async fn create_resource_configuration(
        &self,
        context: &HostContext,
        input: CreateResourceConfiguration,
        confirmed_until: Instant,
    ) -> Result<ResourceConfigurationResult, Error> {
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        let _command = self.scope.commands.lock().await;
        self.check(context, Permission::ConfigureResources)?;
        input.validate()?;
        if confirmed_until <= Instant::now()
            || confirmed_until > Instant::now() + Duration::from_secs(120)
        {
            return Err(Error::LocalAuthority);
        }
        let SessionStore::Current(store) = self.local_store()?;
        let receipt = input.receipt(context, store.identity().0)?;
        let result = store
            .create_resource_configuration(
                &self.current_profile(store)?,
                input,
                receipt,
                confirmed_until,
            )
            .await?;
        self.check(context, Permission::ConfigureResources)
            .map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}
