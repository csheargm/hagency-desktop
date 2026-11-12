//! Wire reviews contain only display data and an opaque, expiring handle.
//! The original non-serializable native review never leaves daemon custody.
use super::client::ManagementClient;
use super::server::Bridge;
use super::{Operation, decode, encode, error};
use crate::native_owner::{self as native, NativeError, NativeManagement};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

pub enum Custody<T> {
    Local(T),
    Remote(String),
}
pub struct PersonalModelReview {
    pub summary: native::PersonalModelSummary,
    custody: Custody<native::PersonalModelReview>,
}
pub struct WebConnectionReview {
    pub summary: native::WebConnectionReviewSummary,
    custody: Custody<native::WebConnectionReview>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct RulesView {
    pub agent_id: String,
    pub expected_generation: i64,
    pub policy_revision: i64,
    pub tools: Vec<String>,
    pub directories: Vec<PathBuf>,
    pub can_edit_directories: bool,
}
pub struct PersonalRulesReview {
    view: RulesView,
    custody: Custody<native::PersonalRulesReview>,
}
impl std::ops::Deref for PersonalRulesReview {
    type Target = RulesView;
    fn deref(&self) -> &RulesView {
        &self.view
    }
}
impl RulesView {
    fn from_native(r: &native::PersonalRulesReview) -> Self {
        Self {
            agent_id: r.agent_id.clone(),
            expected_generation: r.expected_generation,
            policy_revision: r.policy_revision,
            tools: r.tools.clone(),
            directories: r.directories.clone(),
            can_edit_directories: r.can_edit_directories,
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Offered<T> {
    handle: String,
    view: T,
}
enum Retained {
    Model(native::PersonalModelReview),
    Rules(native::PersonalRulesReview),
    Web(native::WebConnectionReview),
}
#[derive(Default)]
pub(super) struct Reviews(HashMap<String, (String, Instant, Retained)>);
impl Reviews {
    pub(super) fn clear(&mut self) {
        self.0.clear();
    }
    fn insert(&mut self, manager: &str, review: Retained) -> Result<String, NativeError> {
        self.0.retain(|_, (_, until, _)| *until > Instant::now());
        if self.0.len() >= 256 {
            return Err(error(503, "daemon_review_limit"));
        }
        let handle = super::super::new_request_id();
        self.0.insert(
            handle.clone(),
            (
                manager.into(),
                Instant::now() + Duration::from_secs(120),
                review,
            ),
        );
        Ok(handle)
    }
    fn take(&mut self, manager: &str, handle: &str) -> Result<Retained, NativeError> {
        if !self
            .0
            .get(handle)
            .is_some_and(|(id, until, _)| id == manager && *until > Instant::now())
        {
            return Err(error(409, "daemon_review_expired"));
        }
        Ok(self.0.remove(handle).unwrap().2)
    }
}
fn wire_deadline(until: Instant) -> Result<u64, NativeError> {
    // A same-machine monotonic deadline, not a duration restarted by the
    // receiving daemon. Read the origin clock first so serialization latency
    // can only shorten the original confirmation window.
    let origin = monotonic_ms()?;
    let duration = until
        .checked_duration_since(Instant::now())
        .ok_or_else(|| error(403, "native_confirmation_expired"))?;
    // Native sheets may allow more time to review (resource edits use five
    // minutes). The IPC execution window is shorter; cap it rather than
    // rejecting a still-valid confirmation made promptly after opening.
    // This only shortens the caller's deadline and never restarts an expired one.
    let duration = duration.min(Duration::from_secs(120));
    origin.checked_add(duration.as_millis() as u64).ok_or_else(|| error(403,"native_confirmation_expired"))
}
fn deadline(ms: u64) -> Result<Instant, NativeError> {
    let origin = Instant::now();
    let left = ms.checked_sub(monotonic_ms()?).filter(|left| (1..=120000).contains(left))
        .ok_or_else(|| error(403,"native_confirmation_expired"))?;
    Ok(origin + Duration::from_millis(left))
}

fn monotonic_ms() -> Result<u64, NativeError> {
    #[cfg(unix)]
    {
        // One local timebase across processes, without wall-clock jumps.
        let time = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        if time.tv_sec < 0 || time.tv_nsec < 0 {
            return Err(error(503,"daemon_clock_unavailable"));
        }
        (time.tv_sec as u64).checked_mul(1000).and_then(|value| value.checked_add(time.tv_nsec as u64 / 1_000_000))
            .ok_or_else(|| error(503,"daemon_clock_unavailable"))
    }
    #[cfg(not(unix))]
    Err(error(503,"owner_daemon_platform_unsupported"))
}
#[derive(Clone, Serialize, Deserialize)]
pub struct AccountLoginAuthorization(String);
impl AccountLoginAuthorization {
    pub fn into_url(self) -> String {
        self.0
    }
}
impl ManagementClient {
    pub async fn review_personal_model(
        &self,
        input: &native::SavePersonalModel,
        selected_workspace: Option<PathBuf>,
    ) -> Result<PersonalModelReview, NativeError> {
        match self {
            Self::Local(m) => {
                let review = m.review_personal_model(input, selected_workspace).await?;
                Ok(PersonalModelReview {
                    summary: review.summary.clone(),
                    custody: Custody::Local(review),
                })
            }
            Self::Remote(m) => {
                let offered: Offered<native::PersonalModelSummary> = decode(
                    m.invoke(
                        Operation::review_personal_model,
                        (input, selected_workspace),
                    )
                    .await?,
                )?;
                Ok(PersonalModelReview {
                    summary: offered.view,
                    custody: Custody::Remote(offered.handle),
                })
            }
        }
    }
    pub async fn save_personal_model(
        &self,
        review: PersonalModelReview,
        confirmed_until: Instant,
    ) -> Result<Value, NativeError> {
        match (self, review.custody) {
            (Self::Local(m), Custody::Local(review)) => {
                m.save_personal_model(review, confirmed_until).await
            }
            (Self::Remote(m), Custody::Remote(handle)) => {
                m.invoke(
                    Operation::save_personal_model,
                    (handle, wire_deadline(confirmed_until)?),
                )
                .await
            }
            _ => Err(error(401, "management_scope_changed")),
        }
    }
    pub async fn review_personal_rules(
        &self,
        agent_id: String,
        expected_generation: i64,
    ) -> Result<PersonalRulesReview, NativeError> {
        match self {
            Self::Local(m) => {
                let review = m
                    .review_personal_rules(agent_id, expected_generation)
                    .await?;
                Ok(PersonalRulesReview {
                    view: RulesView::from_native(&review),
                    custody: Custody::Local(review),
                })
            }
            Self::Remote(m) => {
                let offered: Offered<RulesView> = decode(
                    m.invoke(
                        Operation::review_personal_rules,
                        (agent_id, expected_generation),
                    )
                    .await?,
                )?;
                Ok(PersonalRulesReview {
                    view: offered.view,
                    custody: Custody::Remote(offered.handle),
                })
            }
        }
    }
    pub async fn save_personal_rules(
        &self,
        review: PersonalRulesReview,
        command_id: String,
        tools: Vec<String>,
        directories: Vec<PathBuf>,
        confirmed_until: Instant,
    ) -> Result<Value, NativeError> {
        match (self, review.custody) {
            (Self::Local(m), Custody::Local(review)) => {
                m.save_personal_rules(review, command_id, tools, directories, confirmed_until)
                    .await
            }
            (Self::Remote(m), Custody::Remote(handle)) => {
                m.invoke(
                    Operation::save_personal_rules,
                    (
                        handle,
                        command_id,
                        tools,
                        directories,
                        wire_deadline(confirmed_until)?,
                    ),
                )
                .await
            }
            _ => Err(error(401, "management_scope_changed")),
        }
    }
    pub async fn review_web_connection(
        &self,
        input: native::WebConnectionEdit,
    ) -> Result<WebConnectionReview, NativeError> {
        match self {
            Self::Local(m) => {
                let review = m.review_web_connection(input).await?;
                Ok(WebConnectionReview {
                    summary: review.summary.clone(),
                    custody: Custody::Local(review),
                })
            }
            Self::Remote(m) => {
                let offered: Offered<native::WebConnectionReviewSummary> =
                    decode(m.invoke(Operation::review_web_connection, (input,)).await?)?;
                Ok(WebConnectionReview {
                    summary: offered.view,
                    custody: Custody::Remote(offered.handle),
                })
            }
        }
    }
    pub async fn review_web_connection_import(
        &self,
        command_id: String,
        expected_revision: u64,
        label: String,
        homeserver: String,
        raw: &str,
    ) -> Result<WebConnectionReview, NativeError> {
        match self {
            Self::Local(m) => {
                let review = m
                    .review_web_connection_import(
                        command_id,
                        expected_revision,
                        label,
                        homeserver,
                        raw,
                    )
                    .await?;
                Ok(WebConnectionReview {
                    summary: review.summary.clone(),
                    custody: Custody::Local(review),
                })
            }
            Self::Remote(m) => {
                let offered: Offered<native::WebConnectionReviewSummary> = decode(
                    m.invoke(
                        Operation::review_web_connection_import,
                        (command_id, expected_revision, label, homeserver, raw),
                    )
                    .await?,
                )?;
                Ok(WebConnectionReview {
                    summary: offered.view,
                    custody: Custody::Remote(offered.handle),
                })
            }
        }
    }
    pub async fn review_web_connection_credential(
        &self,
        command_id: String,
        id: String,
        expected_revision: u64,
        credential: Value,
    ) -> Result<WebConnectionReview, NativeError> {
        match self {
            Self::Local(m) => {
                let review = m
                    .review_web_connection_credential(command_id, id, expected_revision, credential)
                    .await?;
                Ok(WebConnectionReview {
                    summary: review.summary.clone(),
                    custody: Custody::Local(review),
                })
            }
            Self::Remote(m) => {
                let offered: Offered<native::WebConnectionReviewSummary> = decode(
                    m.invoke(
                        Operation::review_web_connection_credential,
                        (command_id, id, expected_revision, credential),
                    )
                    .await?,
                )?;
                Ok(WebConnectionReview {
                    summary: offered.view,
                    custody: Custody::Remote(offered.handle),
                })
            }
        }
    }
    pub async fn apply_web_connection(
        &self,
        review: WebConnectionReview,
        confirmed_until: Instant,
    ) -> Result<native::WebConnectionResult, NativeError> {
        match (self, review.custody) {
            (Self::Local(m), Custody::Local(review)) => {
                m.apply_web_connection(review, confirmed_until).await
            }
            (Self::Remote(m), Custody::Remote(handle)) => decode(
                m.invoke(
                    Operation::apply_web_connection,
                    (handle, wire_deadline(confirmed_until)?),
                )
                .await?,
            ),
            _ => Err(error(401, "management_scope_changed")),
        }
    }
    pub async fn download_web_connection_registration(
        &self,
        id: String,
        expected_revision: u64,
    ) -> Result<native::NativeConnectionRegistration, NativeError> {
        match self {
            Self::Local(m) => {
                m.download_web_connection_registration(id, expected_revision)
                    .await
            }
            Self::Remote(m) => {
                let (filename, bytes): (String, Vec<u8>) = decode(
                    m.invoke(
                        Operation::download_web_connection_registration,
                        (id, expected_revision),
                    )
                    .await?,
                )?;
                Ok(native::NativeConnectionRegistration { filename, bytes })
            }
        }
    }
    pub async fn account_login_authorization(
        &self,
        input: native::AccountLoginRequest,
    ) -> Result<AccountLoginAuthorization, NativeError> {
        match self {
            Self::Local(m) => Ok(AccountLoginAuthorization(
                m.account_login_authorization(input).await?.into_url(),
            )),
            Self::Remote(m) => decode(
                m.invoke(Operation::account_login_authorization, (input,))
                    .await?,
            ),
        }
    }
}
macro_rules! deadline_methods {
    ($($name:ident($input:ident:$ty:ty)->$out:ty;)*)=>{impl ManagementClient{$(pub async fn $name(&self,$input:$ty,confirmed_until:Instant)->Result<$out,NativeError>{match self{
        Self::Local(m)=>m.$name($input,confirmed_until).await,
        Self::Remote(m)=>decode(m.invoke(Operation::$name,($input,wire_deadline(confirmed_until)?)).await?)
    }})*}}
}
deadline_methods! {
    retire_account(input:native::RetireAccount)->native::AccountRetirement;
    create_resource_configuration(input:native::CreateResourceConfiguration)->native::ResourceConfigurationResult;
    prepare_personal_workspace(input:native::PreparePersonalWorkspace)->Value;
    edit_local_resource(input:native::EditLocalResource)->native::ResourceConfigurationResult;
    enroll_account_resource(input:native::NamespaceEnrollment)->hagency_store::owner_resources::OwnerResourceReceipt;
}
impl Bridge {
    fn offer<T: Serialize>(
        &self,
        manager: &str,
        value: Retained,
        view: T,
    ) -> Result<Value, NativeError> {
        let handle = self
            .reviews
            .lock()
            .map_err(|_| error(503, "daemon_bridge_unavailable"))?
            .insert(manager, value)?;
        encode(Offered { handle, view })
    }
    fn take(&self, manager: &str, handle: &str) -> Result<Retained, NativeError> {
        self.reviews
            .lock()
            .map_err(|_| error(503, "daemon_bridge_unavailable"))?
            .take(manager, handle)
    }
    pub(super) async fn special(
        &self,
        id: &str,
        m: &Arc<NativeManagement>,
        op: Operation,
        args: Value,
    ) -> Result<Value, NativeError> {
        match op {
            Operation::review_personal_model => {
                let (input, workspace): (native::SavePersonalModel, Option<PathBuf>) =
                    decode(args)?;
                let r = m.review_personal_model(&input, workspace).await?;
                let view = r.summary.clone();
                self.offer(id, Retained::Model(r), view)
            }
            Operation::save_personal_model => {
                let (handle, ms): (String, u64) = decode(args)?;
                let until = deadline(ms)?;
                let Retained::Model(r) = self.take(id, &handle)? else {
                    return Err(error(400, "daemon_review_kind_mismatch"));
                };
                m.save_personal_model(r, until).await
            }
            Operation::review_personal_rules => {
                let (agent, generation): (String, i64) = decode(args)?;
                let r = m.review_personal_rules(agent, generation).await?;
                let view = RulesView::from_native(&r);
                self.offer(id, Retained::Rules(r), view)
            }
            Operation::save_personal_rules => {
                let (handle, command, tools, dirs, ms): (
                    String,
                    String,
                    Vec<String>,
                    Vec<PathBuf>,
                    u64,
                ) = decode(args)?;
                let until = deadline(ms)?;
                let Retained::Rules(r) = self.take(id, &handle)? else {
                    return Err(error(400, "daemon_review_kind_mismatch"));
                };
                m.save_personal_rules(r, command, tools, dirs, until).await
            }
            Operation::review_web_connection => {
                let (input,): (native::WebConnectionEdit,) = decode(args)?;
                let r = m.review_web_connection(input).await?;
                let view = r.summary.clone();
                self.offer(id, Retained::Web(r), view)
            }
            Operation::review_web_connection_import => {
                let (command, revision, label, server, raw): (String, u64, String, String, String) =
                    decode(args)?;
                let r = m
                    .review_web_connection_import(command, revision, label, server, &raw)
                    .await?;
                let view = r.summary.clone();
                self.offer(id, Retained::Web(r), view)
            }
            Operation::review_web_connection_credential => {
                let (command, id_, revision, credential): (String, String, u64, Value) =
                    decode(args)?;
                let r = m
                    .review_web_connection_credential(command, id_, revision, credential)
                    .await?;
                let view = r.summary.clone();
                self.offer(id, Retained::Web(r), view)
            }
            Operation::apply_web_connection => {
                let (handle, ms): (String, u64) = decode(args)?;
                let until = deadline(ms)?;
                let Retained::Web(r) = self.take(id, &handle)? else {
                    return Err(error(400, "daemon_review_kind_mismatch"));
                };
                encode(m.apply_web_connection(r, until).await?)
            }
            Operation::download_web_connection_registration => {
                let (id, revision): (String, u64) = decode(args)?;
                let value = m.download_web_connection_registration(id, revision).await?;
                encode((value.filename, value.bytes))
            }
            Operation::account_login_authorization => {
                let (input,): (native::AccountLoginRequest,) = decode(args)?;
                encode(AccountLoginAuthorization(
                    m.account_login_authorization(input).await?.into_url(),
                ))
            }
            Operation::retire_account => {
                let (input, ms): (native::RetireAccount, u64) = decode(args)?;
                encode(m.retire_account(input, deadline(ms)?).await?)
            }
            Operation::create_resource_configuration => {
                let (input, ms): (native::CreateResourceConfiguration, u64) = decode(args)?;
                encode(
                    m.create_resource_configuration(input, deadline(ms)?)
                        .await?,
                )
            }
            Operation::prepare_personal_workspace => {
                let (input, ms): (native::PreparePersonalWorkspace, u64) = decode(args)?;
                m.prepare_personal_workspace(input, deadline(ms)?).await
            }
            Operation::edit_local_resource => {
                let (input, ms): (native::EditLocalResource, u64) = decode(args)?;
                encode(m.edit_local_resource(input, deadline(ms)?).await?)
            }
            Operation::enroll_account_resource => {
                let (input, ms): (native::NamespaceEnrollment, u64) = decode(args)?;
                encode(m.enroll_account_resource(input, deadline(ms)?).await?)
            }
            _ => Err(error(400, "invalid_management_operation")),
        }
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;
    #[test]
    fn long_review_window_is_capped_without_rejecting_fresh_confirmation() {
        let original = Instant::now() + Duration::from_secs(300);
        let before = monotonic_ms().unwrap();
        let wire = wire_deadline(original).unwrap();
        assert!(wire >= before + 119_000);
        assert!(wire <= monotonic_ms().unwrap() + 120_000);
        let received = deadline(wire).unwrap();
        assert!(received < original);
        assert!(received <= Instant::now() + Duration::from_secs(120));
    }
    #[test]
    fn expired_reviews_and_overlong_wire_deadlines_remain_rejected() {
        let expired = Instant::now() - Duration::from_secs(1);
        assert_eq!(wire_deadline(expired).unwrap_err().code, "native_confirmation_expired");
        assert_eq!(deadline(monotonic_ms().unwrap() + 300_000).unwrap_err().code, "native_confirmation_expired");
    }
    #[test]
    fn confirmation_deadline_is_not_restarted_after_dispatch_delay() {
        let original=Instant::now()+Duration::from_millis(20);
        let wire=wire_deadline(original).unwrap();
        let received=deadline(wire).unwrap();
        assert!(received<=original);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(deadline(wire).unwrap_err().code,"native_confirmation_expired");
    }
}
