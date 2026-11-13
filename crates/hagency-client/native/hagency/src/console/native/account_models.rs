//! Read only from the selected namespace; never fall back to the profile provider.
use super::{NativeManagement, NativeError, AccountModelQuery, failure, management::store_error};
use hagency_store::management::Permission;
use serde_json::{Value, json};
impl NativeManagement {
    pub async fn account_models(&self, input: AccountModelQuery) -> Result<Value, NativeError> {
        input.validate().map_err(store_error)?;
        if input.account_id == super::claude_account::ACCOUNT {
            return self.claude_account_models(input).await;
        }
        if super::octos_account::is_octos_account(&input.account_id) {
            return self.octos_account_models(input).await;
        }
        // Bound concurrent provider children per owner; check authority after waiting.
        let _guard = self.owner.0.calls.lock().await;
        let (context, session) = self.authorized().await?;
        session
            .authorize(&context, Permission::ReadAccountModels)
            .map_err(store_error)?;
        let inspection = session
            .inspect_account(&context, input.clone())
            .await
            .map_err(store_error)?;
        let provider = &self.owner.0.host.console.0.owner_provider;
        inspection.check().await.map_err(store_error)?;
        #[cfg(test)]
        let fixture = self
            .owner
            .0
            .account_logins
            .executable
            .lock()
            .unwrap()
            .clone();
        #[cfg(test)]
        let executable = match fixture {
            Some(p) => p,
            None => {
                super::super::owner_provider::executable().map_err(|e| failure(e.status, e.code))?
            }
        };
        #[cfg(not(test))]
        let executable =
            super::super::owner_provider::executable().map_err(|e| failure(e.status, e.code))?;
        let raw = super::super::owner_provider::managed::inspect_models(
            inspection.launch(),
            provider,
            &executable,
        )
        .await;
        let (fresh, live) = self.authorized().await?;
        if fresh != context {
            return Err(failure(401, "management_scope_changed"));
        }
        live.authorize(&context, Permission::ReadAccountModels)
            .map_err(store_error)?;
        inspection.check().await.map_err(store_error)?;
        let raw = raw.map_err(|e| failure(e.status, e.code))?;
        let projected = super::resource_onboarding::projection("codex", &json!(raw));
        let provider = json!({"runtime":"codex","connected":true,"models":projected["models"]});
        let result = json!({"protocol":"hagency-management-v1","accountId":input.account_id,"revision":input.expected_revision,"enrollmentReady":self.account_enrollment_ready(),"provider":provider});
        if serde_json::to_vec(&result)
            .map_err(|_| failure(503, "management_unavailable"))?
            .len()
            > 60 * 1024
        {
            return Err(failure(503, "management_capacity"));
        }
        Ok(result)
    }
}
