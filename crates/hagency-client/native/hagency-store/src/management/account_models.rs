//! A selected original namespace for metadata observation; never execution authority.
use super::*;
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountModelQuery {
    pub account_id: String,
    pub expected_revision: String,
}
impl AccountModelQuery {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.account_id, 128)?;
        if self.expected_revision.len() != 64
            || !self
                .expected_revision
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        {
            return Err(hagency_core::InvalidInput("invalid account revision").into());
        }
        Ok(())
    }
}
/// Original handle and current selection guard. No path or authority serialization.
pub struct AccountInspection {
    launch: crate::ManagedLaunch,
    session: Session,
    context: HostContext,
    query: AccountModelQuery,
}
impl AccountInspection {
    pub fn launch(&self) -> &crate::ManagedLaunch {
        &self.launch
    }
    pub async fn check(&self) -> Result<(), Error> {
        self.launch.check()?;
        let fresh = self
            .session
            .inspect_account(&self.context, self.query.clone())
            .await?;
        fresh.launch.check()?;
        self.launch.check()
    }
}
impl Session {
    fn account_models_store(&self, context: &HostContext) -> Result<(DomainStore, String), Error> {
        self.check(context, Permission::ReadAccountModels)?;
        match self.local_store()? {
            SessionStore::Current(store) => Ok((
                store.account_login_store(&self.current_profile(store)?)?,
                store.identity().0.into(),
            )),
        }
    }
    pub async fn inspect_account(
        &self,
        context: &HostContext,
        query: AccountModelQuery,
    ) -> Result<AccountInspection, Error> {
        query.validate()?;
        let (store, profile) = self.account_models_store(context)?;
        let session = self.clone();
        let original = context.clone();
        let selection = query.clone();
        let launch = store
            .call(4096, move |db| {
                session.account_models_store(&original)?;
                let launch = db.management_inspect_account(&profile, &selection)?;
                session.account_models_store(&original)?;
                Ok(launch)
            })
            .await?;
        self.account_models_store(context)?;
        Ok(AccountInspection {
            launch,
            session: self.clone(),
            context: context.clone(),
            query,
        })
    }
}
