//! Read-only local account inventory. Recorded bindings are not live login proof.
use super::*;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountInventoryQuery {
    pub id: Option<String>,
    pub after: Option<String>,
}
impl AccountInventoryQuery {
    pub fn validate(&self) -> Result<(), Error> {
        for id in [&self.id, &self.after].into_iter().flatten() {
            identifier(id, 128)?;
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSummary {
    pub id: String,
    #[serde(deserialize_with = "crate::management::wire_static::one")]
    pub kind: crate::management::wire_static::Label,
    pub runtime: String,
    pub state: String,
    pub ordinal: Option<u8>,
    pub revision: Option<String>,
    pub profile: Option<String>,
    #[serde(deserialize_with = "crate::management::wire_static::one")]
    pub readiness: crate::management::wire_static::Label,
    /// Profile-scoped display grouping from recorded provider identity only.
    /// This is neither a current login observation nor an authorization key.
    pub identity_key: Option<String>,
    /// Only resources visible to this Matrix profile. Not a retirement check.
    pub resource_count: u64,
}

#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInventory {
    #[serde(deserialize_with = "crate::management::wire_static::one")]
    pub protocol: crate::management::wire_static::Label,
    pub observed_at_ms: u64,
    pub accounts: Vec<AccountSummary>,
    pub account: Option<AccountSummary>,
    pub resources: Vec<ResourceSummary>,
    pub next_after: Option<String>,
}

impl Session {
    /// Native-only read projection; the profile witness is not deserializable
    /// by a miniapp and the lookup has no enrollment side effect.
    pub async fn octos_account_resource_id(
        &self, context:&HostContext, binding:hagency_runtime::octos::profile_binding::ProfileBinding,
    )->Result<Option<String>,Error> {
        self.check(context,Permission::ReadAccounts)?;
        let SessionStore::Current(store)=self.local_store()?;
        let profile=self.current_profile(store)?;
        let registry=store.enrollment_registry(&profile)?;
        let result=registry.find_octos_account_resource(profile,binding).await?;
        self.check(context,Permission::ReadAccounts)?;
        Ok(result)
    }
    pub async fn octos_account_resources(
        &self, context:&HostContext, binding:hagency_runtime::octos::profile_binding::ProfileBinding,
    )->Result<Vec<(String,String)>,Error> {
        self.check(context,Permission::ReadAccounts)?;
        let SessionStore::Current(store)=self.local_store()?;
        let profile=self.current_profile(store)?;
        let registry=store.enrollment_registry(&profile)?;
        let result=registry.find_octos_account_resources(profile,binding).await?;
        self.check(context,Permission::ReadAccounts)?;
        Ok(result)
    }
    /// Model picker projection; only local profile owners and their visible
    /// resource mappings are exposed, with the shared profile ceiling.
    pub async fn octos_model_overview(
        &self, context:&HostContext, binding:hagency_runtime::octos::profile_binding::ProfileBinding,
    )->Result<(Vec<(String,String)>,Option<u64>),Error> {
        self.check(context,Permission::ReadAccountModels)?;
        let SessionStore::Current(store)=self.local_store()?;
        let profile=self.current_profile(store)?;
        let registry=store.enrollment_registry(&profile)?;
        let result=registry.octos_account_overview(profile,binding).await?;
        self.check(context,Permission::ReadAccountModels)?;
        Ok(result)
    }
    pub async fn account_inventory(
        &self,
        context: &HostContext,
        query: AccountInventoryQuery,
    ) -> Result<AccountInventory, Error> {
        self.check(context, Permission::ReadAccounts)?;
        query.validate()?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .account_inventory(&self.current_profile(store)?, query)
                    .await?
            }
        };
        self.check(context, Permission::ReadAccounts)?;
        Ok(result)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrepareAccount {
    pub command_id: String,
}
impl PrepareAccount {
    pub fn validate(&self) -> Result<(), Error> { identifier(&self.command_id, 128)?; Ok(()) }
}
/// The preparation observation never claims provider authentication or capacity.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountPreparation {
    pub account_id: String,
    pub state: String,
}
impl Session {
    pub async fn prepare_account(&self, context: &HostContext, input: PrepareAccount) -> Result<AccountPreparation, Error> {
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        let _command = self.scope.commands.lock().await;
        self.check(context, Permission::PrepareAccounts)?;
        input.validate()?;
        let authority = self.authority();
        let original = context.clone();
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                let receipt = CommandReceipt::new(context, "account.prepare", &input.command_id, &serde_json::json!({"profile":store.identity().0,"input":input}))?;
                store.prepare_account(&self.current_profile(store)?, receipt, authority, original).await?
            }
        };
        self.check(context, Permission::PrepareAccounts).map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}
