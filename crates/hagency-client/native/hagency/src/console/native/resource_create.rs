//! Retained POST /api/resources semantics without a local HTTP server. Native
//! consent creates another configuration on the same account, never more quota.
use super::{NativeError, NativeManagement, failure, management::store_error};
use hagency_store::{
    ResourceConfigurationResult,
    management::{CreateResourceConfiguration, ResourceDetail},
};
#[cfg(feature = "resource-adoption")]
use hagency_store::management::{Intent, IntentOperation, Permission};
use std::time::Instant;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum ResourceConfigurationReview {
    Available(Box<ResourceDetail>),
    Recorded(ResourceConfigurationResult),
}

pub(super) fn known_failure(error: &NativeError) -> bool {
    (error.status == 400 && error.code == "invalid_management_arguments")
        || (error.status == 409 && matches!(error.code.as_str(), "management_model_unqualified" | "management_resource_source_changed"))
}

impl NativeManagement {
    /// No intent, provider process, new resource or quota is produced by review.
    /// A recorded result is explicitly distinct from the current definition.
    pub async fn review_resource_configuration(
        &self,
        input: &CreateResourceConfiguration,
    ) -> Result<ResourceConfigurationReview, NativeError> {
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
            if let Some(recorded) = session
                .recover_resource_configuration(&context, input)
                .await
                .map_err(store_error)?
            {
                return self
                    .finish(
                        &context,
                        Ok(ResourceConfigurationReview::Recorded(recorded)),
                        false,
                    )
                    .await;
            }
            let detail = session
                .resource_detail(
                    &context,
                    hagency_store::management::ResourceDetailQuery {
                        id: input.source_resource_id.clone(),
                    },
                )
                .await
                .map_err(store_error)?;
            if detail.resource.revision != input.expected_revision {
                return Err(failure(409, "management_resource_source_changed"));
            }
            if let Some(policy) = input.execution_policy {
                policy.validate_for(&detail.resource.framework)
                    .map_err(|_| failure(400, "invalid_management_arguments"))?;
            }
            let (model, reasoning) = match &input.profile {
                hagency_store::ProfileChange::Preserve {} => {
                    (&detail.resource.model, &detail.resource.reasoning)
                }
                hagency_store::ProfileChange::Select { model, reasoning } => (model, reasoning),
            };
            if !detail
                .choices
                .iter()
                .any(|choice| &choice.model == model && &choice.reasoning == reasoning)
            {
                return Err(failure(409, "management_model_unqualified"));
            }
            self.finish(
                &context,
                Ok(ResourceConfigurationReview::Available(Box::new(detail))),
                false,
            )
            .await
        }
    }

    /// Called only by native consent or explicit recovery of the original
    /// operation. Creating a configuration does not assert fresh provider login.
    pub async fn create_resource_configuration(
        &self,
        input: CreateResourceConfiguration,
        confirmed_until: Instant,
    ) -> Result<ResourceConfigurationResult, NativeError> {
        input.validate().map_err(store_error)?;
        #[cfg(not(feature = "resource-adoption"))]
        {
            let _ = confirmed_until;
            Err(failure(409, "device_setup_required"))
        }
        #[cfg(feature = "resource-adoption")]
        {
            if confirmed_until <= Instant::now()
                || confirmed_until > Instant::now() + std::time::Duration::from_secs(120)
            {
                return Err(failure(403, "provider_confirmation_expired"));
            }
            let (context, session) = self.authorized().await?;
            session
                .authorize(&context, Permission::ConfigureResources)
                .map_err(store_error)?;
            session
                .resource(&context, input.source_resource_id.clone())
                .await
                .map_err(store_error)?;
            let intent = Intent::new(
                IntentOperation::CreateResourceConfiguration,
                input.command_id.clone(),
                &input,
            )
            .map_err(store_error)?;
            self.with_intent(
                intent,
                self.create_resource_configuration_now(input, confirmed_until),
            )
            .await
        }
    }

    #[cfg(feature = "resource-adoption")]
    async fn create_resource_configuration_now(
        &self,
        input: CreateResourceConfiguration,
        confirmed_until: Instant,
    ) -> Result<ResourceConfigurationResult, NativeError> {
        let (context, session) = self.authorized().await?;
        let result = session
            .create_resource_configuration(&context, input, confirmed_until)
            .await;
        self.finish(&context, result, true).await
    }
}

#[cfg(all(test, feature = "resource-adoption", feature = "test-support"))]
mod process_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn additional_configuration_keeps_uncertain_original_commands_pending() {
        for (status, code, terminal) in [
            (400, "invalid_management_arguments", true),
            (409, "management_model_unqualified", true),
            (409, "management_revision_conflict", false),
            (409, "management_resource_source_changed", true),
            (503, "management_resource_source_changed", false),
            (409, "management_command_conflict", false),
            (401, "management_scope_changed", false),
            (403, "provider_confirmation_expired", false),
            (503, "management_outcome_unknown", false),
            (503, "management_model_unqualified", false),
        ] {
            assert_eq!(
                known_failure(&failure(status, code)),
                terminal,
                "{status}/{code}"
            );
        }
    }
}
