//! Native execution handoff. The script selects only an original assignment;
//! the trusted panel alone holds and consumes a native prepared start.
use super::*;
pub(crate) const LIST: &str = "hagency.execution.assignments";
pub(crate) const REVIEW: &str = "hagency.execution.review";
pub(crate) const RESUME: &str = "hagency.execution.resume";

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Selection {
    assignment_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    after: Option<String>,
}
fn key(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
    {
        return Err("invalid_management_arguments".into());
    }
    Ok(())
}
impl Selection {
    pub(super) fn parse(args: Value) -> Result<Self, String> {
        let input: Self = parse(args)?;
        key(&input.assignment_id)?;
        Ok(input)
    }
}
// Synchronous package restriction only; the native Service retains the SDK,
// owner and device fences. No SDK mutex or IO is called inside the SQLite gate.
#[cfg(any(feature = "shared-successors", test))]
fn package_live(host: &Host, lease: &Lease, service: &str) -> bool {
    !host.0.retired.load(Ordering::Acquire)
        && host.0.identity == *lease.identity()
        && lease
            .authorize(&host.0.identity.account, service, None)
            .is_ok()
}
#[cfg(feature = "shared-successors")]
pub(crate) use enabled::{ExecutionFlow, ExecutionReview, ExecutionAssessment};
#[cfg(feature = "shared-successors")]
mod enabled {
    use super::*;
    use crate::hagency::backend::{
        Service,
        shared_successors::{PreparedSuccessorStart, SharedSuccessorRequest, SuccessorAssessment},
    };
    pub(crate) enum ExecutionAssessment {
        Ready(ExecutionReview),
        Blocked(::hagency::native_owner::SharedSuccessorBudgetReview),
    }
    pub(crate) struct ExecutionReview {
        pub(super) prepared: PreparedSuccessorStart,
    }
    impl ExecutionReview {
        pub(crate) fn review(&self) -> &::hagency::native_owner::SharedSuccessorReview {
            self.prepared.review()
        }
        pub(crate) fn expired(&self) -> bool {
            self.prepared.expired()
        }
    }
    #[derive(Clone)]
    pub(crate) struct ExecutionFlow {
        host: Host,
        lease: Lease,
        service: Service,
        selection: Selection,
        operation: &'static str,
    }
    impl ExecutionFlow {
        fn check(&self) -> Result<(), String> {
            let account = self.host.check(&self.lease)?;
            self.lease.authorize(&account, self.operation, None)
        }
        pub(crate) fn active(&self) -> bool {
            self.check().is_ok()
        }
        pub(crate) fn account(&self) -> &str {
            &self.lease.identity().account
        }
        pub(crate) fn assignment(&self) -> &str {
            &self.selection.assignment_id
        }
        pub(crate) fn can_resume(&self) -> bool {
            self.check().is_ok() && self.lease.authorize(self.account(), RESUME, None).is_ok()
        }
        pub(crate) fn is_resume(&self) -> bool {
            self.operation == RESUME
        }
        fn request(&self) -> SharedSuccessorRequest {
            SharedSuccessorRequest {
                assignment_id: self.selection.assignment_id.clone(),
            }
        }
        pub(crate) async fn prepare(&self) -> Result<ExecutionAssessment, String> {
            self.check()?;
            if self.is_resume() {
                return Err("native_execution_confirmation_required".into());
            }
            let prepared = self.service.assess_shared_successor(self.request(), self.lease.expires()).await?;
            self.check()?;
            let prepared = match prepared {
                SuccessorAssessment::Blocked(review) => {
                    return Ok(ExecutionAssessment::Blocked(review));
                }
                SuccessorAssessment::Ready(prepared) => prepared,
            };
            let host = self.host.clone();
            let lease = self.lease.clone();
            let prepared =
                prepared.restrict_to_host(Arc::new(move || package_live(&host, &lease, REVIEW)))?;
            Ok(ExecutionAssessment::Ready(ExecutionReview { prepared }))
        }
        pub(crate) async fn start(&self, review: ExecutionReview) -> Result<Value, String> {
            self.check()?;
            if self.is_resume() {
                return Err("native_execution_confirmation_required".into());
            }
            let context = review.review().clone();
            let result = self.service.start_shared_successor(review.prepared).await;
            self.check()
                .map_err(|_| "management_outcome_unknown".to_owned())?;
            let progress =
                serde_json::to_value(result?).map_err(|_| "management_outcome_unknown")?;
            Ok(
                json!({"state":"observed","assignmentId":context.assignment_id,"agentName":context.agent_name,"projectId":context.project_id,"progress":progress,"observedAtUtc":chrono::Utc::now().to_rfc3339()}),
            )
        }
        pub(crate) async fn resume(&self) -> Result<Value, String> {
            self.check()?;
            self.lease.authorize(self.account(), RESUME, None)?;
            let context = self
                .service
                .shared_successor_description(self.request())
                .await?;
            self.check()?;
            self.lease.authorize(self.account(), RESUME, None)?;
            let result = self.service.resume_shared_successor(self.request(), self.lease.expires()).await;
            self.check()
                .map_err(|_| "management_outcome_unknown".to_owned())?;
            let progress =
                serde_json::to_value(result?).map_err(|_| "management_outcome_unknown")?;
            Ok(
                json!({"state":"recovered","assignmentId":context.assignment_id,"agentName":context.agent_name,"projectId":context.project_id,"result":progress,"observedAtUtc":chrono::Utc::now().to_rfc3339()}),
            )
        }
    }
    impl Host {
        pub(crate) fn execution_handoff(
            &self,
            lease: Lease,
            operation: &str,
            args: Value,
        ) -> Result<ExecutionFlow, String> {
            let selection = Selection::parse(args)?;
            let operation = match operation {
                REVIEW => REVIEW,
                RESUME => RESUME,
                _ => return Err("unknown_management_service".into()),
            };
            let account = self.check(&lease)?;
            lease.authorize(&account, operation, None)?;
            let service = Service::for_current()?;
            Ok(ExecutionFlow {
                host: self.clone(),
                lease,
                service,
                selection,
                operation,
            })
        }
    }
}
impl Host {
    pub(super) async fn execution_assignments(
        &self,
        lease: &Lease,
        args: Value,
    ) -> Result<Value, String> {
        let query: Query = parse(args)?;
        if let Some(after) = &query.after {
            key(after)?
        }
        let account = self.check(lease)?;
        lease.authorize(&account, LIST, None)?;
        #[cfg(not(feature = "shared-successors"))]
        return Ok(
            json!({"supported":false,"reason":"shared_successors_disabled","items":[],"nextAfter":null}),
        );
        #[cfg(feature = "shared-successors")]
        {
            let service = crate::hagency::backend::Service::for_current()?;
            let page = service.list_shared_successors(query.after).await;
            self.check(lease)?;
            lease.authorize(&account, LIST, None)?;
            match page {
                Ok(page) => {
                    let mut v =
                        serde_json::to_value(page).map_err(|_| "invalid_management_response")?;
                    v["supported"] = json!(true);
                    v["observedAtUtc"] = json!(chrono::Utc::now().to_rfc3339());
                    rinx_miniapp_core::bounded_reply(v)
                }
                Err(code)
                    if matches!(
                        code.as_str(),
                        "shared_successors_disabled"
                            | "current_profile_required"
                            | "adopted_executor_device_reauthorization_required"
                    ) =>
                {
                    Ok(json!({"supported":false,"reason":code,"items":[],"nextAfter":null}))
                }
                Err(code) => Err(code),
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn execution_package_gate_rejects_retirement_stale_instance_and_missing_operation() {
        let authority = rinx_miniapp_core::SessionAuthority::default();
        let identity = InstanceId {
            app: octosense_app_contract::hagency::APP_ID.into(),
            account: "@owner:test".into(),
            room: None,
            generation: 7,
        };
        let lease = authority.issue(
            identity.clone(),
            [REVIEW.into()].into(),
            Default::default(),
            Instant::now() + Duration::from_secs(60),
        );
        let host = Host(Arc::new(Inner {
            digest: "a".repeat(64),
            identity: identity.clone(),
            epoch: 0,
            retired: AtomicBool::new(false),
            reauthentication_required: AtomicBool::new(false),
            initialising: Default::default(),
            manager: Default::default(),
            grant_access: Default::default(),
        }));
        assert!(package_live(&host, &lease, REVIEW));
        assert!(!package_live(&host, &lease, RESUME));
        let mut replacement = identity;
        replacement.generation += 1;
        let new_lease = authority.issue(
            replacement,
            [REVIEW.into()].into(),
            Default::default(),
            Instant::now() + Duration::from_secs(60),
        );
        assert!(!package_live(&host, &new_lease, REVIEW));
        host.retire();
        assert!(!package_live(&host, &lease, REVIEW));
    }
    #[test]
    fn execution_selection_is_closed_and_native_authority_is_not_script_granted() {
        assert!(Selection::parse(json!({"assignmentId":"assignment_1"})).is_ok());
        for field in [
            "consent",
            "startIntentId",
            "commandId",
            "provider",
            "path",
            "budget",
            "owner",
            "processAttempt",
        ] {
            let mut v = json!({"assignmentId":"assignment_1"});
            v[field] = json!(true);
            assert!(Selection::parse(v).is_err());
        }
        for id in ["", "../foreign", "with space"] {
            assert!(Selection::parse(json!({"assignmentId":id})).is_err())
        }
        assert!(parse::<Query>(json!({"limit":1000})).is_err());
        let authority = rinx_miniapp_core::SessionAuthority::default();
        let lease = authority.issue(
            InstanceId {
                app: octosense_app_contract::hagency::APP_ID.into(),
                account: "@owner:test".into(),
                room: None,
                generation: 1,
            },
            [LIST.into(), REVIEW.into(), RESUME.into()].into(),
            Default::default(),
            Instant::now() + Duration::from_secs(30),
        );
        assert!(super::super::permissions(&lease).is_empty());
        for cap in [LIST, REVIEW, RESUME] {
            assert!(octosense_app_contract::hagency::SERVICES.contains(&cap));
            assert!(lease.authorize("@other:test", cap, None).is_err());
        }
        authority.invalidate();
        assert!(lease.authorize("@owner:test", REVIEW, None).is_err());
    }
}
