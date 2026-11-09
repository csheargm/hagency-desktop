//! Retirement changes local namespace eligibility; it is not provider logout.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetireAccount {
    pub command_id: String,
    pub account_id: String,
    pub expected_revision: String,
}
impl RetireAccount {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.command_id, 128)?;
        AccountModelQuery { account_id: self.account_id.clone(), expected_revision: self.expected_revision.clone() }.validate()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountRetirement {
    pub account_id: String,
    pub state: String,
    pub revision: String,
    pub provider_logout: String,
}
impl Session {
    fn account_retirement_store(&self, context: &HostContext) -> Result<(DomainStore, String), Error> {
        self.check(context, Permission::RetireAccounts)?;
        match self.local_store()? {
            SessionStore::Current(store) => Ok((store.account_login_store(&self.current_profile(store)?)?, store.identity().0.into())),
        }
    }
    pub async fn retire_account(&self, context: &HostContext, input: RetireAccount, confirmed_until: Instant) -> Result<AccountRetirement, Error> {
        input.validate()?;
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        let _command = self.scope.commands.lock().await;
        let (store, profile) = self.account_retirement_store(context)?;
        let receipt = CommandReceipt::new(context, "account.retire", &input.command_id, &serde_json::json!({"profile":profile,"input":input}))?;
        let session = self.clone(); let original = context.clone();
        let result = store.call(4096, move |db| db.management_retire_account(&profile, &input, receipt, || { if Instant::now() >= confirmed_until { return Err(Error::LocalAuthority); } session.account_retirement_store(&original).map(|_| ()) })).await?;
        self.account_retirement_store(context).map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}
