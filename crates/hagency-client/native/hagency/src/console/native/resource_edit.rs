//! Native-confirmed local edits, with fresh original-namespace metadata and
//! immutable command recovery. No inference or legacy configuration dispatch.
use super::{NativeManagement, NativeError, failure, management::store_error};
use hagency_store::{
    ResourceConfigurationResult,
    management::{EditLocalResource, Intent, IntentOperation, Permission},
};
use std::time::Instant;

/// Native-only review. A recorded original result is not the current resource
/// definition and must be presented as recovery, never another configuration.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum LocalResourceEditReview {
    Editable(Box<hagency_store::management::ResourceDetail>),
    Recorded(ResourceConfigurationResult),
    /// A ceiling-only request (`expectedCeilingSequence`) on a resource that
    /// was offered, published or retains commitments: an append-only
    /// amendment of its monthly maximum. The definition, its server admission,
    /// running Agents and the live per-request limit are unchanged.
    CeilingAmendment(CeilingAmendmentReview),
}

/// Exactly what the native confirmation shows and the writer re-checks.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CeilingAmendmentReview {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar: Option<hagency_core::recurring_quota::Calendar>,
    pub resource_id: String,
    /// The owner's local name for the resource, when one is set.
    pub name: Option<String>,
    /// The definition revision; an amendment leaves it unchanged.
    pub revision: String,
    /// The amendment sequence the request expects: the current one.
    pub sequence: u64,
    /// The monthly maximum in force now.
    pub previous_tokens: u64,
    /// The proposed monthly maximum.
    pub tokens: u64,
    /// The live grant's per-request limit, which this amendment never changes.
    pub per_request_tokens: Option<u64>,
}

/// The review checks the writer repeats atomically, so a refusal is shown
/// before the owner confirms anything.
#[cfg(feature = "resource-adoption")]
fn ceiling_amendment_review(
    input: &EditLocalResource,
    detail: &hagency_store::management::ResourceDetail,
) -> Result<CeilingAmendmentReview, NativeError> {
    let current = detail.ceiling.as_ref().and_then(|c| c.tokens).map(u64::from);
    let (previous_tokens, tokens) = checked_amendment(input, &detail.ceiling_amendment, current)?;
    Ok(CeilingAmendmentReview {
        calendar: detail.calendar.clone(),
        resource_id: detail.resource.id.clone(),
        name: detail
            .resource
            .local_name
            .as_ref()
            .map(|n| n.name.clone())
            .filter(|n| !n.is_empty()),
        revision: detail.resource.revision.clone(),
        sequence: detail.ceiling_amendment.sequence,
        previous_tokens,
        tokens,
        per_request_tokens: detail.ceiling_amendment.per_request_tokens,
    })
}

/// `(previous, proposed)` monthly maximum when the request passes review:
/// the reviewed sequence is current, the resource can be amended, and the
/// proposal is neither a clear, nor unchanged, nor below this month's floor.
#[cfg(any(feature = "resource-adoption", test))]
fn checked_amendment(
    input: &EditLocalResource,
    amendment: &hagency_store::management::CeilingAmendment,
    current: Option<u64>,
) -> Result<(u64, u64), NativeError> {
    if input.expected_ceiling_sequence != Some(amendment.sequence) {
        return Err(failure(409, "management_resource_edit_changed"));
    }
    if !amendment.allowed {
        return Err(failure(409, "management_ceiling_amendment_not_available"));
    }
    let previous =
        current.ok_or_else(|| failure(409, "management_ceiling_amendment_not_available"))?;
    let tokens = match &input.ceiling {
        hagency_store::CeilingChange::Monthly { tokens } => u64::from(*tokens),
        hagency_store::CeilingChange::Clear {} => {
            return Err(failure(409, "management_ceiling_clear_refused"));
        }
        hagency_store::CeilingChange::Preserve {} => {
            return Err(failure(400, "invalid_management_arguments"));
        }
    };
    if tokens == previous {
        return Err(failure(409, "management_ceiling_unchanged"));
    }
    if tokens < previous {
        match amendment.minimum_tokens {
            None => return Err(failure(409, "management_ceiling_usage_unsettled")),
            Some(minimum) if tokens < minimum => {
                return Err(failure(409, "management_ceiling_below_commitments"));
            }
            Some(_) => {}
        }
    }
    Ok((previous, tokens))
}

pub(super) fn known_failure(error: &NativeError) -> bool {
    (error.status == 400 && error.code == "invalid_management_arguments")
        || (error.status == 409
            && matches!(
                error.code.as_str(),
                "management_resource_edit_changed"
                    | "management_resource_edit_not_available"
                    | "management_model_unqualified"
                    | "provider_model_changed"
                    | "resource_namespace_required"
                    | "provider_connection_required"
                    | "management_ceiling_amendment_not_available"
                    | "management_ceiling_clear_refused"
                    | "management_ceiling_unchanged"
                    | "management_ceiling_below_commitments"
                    | "management_ceiling_usage_unsettled"
            ))
}

impl NativeManagement {
    /// Pure readback before showing native consent. Creating or accepting an
    /// intent is deliberately excluded; cancelling a review has no write.
    pub async fn review_local_resource_edit(
        &self,
        input: &EditLocalResource,
    ) -> Result<LocalResourceEditReview, NativeError> {
        input.validate().map_err(store_error)?;
        #[cfg(not(feature = "resource-adoption"))]
        {
            Err(failure(409, "device_setup_required"))
        }
        #[cfg(feature = "resource-adoption")]
        {
            let (context, session) = self.authorized().await?;
            session
                .local_resource_editor_registry(&context)
                .map_err(store_error)?;
            if let Some(saved) = session
                .recover_local_resource_edit(&context, input)
                .await
                .map_err(store_error)?
            {
                return self
                    .finish(
                        &context,
                        Ok(LocalResourceEditReview::Recorded(saved)),
                        false,
                    )
                    .await;
            }
            let detail = session
                .resource_detail(
                    &context,
                    hagency_store::management::ResourceDetailQuery {
                        id: input.resource_id.clone(),
                    },
                )
                .await
                .map_err(store_error)?;
            if detail.resource.revision != input.expected_revision {
                return Err(failure(409, "management_resource_edit_changed"));
            }
            if input.is_ceiling_amendment() {
                let review = ceiling_amendment_review(input, &detail)?;
                return self
                    .finish(
                        &context,
                        Ok(LocalResourceEditReview::CeilingAmendment(review)),
                        false,
                    )
                    .await;
            }
            if let Some(policy) = input.execution_policy {
                policy.validate_for(&detail.resource.framework)
                    .map_err(|_| failure(400, "invalid_management_arguments"))?;
            }
            if !detail.editing.allowed {
                return Err(failure(409, "management_resource_edit_not_available"));
            }
            if let hagency_store::ProfileChange::Select { model, reasoning } = &input.profile
                && !detail
                    .choices
                    .iter()
                    .any(|choice| &choice.model == model && &choice.reasoning == reasoning)
            {
                return Err(failure(409, "management_model_unqualified"));
            }
            self.finish(
                &context,
                Ok(LocalResourceEditReview::Editable(Box::new(detail))),
                false,
            )
            .await
        }
    }

    /// Only a trusted native review or explicit original-command recovery calls
    /// this method. The mini-app has no generic configuration writer.
    pub async fn edit_local_resource(
        &self,
        input: EditLocalResource,
        confirmed_until: Instant,
    ) -> Result<ResourceConfigurationResult, NativeError> {
        input.validate().map_err(store_error)?;
        if Instant::now() >= confirmed_until {
            return Err(failure(403, "provider_confirmation_expired"));
        }
        let (context, session) = self.authorized().await?;
        session
            .authorize(&context, Permission::ConfigureResources)
            .map_err(store_error)?;
        session
            .resource(&context, input.resource_id.clone())
            .await
            .map_err(store_error)?;
        let intent = Intent::new(
            IntentOperation::EditLocalResource,
            input.command_id.clone(),
            &input,
        )
        .map_err(store_error)?;
        self.with_intent(intent, self.edit_local_resource_now(input, confirmed_until))
            .await
    }
    async fn edit_local_resource_now(
        &self,
        input: EditLocalResource,
        confirmed_until: Instant,
    ) -> Result<ResourceConfigurationResult, NativeError> {
        #[cfg(not(feature = "resource-adoption"))]
        {
            let _ = (input, confirmed_until);
            Err(failure(409, "device_setup_required"))
        }
        #[cfg(feature = "resource-adoption")]
        {
            let _guard = self.owner.0.calls.lock().await;
            let (context, session) = self.authorized().await?;
            if let Some(result) = session
                .recover_local_resource_edit(&context, &input)
                .await
                .map_err(store_error)?
            {
                return self.finish(&context, Ok(result), false).await;
            }
            // A monthly-maximum amendment needs no provider: the definition,
            // its model and its account namespace are all left unchanged.
            if input.is_ceiling_amendment() {
                if Instant::now() >= confirmed_until {
                    return Err(failure(403, "provider_confirmation_expired"));
                }
                let deadline = session
                    .valid_until()
                    .min(confirmed_until)
                    .min(Instant::now() + std::time::Duration::from_secs(30));
                let result = session
                    .amend_resource_ceiling(&context, input, deadline)
                    .await;
                return self.finish(&context, result, true).await;
            }
            let (registry, profile) = session
                .local_resource_editor_registry(&context)
                .map_err(store_error)?;
            let current = session
                .resource(&context, input.resource_id.clone())
                .await
                .map_err(store_error)?;
            if current.revision != input.expected_revision {
                return Err(failure(409, "management_resource_edit_changed"));
            }
            let namespace = registry
                .resource_namespace(
                    profile.clone(),
                    input.resource_id.clone(),
                    input.expected_revision.clone(),
                )
                .await
                .map_err(store_error)?
                .ok_or_else(|| failure(409, "resource_namespace_required"))?;
            namespace.check().await.map_err(store_error)?;
            let deadline = session
                .valid_until()
                .min(confirmed_until)
                .min(Instant::now() + std::time::Duration::from_secs(30));
            if Instant::now() >= deadline {
                return Err(failure(403, "provider_confirmation_expired"));
            }
            let observed = self
                .observe_account_namespace(namespace.launch(), deadline)
                .await;
            let (fresh, live) = self.authorized().await?;
            if fresh != context {
                return Err(failure(401, "management_scope_changed"));
            }
            live.authorize(&context, Permission::ConfigureResources)
                .map_err(store_error)?;
            namespace.check().await.map_err(store_error)?;
            let (provider, catalog) = observed?;
            let (model, reasoning) = match &input.profile {
                hagency_store::ProfileChange::Preserve {} => (&current.model, &current.reasoning),
                hagency_store::ProfileChange::Select { model, reasoning } => (model, reasoning),
            };
            validate_model(
                &current.framework,
                model,
                reasoning,
                &serde_json::json!(catalog),
            )?;
            let verified = registry
                .verify_provider_resource(profile, input.resource_id.clone(), provider)
                .await
                .map_err(store_error)?;
            let binding =
                hagency_store::owner_resources::ResourceEditBinding::new(verified, Some(namespace))
                    .map_err(store_error)?;
            if Instant::now() >= deadline {
                return Err(failure(403, "provider_confirmation_expired"));
            }
            let result = live.edit_local_resource(&context, input, binding).await;
            self.finish(&context, result, true).await
        }
    }
}

#[cfg(any(feature = "resource-adoption", test))]
fn validate_model(
    runtime: &str,
    model: &str,
    reasoning: &Option<String>,
    catalog: &serde_json::Value,
) -> Result<(), NativeError> {
    let projected = super::resource_onboarding::projection(runtime, catalog);
    let row = projected["models"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["model"].as_str() == Some(model)))
        .ok_or_else(|| failure(409, "provider_model_changed"))?;
    if !row["sharedReasoning"]
        .as_array()
        .is_some_and(|values| values.contains(&serde_json::json!(reasoning)))
    {
        return Err(failure(409, "management_model_unqualified"));
    }
    Ok(())
}

#[cfg(all(test, unix, feature="resource-adoption", feature="test-support"))]
#[path="resource_model_acceptance_tests.rs"]
mod process_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_editor_model_validation_uses_fresh_names_and_reasoning() {
        let catalog = serde_json::json!({"models":[{"id":"coding-alias","model":"gpt-5.6-sol","supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"unsupported"}]}]});
        validate_model("codex", "gpt-5.6-sol", &Some("medium".into()), &catalog).unwrap();
        assert_eq!(
            validate_model("codex", "coding-alias", &Some("medium".into()), &catalog)
                .unwrap_err()
                .code,
            "provider_model_changed"
        );
        assert_eq!(
            validate_model(
                "codex",
                "gpt-5.6-sol",
                &Some("unsupported".into()),
                &catalog
            )
            .unwrap_err()
            .code,
            "management_model_unqualified"
        );
        assert!(validate_model("codex", "gpt-5.6-sol", &None, &catalog).is_err());
        assert!(
            validate_model(
                "codex",
                "gpt-5.6-sol",
                &Some("medium".into()),
                &serde_json::json!({"models":[]})
            )
            .is_err()
        );
    }
    #[test]
    fn ceiling_amendment_review_refuses_before_native_confirmation() {
        use hagency_store::management::{CeilingAmendment, CeilingAmendmentBlocker};
        use hagency_store::{CeilingChange, ProfileChange};
        let state = |allowed: bool, minimum: Option<u64>| CeilingAmendment {
            allowed,
            reason: (!allowed).then_some(CeilingAmendmentBlocker::NotOffered),
            sequence: 2,
            minimum_tokens: minimum,
            per_request_tokens: Some(700),
        };
        let request = |sequence: Option<u64>, ceiling: CeilingChange| EditLocalResource {
            command_id: "amend".into(),
            resource_id: "resource_1".into(),
            expected_revision: "a".repeat(64),
            profile: ProfileChange::Preserve {},
            ceiling,
            execution_policy: None,
            expected_ceiling_sequence: sequence,
        };
        let monthly = |tokens: u64| CeilingChange::Monthly {
            tokens: tokens.try_into().unwrap(),
        };
        let code = |input: EditLocalResource, state: CeilingAmendment, current: Option<u64>| {
            checked_amendment(&input, &state, current).map_err(|e| e.code)
        };
        // Raising and lowering to the floor pass; the floor bounds only lowering.
        assert_eq!(code(request(Some(2), monthly(1500)), state(true, Some(800)), Some(1000)), Ok((1000, 1500)));
        assert_eq!(code(request(Some(2), monthly(800)), state(true, Some(800)), Some(1000)), Ok((1000, 800)));
        assert_eq!(code(request(Some(2), monthly(1500)), state(true, None), Some(1000)), Ok((1000, 1500)));
        assert_eq!(code(request(Some(2), monthly(1100)), state(true, Some(1200)), Some(1000)), Ok((1000, 1100)));
        for (input, state, current, expected) in [
            (request(Some(2), monthly(799)), state(true, Some(800)), Some(1000), "management_ceiling_below_commitments"),
            (request(Some(2), monthly(999)), state(true, None), Some(1000), "management_ceiling_usage_unsettled"),
            (request(Some(2), CeilingChange::Clear {}), state(true, Some(800)), Some(1000), "management_ceiling_clear_refused"),
            (request(Some(2), monthly(1000)), state(true, Some(800)), Some(1000), "management_ceiling_unchanged"),
            (request(Some(1), monthly(1500)), state(true, Some(800)), Some(1000), "management_resource_edit_changed"),
            (request(None, monthly(1500)), state(true, Some(800)), Some(1000), "management_resource_edit_changed"),
            (request(Some(2), monthly(1500)), state(false, None), Some(1000), "management_ceiling_amendment_not_available"),
            (request(Some(2), monthly(1500)), state(true, Some(800)), None, "management_ceiling_amendment_not_available"),
        ] {
            assert_eq!(code(input, state, current), Err(expected.to_owned()), "{expected}");
            assert!(known_failure(&failure(409, expected)), "{expected} settles its intent");
        }
    }
    #[test]
    fn local_editor_known_failures_never_settle_uncertain_or_scope_errors() {
        for (status, code, terminal) in [
            (409, "management_resource_edit_changed", true),
            (409, "management_resource_edit_not_available", true),
            (409, "provider_model_changed", true),
            (400, "invalid_management_arguments", true),
            (409, "management_revision_conflict", false),
            (409, "management_outcome_unknown", false),
            (401, "management_scope_changed", false),
            (503, "provider_model_changed", false),
        ] {
            assert_eq!(
                known_failure(&failure(status, code)),
                terminal,
                "{status}/{code}"
            );
        }
    }
}
