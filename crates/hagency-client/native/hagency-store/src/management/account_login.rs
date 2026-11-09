//! Typed native custody; script cannot supply a path, login URL or verdict.
use super::*;
use crate::{LoginAttempt, LoginVerdict, ManagedLaunch};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountLoginRequest {
    pub command_id: String,
    pub account_id: String,
    pub expected_revision: String,
}
impl AccountLoginRequest {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.command_id, 128)?;
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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountLoginStatus {
    pub command_id: String,
    pub account_id: String,
    pub state: String,
}
/// Only the first committed attempt can contain a launch handle.
pub struct AccountLoginReservation {
    pub status: AccountLoginStatus,
    pub handle: Option<AccountLoginHandle>,
}
/// Private original-attempt custody. Neither Clone nor Deserialize.
/// Settlement reports only that original attempt, including after revocation.
pub struct AccountLoginHandle {
    store: DomainStore,
    attempt: LoginAttempt,
    launch: ManagedLaunch,
}
impl AccountLoginHandle {
    pub fn launch(&self) -> &ManagedLaunch {
        &self.launch
    }
    pub async fn settle(self, verdict: LoginVerdict) -> Result<(), Error> {
        let Self {
            store,
            attempt,
            launch,
        } = self;
        store
            .call(4096, move |db| {
                let _retained = launch;
                db.management_settle_account_login(attempt, verdict)
            })
            .await
    }
}
impl Session {
    fn account_login_backing(&self, context: &HostContext) -> Result<(DomainStore, String), Error> {
        self.check(context, Permission::LoginAccounts)?;
        match self.local_store()? {
            SessionStore::Current(store) => Ok((
                store.account_login_store(&self.current_profile(store)?)?,
                store.identity().0.into(),
            )),
        }
    }
    fn account_login_receipt(
        context: &HostContext,
        profile: &str,
        input: &AccountLoginRequest,
    ) -> Result<CommandReceipt, Error> {
        CommandReceipt::new(
            context,
            "account.login",
            &input.command_id,
            &serde_json::json!({"profile":profile,"input":input}),
        )
    }
    pub async fn begin_account_login(
        &self,
        context: &HostContext,
        input: AccountLoginRequest,
    ) -> Result<AccountLoginReservation, Error> {
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        let _command = self.scope.commands.lock().await;
        input.validate()?;
        let (store, profile) = self.account_login_backing(context)?;
        let receipt = Self::account_login_receipt(context, &profile, &input)?;
        let original = context.clone();
        let session = self.clone();
        let (status, launch) = store
            .call(4096, move |db| {
                db.management_begin_account_login(&profile, input, receipt, || {
                    session.account_login_backing(&original).map(|_| ())
                })
            })
            .await?;
        self.account_login_backing(context)
            .map_err(|_| Error::OutcomeUnknown)?;
        Ok(AccountLoginReservation {
            status,
            handle: launch.map(|(attempt, launch)| AccountLoginHandle {
                store,
                attempt,
                launch,
            }),
        })
    }
    pub async fn account_login_status(
        &self,
        context: &HostContext,
        input: AccountLoginRequest,
    ) -> Result<AccountLoginStatus, Error> {
        input.validate()?;
        let (store, profile) = self.account_login_backing(context)?;
        let receipt = Self::account_login_receipt(context, &profile, &input)?;
        let original = context.clone();
        let session = self.clone();
        let result = store
            .call(4096, move |db| {
                session.account_login_backing(&original)?;
                let status = db.management_account_login_status(&profile, input, receipt)?;
                session.account_login_backing(&original)?;
                Ok(status)
            })
            .await?;
        self.account_login_backing(context)?;
        Ok(result)
    }
}
