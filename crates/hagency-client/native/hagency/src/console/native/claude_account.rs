//! The owner's own Claude Code configuration as the account a pool is made
//! from. It has no sign-in folder of its own: the config folder is the one the
//! owner connected for local Agents, with whichever sign-in it has: an API
//! key or an endpoint token in its settings file, or a claude.ai sign-in.
//! No path, endpoint, credential or model mapping is accepted from a caller.
use super::{
    AccountInventory, AccountModelQuery, AccountSummary, NamespaceEnrollment, NativeError,
    NativeManagement, ResourceQuery, failure, management::store_error,
};
use crate::console::owner_provider::{self, ProviderPaths};
use hagency_runtime::claude::account::SharedCredential;
use hagency_store::{
    management::{HostContext, Permission, Session},
    owner_resources::{OwnerResourceReceipt, VerifiedProviderAccount},
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// One Claude Code configuration per computer user in this version.
pub(super) const ACCOUNT: &str = "account_claude_code";
const KIND: &str = "local_configuration";
const EFFORTS: [&str; 3] = ["medium", "low", "high"];

fn revision(paths: &ProviderPaths) -> Result<String, NativeError> {
    // Which folder is connected, not what is in it: the credential itself is
    // bound by the identity check of each call that uses it.
    hagency_core::canonical::digest(
        &json!({"version":1,"account":ACCOUNT,"selection":paths.credential_ref}),
    )
    .map_err(|_| failure(503, "management_unavailable"))
}

/// What the configuration can be shared as. On Anthropic's own endpoint the
/// names are the ones the capacity table rates. On the owner's own endpoint
/// they are the names the owner's settings give, and the table's when the
/// settings give none.
fn catalog(credential: &SharedCredential) -> (Option<&'static str>, Value) {
    let own = credential.binding().own_endpoint();
    let mut names: Vec<String> = if own {
        credential.model_names().into_iter().map(str::to_owned).collect()
    } else {
        Vec::new()
    };
    if names.is_empty() {
        let profile = hagency_core::qualification::ModelProfile {
            framework: "claude".into(),
            ..Default::default()
        };
        for choice in hagency_core::qualification::configuration_choices(&profile).unwrap_or_default() {
            if !names.contains(&choice.model) {
                names.push(choice.model);
            }
        }
    }
    let efforts: Vec<Value> = EFFORTS
        .iter()
        .map(|effort| json!({"reasoningEffort":effort,"description":""}))
        .collect();
    let models: Vec<Value> = names
        .iter()
        .take(32)
        .enumerate()
        .map(|(index, name)| {
            json!({"id":name,"model":name,"displayName":name,"isDefault":index==0,
                "defaultReasoningEffort":EFFORTS[0],"supportedReasoningEfforts":efforts})
        })
        .collect();
    (
        own.then_some(hagency_core::qualification::OWNER_ENDPOINT),
        json!({"state":"connected","authenticated":true,"accountVerified":true,"models":models}),
    )
}

fn projected(credential: &SharedCredential) -> Value {
    let (provider, catalog) = catalog(credential);
    let mut result = super::resource_onboarding::projection_for("claude", provider, &catalog);
    result["ownEndpoint"] = json!(provider.is_some());
    result
}

fn validate_model(input: &NamespaceEnrollment, credential: &SharedCredential) -> Result<(), NativeError> {
    let projected = projected(credential);
    let row = projected["models"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["model"].as_str() == Some(input.model.as_str())))
        .ok_or_else(|| failure(409, "provider_model_changed"))?;
    if !row["sharedReasoning"]
        .as_array()
        .is_some_and(|efforts| efforts.contains(&json!(input.reasoning)))
    {
        return Err(failure(409, "management_model_unqualified"));
    }
    Ok(())
}

impl NativeManagement {
    /// The config folder the owner connected for local Agents.
    async fn claude_paths(&self, context: &HostContext) -> Result<ProviderPaths, NativeError> {
        #[cfg(not(feature = "resource-adoption"))]
        {
            let _ = context;
            Err(failure(409, "device_setup_required"))
        }
        #[cfg(feature = "resource-adoption")]
        {
            let console = &self.owner.0.host.console;
            let device = console
                .authorized_device()
                .await
                .map_err(|_| failure(401, "owner_authorization_required"))?;
            device
                .bearer()
                .map_err(|_| failure(401, "owner_authorization_required"))?;
            self.check_observation_device(context, &device)?;
            let root = console
                .0
                .server_login
                .state_directory()
                .ok_or_else(|| failure(503, "provider_directory_unavailable"))?;
            owner_provider::external::connected_paths(
                &root,
                device.origin(),
                device.issuer(),
                device.subject(),
                device.owner_mxid(),
                hagency_agent_local::runtime::Kind::Claude,
            )
            .map_err(|error| failure(error.status, error.code))
        }
    }

    /// The sign-in of the connected configuration, observed now. The caller
    /// holds the command lane and has checked its own permission.
    async fn claude_credential(
        &self,
        context: &HostContext,
        expected_revision: Option<&str>,
        deadline: Instant,
    ) -> Result<SharedCredential, NativeError> {
        let paths = self.claude_paths(context).await?;
        if expected_revision.is_some_and(|expected| revision(&paths).ok().as_deref() != Some(expected)) {
            return Err(failure(409, "provider_account_changed"));
        }
        let observed = owner_provider::claude_identity::observe_for_pool(&paths, deadline)
            .await
            .map_err(|error| failure(error.status, error.code))?;
        // The owner connecting another folder meanwhile is another account.
        let current = self.claude_paths(context).await?;
        if current.home != paths.home
            || current.codex_home != paths.codex_home
            || current.credential_ref != paths.credential_ref
        {
            return Err(failure(409, "provider_account_changed"));
        }
        Ok(observed)
    }

    /// Proof for a Claude resource that has no sign-in folder: publication,
    /// re-check and resume. The caller holds the command lane.
    pub(super) async fn claude_resource_proof(
        &self,
        context: &HostContext,
        session: &Session,
    ) -> Result<VerifiedProviderAccount, NativeError> {
        let deadline = session
            .valid_until()
            .min(Instant::now() + Duration::from_secs(30));
        let credential = self.claude_credential(context, None, deadline).await?;
        VerifiedProviderAccount::from_claude_binding(credential.binding().clone()).map_err(store_error)
    }

    async fn claude_account(
        &self,
        context: &HostContext,
        session: &Session,
    ) -> Option<AccountSummary> {
        if !self.account_enrollment_ready() {
            return None;
        }
        // Not connected for local Agents: there is no such account to show.
        let paths = self.claude_paths(context).await.ok()?;
        let revision = revision(&paths).ok()?;
        let sign_in = tokio::time::timeout(
            Duration::from_secs(8),
            owner_provider::claude_identity::sign_in(&paths),
        )
        .await;
        let readiness = match sign_in {
            Ok(Ok(Some(true))) => "api_key",
            Ok(Ok(Some(false))) => "subscription",
            _ => "unknown",
        };
        let mut count = 0u64;
        let mut after = String::new();
        for _ in 0..8 {
            let query = ResourceQuery {
                after: after.clone(),
                framework: Some("claude".into()),
                ..Default::default()
            };
            let Ok(page) = session.resources(context, query).await else {
                break;
            };
            count += page.resources.len() as u64;
            match page.next_after {
                Some(next) => after = next,
                None => break,
            }
        }
        Some(AccountSummary {
            id: ACCOUNT.into(),
            kind: KIND,
            runtime: "claude".into(),
            state: "active".into(),
            ordinal: None,
            revision: Some(revision),
            profile: None,
            readiness,
            identity_key: None,
            resource_count: count,
        })
    }

    /// The first page of the account list also names this configuration. Its
    /// resources appear under it on every page, not as a second, unnamed
    /// association.
    pub(super) async fn add_claude_account(
        &self,
        context: &HostContext,
        session: &Session,
        page: &mut AccountInventory,
        first: bool,
    ) {
        let listed = if first {
            let account = self.claude_account(context, session).await;
            let listed = account.is_some();
            if let Some(account) = account {
                page.accounts.insert(0, account);
            }
            listed
        } else {
            // A later page: the same answer without asking the CLI again.
            self.account_enrollment_ready() && self.claude_paths(context).await.is_ok()
        };
        if listed {
            page.accounts
                .retain(|row| !(row.kind == "resource_binding" && row.runtime == "claude"));
        }
    }

    /// `after` continues the list of this account's resources.
    pub(super) async fn claude_account_detail(
        &self,
        context: &HostContext,
        session: &Session,
        after: Option<String>,
    ) -> Result<AccountInventory, NativeError> {
        session
            .authorize(context, Permission::ReadAccounts)
            .map_err(store_error)?;
        let account = self
            .claude_account(context, session)
            .await
            .ok_or_else(|| failure(404, "management_not_found"))?;
        let query = ResourceQuery {
            after: after.unwrap_or_default(),
            framework: Some("claude".into()),
            ..Default::default()
        };
        // A package that may not read resources still sees the account.
        let (resources, next_after) = session
            .resources(context, query)
            .await
            .map(|page| (page.resources, page.next_after))
            .unwrap_or_default();
        let observed_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
            .ok_or_else(|| failure(503, "management_unavailable"))?;
        let page = AccountInventory {
            protocol: hagency_store::management::PROTOCOL,
            observed_at_ms,
            accounts: Vec::new(),
            account: Some(account),
            resources,
            next_after,
        };
        self.finish(context, Ok(page), false).await
    }

    pub(super) async fn claude_account_models(
        &self,
        input: AccountModelQuery,
    ) -> Result<Value, NativeError> {
        // Bound concurrent provider children per owner; check authority after waiting.
        let _guard = self.owner.0.calls.lock().await;
        let (context, session) = self.authorized().await?;
        session
            .authorize(&context, Permission::ReadAccountModels)
            .map_err(store_error)?;
        let deadline = session
            .valid_until()
            .min(Instant::now() + Duration::from_secs(30));
        let observed = self
            .claude_credential(&context, Some(&input.expected_revision), deadline)
            .await;
        let (fresh, live) = self.authorized().await?;
        if fresh != context {
            return Err(failure(401, "management_scope_changed"));
        }
        live.authorize(&context, Permission::ReadAccountModels)
            .map_err(store_error)?;
        let provider = projected(&observed?);
        Ok(json!({"protocol":hagency_store::management::PROTOCOL,"accountId":input.account_id,
            "revision":input.expected_revision,"enrollmentReady":self.account_enrollment_ready(),
            "provider":provider}))
    }

    /// The owner confirmed this model and monthly maximum in the native
    /// review. The resource is bound to the credential observed here, not to
    /// one observed when the models were listed.
    pub(super) async fn enroll_claude_resource(
        &self,
        input: NamespaceEnrollment,
        confirmed_until: Instant,
    ) -> Result<OwnerResourceReceipt, NativeError> {
        #[cfg(not(feature = "resource-adoption"))]
        {
            let _ = (input, confirmed_until);
            Err(failure(409, "device_setup_required"))
        }
        #[cfg(feature = "resource-adoption")]
        {
            let _guard = self.owner.0.calls.lock().await;
            let (context, session) = self.authorized().await?;
            let (registry, profile) = session
                .namespace_enrollment_registry(&context)
                .map_err(store_error)?;
            if Instant::now() >= confirmed_until {
                return Err(failure(403, "provider_confirmation_expired"));
            }
            let deadline = session
                .valid_until()
                .min(confirmed_until)
                .min(Instant::now() + Duration::from_secs(30));
            let observed = self
                .claude_credential(&context, Some(&input.account.expected_revision), deadline)
                .await;
            let (fresh, live) = self.authorized().await?;
            if fresh != context {
                return Err(failure(401, "management_scope_changed"));
            }
            live.authorize(&context, Permission::EnrollResources)
                .map_err(store_error)?;
            let credential = observed?;
            validate_model(&input, &credential)?;
            if Instant::now() >= confirmed_until {
                return Err(failure(403, "provider_confirmation_expired"));
            }
            let proof = VerifiedProviderAccount::from_claude_binding(credential.binding().clone())
                .map_err(store_error)?;
            drop(credential);
            let consent = registry
                .prepare_enrollment(
                    profile,
                    input.command_id,
                    proof,
                    input.model,
                    input.reasoning,
                    input.monthly_tokens,
                    deadline,
                )
                .map_err(store_error)?;
            let result = consent.approve().await;
            self.finish(&context, result, true).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }
    fn input(model: &str, reasoning: Option<&str>) -> NamespaceEnrollment {
        NamespaceEnrollment {
            command_id: "original".into(),
            account: AccountModelQuery {
                account_id: ACCOUNT.into(),
                expected_revision: "a".repeat(64),
            },
            model: model.into(),
            reasoning: reasoning.map(str::to_owned),
            monthly_tokens: 1000,
        }
    }
    fn names(value: &Value) -> Vec<&str> {
        value["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["model"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn anthropic_key_offers_the_rated_models_with_their_own_roles() {
        let key = SharedCredential::from_api_settings(None, "sk-ant-api03-example".into(), true, &[], soon()).unwrap();
        let provider = projected(&key);
        assert_eq!(provider["runtime"], "claude");
        assert_eq!(provider["connected"], true);
        assert_eq!(provider["ownEndpoint"], false);
        let listed = names(&provider);
        assert!(listed.contains(&"claude-opus-5") && listed.contains(&"claude-haiku-4-5"), "{listed:?}");
        for row in provider["models"].as_array().unwrap() {
            assert_eq!(row["sharedReasoning"], json!(EFFORTS), "{row}");
            assert_eq!(row["sharedDefaultReasoning"], "medium");
        }
        validate_model(&input("claude-opus-5", Some("high")), &key).unwrap();
        // The owner's mapping does not add names Anthropic's endpoint is not rated for.
        let mapped = [("ANTHROPIC_MODEL".to_string(), "open-model".to_string())];
        let key = SharedCredential::from_api_settings(None, "sk-ant-api03-example".into(), true, &mapped, soon()).unwrap();
        assert!(!names(&projected(&key)).contains(&"open-model"));
        assert_eq!(validate_model(&input("open-model", Some("medium")), &key).unwrap_err().code, "provider_model_changed");
    }

    #[test]
    fn own_endpoint_offers_the_owners_names_and_falls_back_to_the_rated_ones() {
        let mapped = [
            ("ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(), "open-model-a".to_string()),
            ("ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(), "open-model-b".to_string()),
        ];
        let gateway = SharedCredential::from_api_settings(Some("https://gateway.example"), "token-one".into(), false, &mapped, soon()).unwrap();
        let provider = projected(&gateway);
        assert_eq!(provider["ownEndpoint"], true);
        assert_eq!(names(&provider), ["open-model-a", "open-model-b"]);
        assert_eq!(provider["models"][0]["sharedReasoning"], json!(EFFORTS));
        validate_model(&input("open-model-b", Some("low")), &gateway).unwrap();
        for (model, reasoning, code) in [
            ("claude-opus-5", Some("medium"), "provider_model_changed"),
            ("open-model-a", Some("max"), "management_model_unqualified"),
            ("open-model-a", None, "management_model_unqualified"),
        ] {
            assert_eq!(validate_model(&input(model, reasoning), &gateway).unwrap_err().code, code);
        }
        let plain = SharedCredential::from_api_settings(Some("https://gateway.example"), "token-one".into(), false, &[], soon()).unwrap();
        let provider = projected(&plain);
        assert!(names(&provider).contains(&"claude-sonnet-5"));
        assert_eq!(provider["models"][0]["sharedReasoning"], json!(EFFORTS));
    }

    #[test]
    fn account_revision_names_the_connected_folder_only() {
        let paths = |reference: &str| ProviderPaths {
            home: "/home".into(),
            codex_home: "/home/.claude".into(),
            credential_ref: reference.into(),
            shared: true,
        };
        let first = revision(&paths("claude-a")).unwrap();
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(first, revision(&paths("claude-a")).unwrap());
        assert_ne!(first, revision(&paths("claude-b")).unwrap());
        input("claude-opus-5", Some("medium")).validate().unwrap();
        hagency_core::project::identifier(ACCOUNT, 128).unwrap();
    }
}
