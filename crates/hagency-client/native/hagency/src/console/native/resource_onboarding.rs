//! Provider discovery projects public model choices; native enrollment keeps
//! provider account references inside the host and never launches a model turn.
use super::{NativeError, NativeManagement, failure};
use hagency_store::management::{
    HostContext, Permission, ResourceSummary, Session,
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ManagementRuntime {
    Codex,
    Claude,
    Octos,
}
impl ManagementRuntime {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Octos => "octos",
        }
    }
}
pub(super) fn projection(runtime: &str, status: &Value) -> Value {
    projection_for(runtime, None, status)
}
/// `provider` is the resource provider the models would be enrolled under.
pub(super) fn projection_for(runtime: &str, provider: Option<&str>, status: &Value) -> Value {
    let models = status["models"].as_array().map(|rows| rows.iter().take(100).map(|row| {
        let reasoning = row["supportedReasoningEfforts"].as_array().map(|efforts| efforts.iter()
            .take(16).filter_map(|v| v["reasoningEffort"].as_str().map(str::to_owned))
            .collect::<Vec<_>>()).unwrap_or_default();
        let candidates: Vec<Option<String>> = if reasoning.is_empty() { vec![None] }
            else { reasoning.iter().cloned().map(Some).collect() };
        let shared_reasoning: Vec<_> = candidates.into_iter().filter(|effort| {
            let profile = hagency_core::qualification::ModelProfile {
                framework: runtime.into(), model: row["model"].as_str().unwrap_or("").into(),
                provider: provider.map(str::to_owned), reasoning: effort.clone(),
            };
            hagency_core::qualification::roles().any(|role| hagency_core::qualification::qualifies(&profile, role, None))
        }).collect();
        let default = row["defaultReasoningEffort"].as_str().map(str::to_owned);
        let shared_default = if shared_reasoning.contains(&default) { default }
            else { shared_reasoning.first().cloned().flatten() };
        json!({
            "id":row["id"],"model":row["model"],"label":row["displayName"],
            "default":row["isDefault"],"defaultReasoning":row["defaultReasoningEffort"],
            "reasoning":reasoning,"sharedReasoning":shared_reasoning,"sharedDefaultReasoning":shared_default,
        })
    }).collect::<Vec<_>>()).unwrap_or_default();
    json!({"runtime":runtime,"state":status["state"],"connected":status["authenticated"]==true,
        "accountVerified":status["authenticated"]==true && (runtime=="codex" || status["accountVerified"]==true),
        "strictTokenCap":status["strictTokenCap"]==true,"models":models})
}
impl NativeManagement {
    pub(super) async fn verify_local_resource(
        &self,
        context: &HostContext,
        session: &Session,
        resource: &ResourceSummary,
        resume: bool,
    ) -> Result<ResourceSummary, NativeError> {
        #[cfg(not(feature = "resource-adoption"))]
        { let _=(context,session,resource,resume); return Err(failure(409,"resource_setup_required")); }
        #[cfg(feature = "resource-adoption")]
        {
            // The adopted profile intentionally cannot execute legacy owner
            // commands. Observe only the selected provider's quota identity;
            // this does not qualify a model or make an agent runnable.
            let _guard = self.owner.0.calls.lock().await;
            let permission = if resume {
                Permission::ResumeConnections
            } else {
                Permission::VerifyConnections
            };
            session
                .authorize(context, permission)
                .map_err(|_| failure(403, "management_permission_required"))?;
            if resource.framework == "claude" {
                // No sign-in folder: the owner's connected configuration.
                let proof = self.claude_resource_proof(context, session).await?;
                return self
                    .verify_canonical_connection(context, resource, resume, proof)
                    .await;
            }
            if resource.framework != "codex" {
                return Err(failure(409, "provider_identity_unavailable"));
            }
            let namespace=session.connection_namespace(context,resource,resume).await.map_err(super::management::store_error)?;
            let proof=if let Some(namespace)=namespace {
                let deadline=session.valid_until().min(Instant::now()+Duration::from_secs(30));
                self.namespace_provider(&namespace,deadline).await?
            } else {
                // Only historical resources without a namespace binding use the
                // original profile adapter. Missing recorded bindings fail above.
                let cookie=self.owner.0.cookie.lock().await.clone();
                let raw=cookie.strip_prefix("hagency_console=").ok_or_else(||failure(401,"owner_authorization_required"))?;
                let (proof,deadline)=crate::console::owner_provider::identity::verify(&self.owner.0.host.console,raw).await.map_err(|e|failure(e.status,e.code))?;
                if Instant::now()>=deadline {return Err(failure(409,"provider_identity_unavailable"));}
                proof
            };
            return self
                .verify_canonical_connection(context, resource, resume, proof)
                .await;
        }
    }
    #[cfg(feature = "resource-adoption")]
    pub(super) async fn verify_canonical_connection(
        &self,
        context: &HostContext,
        resource: &ResourceSummary,
        resume: bool,
        proof: hagency_store::owner_resources::VerifiedProviderAccount,
    ) -> Result<ResourceSummary, NativeError> {
        let (fresh, live) = self.authorized().await?;
        if fresh != *context {
            return Err(failure(401, "management_scope_changed"));
        }
        let permission = if resume {
            Permission::ResumeConnections
        } else {
            Permission::VerifyConnections
        };
        live.authorize(context, permission)
            .map_err(|_| failure(403, "management_permission_required"))?;
        // The provider observation may outlive the initially captured package
        // session. Use the freshly authorized same-context session; never
        // revive the expired proof or extend the provider witness deadline.
        let result = live
            .verify_canonical_connection(
                context,
                resource.id.clone(),
                resource.revision.clone(),
                proof,
                resume,
            )
            .await
            .map_err(|_| failure(409, "management_executor_changed"))?;
        let (fresh, live) = self.authorized().await?;
        if fresh != *context {
            return Err(failure(401, "management_scope_changed"));
        }
        live.authorize(context, permission)
            .map_err(|_| failure(403, "management_permission_required"))?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn current_codex_models_use_only_live_qualified_efforts_without_switching_defaults() {
        for model in [
            "gpt-6.1-sol",
            "gpt-6-astra",
            "gpt-6.1-terra",
            "gpt-6-luna",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
        ] {
            let raw = json!({"authenticated":true,"models":[{
                "id":model,"model":model,"displayName":model,"isDefault":false,
                "defaultReasoningEffort":"medium",
                "supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"medium"},{"reasoningEffort":"high"},{"reasoningEffort":"xhigh"},{"reasoningEffort":"max"},{"reasoningEffort":"ultra"}]
            }]});
            let result = projection("codex", &raw);
            let row = &result["models"][0];
            assert_eq!(row["model"], model);
            assert_eq!(row["default"], false);
            assert_eq!(row["sharedReasoning"], json!(["low","medium","high","xhigh","max"]));
            assert_eq!(row["sharedDefaultReasoning"], "medium");
            // An older/restricted provider catalog must never gain settings just
            // because the role policy knows that model supports them elsewhere.
            let mut restricted = raw;
            restricted["models"][0]["supportedReasoningEfforts"] = json!([{"reasoningEffort":"high"}]);
            let result = projection("codex", &restricted);
            assert_eq!(result["models"][0]["sharedReasoning"], json!(["high"]));
            assert_eq!(result["models"][0]["sharedDefaultReasoning"], "high");
            restricted["models"][0]["supportedReasoningEfforts"] = json!([]);
            let result = projection("codex", &restricted);
            assert_eq!(result["models"][0]["sharedReasoning"], json!([]));
            assert_eq!(result["models"][0]["sharedDefaultReasoning"], Value::Null);
        }
    }
    #[test]
    fn shared_choices_intersect_provider_catalog_with_existing_resource_policy() {
        let status = json!({"authenticated":true,"models":[
            {"id":"sol","model":"gpt-5.6-sol","defaultReasoningEffort":"unsupported",
             "supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"high"},{"reasoningEffort":"unsupported"}]},
            {"id":"future","model":"unqualified-future-model","supportedReasoningEfforts":[{"reasoningEffort":"high"}]}
        ]});
        let models = projection("codex", &status)["models"].clone();
        assert_eq!(
            models[0]["reasoning"],
            json!(["low", "high", "unsupported"])
        );
        assert_eq!(models[0]["sharedReasoning"], json!(["low", "high"]));
        assert_eq!(models[0]["sharedDefaultReasoning"], "low");
        assert_eq!(models[1]["sharedReasoning"], json!([]));
        // A provider model remains inspectable even when it cannot be enrolled.
        assert_eq!(models[1]["id"], "future");
        let claude = projection(
            "claude",
            &json!({"models":[{"id":"opus","model":"claude-opus-5"}]}),
        );
        assert_eq!(claude["models"][0]["sharedReasoning"], json!([null]));
    }
    #[test]
    fn provider_projection_strips_private_native_fields_and_reports_unverified_accounts() {
        let value = json!({"state":"ready","authenticated":true,"credentialRef":"must-not-escape","workspace":"/private/home","authUrl":"https://login/?secret=private","models":[{"id":"work","model":"kimi-k3","displayName":"Work","credentialRef":"private-ref","supportedReasoningEfforts":[{"reasoningEffort":"none","token":"private-token"}]}]});
        let projected = projection("octos", &value);
        assert_eq!(projected["connected"], true);
        assert_eq!(projected["accountVerified"], false);
        assert_eq!(projected["models"][0]["reasoning"], json!(["none"]));
        assert_eq!(
            projection(
                "claude",
                &json!({"authenticated":false,"accountVerified":true})
            )["accountVerified"],
            false
        );
        let text = projected.to_string();
        for value in [
            "must-not-escape",
            "/private",
            "secret=",
            "private-ref",
            "private-token",
            "credentialRef",
            "authUrl",
        ] {
            assert!(!text.contains(value));
        }
    }
}
