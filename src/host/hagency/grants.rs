//! Native owner-budget handoff. No script receives ConsentGrants, a registry,
//! provider identity, private receipt, or caller-selected original command IDs.
use super::{Host, Lease, parse};
use ::hagency::native_owner::{
    OwnerGrantChoices, OwnerGrantProgress, OwnerGrantPublicationProgress, OwnerGrantState, OwnerGrantForm, OwnerGrantSummary, OwnerGrantSuccessionForm,
    OwnerGrantSuccessionKind, OwnerGrantSuccessionState,
};
use ::hagency::daemon::bridge::{OwnerGrantAccess, OwnerGrantReview, OwnerGrantReviewResult, OwnerGrantPublicationReview, OwnerGrantPublicationResult, OwnerGrantCancellationReview, OwnerGrantCancellationResult};
use serde::Deserialize;
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

pub(crate) const REVIEW_GRANT: &str = "hagency.grants.review";
/// Read-only: who may request from a pool this month, from the native record.
pub(crate) const APPLICANTS: &str = "hagency.grants.applicants";
/// Read-only: what running Agents hold in a pool's published month, from the
/// native amendment form: the per-request floor the window enforces.
pub(crate) const FLOOR: &str = "hagency.grants.floor";
/// Largest grant amount, in tokens, that stays an exact JSON integer.
pub(crate) const MAX_GRANT_TOKENS: u64 = 9_007_199_254_740_991;

/// A change to the pool's published grant that a script may ask the native
/// panel to prepare. Only the native form commits it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Succession {
    /// The same UTC month: a new per-request limit from now on.
    Amendment,
    /// Next UTC month, scheduled in the last days of this one.
    Renewal,
}
impl Succession {
    pub(crate) fn native(self) -> OwnerGrantSuccessionKind {
        match self {
            Self::Amendment => OwnerGrantSuccessionKind::Amendment,
            Self::Renewal => OwnerGrantSuccessionKind::Renewal,
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Selector {
    contribution_id: Option<String>,
    intent_id: Option<String>,
    /// A script may only propose an amount for a new grant on this pool. It
    /// fills the native tokens field once; prepare never reads it.
    proposed_tokens: Option<u64>,
    proposed_per_request_tokens: Option<u64>,
    /// Open the native form for an amendment or a renewal of the pool's
    /// published grant instead of a new grant.
    succession: Option<Succession>,
    /// Agent applicants a script proposes. They only pre-select the native
    /// applicants field; the owner still confirms them natively.
    applicants: Option<Vec<String>>,
    /// Open the people picker. This is presentation only, never permission.
    #[serde(default)]
    choose_people: bool,
}
/// A complete Matrix user ID (`@name:server`), as the native form accepts.
pub(crate) fn matrix_user(id: &str) -> bool {
    id.len() <= 255
        && !id.chars().any(|c| c.is_whitespace() || c.is_control())
        && id.split_once(':').is_some_and(|(name, server)| {
            name.starts_with('@') && name.len() > 1 && !server.is_empty()
        })
}
fn server_of(id: &str) -> Option<&str> {
    id.split_once(':').map(|(_, server)| server)
}
/// A pool or record ID a script may name: 1-128 ASCII letters, digits, `_` or `-`.
fn grant_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
impl Selector {
    fn parse(value: Value) -> Result<Self, String> {
        let mut value: Self = parse(value)?;
        let keys: Vec<_> = [&value.contribution_id, &value.intent_id]
            .into_iter()
            .flatten()
            .collect();
        if keys.len() != 1
            || keys.iter().any(|key| !grant_key(key))
            || value.proposed_tokens.is_some_and(|tokens| {
                value.contribution_id.is_none() || tokens == 0 || tokens > MAX_GRANT_TOKENS
            })
            || value.proposed_per_request_tokens.is_some_and(|n| value.contribution_id.is_none() || n == 0 || n > MAX_GRANT_TOKENS)
            || (value.succession.is_some() && value.contribution_id.is_none())
            || (value.choose_people && value.contribution_id.is_none())
        {
            return Err("invalid_management_arguments".into());
        }
        if let Some(applicants) = &mut value.applicants {
            applicants.sort();
            applicants.dedup();
            if value.contribution_id.is_none()
                || applicants.is_empty()
                || applicants.len() > 64
                || !applicants.iter().all(|id| matrix_user(id))
            {
                return Err("invalid_management_arguments".into());
            }
        }
        Ok(value)
    }
    /// Applicants must be selectable by the native form: users of the
    /// owner's own homeserver, the only ones a grant may name.
    fn check_applicants(&self, owner: &str) -> Result<(), String> {
        let server = server_of(owner).ok_or("invalid_management_arguments")?;
        if self
            .applicants
            .iter()
            .flatten()
            .any(|id| server_of(id) != Some(server))
        {
            return Err("invalid_management_arguments".into());
        }
        Ok(())
    }
}

/// Strings read from native widgets only. This is intentionally not Deserialize.
/// A script proposal can only pre-fill the native tokens field; these values
/// are always read back from the widgets the owner sees before committing.
#[derive(Clone, Default)]
pub(crate) struct GrantChoices {
    pub coordinator: String,
    pub managers: String,
    pub tokens: String,
    pub per_request: String,
    pub expires_utc: String,
    pub self_approval: bool,
    pub calendar: Option<hagency_core::recurring_quota::Calendar>,
    pub renewal_mode: Option<hagency_core::recurring_quota::RenewalMode>,
}
impl GrantChoices {
    pub fn parse(self) -> Result<OwnerGrantChoices, String> {
        let amount = self
            .tokens
            .trim()
            .parse::<u64>()
            .map_err(|_| "invalid_grant_budget")?;
        let per_request_tokens = if self.per_request.trim().is_empty() { None } else {
            let n = self.per_request.trim().parse::<u64>().map_err(|_| "invalid_grant_request_limit")?;
            if n == 0 || n > amount { return Err("invalid_grant_request_limit".into()); }
            Some(n)
        };
        let expiry = match &self.calendar {
            Some(calendar) => calendar.period_at(now_ms()).map(|period| chrono::DateTime::from_timestamp_millis(period.end_ms as i64).unwrap().to_rfc3339()).map_err(|_| "invalid_grant_calendar")?,
            None => self.expires_utc.clone(),
        };
        let until = chrono::DateTime::parse_from_rfc3339(expiry.trim())
            .map_err(|_| "invalid_grant_expiry")?;
        let now = chrono::Utc::now().timestamp_millis();
        if until.offset().local_minus_utc() != 0
            || until.timestamp_millis() <= now
            || until.timestamp_millis() > now.saturating_add(366 * 86_400_000)
        {
            return Err("invalid_grant_expiry".into());
        }
        let mut managers: Vec<String> = self
            .managers
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        managers.sort();
        managers.dedup();
        if amount == 0
            || amount > MAX_GRANT_TOKENS
            || managers.is_empty()
            || managers.len() > 64
        {
            return Err("invalid_grant_budget".into());
        }
        let coordinator = self.coordinator.trim().to_owned();
        if std::iter::once(&coordinator)
            .chain(&managers)
            .any(|id| !matrix_user(id))
        {
            return Err("invalid_grant_manager".into());
        }
        Ok(OwnerGrantChoices { renewal_mode: self.calendar.as_ref().map(|_| self.renewal_mode.unwrap_or(hagency_core::recurring_quota::RenewalMode::Automatic)), per_request_tokens: self.calendar.as_ref().map(|_| per_request_tokens.unwrap_or(amount)).or(per_request_tokens),
            coordinator_mxid: coordinator,
            delegation_expires_at_ms: until.timestamp_millis() as u64,
            allow_self_approval: self.self_approval,
            eligible_managers: managers,
            allocated_tokens: amount,
            calendar: self.calendar,
        })
    }
}

pub(crate) enum GrantResult {
    Choose(OwnerGrantForm),
    /// Terms for an amendment or renewal of the published grant.
    ChooseSuccessor(Box<OwnerGrantSuccessionForm>),
    Review(Box<OwnerGrantReview>),
    Progress(Box<OwnerGrantProgress>),
    PublicationReview(Box<OwnerGrantPublicationReview>),
    PublicationProgress(Box<OwnerGrantPublicationProgress>),
    CancellationReview(Box<OwnerGrantCancellationReview>),
}
pub(crate) fn now_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0)
}
/// A grant's UTC month has ended. Its receipt stays history; the server and the
/// native ledger accept one new grant for the same admission in a later month.
pub(crate) fn month_ended(summary: &OwnerGrantSummary, now: u64) -> bool {
    summary.period_end_ms <= now
}
/// A successor original that still names its predecessor: anything but a
/// cancelled review or a successor the native ledger rejected or cancelled.
fn live_successor(record: &OwnerGrantProgress) -> bool {
    record.summary.succession.is_some()
        && !matches!(record.state, OwnerGrantState::Declined)
        && !matches!(
            record.succession_state,
            Some(OwnerGrantSuccessionState::Rejected | OwnerGrantSuccessionState::Cancelled)
        )
}
/// A successor that still needs the owner: an unattempted or unknown review,
/// or a recorded change that the server has not been seen to accept.
pub(crate) fn unfinished_successor(record: &OwnerGrantProgress) -> bool {
    live_successor(record)
        && !(matches!(record.state, OwnerGrantState::Recorded)
            && matches!(record.succession_state, Some(OwnerGrantSuccessionState::Effective)))
}
/// `hagency.grants.applicants`: one pool, named by its contribution. Nothing
/// else a script sends can widen or steer the read.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ApplicantsQuery {
    contribution_id: String,
}
impl ApplicantsQuery {
    pub(crate) fn parse(value: Value) -> Result<Self, String> {
        let query: Self = parse(value)?;
        if !grant_key(&query.contribution_id) {
            return Err("invalid_management_arguments".into());
        }
        Ok(query)
    }
}
/// This UTC month's grant in force for one pool, among its native records in
/// journal (creation) order: recorded, not closed, in its month now, a root or
/// a successor the native ledger marked effective, and not replaced by an
/// effective amendment. A pending change or next month's scheduled renewal
/// never stands in for it; the latest such record wins.
pub(crate) fn effective_original(
    records: &[OwnerGrantProgress],
    now: u64,
) -> Option<&OwnerGrantProgress> {
    let in_force = |record: &OwnerGrantProgress| {
        matches!(record.state, OwnerGrantState::Recorded)
            && record.closure.is_none()
            && matches!(
                record.succession_state,
                None | Some(OwnerGrantSuccessionState::Effective)
            )
    };
    let replaced: std::collections::BTreeSet<&str> = records
        .iter()
        .filter(|record| in_force(record) && record.summary.period_start_ms <= now)
        .filter_map(|record| record.summary.succession.as_ref())
        .map(|succession| succession.predecessor_grant_id.as_str())
        .collect();
    records.iter().rev().find(|record| {
        in_force(record)
            && record.summary.period_start_ms <= now
            && now < record.summary.period_end_ms
            && !replaced.contains(record.summary.grant_id.as_str())
    })
}
/// The page's read of this month's list. `state` says whether the server has
/// it: `published`, `unconfirmed` (not known yet), `unpublished` (recorded on
/// this computer only, or refused) or `none` (nothing in force this month).
fn applicants_reply(
    contribution: &str,
    current: Option<&OwnerGrantProgress>,
    state: &str,
    change_pending: bool,
) -> Value {
    let summary = current.map(|record| &record.summary);
    let mut applicants = summary.map(|s| s.eligible_managers.clone()).unwrap_or_default();
    applicants.sort();
    applicants.dedup();
    serde_json::json!({
        "contributionId": contribution,
        "state": if summary.is_some() { state } else { "none" },
        "periodKey": summary.map(|s| s.period_key.clone()),
        "periodStartMs": summary.map(|s| s.period_start_ms),
        "periodEndMs": summary.map(|s| s.period_end_ms),
        "grantId": summary.map(|s| s.grant_id.clone()),
        "applicants": applicants,
        "changePending": change_pending,
    })
}
/// The page's read of a published month's floor, in the amendment window's
/// own figures: what running Agents hold (and how many), the per-request limit
/// in force, and the most a new limit can be.
fn floor_reply(contribution: &str, form: &OwnerGrantSuccessionForm) -> Value {
    serde_json::json!({
        "contributionId": contribution,
        "heldTokens": form.floor.retained_tokens,
        "heldAgents": form.floor.children,
        "perRequestTokens": form.predecessor.per_request_tokens.unwrap_or(form.predecessor.allocated_tokens),
        "mostTokens": form.context.remaining_tokens,
        "periodEndMs": form.period_end_ms,
    })
}
/// What the server was last seen to hold for a recorded root grant.
fn publication_state(publication: Option<&OwnerGrantPublicationProgress>) -> &'static str {
    use ::hagency::native_owner::OwnerGrantPublicationState as Seen;
    match publication {
        None => "unpublished",
        Some(p) => match (&p.state, p.server_state.as_deref()) {
            (Seen::ServerObserved, Some("active")) => "published",
            (Seen::ServerObserved, Some("revoked" | "expired")) | (Seen::Rejected, _) => "unpublished",
            _ => "unconfirmed",
        },
    }
}
// Journal order is creation order. Cancelled, canonically closed, ended or
// succeeded originals remain in history without hiding a pending, unknown,
// active or scheduled original. A grant is succeeded once a live successor
// names it: an amendment that replaced it, or next month's renewal.
fn existing_progress(
    records: Vec<OwnerGrantProgress>,
    now: u64,
) -> Result<Option<OwnerGrantProgress>, String> {
    let succeeded: std::collections::BTreeSet<String> = records
        .iter()
        .filter(|record| live_successor(record))
        .filter_map(|record| record.summary.succession.as_ref())
        .map(|succession| succession.predecessor_grant_id.clone())
        .collect();
    let mut pending = None;
    let mut declined = None;
    for record in records {
        if matches!(record.state, OwnerGrantState::Declined)
            || (matches!(record.state, OwnerGrantState::Recorded)
                && (record.closure.is_some()
                    || month_ended(&record.summary, now)
                    || succeeded.contains(&record.summary.grant_id)
                    || matches!(
                        record.succession_state,
                        Some(
                            OwnerGrantSuccessionState::Rejected
                                | OwnerGrantSuccessionState::Cancelled
                        )
                    )))
        {
            declined = Some(record);
        } else if pending.replace(record).is_some() {
            return Err("owner_grant_history_unavailable".into());
        }
    }
    Ok(pending.or(declined))
}

// Publishing again after a legacy month ended should show today's capacity
// and its actual blocker, not ask the owner to change an already-saved ceiling.
// Uncertain originals and recurring grants keep their own recovery paths.
fn open_recurring_renewal_form(progress: &OwnerGrantProgress, now: u64) -> bool {
    matches!(progress.state, OwnerGrantState::Recorded)
        && progress.closure.is_none()
        && progress.summary.calendar.is_some()
        && month_ended(&progress.summary, now)
        && !matches!(progress.succession_state, Some(OwnerGrantSuccessionState::Rejected | OwnerGrantSuccessionState::Cancelled))
}
fn open_current_period_form(progress: &OwnerGrantProgress, now: u64) -> bool {
    matches!(progress.state, OwnerGrantState::Recorded)
        && progress.closure.is_none()
        && progress.summary.calendar.is_none()
        && month_ended(&progress.summary, now)
}
/// Only the native panel owns this. The exact host, package lease and native
/// manager stay bound throughout selection, review and asynchronous confirmation.
#[derive(Clone)]
pub(crate) struct GrantFlow {
    host: Host,
    lease: Lease,
    access: OwnerGrantAccess,
    selector: Selector,
    /// Whether the server advertises `pool-grant-succession-v1`: 0 not yet
    /// asked, 1 no, 2 yes. Only gates which native form is offered.
    succession_support: Arc<AtomicU8>,
}
impl GrantFlow {
    pub fn choose_people(&self) -> bool { self.selector.choose_people }
    pub async fn search_people(&self, query: String) -> Result<super::grant_people::People, String> {
        self.check()?;
        let client = crate::sliding_sync::get_client().ok_or("management_session_required")?;
        let result = super::grant_people::search(&client, self.account(), &query).await;
        self.check()?;
        result
    }
    async fn verify_people(&self, people: &[String]) -> Result<(), String> {
        self.check()?;
        if people.is_empty() { return Ok(()); }
        let client = crate::sliding_sync::get_client().ok_or("management_session_required")?;
        for person in people {
            super::grant_people::verify(&client, self.account(), person).await?;
            self.check()?;
        }
        Ok(())
    }
    async fn form(&self) -> Result<GrantResult, String> {
        self.check()?;
        let form = self.access.publication_form(self.selector.contribution_id.clone()
            .ok_or("owner_grant_original_intent_required")?).await.map_err(|e| e.code)?;
        self.check()?;
        Ok(GrantResult::Choose(form))
    }
    fn check(&self) -> Result<(), String> {
        let account = self.host.check(&self.lease)?;
        self.lease.authorize(&account, REVIEW_GRANT, None)?;
        if self.access.retired() {
            return Err("management_scope_changed".into());
        }
        Ok(())
    }
    pub async fn open(&self) -> Result<GrantResult, String> {
        self.check()?;
        if let Some(contribution) = &self.selector.contribution_id {
            let originals = self.access.list_existing().await.map_err(|e| e.code)?;
            self.check()?;
            let matching = originals
                .into_iter()
                .filter(|s| &s.contribution_id == contribution);
            let mut records = Vec::new();
            for original in matching {
                records.push(self.access.recover(original.intent_id).await.map_err(|e| e.code)?);
                self.check()?;
            }
            let current = existing_progress(records, now_ms())?;
            if let Some(kind) = self.selector.succession {
                // An unfinished change is resolved before another one starts:
                // it is published, recovered or cancelled from its own record.
                if let Some(progress) = current.filter(unfinished_successor) {
                    return self.observe_progress(progress).await;
                }
                return self.successor_form(kind).await;
            }
            if let Some(progress) = current {
                if open_recurring_renewal_form(&progress, now_ms()) {
                    return self.successor_form(Succession::Renewal).await;
                }
                if open_current_period_form(&progress, now_ms()) {
                    return self.new_limits().await;
                }
                return self.observe_progress(progress).await;
            }
            self.form().await
        } else {
            self.recover(self.selector.intent_id.clone().ok_or("owner_grant_original_intent_required")?).await
        }
    }
    pub fn can_choose_new_limits(&self) -> bool {
        self.selector.contribution_id.is_some()
    }
    /// The owner, reading a record opened by its intent, chose in the native
    /// window to add people to that record's own pool. The pool comes from
    /// the native record shown, never from the script; the amendment form,
    /// its review and confirmation still commit everything natively.
    pub fn for_record_pool(&self, contribution: String) -> Result<Self, String> {
        if self.selector.intent_id.is_none() || !grant_key(&contribution) {
            return Err("owner_grant_original_intent_required".into());
        }
        let mut flow = self.clone();
        flow.selector = Selector {
            contribution_id: Some(contribution),
            intent_id: None,
            proposed_tokens: None,
            proposed_per_request_tokens: None,
            succession: Some(Succession::Amendment),
            applicants: None,
            choose_people: true,
        };
        flow.check()?;
        Ok(flow)
    }
    /// The script's proposed amount for the native tokens field, if any.
    pub fn proposed_per_request_tokens(&self) -> Option<u64> { self.selector.proposed_per_request_tokens }
    pub fn proposed_tokens(&self) -> Option<u64> {
        self.selector.proposed_tokens
    }
    /// Applicants the script proposed for the native applicants field.
    pub fn applicants(&self) -> &[String] {
        self.selector.applicants.as_deref().unwrap_or_default()
    }
    /// The change the script asked the panel to open, if any. It only names
    /// the window while its native form loads; the form decides what commits.
    pub fn requested_succession(&self) -> Option<Succession> {
        self.selector.succession
    }
    /// Last known server support for amending or renewing a published grant.
    pub fn succession_supported(&self) -> Option<bool> {
        match self.succession_support.load(Ordering::Acquire) {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        }
    }
    async fn refresh_succession_support(&self) -> Result<bool, String> {
        self.check()?;
        let supported = self.access.supports_succession().await.map_err(|e| e.code)?;
        self.check()?;
        self.succession_support
            .store(if supported { 2 } else { 1 }, Ordering::Release);
        Ok(supported)
    }
    async fn successor_form(&self, kind: Succession) -> Result<GrantResult, String> {
        self.check()?;
        let contribution = self
            .selector
            .contribution_id
            .clone()
            .ok_or("owner_grant_original_intent_required")?;
        let form = self
            .access
            .succession_form(contribution, kind.native())
            .await
            .map_err(|e| e.code)?;
        self.check()?;
        Ok(GrantResult::ChooseSuccessor(Box::new(form)))
    }
    /// Open the amendment or renewal form for this pool's published grant.
    /// The native form, review and publication still commit everything.
    pub async fn choose_successor(&self, kind: Succession) -> Result<GrantResult, String> {
        if !self.can_choose_new_limits() {
            return Err("owner_grant_original_intent_required".into());
        }
        self.successor_form(kind).await
    }
    pub async fn prepare_successor(
        &self,
        kind: Succession,
        mut choices: GrantChoices,
    ) -> Result<GrantResult, String> {
        self.check()?;
        choices.coordinator = self.account().to_owned();
        let choices = choices.parse()?;
        let contribution = self
            .selector
            .contribution_id
            .clone()
            .ok_or("owner_grant_original_intent_required")?;
        let form = self.access.succession_form(contribution.clone(), kind.native())
            .await.map_err(|e| e.code)?;
        let added: Vec<String> = choices.eligible_managers.iter()
            .filter(|id| !form.predecessor.eligible_managers.contains(id)).cloned().collect();
        self.verify_people(&added).await?;
        let review = self
            .access
            .prepare_successor(contribution, kind.native(), choices)
            .await
            .map_err(|e| e.code)?;
        self.check()?;
        Ok(GrantResult::Review(Box::new(review)))
    }
    /// Closed grants require a currently approved, distinct admission before
    /// reopening choices. A grant whose UTC month ended may instead be followed
    /// by this month's grant on the same admission: the same native check
    /// verifies that regrant. Prepare and the atomic writer repeat their checks.
    pub async fn choose_new_limits(&self) -> Result<GrantResult, String> {
        if !self.can_choose_new_limits() {
            return Err("owner_grant_original_intent_required".into());
        }
        let now = now_ms();
        match self.open().await? {
            GrantResult::Progress(p) if matches!(p.state, OwnerGrantState::Declined) => self.form().await,
            GrantResult::Progress(p) if matches!(p.state, OwnerGrantState::Recorded)
                && (p.closure.is_some() || month_ended(&p.summary, now)) => self.new_limits().await,
            GrantResult::PublicationProgress(p) if month_ended(&p.summary, now) => self.new_limits().await,
            result => Ok(result),
        }
    }
    async fn new_limits(&self) -> Result<GrantResult, String> {
        self.access
            .check_new_admission(
                self.selector
                    .contribution_id
                    .clone()
                    .ok_or("owner_grant_original_intent_required")?,
            )
            .await
            .map_err(|e| e.code)?;
        self.check()?;
        self.form().await
    }
    pub fn account(&self) -> &str {
        &self.lease.identity().account
    }
    pub fn active(&self) -> bool {
        self.check().is_ok()
    }
    pub async fn review(&self, id: Option<String>) -> Result<GrantResult, String> {
        self.check()?;
        let id = id
            .or_else(|| self.selector.intent_id.clone())
            .ok_or("owner_grant_original_intent_required")?;
        let result = self.access.review(id).await.map_err(|e| e.code)?;
        self.check()?;
        Ok(match result {
            OwnerGrantReviewResult::Review(review) => GrantResult::Review(review),
            OwnerGrantReviewResult::Progress(progress) => GrantResult::Progress(progress),
        })
    }
    pub async fn prepare(&self, mut choices: GrantChoices) -> Result<GrantResult, String> {
        self.check()?;
        choices.coordinator = self.account().to_owned();
        let choices = choices.parse()?;
        self.verify_people(&choices.eligible_managers).await?;
        let contribution = self
            .selector
            .contribution_id
            .clone()
            .ok_or("owner_grant_original_intent_required")?;
        let review = self
            .access
            .prepare(contribution, choices)
            .await
            .map_err(|e| e.code)?;
        self.check()?;
        Ok(GrantResult::Review(Box::new(review)))
    }
    pub async fn confirm(&self, review: OwnerGrantReview) -> Result<GrantResult, String> {
        self.check()?;
        if review.expired() {
            return Err("owner_grant_original_review_expired".into());
        }
        let result = self.access.confirm(review).await.map_err(|e| e.code);
        if self.check().is_err() {
            return Err("management_outcome_unknown".into());
        }
        result.map(|progress| GrantResult::Progress(Box::new(progress)))
    }
    pub async fn publication_review(&self, id: String) -> Result<GrantResult, String> {
        self.check()?;
        let result = self
            .access
            .review_publication(id)
            .await
            .map_err(|e| e.code)?;
        self.check()?;
        Ok(match result {
            OwnerGrantPublicationResult::Review(review) => GrantResult::PublicationReview(review),
            OwnerGrantPublicationResult::Progress(progress) => {
                GrantResult::PublicationProgress(progress)
            }
        })
    }
    pub async fn publication_confirm(
        &self,
        review: OwnerGrantPublicationReview,
    ) -> Result<GrantResult, String> {
        self.check()?;
        if review.expired() {
            return Err("owner_grant_publication_review_expired".into());
        }
        let result = self
            .access
            .confirm_publication(review)
            .await
            .map_err(|e| e.code);
        if self.check().is_err() {
            return Err("management_outcome_unknown".into());
        }
        result.map(|p| GrantResult::PublicationProgress(Box::new(p)))
    }
    pub async fn recover(&self, id: String) -> Result<GrantResult, String> {
        self.check()?;
        let result = self.access.recover(id).await.map_err(|e| e.code)?;
        self.check()?;
        self.observe_progress(result).await
    }
    async fn observe_progress(&self, progress: OwnerGrantProgress) -> Result<GrantResult, String> {
        self.check()?;
        // Keep canonical closure and unsettled local outcomes visible. Only a
        // recorded, open reservation can be replaced by its publication status.
        if matches!(progress.state, OwnerGrantState::Recorded) && progress.closure.is_none() {
            let publication = self.access.observe_publication(progress.summary.intent_id.clone())
                .await.map_err(|e| e.code)?;
            self.check()?;
            if let Some(publication) = publication {
                // A published grant with a proposal or a requested change can
                // be amended only if the server advertises succession, and a
                // record opened by itself offers to add people only then.
                // A failed probe leaves support unknown, which keeps today's
                // read-only answer; it never hides the published grant.
                if (self.selector.proposed_tokens.is_some()
                    || self.selector.succession.is_some()
                    || self.selector.intent_id.is_some())
                    && self.succession_supported().is_none()
                {
                    let _ = self.refresh_succession_support().await;
                    self.check()?;
                }
                return Ok(GrantResult::PublicationProgress(Box::new(publication)));
            }
        }
        Ok(GrantResult::Progress(Box::new(progress)))
    }
    pub async fn cancellation_review(&self, id: String) -> Result<GrantResult, String> {
        self.check()?;
        let result = self.access.review_cancellation(id).await.map_err(|e| e.code)?;
        self.check()?;
        Ok(match result {
            OwnerGrantCancellationResult::Review(review) => GrantResult::CancellationReview(review),
            OwnerGrantCancellationResult::Progress(progress) => GrantResult::Progress(progress),
        })
    }
    pub async fn cancellation_confirm(&self, review: OwnerGrantCancellationReview) -> Result<GrantResult, String> {
        self.check()?;
        let result = self.access.confirm_cancellation(review).await.map_err(|e| e.code);
        if self.check().is_err() {
            return Err("management_outcome_unknown".into());
        }
        result.map(|p| GrantResult::Progress(Box::new(p)))
    }
}
impl Host {
    /// The owner's native grant access, bound to this host and package lease.
    /// Only the native adoption handoff or authenticated descriptor reload can
    /// supply this qualified registry/session; a script cannot choose a path
    /// or initialize a new resource authority.
    fn host_grant_access(&self, lease: &Lease) -> Result<OwnerGrantAccess, String> {
        let access = self
            .0
            .grant_access
            .lock()
            .map_err(|_| "management_unavailable")?
            .clone()
            .ok_or("current_profile_required")?;
        let weak = std::sync::Arc::downgrade(&self.0);
        let gate_lease = lease.clone();
        access
            .restrict_to_host(move || {
                weak.upgrade().is_some_and(|host| {
                    !host.retired.load(std::sync::atomic::Ordering::Acquire)
                        && host.identity == *gate_lease.identity()
                        && gate_lease.check(&host.identity.account).is_ok()
                })
            })
            .map_err(|error| error.code)
    }
    /// `hagency.grants.applicants`: this month's people who may request from
    /// one pool, read from the native records the native grant window reads.
    /// Read-only: it lists and recovers records and observes a root's
    /// publication; it never prepares, confirms, publishes or retries.
    pub(crate) async fn grant_applicants(&self, lease: &Lease, args: Value) -> Result<Value, String> {
        let account = self.check(lease)?;
        lease.authorize(&account, APPLICANTS, None)?;
        let query = ApplicantsQuery::parse(args)?;
        // The adopted grant access is attached when the manager starts.
        self.manager(lease).await?;
        let access = self.host_grant_access(lease)?;
        let check = |access: &OwnerGrantAccess| -> Result<(), String> {
            let account = self.check(lease)?;
            lease.authorize(&account, APPLICANTS, None)?;
            if access.retired() {
                return Err("management_scope_changed".into());
            }
            Ok(())
        };
        let originals = access.list_existing().await.map_err(|e| e.code)?;
        check(&access)?;
        let mut records = Vec::new();
        for original in originals
            .into_iter()
            .filter(|original| original.contribution_id == query.contribution_id)
        {
            records.push(access.recover(original.intent_id).await.map_err(|e| e.code)?);
            check(&access)?;
        }
        let change_pending = records.iter().any(unfinished_successor);
        let current = effective_original(&records, now_ms());
        let state = match current {
            // The native ledger marks a successor effective only once the
            // server was seen to accept it.
            Some(record) if record.summary.succession.is_some() => "published",
            Some(record) => {
                let seen = access
                    .observe_publication(record.summary.intent_id.clone())
                    .await
                    .map_err(|e| e.code)?;
                check(&access)?;
                publication_state(seen.as_ref())
            }
            None => "none",
        };
        Ok(applicants_reply(&query.contribution_id, current, state, change_pending))
    }
    /// `hagency.grants.floor`: the floor of a published month's per-request
    /// limit, as the amendment window reads it, so the pool's page bounds its
    /// slider with the window's figure instead of an estimate. Read-only: the
    /// amendment form opens no hold, intent or publication; nothing is
    /// prepared, confirmed, published or retried.
    pub(crate) async fn grant_floor(&self, lease: &Lease, args: Value) -> Result<Value, String> {
        let account = self.check(lease)?;
        lease.authorize(&account, FLOOR, None)?;
        let query = ApplicantsQuery::parse(args)?;
        self.manager(lease).await?;
        let access = self.host_grant_access(lease)?;
        let form = access
            .succession_form(query.contribution_id.clone(), OwnerGrantSuccessionKind::Amendment)
            .await
            .map_err(|e| e.code)?;
        let account = self.check(lease)?;
        lease.authorize(&account, FLOOR, None)?;
        if access.retired() {
            return Err("management_scope_changed".into());
        }
        Ok(floor_reply(&query.contribution_id, &form))
    }
    pub fn grant_handoff(&self, lease: Lease, args: Value) -> Result<GrantFlow, String> {
        let selector = Selector::parse(args)?;
        let account = self.check(&lease)?;
        lease.authorize(&account, REVIEW_GRANT, None)?;
        selector.check_applicants(&account)?;
        let access = self.host_grant_access(&lease)?;
        let flow = GrantFlow {
            host: self.clone(),
            lease,
            access,
            selector,
            succession_support: Arc::new(AtomicU8::new(0)),
        };
        flow.check()?;
        Ok(flow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn progress(id: &str, state: OwnerGrantState) -> OwnerGrantProgress {
        OwnerGrantProgress {
            state,
            closure: None,
            succession_state: None,
            summary: ::hagency::native_owner::OwnerGrantSummary { renewal_mode: None, per_request_tokens: None,
                intent_id: id.into(), grant_id: format!("grant_{id}"), owner_mxid: "@owner:test".into(),
                contribution_id: "contribution".into(), resource_id: "resource".into(),
                coordinator_mxid: "@coordinator:test".into(), allocated_tokens: 700,
                eligible_managers: vec!["@manager:test".into()], allow_self_approval: false,
                delegation_expires_at_ms: 1793491200000, period_key: "2026-10".into(),
                period_start_ms: 1790812800000, period_end_ms: 1793491200000,
                calendar: None, succession: None,
            },
        }
    }
    /// A successor original of `predecessor`, in the given native state.
    fn successor(
        id: &str,
        predecessor: &OwnerGrantProgress,
        kind: OwnerGrantSuccessionKind,
        state: OwnerGrantState,
        native: Option<OwnerGrantSuccessionState>,
    ) -> OwnerGrantProgress {
        let mut p = progress(id, state);
        p.succession_state = native;
        p.summary.succession = Some(::hagency::native_owner::OwnerGrantSuccessionSummary {
            kind,
            predecessor_grant_id: predecessor.summary.grant_id.clone(),
            predecessor_allocated_tokens: predecessor.summary.allocated_tokens,
            carried_children: 2,
            carried_retained_tokens: 300,
        });
        p
    }
    /// Inside the fixtures' 2026-10 month, and that month's end (2026-11-01).
    const OCTOBER: u64 = 1_792_000_000_000;
    const NOVEMBER: u64 = 1_793_491_200_000;
    #[test]
    fn ended_recurring_pool_opens_one_owner_renewal_form() {
        let mut old = progress("original", OwnerGrantState::Recorded);
        old.summary.calendar = Some(hagency_core::recurring_quota::Calendar {
            cadence: hagency_core::recurring_quota::Cadence::Monthly,
            time_zone: "America/Los_Angeles".into(),
        });
        old.summary.renewal_mode = Some(hagency_core::recurring_quota::RenewalMode::Owner);
        let end = old.summary.period_end_ms;
        assert!(!open_recurring_renewal_form(&old, end - 1));
        assert!(open_recurring_renewal_form(&old, end));
        old.succession_state = Some(OwnerGrantSuccessionState::Cancelled);
        assert!(!open_recurring_renewal_form(&old, end));
        old.succession_state = None;
        old.state = OwnerGrantState::OutcomeUnknown;
        assert!(!open_recurring_renewal_form(&old, end));
    }
    #[test]
    fn expired_legacy_publish_opens_current_capacity_without_replacing_uncertain_requests() {
        let mut old = progress("old", OwnerGrantState::Recorded);
        assert!(!open_current_period_form(&old, NOVEMBER - 1));
        assert!(open_current_period_form(&old, NOVEMBER));
        old.summary.calendar = Some(serde_json::from_value(json!({"cadence":"monthly","timeZone":"America/Los_Angeles"})).unwrap());
        assert!(!open_current_period_form(&old, NOVEMBER));
        for state in [OwnerGrantState::ReviewRequired, OwnerGrantState::Declined] {
            assert!(!open_current_period_form(&progress("original", state), NOVEMBER));
        }
    }
    fn in_month(mut p: OwnerGrantProgress, key: &str, start: u64, end: u64) -> OwnerGrantProgress {
        p.summary.period_key = key.into();
        p.summary.period_start_ms = start;
        p.summary.period_end_ms = end;
        p
    }
    #[test]
    fn applicants_read_names_one_pool_and_nothing_else() {
        let query = ApplicantsQuery::parse(json!({"contributionId": "pool_original-2"})).unwrap();
        assert_eq!(query.contribution_id, "pool_original-2");
        for forged in [
            json!({}),
            json!({"contributionId": ""}),
            json!({"contributionId": "../pool"}),
            json!({"contributionId": "pool one"}),
            json!({"contributionId": "p".repeat(129)}),
            json!({"contributionId": 7}),
            json!({"contributionId": "pool", "intentId": "intent"}),
            json!({"contributionId": "pool", "applicants": ["@a:test"]}),
            json!({"contributionId": "pool", "proposedTokens": 1}),
            json!({"contributionId": "pool", "grantId": "grant"}),
            json!({"contributionId": "pool", "ownerMxid": "@owner:test"}),
            json!({"contributionId": "pool", "succession": "amendment"}),
            json!(["pool"]),
        ] {
            assert_eq!(ApplicantsQuery::parse(forged.clone()).unwrap_err(), "invalid_management_arguments", "{forged}");
        }
        assert_eq!(ApplicantsQuery::parse(json!({"contributionId": "p".repeat(128)})).unwrap().contribution_id.len(), 128);
        assert!(octosense_app_contract::hagency::SERVICES.contains(&APPLICANTS));
        assert!(octosense_app_contract::KNOWN_CAPABILITIES.contains(&APPLICANTS));
        assert!(octosense_app_contract::hagency::words(APPLICANTS).is_some());
        let shipped: Value = serde_json::from_str(include_str!("../../../apps/hagency/bundle/manifest.json")).unwrap();
        assert!(shipped["capabilities"].as_array().unwrap().iter().any(|cap| cap == APPLICANTS));
        // A read, like grants.review: no management permission comes with it.
        let identity = crate::miniapps::InstanceId {
            app: octosense_app_contract::hagency::APP_ID.into(),
            account: "@owner:test".into(),
            room: None,
            generation: 1,
        };
        let lease = rinx_miniapp_core::SessionAuthority::default().issue(
            identity,
            [APPLICANTS.to_owned()].into(),
            Default::default(),
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );
        assert!(super::super::permissions(&lease).is_empty());
    }
    /// The floor read names one pool and answers in the amendment window's own
    /// figures, so the pool's page and the window agree on the floor.
    #[test]
    fn floor_read_is_a_closed_pool_read_with_the_windows_figures() {
        for forged in [
            json!({}),
            json!({"contributionId": "pool", "succession": "amendment"}),
            json!({"contributionId": "pool", "proposedTokens": 1}),
            json!({"contributionId": "../pool"}),
        ] {
            assert_eq!(ApplicantsQuery::parse(forged.clone()).unwrap_err(), "invalid_management_arguments", "{forged}");
        }
        let form = ::hagency::native_owner::OwnerGrantSuccessionForm {
            kind: OwnerGrantSuccessionKind::Amendment,
            contribution_id: "pool_codex".into(),
            resource_id: "resource".into(),
            label: "codex".into(),
            context: ::hagency::native_owner::OwnerGrantReviewContext {
                server_origin: "https://matrix.test".into(),
                profile_id: "profile".into(),
                resource_revision: "revision".into(),
                framework: "codex".into(),
                model: "model".into(),
                remaining_tokens: Some(9_000_000),
                resource_total_tokens: Some(9_000_000),
                shared_account_total_tokens: None,
                capacity_blocker: None,
                observed_at_ms: 1,
            },
            predecessor: progress("root", OwnerGrantState::Recorded).summary,
            floor: ::hagency::native_owner::OwnerGrantFloor { children: 1, retained_tokens: 9_000_000 },
            period_key: "2026-10".into(),
            period_start_ms: OCTOBER,
            period_end_ms: NOVEMBER,
            window_opens_at_ms: NOVEMBER - 7 * 86_400_000,
            window_closes_at_ms: NOVEMBER,
        };
        assert_eq!(
            floor_reply("pool_codex", &form),
            json!({"contributionId": "pool_codex", "heldTokens": 9_000_000, "heldAgents": 1,
                   "perRequestTokens": form.predecessor.per_request_tokens.unwrap_or(form.predecessor.allocated_tokens), "mostTokens": 9_000_000, "periodEndMs": NOVEMBER})
        );
        assert!(octosense_app_contract::hagency::SERVICES.contains(&FLOOR));
        assert!(octosense_app_contract::KNOWN_CAPABILITIES.contains(&FLOOR));
        assert!(octosense_app_contract::hagency::words(FLOOR).is_some());
        let shipped: Value = serde_json::from_str(include_str!("../../../apps/hagency/bundle/manifest.json")).unwrap();
        assert!(shipped["capabilities"].as_array().unwrap().iter().any(|cap| cap == FLOOR));
        let lease = rinx_miniapp_core::SessionAuthority::default().issue(
            crate::miniapps::InstanceId {
                app: octosense_app_contract::hagency::APP_ID.into(),
                account: "@owner:test".into(),
                room: None,
                generation: 1,
            },
            [FLOOR.to_owned()].into(),
            Default::default(),
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );
        assert!(super::super::permissions(&lease).is_empty());
    }
    #[test]
    fn this_months_list_comes_from_the_grant_in_force() {
        let id = |record: Option<&OwnerGrantProgress>| record.map(|r| r.summary.grant_id.clone());
        let root = progress("root", OwnerGrantState::Recorded);
        assert_eq!(id(effective_original(std::slice::from_ref(&root), OCTOBER)), Some("grant_root".into()));
        // An amendment the ledger marked effective replaces it; a pending one doesn't.
        let mut amended = successor("amend", &root, OwnerGrantSuccessionKind::Amendment, OwnerGrantState::Recorded,
            Some(OwnerGrantSuccessionState::Effective));
        amended.summary.eligible_managers = vec!["@manager:test".into(), "@lisi:test".into()];
        assert_eq!(id(effective_original(&[root.clone(), amended.clone()], OCTOBER)), Some("grant_amend".into()));
        let pending = successor("amend", &root, OwnerGrantSuccessionKind::Amendment, OwnerGrantState::Recorded,
            Some(OwnerGrantSuccessionState::Pending));
        assert_eq!(id(effective_original(&[root.clone(), pending], OCTOBER)), Some("grant_root".into()));
        for rejected in [OwnerGrantSuccessionState::Rejected, OwnerGrantSuccessionState::Cancelled] {
            let change = successor("amend", &root, OwnerGrantSuccessionKind::Amendment, OwnerGrantState::Recorded, Some(rejected));
            assert_eq!(id(effective_original(&[root.clone(), change], OCTOBER)), Some("grant_root".into()));
        }
        // A change still under review, or one whose outcome isn't known, isn't in force.
        let unknown = successor("amend", &root, OwnerGrantSuccessionKind::Amendment, OwnerGrantState::OutcomeUnknown, None);
        assert_eq!(id(effective_original(&[root.clone(), unknown], OCTOBER)), Some("grant_root".into()));
        // Next month's renewal, even scheduled and effective, never stands in for this month.
        let renewal = in_month(
            successor("renew", &amended, OwnerGrantSuccessionKind::Renewal, OwnerGrantState::Recorded, Some(OwnerGrantSuccessionState::Effective)),
            "2026-11", NOVEMBER, 1_796_083_200_000,
        );
        let chain = [root.clone(), amended.clone(), renewal];
        assert_eq!(id(effective_original(&chain, OCTOBER)), Some("grant_amend".into()));
        // Once November starts, the renewal is the grant in force.
        assert_eq!(id(effective_original(&chain, NOVEMBER + 1)), Some("grant_renew".into()));
        // A month that has ended, a closed grant and a discarded publish give nothing.
        assert!(effective_original(std::slice::from_ref(&root), NOVEMBER).is_none());
        let mut closed = root.clone();
        closed.closure = Some(::hagency::native_owner::OwnerGrantClosure {
            retained_tokens: 0, known_consumed_tokens: 0, unresolved_tokens: 0, observed_at_ms: 1,
        });
        assert!(effective_original(&[closed], OCTOBER).is_none());
        for state in [OwnerGrantState::Declined, OwnerGrantState::ReviewRequired, OwnerGrantState::ReviewExpired, OwnerGrantState::OutcomeUnknown] {
            assert!(effective_original(&[progress("root", state)], OCTOBER).is_none());
        }
        // Last month's grant stays history beside this month's regrant.
        let september = in_month(progress("sep", OwnerGrantState::Recorded), "2026-09", 1_788_220_800_000, 1_790_812_800_000);
        assert_eq!(id(effective_original(&[september, root.clone()], OCTOBER)), Some("grant_root".into()));
        // The reply lists the people in force, sorted, and says whether the server has them.
        let reply = applicants_reply("contribution", Some(&amended), "published", true);
        assert_eq!(reply, json!({
            "contributionId": "contribution", "state": "published", "periodKey": "2026-10",
            "periodStartMs": 1790812800000u64, "periodEndMs": 1793491200000u64, "grantId": "grant_amend",
            "applicants": ["@lisi:test", "@manager:test"], "changePending": true,
        }));
        assert_eq!(applicants_reply("contribution", None, "published", false), json!({
            "contributionId": "contribution", "state": "none", "periodKey": null, "periodStartMs": null,
            "periodEndMs": null, "grantId": null, "applicants": [], "changePending": false,
        }));
    }
    #[test]
    fn a_root_list_is_published_only_once_the_server_holds_it_active() {
        use ::hagency::native_owner::OwnerGrantPublicationState as Seen;
        let seen = |state: Seen, server: Option<&str>| OwnerGrantPublicationProgress {
            summary: progress("root", OwnerGrantState::Recorded).summary,
            state,
            server_state: server.map(str::to_owned),
            server_revision: server.map(|_| 1),
            observed_at_ms: server.map(|_| 1),
        };
        assert_eq!(publication_state(None), "unpublished");
        assert_eq!(publication_state(Some(&seen(Seen::ServerObserved, Some("active")))), "published");
        assert_eq!(publication_state(Some(&seen(Seen::ServerObserved, Some("revoked")))), "unpublished");
        assert_eq!(publication_state(Some(&seen(Seen::ServerObserved, Some("expired")))), "unpublished");
        assert_eq!(publication_state(Some(&seen(Seen::ServerObserved, Some("superseded")))), "unconfirmed");
        assert_eq!(publication_state(Some(&seen(Seen::ServerObserved, None))), "unconfirmed");
        assert_eq!(publication_state(Some(&seen(Seen::OutcomeUnknown, None))), "unconfirmed");
        assert_eq!(publication_state(Some(&seen(Seen::Rejected, None))), "unpublished");
    }
    #[test]
    fn closed_originals_never_hide_pending_unknown_or_recorded_effects() {
        for state in [OwnerGrantState::ReviewRequired, OwnerGrantState::ReviewExpired,
            OwnerGrantState::OutcomeUnknown, OwnerGrantState::Recorded] {
            let result = existing_progress(vec![progress("closed-before", OwnerGrantState::Declined),
                progress("original", state), progress("closed-after", OwnerGrantState::Declined)], OCTOBER).unwrap().unwrap();
            assert_eq!(result.summary.intent_id, "original");
            assert!(!matches!(result.state, OwnerGrantState::Declined));
        }
        assert!(existing_progress(vec![progress("one", OwnerGrantState::OutcomeUnknown),
            progress("two", OwnerGrantState::ReviewRequired)], OCTOBER).is_err());
    }
    #[test]
    fn cancellation_history_keeps_latest_closed_original_for_explicit_new_choices() {
        let result = existing_progress(vec![progress("first", OwnerGrantState::Declined),
            progress("second", OwnerGrantState::Declined)], OCTOBER).unwrap().unwrap();
        assert_eq!(result.summary.intent_id, "second");
        assert!(matches!(result.state, OwnerGrantState::Declined));
        assert!(existing_progress(vec![], OCTOBER).unwrap().is_none());
    }
    #[test]
    fn ended_months_are_history_beside_this_month_original() {
        let september = in_month(
            progress("september", OwnerGrantState::Recorded),
            "2026-09",
            1_788_220_800_000,
            1_790_812_800_000,
        );
        let october = progress("october", OwnerGrantState::Recorded);
        assert!(!month_ended(&october.summary, NOVEMBER - 1));
        assert!(month_ended(&october.summary, NOVEMBER));
        // In its own month a recorded grant is the current original.
        let current = existing_progress(vec![september.clone(), october.clone()], OCTOBER)
            .unwrap()
            .unwrap();
        assert_eq!(current.summary.intent_id, "october");
        assert!(
            existing_progress(
                vec![
                    october.clone(),
                    progress("review", OwnerGrantState::ReviewRequired)
                ],
                OCTOBER
            )
            .is_err()
        );
        // Several ended grants are history, never an ambiguous history.
        let ended = existing_progress(vec![september.clone(), october.clone()], NOVEMBER)
            .unwrap()
            .unwrap();
        assert_eq!(ended.summary.intent_id, "october");
        assert!(matches!(ended.state, OwnerGrantState::Recorded));
        // This month's original is current beside them, whatever its state.
        for state in [
            OwnerGrantState::ReviewRequired,
            OwnerGrantState::ReviewExpired,
            OwnerGrantState::OutcomeUnknown,
            OwnerGrantState::Recorded,
        ] {
            let november = in_month(
                progress("november", state),
                "2026-11",
                NOVEMBER,
                1_796_083_200_000,
            );
            let p = existing_progress(vec![september.clone(), november, october.clone()], NOVEMBER)
                .unwrap()
                .unwrap();
            assert_eq!(p.summary.intent_id, "november");
        }
        // An unresolved original is never history, even after its month.
        let unknown = progress("unknown", OwnerGrantState::OutcomeUnknown);
        let p = existing_progress(vec![october.clone(), unknown.clone()], NOVEMBER)
            .unwrap()
            .unwrap();
        assert_eq!(p.summary.intent_id, "unknown");
        assert!(
            existing_progress(
                vec![
                    unknown,
                    in_month(
                        progress("november", OwnerGrantState::ReviewRequired),
                        "2026-11",
                        NOVEMBER,
                        1_796_083_200_000
                    )
                ],
                NOVEMBER
            )
            .is_err()
        );
    }
    fn settled(id: &str) -> OwnerGrantProgress {
        let mut p = progress(id, OwnerGrantState::Recorded);
        p.closure = Some(::hagency::native_owner::OwnerGrantClosure {
            retained_tokens: 200, known_consumed_tokens: 40,
            unresolved_tokens: 160, observed_at_ms: 1792340000000,
        });
        p
    }
    #[test]
    fn settled_rounds_keep_history_without_hiding_the_current_original() {
        for state in [OwnerGrantState::ReviewRequired, OwnerGrantState::ReviewExpired,
            OwnerGrantState::OutcomeUnknown, OwnerGrantState::Recorded] {
            let p = existing_progress(vec![settled("old"), progress("current", state),
                settled("other_closed")], OCTOBER).unwrap().unwrap();
            assert_eq!(p.summary.intent_id, "current");
            assert!(p.closure.is_none());
        }
        let p = existing_progress(vec![settled("first"), settled("last")], OCTOBER)
            .unwrap()
            .unwrap();
        assert_eq!(p.summary.intent_id, "last");
        assert_eq!(p.closure.unwrap().retained_tokens, 200);
    }
    #[test]
    fn settlement_metadata_cannot_hide_an_unknown_effect() {
        let mut unknown = settled("unknown");
        unknown.state = OwnerGrantState::OutcomeUnknown;
        assert!(
            existing_progress(
                vec![unknown, progress("current", OwnerGrantState::Recorded)],
                OCTOBER
            )
            .is_err()
        );
    }
    #[test]
    fn handoff_rejects_authority_fields_and_ambiguous_selectors() {
        for bad in [
            json!({}),
            json!({"contributionId":"a","intentId":"b"}),
            json!({"intentId":"a","allocatedTokens":500}),
            json!({"contributionId":"a","registryPath":"/tmp"}),
            json!({"contributionId":"a","ownerMxid":"@other:test"}),
            json!({"intentId":"../other"}),
        ] {
            assert!(Selector::parse(bad).is_err());
        }
        assert!(Selector::parse(json!({"intentId":"original_1"})).is_ok());
    }
    #[test]
    fn proposed_tokens_are_a_bounded_integer_for_a_pool_only() {
        let proposal = |value: Value| Selector::parse(value).map(|s| s.proposed_tokens);
        assert_eq!(proposal(json!({"contributionId":"pool_1","proposedTokens":1})), Ok(Some(1)));
        assert_eq!(
            proposal(json!({"contributionId":"pool_1","proposedTokens":MAX_GRANT_TOKENS})),
            Ok(Some(MAX_GRANT_TOKENS))
        );
        assert_eq!(proposal(json!({"contributionId":"pool_1"})), Ok(None));
        assert_eq!(proposal(json!({"contributionId":"pool_1","proposedTokens":null})), Ok(None));
        for bad in [
            json!({"proposedTokens":3000000}),
            json!({"intentId":"original_1","proposedTokens":3000000}),
            json!({"contributionId":"pool_1","intentId":"original_1","proposedTokens":3000000}),
            json!({"contributionId":"pool_1","proposedTokens":0}),
            json!({"contributionId":"pool_1","proposedTokens":MAX_GRANT_TOKENS + 1}),
            json!({"contributionId":"pool_1","proposedTokens":-1}),
            json!({"contributionId":"pool_1","proposedTokens":1.5}),
            json!({"contributionId":"pool_1","proposedTokens":"3000000"}),
            json!({"contributionId":"pool_1","proposedTokens":true}),
            json!({"contributionId":"pool_1","proposedTokens":[3000000]}),
            json!({"contributionId":"pool_1","proposedTokens":3000000,"allocatedTokens":3000000}),
        ] {
            assert_eq!(proposal(bad.clone()), Err("invalid_management_arguments".into()), "{bad}");
        }
    }
    #[test]
    fn an_amendment_replaces_the_published_grant_as_the_current_original() {
        use OwnerGrantSuccessionKind::Amendment;
        use OwnerGrantSuccessionState::*;
        let root = progress("root", OwnerGrantState::Recorded);
        // Every live state of a successor makes it the current original:
        // its review, an unknown outcome, a saved or an effective change.
        for (state, native) in [
            (OwnerGrantState::ReviewRequired, None),
            (OwnerGrantState::ReviewExpired, None),
            (OwnerGrantState::OutcomeUnknown, None),
            (OwnerGrantState::Recorded, Some(Pending)),
            (OwnerGrantState::Recorded, Some(Effective)),
        ] {
            let change = successor("change", &root, Amendment, state.clone(), native);
            let current = existing_progress(vec![root.clone(), change], OCTOBER).unwrap().unwrap();
            assert_eq!(current.summary.intent_id, "change");
            assert_eq!(
                unfinished_successor(&current),
                !matches!(native, Some(Effective))
            );
        }
        // A cancelled review or a rejected change leaves the published grant current.
        for (state, native) in [
            (OwnerGrantState::Declined, None),
            (OwnerGrantState::Recorded, Some(Rejected)),
            (OwnerGrantState::Recorded, Some(Cancelled)),
        ] {
            let change = successor("change", &root, Amendment, state, native);
            assert!(!unfinished_successor(&change));
            let current = existing_progress(vec![root.clone(), change.clone()], OCTOBER).unwrap().unwrap();
            assert_eq!(current.summary.intent_id, "root");
            // A later change after a rejected one is current again.
            let again = successor("again", &root, Amendment, OwnerGrantState::Recorded, Some(Pending));
            let current = existing_progress(vec![root.clone(), change, again], OCTOBER).unwrap().unwrap();
            assert_eq!(current.summary.intent_id, "again");
        }
        // A chain of amendments: only its head is current.
        let first = successor("first", &root, Amendment, OwnerGrantState::Recorded, Some(Effective));
        let second = successor("second", &first, Amendment, OwnerGrantState::Recorded, Some(Effective));
        let current = existing_progress(vec![root.clone(), first, second], OCTOBER).unwrap().unwrap();
        assert_eq!(current.summary.intent_id, "second");
        // Two live originals that do not succeed each other stay ambiguous.
        assert!(existing_progress(vec![root.clone(), progress("other", OwnerGrantState::Recorded)], OCTOBER).is_err());
    }
    #[test]
    fn a_scheduled_renewal_is_current_and_survives_the_boundary() {
        let october = progress("october", OwnerGrantState::Recorded);
        let mut renewal = successor(
            "renewal",
            &october,
            OwnerGrantSuccessionKind::Renewal,
            OwnerGrantState::Recorded,
            Some(OwnerGrantSuccessionState::Effective),
        );
        renewal = in_month(renewal, "2026-11", NOVEMBER, 1_796_083_200_000);
        for now in [OCTOBER, NOVEMBER, NOVEMBER + 1] {
            let current = existing_progress(vec![october.clone(), renewal.clone()], now).unwrap().unwrap();
            assert_eq!(current.summary.intent_id, "renewal");
        }
        // Once the renewal's own month ends, it is history like any grant.
        let ended = existing_progress(vec![october, renewal], 1_796_083_200_000).unwrap().unwrap();
        assert!(matches!(ended.state, OwnerGrantState::Recorded));
    }
    #[test]
    fn succession_and_applicants_are_bounded_pool_arguments() {
        let parsed = |value: Value| Selector::parse(value);
        let s = parsed(json!({"contributionId":"pool_1","proposedTokens":800000,"succession":"amendment"})).unwrap();
        assert_eq!(s.succession, Some(Succession::Amendment));
        assert_eq!(s.proposed_tokens, Some(800000));
        assert!(!s.choose_people);
        assert!(parsed(json!({"contributionId":"pool_1","succession":"amendment","choosePeople":true})).unwrap().choose_people);
        assert_eq!(parsed(json!({"contributionId":"pool_1","succession":"renewal"})).unwrap().succession, Some(Succession::Renewal));
        let s = parsed(json!({"contributionId":"pool_1","applicants":["@b:test","@a:test","@b:test"]})).unwrap();
        assert_eq!(s.applicants.as_deref(), Some(&["@a:test".to_owned(), "@b:test".to_owned()][..]));
        assert!(s.check_applicants("@owner:test").is_ok());
        assert_eq!(s.check_applicants("@owner:other.test"), Err("invalid_management_arguments".into()));
        let many: Vec<String> = (0..65).map(|n| format!("@m{n}:test")).collect();
        for bad in [
            json!({"intentId":"original_1","choosePeople":true}),
            json!({"intentId":"original_1","succession":"amendment"}),
            json!({"contributionId":"pool_1","succession":"revoke"}),
            json!({"contributionId":"pool_1","succession":"Amendment"}),
            json!({"contributionId":"pool_1","succession":true}),
            json!({"intentId":"original_1","applicants":["@a:test"]}),
            json!({"contributionId":"pool_1","applicants":[]}),
            json!({"contributionId":"pool_1","applicants":["a:test"]}),
            json!({"contributionId":"pool_1","applicants":["@:test"]}),
            json!({"contributionId":"pool_1","applicants":["@a:"]}),
            json!({"contributionId":"pool_1","applicants":["@a b:test"]}),
            json!({"contributionId":"pool_1","applicants":"@a:test"}),
            json!({"contributionId":"pool_1","applicants":many}),
            json!({"contributionId":"pool_1","applicants":["@a:test"],"eligibleManagers":["@a:test"]}),
        ] {
            assert!(parsed(bad.clone()).is_err(), "{bad}");
        }
    }
    #[test]
    fn native_recurring_calendar_binds_owner_zone_and_period_end() {
        use hagency_core::recurring_quota::{Calendar,Cadence};
        let calendar=Calendar{cadence:Cadence::Weekly,time_zone:"America/Los_Angeles".into()};
        let choices=GrantChoices{ per_request: String::new(),coordinator:"@owner:test".into(),managers:"@chris:test".into(),tokens:"2000".into(),calendar:Some(calendar.clone()),..Default::default()};
        let parsed=choices.clone().parse().unwrap();
        assert_eq!(parsed.calendar,Some(calendar.clone()));
        assert_eq!(parsed.delegation_expires_at_ms,calendar.period_at(now_ms()).unwrap().end_ms);
        let mut bad=choices;bad.calendar.as_mut().unwrap().time_zone="PST".into();
        assert!(bad.parse().is_err());
    }
    #[test]
    fn native_choices_require_explicit_utc_and_bounded_budget() {
        let input = |expiry: &str, tokens: &str| GrantChoices { renewal_mode: None, per_request: String::new(),
            coordinator: "@coordinator:test".into(),
            managers: "@b:test\n@a:test\n@b:test".into(),
            tokens: tokens.into(),
            expires_utc: expiry.into(),
            self_approval: false,
            calendar: None,
        };
        let future = (chrono::Utc::now() + chrono::Duration::days(1))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let parsed = input(&future, "700").parse().unwrap();
        assert_eq!(parsed.eligible_managers, ["@a:test", "@b:test"]);
        assert!(!parsed.allow_self_approval);
        assert!(input("2030-01-01T08:00:00+08:00", "700").parse().is_err());
        assert!(input(&future, "0").parse().is_err());
        assert!(input(&future, "9007199254740992").parse().is_err());
    }
    #[test]
    fn bundle_permission_mapping_never_grants_owner_consent() {
        let authority = rinx_miniapp_core::SessionAuthority::default();
        let lease = authority.issue(
            crate::miniapps::InstanceId {
                app: octosense_app_contract::hagency::APP_ID.into(),
                account: "@owner:test".into(),
                room: None,
                generation: 1,
            },
            [
                REVIEW_GRANT.to_owned(),
                "hagency.onboarding.review".to_owned(),
                "hagency.onboarding.status".to_owned(),
            ]
            .into(),
            Default::default(),
            std::time::Instant::now() + std::time::Duration::from_secs(10),
        );
        assert!(super::super::permissions(&lease).is_empty());
        for capability in [
            REVIEW_GRANT,
            "hagency.onboarding.review",
            "hagency.onboarding.status",
        ] {
            assert!(octosense_app_contract::hagency::SERVICES.contains(&capability));
        }
    }
}
