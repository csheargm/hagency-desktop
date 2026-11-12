//! Native-confirmed enrollment from an original managed account. The journal
//! fixes the input before observation; canonical receipt recovery precedes RPCs.
use super::{NativeManagement,NativeError,NamespaceEnrollment,failure,management::store_error};
use hagency_store::{management::{Intent,IntentOperation,Permission},owner_resources::OwnerResourceReceipt};
use std::time::Instant;
impl NativeManagement {
    /// Trusted native consent only. No generic script dispatch calls this API.
    /// A saved original intent may be explicitly retried with current authority.
    pub async fn enroll_account_resource(&self,input:NamespaceEnrollment,confirmed_until:Instant)->Result<OwnerResourceReceipt,NativeError>{
        input.validate().map_err(store_error)?;
        if Instant::now()>=confirmed_until {return Err(failure(403,"provider_confirmation_expired"));}
        let (context,session)=self.authorized().await?;
        session.authorize(&context,Permission::EnrollResources).map_err(store_error)?;
        if !self.account_enrollment_ready(){return Err(failure(409,"device_setup_required"));}
        let intent=Intent::new(IntentOperation::EnrollAccountResource,input.command_id.clone(),&input).map_err(store_error)?;
        self.with_intent(intent,self.enroll_account_resource_now(input,confirmed_until)).await
    }
    async fn enroll_account_resource_now(&self,input:NamespaceEnrollment,confirmed_until:Instant)->Result<OwnerResourceReceipt,NativeError>{
        // The owner's own Claude Code configuration has no sign-in folder.
        if input.account.account_id==super::claude_account::ACCOUNT {return self.enroll_claude_resource(input,confirmed_until).await;}
        #[cfg(not(feature="resource-adoption"))]
        {let _=(input,confirmed_until);Err(failure(409,"device_setup_required"))}
        #[cfg(feature="resource-adoption")]
        {
            let _guard=self.owner.0.calls.lock().await;
            let (context,session)=self.authorized().await?;
            let (registry,profile)=session.namespace_enrollment_registry(&context).map_err(store_error)?;
            if let Some(original)=registry.recover_namespace_enrollment(profile.clone(),input.clone()).await.map_err(store_error)? {
                return self.finish(&context,Ok(original),false).await;
            }
            if Instant::now()>=confirmed_until {return Err(failure(403,"provider_confirmation_expired"));}
            let selection=registry.select_resource_account(profile.clone(),input.account.clone()).await.map_err(store_error)?;
            let deadline=session.valid_until().min(confirmed_until).min(Instant::now()+std::time::Duration::from_secs(30));
            let observation=self.observe_account_namespace(selection.launch(),deadline).await;
            let (fresh,live)=self.authorized().await?;
            if fresh!=context {return Err(failure(401,"management_scope_changed"));}
            live.authorize(&context,Permission::EnrollResources).map_err(store_error)?;
            selection.check_current().await.map_err(store_error)?;
            let (proof,catalog)=observation?;
            validate_model(&input,&serde_json::json!(catalog))?;
            if Instant::now()>=confirmed_until {return Err(failure(403,"provider_confirmation_expired"));}
            let consent=registry.prepare_namespace_enrollment(profile,input,selection,proof).map_err(store_error)?;
            let result=consent.approve().await;
            self.finish(&context,result,true).await
        }
    }
}
#[cfg(any(feature="resource-adoption",test))]
fn validate_model(input:&NamespaceEnrollment,catalog:&serde_json::Value)->Result<(),NativeError>{
    let projected=super::resource_onboarding::projection("codex",catalog);
    let row=projected["models"].as_array().and_then(|rows|rows.iter().find(|r|r["model"].as_str()==Some(input.model.as_str()))).ok_or_else(||failure(409,"provider_model_changed"))?;
    if !row["sharedReasoning"].as_array().is_some_and(|efforts|efforts.contains(&serde_json::json!(input.reasoning))) {
        return Err(failure(409,"management_model_unqualified"));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn enrollment_uses_fresh_model_name_and_qualified_reasoning_not_catalog_alias(){
        let mut input=NamespaceEnrollment{command_id:"original".into(),account:super::super::AccountModelQuery{account_id:"account_original".into(),expected_revision:"a".repeat(64)},model:"gpt-5.6-sol".into(),reasoning:Some("medium".into()),monthly_tokens:1000};
        let catalog=json!({"models":[{"id":"coding-alias","model":"gpt-5.6-sol","supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"unsupported"}]}]});
        validate_model(&input,&catalog).unwrap();
        input.model="coding-alias".into();assert_eq!(validate_model(&input,&catalog).unwrap_err().code,"provider_model_changed");
        input.model="gpt-5.6-sol".into();input.reasoning=Some("unsupported".into());assert_eq!(validate_model(&input,&catalog).unwrap_err().code,"management_model_unqualified");
        input.reasoning=None;assert!(validate_model(&input,&catalog).is_err());
        assert!(validate_model(&input,&json!({"models":[]})).is_err());
    }
}
