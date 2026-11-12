//! Trusted native owner grant screen. The package cannot render or acknowledge
//! this confirmation; its reply contains only public progress and original IDs.
use super::*;
use hagency_core::recurring_quota::{Calendar, Cadence, RenewalMode};
use super::sheet_ui::{Fact, Figure, Person, SheetGlyph, SheetTone, SheetView};
use crate::host::hagency::grants::{
    GrantChoices, GrantFlow, GrantResult, Succession, month_ended, now_ms,
};
use ::hagency::native_owner::{
    OwnerGrantProgress, OwnerGrantState, OwnerGrantPublicationProgress, OwnerGrantPublicationState, OwnerGrantSummary, OwnerGrantForm, OwnerGrantSuccessionForm, OwnerGrantSuccessionKind,
};

use ::hagency::daemon::bridge::{OwnerGrantReview, OwnerGrantPublicationReview, OwnerGrantCancellationReview};

pub(super) struct GrantPrompt {
    flow: GrantFlow,
    reply: (usize, u64),
    phase: Phase,
    original: Option<String>,
    receiver: Option<Receiver<Result<GrantResult, String>>>,
    draft: GrantChoices,
    publishing: bool,
    cancelling: bool,
    form: Option<OwnerGrantForm>,
    /// One explicit owner submission covers reservation and publication of
    /// these same terms. Recovery never sets this flag.
    submit_publication: bool,
    /// The script's proposed tokens, shown beside the current amount until the
    /// owner submits terms natively. Only the native tokens field commits.
    proposal: Option<u64>,
    /// The owner asked for this month's limits after a grant's month ended.
    regrant: bool,
    /// While choosing an amendment or renewal: the native successor facts.
    /// `form` then stays empty; the Choose phase reads one or the other.
    successor: Option<OwnerGrantSuccessionForm>,
    /// The owner submitted terms here; the result shown is theirs.
    submitted: bool,
    /// Whether the owner opened the sharing period, the details or a folded list.
    disclosure: Disclosure,
    /// Why the person last typed into the add field wasn't added.
    add_error: String,
    /// The owner chose to add people from this month's record in this window.
    add_people: bool,
    /// Each lookup owns its receiver. Replacing it on typing or reopening
    /// drops late results, so only the visible search can supply a selection.
    people_search: PeopleSearch,
}

#[derive(Default)]
struct PeopleSearch {
    query: String,
    selected: Option<String>,
    receiver: Option<Receiver<Result<crate::host::hagency::grant_people::People, String>>>,
    result: Option<crate::host::hagency::grant_people::People>,
    error: Option<String>,
}
impl PeopleSearch {
    const VISIBLE_LIMIT: usize = 6;
    fn candidates(&self, selected: &[String]) -> Vec<crate::host::hagency::grant_people::Person> {
        self.result.as_ref().map(|r| r.items.iter()
            .filter(|p| !selected.contains(&p.id)).take(Self::VISIBLE_LIMIT).cloned().collect()).unwrap_or_default()
    }
    fn limited(&self, selected: &[String]) -> bool {
        self.result.as_ref().is_some_and(|r| r.limited || r.items.iter()
            .filter(|p| !selected.contains(&p.id)).count() > Self::VISIBLE_LIMIT)
    }
}
enum Phase {
    Loading,
    Choose,
    Review(Box<OwnerGrantReview>),
    Progress(OwnerGrantProgress),
    PublicationReview(Box<OwnerGrantPublicationReview>),
    PublicationProgress(OwnerGrantPublicationProgress),
    CancellationReview(Box<OwnerGrantCancellationReview>),
    Failed(String),
}
fn amount(value: Option<u64>) -> String {
    value
        .map(|n| format!("{} M", crate::utils::token_millions(n)))
        .unwrap_or_else(|| crate::i18n::tr("Unavailable").into())
}
/// A month as owners read it: "10 月" in Chinese, "October" in English.
const CALENDAR_ZONES: &[&str] = &["America/Los_Angeles", "America/New_York", "Europe/London", "Europe/Berlin", "Asia/Shanghai", "Asia/Tokyo", "Asia/Singapore", "Asia/Kolkata", "Australia/Sydney", "UTC"];
fn default_calendar() -> Calendar {
    let time_zone = iana_time_zone::get_timezone().ok().filter(|zone| zone.parse::<chrono_tz::Tz>().is_ok()).unwrap_or_else(|| "UTC".into());
    Calendar {cadence: Cadence::Monthly, time_zone}
}
fn calendar_note(calendar: &Calendar, now: u64) -> String {
    let tr = crate::i18n::tr;
    let Ok(period) = calendar.period_at(now) else {return tr("Enter a valid IANA time zone, such as America/Los_Angeles.").into()};
    let Ok(zone) = calendar.time_zone.parse::<chrono_tz::Tz>() else {return String::new()};
    let at = utc_at(period.end_ms).with_timezone(&zone);
    format!("{} {} · {}. {}", tr("First reset:"), at.format("%Y-%m-%d %H:%M %Z"), calendar.time_zone, tr(if calendar.cadence == Cadence::Weekly { "Weeks start Monday. Unused tokens do not carry over; exhausted Agents wait for the next period." } else { "Unused tokens do not carry over; exhausted Agents wait for the next period." }))
}
pub(super) fn calendar_fact(calendar: &Calendar, end_ms: Option<u64>, now: u64) -> Fact {
    let tr=crate::i18n::tr;
    let mut note=calendar.time_zone.clone();
    if let (Some(end),Ok(zone))=(end_ms.or_else(|| calendar.period_at(now).ok().map(|p|p.end_ms)),calendar.time_zone.parse::<chrono_tz::Tz>()) {
        note=format!("{} · {} {}",note,tr("Period ends:"),utc_at(end).with_timezone(&zone).format("%Y-%m-%d %H:%M %Z"));
    }
    Fact::new(tr("Authorization calendar"),tr(match calendar.cadence {Cadence::Daily=>"Daily",Cadence::Weekly=>"Weekly",Cadence::Monthly=>"Monthly"})).note(note)
}
fn month_name(month: u32) -> String {
    const NAMES: [&str; 12] = [
        "January", "February", "March", "April", "May", "June", "July", "August",
        "September", "October", "November", "December",
    ];
    if crate::i18n::language() == crate::i18n::Language::Chinese {
        format!("{month} 月")
    } else {
        NAMES[(month.clamp(1, 12) - 1) as usize].to_owned()
    }
}
/// The UTC month a grant covers, from its period key ("2026-10").
fn key_month(key: &str) -> Option<String> {
    key.get(5..7)?.parse::<u32>().ok().filter(|m| (1..=12).contains(m)).map(month_name)
}
/// The UTC month that contains `ms`, which is the month a new grant covers.
fn ms_month(ms: u64) -> String {
    use chrono::Datelike;
    let month = i64::try_from(ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map_or(1, |date| date.month());
    month_name(month)
}
/// A day and time in the owner's own time zone, named as the pool's page
/// names it: "11 月 1 日 08:00（北京时间）" / "November 1, 8:00 AM (Beijing time)".
fn local_time(ms: u64) -> String {
    let Some(date) = i64::try_from(ms).ok().and_then(chrono::DateTime::from_timestamp_millis) else {
        return crate::i18n::tr("Unavailable").into();
    };
    let zone = crate::host::hagency::local_zone::label(Some(ms));
    let date = date.with_timezone(&chrono::Local);
    if crate::i18n::language() == crate::i18n::Language::Chinese {
        format!("{}（{zone}）", date.format("%-m 月 %-d 日 %H:%M"))
    } else {
        format!("{} ({zone})", date.format("%B %-d, %-I:%M %p"))
    }
}
/// A day and time in the owner's own time zone without naming the zone, for
/// a big figure whose sentence names it: "10 月 31 日 16:37" / "Oct 31, 4:37 PM".
fn local_day_time(ms: u64) -> String {
    let Some(date) = i64::try_from(ms).ok().and_then(chrono::DateTime::from_timestamp_millis) else {
        return crate::i18n::tr("Unavailable").into();
    };
    let date = date.with_timezone(&chrono::Local);
    if crate::i18n::language() == crate::i18n::Language::Chinese {
        date.format("%-m 月 %-d 日 %H:%M").to_string()
    } else {
        date.format("%b %-d, %-I:%M %p").to_string()
    }
}
/// When sharing ends, said the same way in every window and state, in the
/// owner's own time zone: "11 月 1 日 08:00（北京时间）前" for a UTC month end in UTC+8.
fn ends_at(ms: u64) -> String {
    crate::i18n::format("Before {when}", &[("when", local_time(ms))])
}
/// The end of sharing a draft would commit, else the end of its month.
fn draft_end(bounds: &Bounds, expires_utc: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(expires_utc.trim())
        .ok()
        .and_then(|at| u64::try_from(at.timestamp_millis()).ok())
        .unwrap_or(bounds.expiry_until_ms)
}
/// A person on the list as the mini app shows them: "@lisi". Every applicant
/// is on the owner's own server, so the server part only appears in details.
fn person(id: &str) -> &str {
    id.split_once(':').map_or(id, |(name, _)| name)
}
/// Up to three people by name, then a count, for one-line sentences.
fn people_text(ids: &[String]) -> String {
    let separator = if crate::i18n::language() == crate::i18n::Language::Chinese { "、" } else { ", " };
    let names: Vec<&str> = ids.iter().map(|id| person(id)).collect();
    if names.len() <= 3 {
        return names.join(separator);
    }
    crate::i18n::format(
        "{first} and {more} more",
        &[("first", names[..2].join(separator)), ("more", (names.len() - 2).to_string())],
    )
}
/// Matrix IDs from the native applicants field, in the order they are kept.
fn managers_of(text: &str) -> Vec<String> {
    let mut managers: Vec<String> = text.split_whitespace().map(str::to_owned).collect();
    managers.sort();
    managers.dedup();
    managers
}
fn publication_period_end() -> chrono::DateTime<chrono::Utc> {
    publication_period_end_at(now_ms())
}
/// The end of the UTC month that contains `now`.
fn publication_period_end_at(now: u64) -> chrono::DateTime<chrono::Utc> {
    use chrono::Datelike;
    let first = utc_at(now).date_naive().with_day(1).unwrap();
    (first + chrono::Duration::days(32)).with_day(1).unwrap().and_hms_opt(0,0,0).unwrap().and_utc()
}
fn utc_at(now: u64) -> chrono::DateTime<chrono::Utc> {
    i64::try_from(now).ok().and_then(chrono::DateTime::from_timestamp_millis).unwrap_or_else(chrono::Utc::now)
}
fn publication_expiry(days: Option<i64>) -> String {
    publication_expiry_at(days, now_ms())
}
fn publication_expiry_at(days: Option<i64>, now: u64) -> String {
    let end = publication_period_end_at(now);
    days.map(|days| (utc_at(now)+chrono::Duration::days(days)).min(end)).unwrap_or(end)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
fn publication_preset_available(days: Option<i64>) -> bool {
    publication_preset_available_at(days, now_ms())
}
fn publication_preset_available_at(days: Option<i64>, now: u64) -> bool {
    days.is_none_or(|n| utc_at(now)+chrono::Duration::days(n)<=publication_period_end_at(now))
}
/// What the native choose form may commit, from a new grant's form or from
/// an amendment or renewal form. The writer re-checks every bound natively.
#[derive(Clone, Debug, PartialEq)]
struct Bounds {
    /// Most tokens the grant may hold; None while capacity is unknown.
    remaining: Option<u64>,
    total_known: bool,
    /// Fewest tokens: 1, or what Agents carried into a successor already hold.
    minimum: u64,
    /// Applicants a successor must keep: they can be added, never removed.
    keep_managers: Vec<String>,
    /// The delegation must end after this instant (a renewal's month start)
    expiry_after_ms: u64,
    /// and no later than the end of the month being granted.
    expiry_until_ms: u64,
}
fn root_bounds(form: &OwnerGrantForm) -> Bounds {
    Bounds {
        remaining: form.context.remaining_tokens,
        total_known: form.context.resource_total_tokens.is_some(),
        minimum: 1,
        keep_managers: Vec::new(),
        expiry_after_ms: 0,
        expiry_until_ms: publication_period_end().timestamp_millis() as u64,
    }
}
fn renewal(form: &OwnerGrantSuccessionForm) -> bool {
    form.kind == OwnerGrantSuccessionKind::Renewal
}
fn successor_bounds(form: &OwnerGrantSuccessionForm) -> Bounds {
    Bounds {
        remaining: form.context.remaining_tokens,
        total_known: form.context.resource_total_tokens.is_some(),
        minimum: form.floor.retained_tokens.max(1),
        keep_managers: form.predecessor.eligible_managers.clone(),
        expiry_after_ms: if renewal(form) { form.period_start_ms } else { 0 },
        expiry_until_ms: form.period_end_ms,
    }
}
fn choices_valid(bounds: &Bounds, draft: &GrantChoices) -> bool {
    draft.clone().parse().is_ok_and(|choices| {
        bounds.total_known
            && bounds.remaining.is_some_and(|n| n >= choices.allocated_tokens)
            && choices.allocated_tokens >= bounds.minimum
            && bounds
                .keep_managers
                .iter()
                .all(|manager| choices.eligible_managers.contains(manager))
            && choices.delegation_expires_at_ms > bounds.expiry_after_ms
            && (choices.calendar.is_some() || choices.delegation_expires_at_ms <= bounds.expiry_until_ms)
    })
}
#[cfg(test)]
fn form_choices_valid(form: &OwnerGrantForm, draft: &GrantChoices) -> bool {
    choices_valid(&root_bounds(form), draft)
}
fn rfc3339(ms: u64) -> String {
    i64::try_from(ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|date| date.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}
/// The expiry an amendment or renewal preset fills in, if it is inside the
/// month being granted: `days` from now (capped at its end), or its end.
fn successor_preset(form: &OwnerGrantSuccessionForm, days: Option<i64>, now: u64) -> Option<String> {
    let bounds = successor_bounds(form);
    let value = match days {
        Some(days) => {
            let at = now.saturating_add(u64::try_from(days).ok()?.saturating_mul(86_400_000));
            if at > bounds.expiry_until_ms {
                return None;
            }
            at
        }
        None => bounds.expiry_until_ms,
    };
    (value > bounds.expiry_after_ms.max(now)).then(|| rfc3339(value))
}
/// A script proposal only seeds the native tokens field, and its applicants
/// only the applicants field; the owner can edit both and the committed values
/// are always read back from those widgets.
fn initial_draft(proposal: Option<u64>, applicants: &[String]) -> GrantChoices {
    GrantChoices { per_request: String::new(),
        tokens: proposal.map(|tokens| tokens.to_string()).unwrap_or_default(),
        managers: applicants.join(" "),
        ..GrantChoices::default()
    }
}
/// Defaults for an amendment or renewal: the current limit unless a proposal
/// filled the field, the current applicants plus any typed or proposed ones
/// (applicants are never removed), and the current delegation for an
/// amendment or the end of the new month for a renewal.
fn fill_successor_defaults(draft: &mut GrantChoices, form: &OwnerGrantSuccessionForm, now: u64) {
    draft.calendar = form.predecessor.calendar.clone();
    draft.renewal_mode = form.predecessor.renewal_mode.or_else(|| draft.calendar.as_ref().map(|_| RenewalMode::Automatic));
    if draft.tokens.is_empty() {
        draft.tokens = form.predecessor.allocated_tokens.to_string();
    }
    // A value from the pool's page below what running Agents hold could never
    // be confirmed here: the form starts at that floor and says why.
    if let Ok(tokens) = draft.tokens.trim().parse::<u64>() {
        draft.tokens = in_range(tokens, &successor_bounds(form)).to_string();
    }
    // Legacy grants have no independent Agent cap. Their default follows the
    // proposed total; an explicit V5 cap or a caller's proposal stays intact.
    if draft.per_request.is_empty() {
        draft.per_request = form.predecessor.per_request_tokens
            .map(|n| n.to_string()).unwrap_or_else(|| draft.tokens.clone());
    }
    let mut managers: Vec<String> = form
        .predecessor
        .eligible_managers
        .iter()
        .cloned()
        .chain(draft.managers.split_whitespace().map(str::to_owned))
        .collect();
    managers.sort();
    managers.dedup();
    draft.managers = managers.join(" ");
    let current = form.predecessor.delegation_expires_at_ms;
    draft.expires_utc = if !renewal(form) && current > now && current <= form.period_end_ms {
        rfc3339(current)
    } else {
        rfc3339(form.period_end_ms)
    };
}
/// `tokens` raised to what running Agents hold, or lowered to what is left,
/// whichever bound it is outside. Unknown capacity bounds nothing above.
fn in_range(tokens: u64, bounds: &Bounds) -> u64 {
    let top = bounds.remaining.filter(|left| *left >= bounds.minimum);
    match top {
        Some(top) => tokens.clamp(bounds.minimum, top),
        None => tokens.max(bounds.minimum),
    }
}
/// `tokens` when the form's bounds allow it: no less than what running
/// Agents hold and no more than what is left.
fn allowed_amount(bounds: Option<&Bounds>, tokens: Option<u64>) -> Option<u64> {
    tokens.filter(|n| bounds.is_some_and(|b| *n >= b.minimum && b.remaining.is_some_and(|left| *n <= left)))
}
/// The amount can't move: what running Agents hold is all this month can
/// give out, so the floor is also the top.
fn fixed_amount(bounds: &Bounds) -> Option<u64> {
    bounds.remaining.filter(|left| *left == bounds.minimum && bounds.minimum > 1)
}
/// An amendment that moves when sharing ends. Compared to the second, as the
/// presets and the record write it.
fn sharing_moved(form: &OwnerGrantSuccessionForm, draft: &GrantChoices) -> bool {
    !renewal(form)
        && chrono::DateTime::parse_from_rfc3339(draft.expires_utc.trim()).is_ok()
        && draft_end(&successor_bounds(form), &draft.expires_utc) / 1000
            != form.predecessor.delegation_expires_at_ms / 1000
}
/// Defaults for fields that are still empty when the form opens. A proposal
/// is kept exactly, even above the available capacity: the form then shows it
/// and stays unconfirmable instead of silently lowering it.
fn fill_form_defaults(draft: &mut GrantChoices, form: &OwnerGrantForm) {
    if form.calendar.is_some() {draft.calendar = form.calendar.clone();}
    if draft.calendar.is_some() && draft.renewal_mode.is_none() { draft.renewal_mode = Some(RenewalMode::Automatic); }
    if draft.tokens.is_empty() {
        draft.tokens = form.context.resource_total_tokens.unwrap_or(0).to_string();
    }
    if draft.per_request.is_empty() { draft.per_request = draft.tokens.clone(); }
    if draft.expires_utc.is_empty() {
        draft.expires_utc = publication_expiry(None);
    }
}
/// Whether a phase offers the native primary action. A proposal never commits
/// anything here: beside a published grant it can only open the native
/// amendment form (`amendable`), and only when the server supports it.
/// A grant whose UTC month ended can be followed by this month's grant.
fn primary_available(
    phase: &Phase,
    bounds: Option<&Bounds>,
    draft: &GrantChoices,
    has_original: bool,
    can_choose_new_limits: bool,
    amendable: bool,
    now: u64,
) -> bool {
    if ended_grant(phase, now) {
        return can_choose_new_limits;
    }
    if published_grant(phase, now) {
        return amendable && can_choose_new_limits && live_grant_tokens(phase, now).is_some();
    }
    match phase {
        Phase::Choose => bounds.is_some_and(|b| choices_valid(b, draft)),
        Phase::Review(review) => !review.expired() && review.capacity_available(),
        Phase::Progress(progress)
            if progress.closure.is_some() && matches!(progress.state, OwnerGrantState::Recorded) =>
        {
            can_choose_new_limits
        }
        Phase::Progress(progress) => {
            matches!(progress.state, OwnerGrantState::ReviewRequired | OwnerGrantState::Recorded)
                || (matches!(progress.state, OwnerGrantState::Declined) && can_choose_new_limits)
        }
        Phase::PublicationReview(review) => !review.expired(),
        Phase::CancellationReview(review) => !review.expired(),
        Phase::Failed(code) => {
            has_original
                && matches!(
                    code.as_str(),
                    "owner_grant_review_changed"
                        | "owner_grant_original_review_expired"
                        | "owner_grant_publication_review_expired"
                )
        }
        _ => false,
    }
}
/// A recorded grant whose UTC month has ended. The pool has no capacity from
/// it any more, and this month's grant may follow it on the same admission.
fn ended_grant(phase: &Phase, now: u64) -> bool {
    match phase {
        Phase::PublicationProgress(p) => month_ended(&p.summary, now),
        Phase::Progress(p) => {
            matches!(p.state, OwnerGrantState::Recorded)
                && p.closure.is_none()
                && month_ended(&p.summary, now)
        }
        _ => false,
    }
}
/// The server has accepted a grant for this pool's current admission and
/// month. It accepts only one per admission each month, even after
/// revocation, so a proposal is never applied here and nothing is revoked to
/// make room. Once that month has ended the grant no longer blocks a proposal.
fn published_grant(phase: &Phase, now: u64) -> bool {
    matches!(phase, Phase::PublicationProgress(p) if matches!(p.state, OwnerGrantPublicationState::ServerObserved))
        && !ended_grant(phase, now)
}
/// The pool's current pool total allowance: the tokens of a grant the server last
/// reported as active whose month has started. Reserved, rejected, revoked,
/// superseded, expired or scheduled grants set none.
fn live_grant_tokens(phase: &Phase, now: u64) -> Option<u64> {
    match phase {
        Phase::PublicationProgress(p)
            if published_grant(phase, now)
                && p.server_state.as_deref() == Some("active")
                && p.summary.period_start_ms <= now =>
        {
            Some(p.summary.allocated_tokens)
        }
        _ => None,
    }
}
/// What the mini app learns when the panel closes. A proposal beside this
/// month's published grant on a server that cannot amend it was never
/// applied: say why. Otherwise the reply is the public progress shown.
fn close_reply(
    phase: &Phase,
    proposal: Option<u64>,
    in_flight: bool,
    original: Option<&str>,
    succession_supported: bool,
    now: u64,
) -> Result<String, String> {
    if proposal.is_some() && published_grant(phase, now) && !succession_supported {
        return Err("pool_grant_already_published".into());
    }
    match phase {
        Phase::Progress(p) => serde_json::to_string(p).map_err(|_| "management_outcome_unknown".to_owned()),
        Phase::PublicationProgress(p) => serde_json::to_string(p).map_err(|_| "management_outcome_unknown".to_owned()),
        _ => Ok(json!({"state":if in_flight {"outcome_unknown"} else {"review_closed"},"intentId":original}).to_string()),
    }
}
/// An unattempted original reserves nothing; discarding it reopens the form.
const PENDING_NOTE: &str = "An unfinished publish is waiting, so this amount isn’t used yet. Discard that publish first, then choose again.";
/// One grant per admission per UTC month, even after revocation, on a server
/// without succession support.
const PUBLISHED_NOTE: &str = "This pool is already published this month and the server takes one publish a month, so this amount wasn’t applied. Nothing was withdrawn or changed.";
/// Beside a published grant on a server that supports amendments.
const AMENDABLE_NOTE: &str = "Not applied yet. Tap Change Pool Limits to continue; what running Agents hold stays as it is.";
/// Beside a revoked, superseded or expired grant this month.
const NOT_AMENDABLE_NOTE: &str = "This publish no longer applies, so it can’t be changed.";
/// Beside next month's scheduled renewal: this month can no longer change here.
const SCHEDULED_NOTE: &str = "Next month is already published ahead, so this month’s pool total allowance can’t change now.";
/// A published grant whose month has not started: a scheduled renewal.
fn scheduled_grant(phase: &Phase, now: u64) -> bool {
    matches!(phase, Phase::PublicationProgress(p) if p.summary.period_start_ms > now)
        && published_grant(phase, now)
}
/// Shown for a grant whose month ended: the pool is closed until the owner
/// publishes this month's grant, which needs last month's Agents settled.
const ENDED_LEAD: &str = "Last month’s capacity has ended, so this pool has none right now.";
const ENDED_STATUS: &str = "Set this month’s capacity to publish again. Last month’s Agents must first stop and report their final use, which needs this computer online after the month ends.";
/// This month's form cannot show capacity while last month's usage is unknown.
const REGRANT_UNKNOWN_STATUS: &str = "Last month’s Agent usage is not settled. Check the execution records and refresh after settlement. This month has not been published.";
const UNKNOWN_CAPACITY_STATUS: &str = "Available capacity could not be verified. Refresh to read the pool again. Nothing has been published.";

fn capacity_problem(s: &GrantState) -> String {
    let tr = crate::i18n::tr;
    let context = s.context();
    tr(match context.and_then(|c| c.capacity_blocker.as_deref()) {
        Some("pool_limit_missing") => "Set a monthly maximum for this pool before publishing.",
        Some("usage_unsettled") => "Final usage is still unsettled for calls covered by this pool’s budget. Check the Agent execution records, then refresh after settlement.",
        Some("account_period_mismatch") => "A shared account limit requires all its pools to use UTC and the same period. Review the account limit or use a compatible pool.",
        Some("account_limit_reached") => "The explicitly declared shared account limit is fully allocated. Review its existing allocations before publishing more capacity.",
        Some("pool_limit_reached") => "This pool has no capacity left to publish. Review its existing allocations or change its monthly maximum.",
        _ if s.regrant => REGRANT_UNKNOWN_STATUS,
        _ if context.is_some_and(|c| c.remaining_tokens == Some(0)) => "This pool has no capacity left to publish. Review its existing allocations or change its monthly maximum.",
        _ => UNKNOWN_CAPACITY_STATUS,
    }).into()
}

/// What the window is about, named the way the mini app names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GrantKind {
    /// This month's publish of a pool.
    Publish,
    /// A new pool total allowance this month.
    Amend,
    /// People added to this month's list, the limit unchanged.
    AddPeople,
    /// Next month, published ahead.
    Renew,
    /// An existing record opened by itself, such as this month's list.
    Record,
}
/// The sheet's disclosures, which start closed: the technical details, a
/// list of people that this window doesn't change, and the sharing period's
/// choice under its row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Disclosure {
    pub more: bool,
    pub people: bool,
    pub sharing: bool,
}
/// The native facts one rendering is computed from. The flow's facts are
/// copied in, so the shipped panel and its offline preview share this code.
pub(super) struct GrantState<'a> {
    phase: &'a Phase,
    draft: &'a GrantChoices,
    proposal: Option<u64>,
    form: Option<&'a OwnerGrantForm>,
    successor: Option<&'a OwnerGrantSuccessionForm>,
    requested: Option<Succession>,
    applicants: &'a [String],
    account: &'a str,
    original: bool,
    in_flight: bool,
    /// The owner submitted terms in this window, so a result is theirs.
    submitted: bool,
    /// That submission is still moving through its native reviews.
    submitting: bool,
    publishing: bool,
    cancelling: bool,
    regrant: bool,
    active: bool,
    can_choose_new_limits: bool,
    succession_supported: Option<bool>,
    disclosure: Disclosure,
    /// Why the last person typed into the add field wasn't added.
    add_error: &'a str,
    /// The owner chose to add people from this month's record here.
    add_people: bool,
}
impl GrantState<'_> {
    fn actionable(&self) -> bool {
        !self.in_flight && self.active
    }
    /// The bounds of whichever native form the Choose phase shows.
    fn bounds(&self) -> Option<Bounds> {
        match (self.successor, self.form) {
            (Some(form), _) => Some(successor_bounds(form)),
            (None, Some(form)) => Some(root_bounds(form)),
            _ => None,
        }
    }
    /// A proposal beside this month's published grant may open the native
    /// amendment form, but only once the server is known to support it.
    fn amendable(&self) -> bool {
        self.proposal.is_some() && self.succession_supported == Some(true)
    }
    /// An amendment confirms only a real change: a new limit, new people or
    /// a new end of sharing.
    fn changes_something(&self) -> bool {
        match (self.phase, self.successor) {
            (Phase::Choose, Some(form)) if !renewal(form) => {
                self.draft.tokens.trim().parse::<u64>().ok() != Some(form.predecessor.allocated_tokens)
                    || (!self.draft.per_request.trim().is_empty() && self.draft.per_request.trim().parse::<u64>().ok() != Some(form.predecessor.per_request_tokens.unwrap_or(form.predecessor.allocated_tokens)))
                    || !added_people(form, &managers_of(&self.draft.managers)).is_empty()
                    || sharing_moved(form, self.draft)
            }
            _ => true,
        }
    }
    fn can_primary(&self, now: u64) -> bool {
        self.actionable()
            && primary_available(
                self.phase,
                self.bounds().as_ref(),
                self.draft,
                self.original,
                self.can_choose_new_limits,
                self.amendable(),
                now,
            )
            && self.changes_something()
    }
    fn can_recover(&self) -> bool {
        self.actionable()
            && !matches!(self.phase, Phase::Loading)
            && !matches!(self.phase, Phase::Review(review) if review.capacity_available())
            && !matches!(self.phase, Phase::PublicationReview(review) if !review.expired())
            && !matches!(self.phase, Phase::CancellationReview(review) if !review.expired())
    }
    fn can_cancel_intent(&self) -> bool {
        self.actionable()
            && self.original
            && !self.publishing
            && !self.cancelling
            && match self.phase {
                Phase::Review(_) | Phase::Failed(_) => true,
                Phase::Progress(p) => matches!(
                    p.state,
                    OwnerGrantState::ReviewRequired | OwnerGrantState::ReviewExpired
                ),
                _ => false,
            }
    }
    /// The form's facts: the pool's monthly maximum and how much this grant may hold.
    fn context(&self) -> Option<&::hagency::native_owner::OwnerGrantReviewContext> {
        match (self.successor, self.form, self.phase) {
            (Some(form), _, _) => Some(&form.context),
            (None, Some(form), _) => Some(&form.context),
            (None, None, Phase::Review(review)) => Some(review.context()),
            _ => None,
        }
    }
}
/// Everything the window shows, already in the owner's language.
pub(super) type GrantView = SheetView;
fn phase_summary(phase: &Phase) -> Option<OwnerGrantSummary> {
    match phase {
        Phase::Review(review) => Some(review.summary()),
        Phase::Progress(p) => Some(p.summary.clone()),
        Phase::PublicationReview(review) => Some(review.summary()),
        Phase::PublicationProgress(p) => Some(p.summary.clone()),
        Phase::CancellationReview(review) => Some(review.summary()),
        _ => None,
    }
}
/// People on `managers` who are not on the published list being changed.
fn added_people(form: &OwnerGrantSuccessionForm, managers: &[String]) -> Vec<String> {
    managers
        .iter()
        .filter(|id| !form.predecessor.eligible_managers.contains(id))
        .cloned()
        .collect()
}
fn grant_kind(s: &GrantState) -> GrantKind {
    if let Some(form) = s.successor {
        if renewal(form) {
            return GrantKind::Renew;
        }
        let (tokens, managers) = match phase_summary(s.phase) {
            Some(summary) if summary.succession.is_some() => {
                (Some(summary.allocated_tokens), summary.eligible_managers)
            }
            _ => (s.draft.tokens.trim().parse::<u64>().ok(), managers_of(&s.draft.managers)),
        };
        let only_people = tokens == Some(form.predecessor.allocated_tokens)
            && (!added_people(form, &managers).is_empty() || !s.applicants.is_empty() || s.add_people);
        return if only_people { GrantKind::AddPeople } else { GrantKind::Amend };
    }
    match s.requested {
        Some(Succession::Renewal) => GrantKind::Renew,
        Some(Succession::Amendment) if !s.applicants.is_empty() || s.add_people => GrantKind::AddPeople,
        Some(Succession::Amendment) => GrantKind::Amend,
        None if !s.can_choose_new_limits => GrantKind::Record,
        None => GrantKind::Publish,
    }
}
/// The month a window is about: the record's, the form's, or this UTC month
/// for a new publish. Unknown for a renewal whose form has not loaded.
fn grant_month(s: &GrantState, summary: Option<&OwnerGrantSummary>, now: u64) -> Option<String> {
    if let Some(summary) = summary {
        return key_month(&summary.period_key);
    }
    if let Some(form) = s.successor {
        return key_month(&form.period_key);
    }
    match s.requested {
        Some(Succession::Renewal) => None,
        _ => Some(ms_month(now)),
    }
}
/// The icon a proposal of this kind shows.
fn kind_glyph(kind: GrantKind) -> SheetGlyph {
    match kind {
        GrantKind::Publish | GrantKind::Record => SheetGlyph::Publish,
        GrantKind::Amend => SheetGlyph::Change,
        GrantKind::AddPeople => SheetGlyph::People,
        GrantKind::Renew => SheetGlyph::Renew,
    }
}
/// What the window does (a proposal), what it did, or what it didn't do.
fn grant_title(kind: GrantKind, tone: SheetTone, s: &GrantState, month: Option<&str>) -> String {
    let tr = crate::i18n::tr;
    let fmt = |key: &str, month: &str| crate::i18n::format(key, &[("month", month.to_owned())]);
    if matches!(s.phase, Phase::CancellationReview(_)) {
        return tr("Discard Unfinished Publish").into();
    }
    match (tone, kind, month) {
        (SheetTone::Done, GrantKind::Publish, Some(month)) => fmt("Published {month}", month),
        (SheetTone::Done, GrantKind::Publish, None) => tr("Pool Published").into(),
        (SheetTone::Done, GrantKind::Amend, _) => tr("Pool Limits Saved").into(),
        (SheetTone::Done, GrantKind::AddPeople, _) => tr("People Added").into(),
        (SheetTone::Done, GrantKind::Renew, Some(month)) => fmt("Published {month} Ahead", month),
        (SheetTone::Done, GrantKind::Renew, None) => tr("Next Month Published Ahead").into(),
        (SheetTone::Done, GrantKind::Record, Some(month)) => fmt("{month} Is Published", month),
        (SheetTone::Done, GrantKind::Record, None) => tr("This Pool Is Published").into(),
        (SheetTone::Failed, GrantKind::Publish | GrantKind::Record, Some(month)) => fmt("{month} Wasn’t Published", month),
        (SheetTone::Failed, GrantKind::Publish | GrantKind::Record, None) => tr("Not Published").into(),
        (SheetTone::Failed, GrantKind::Amend, _) => tr("Pool Limits Not Saved").into(),
        (SheetTone::Failed, GrantKind::AddPeople, _) => tr("No One Was Added").into(),
        (SheetTone::Failed, GrantKind::Renew, Some(month)) => fmt("{month} Wasn’t Published Ahead", month),
        (SheetTone::Failed, GrantKind::Renew, None) => tr("Next Month Wasn’t Published Ahead").into(),
        (_, GrantKind::Publish, Some(month)) => fmt("Publish {month}", month),
        (_, GrantKind::Publish, None) => tr("Publish This Pool").into(),
        (_, GrantKind::Amend, _) => tr("Change Pool Limits").into(),
        (_, GrantKind::AddPeople, _) => tr("Add People Who Can Request").into(),
        (_, GrantKind::Renew, Some(month)) => fmt("Publish {month} Ahead", month),
        (_, GrantKind::Renew, None) => tr("Publish Next Month Ahead").into(),
        (_, GrantKind::Record, Some(month)) => fmt("{month} Capacity", month),
        (_, GrantKind::Record, None) => tr("This Pool’s Capacity").into(),
    }
}
/// The pool and its model, so the owner knows which pool this is.
fn pool_line(s: &GrantState) -> String {
    let (label, model) = match (s.successor, s.form, s.phase) {
        (Some(form), _, _) => (form.label.clone(), form.context.model.clone()),
        (None, Some(form), _) => (form.label.clone(), form.context.model.clone()),
        (None, None, Phase::Review(review)) => (String::new(), review.context().model.clone()),
        _ => (String::new(), String::new()),
    };
    match (label.is_empty(), model.is_empty()) {
        (false, false) if label != model => format!("{label} · {model}"),
        (false, _) => label,
        (true, false) => model,
        (true, true) => String::new(),
    }
}
/// "3 people" / "3 人".
fn people_count(count: usize) -> String {
    if count == 1 {
        crate::i18n::tr("1 person").into()
    } else {
        crate::i18n::format("{count} people", &[("count", count.to_string())])
    }
}
/// What running Agents were given, as a row value: "0.1 M · 1 Agent".
fn held_value(retained: u64, children: u64) -> String {
    if children == 0 && retained == 0 {
        return crate::i18n::tr("None").into();
    }
    let agents = if children == 1 {
        crate::i18n::tr("1 Agent").to_owned()
    } else {
        crate::i18n::format("{count} Agents", &[("count", children.to_string())])
    };
    format!("{} · {agents}", amount(Some(retained)))
}
/// The first thing wrong with what the native fields hold, said beside them.
/// It mirrors `choices_valid`; the writer still re-checks everything.
fn draft_problem(bounds: &Bounds, draft: &GrantChoices, renewing: Option<&str>, now: u64) -> Option<String> {
    let tr = crate::i18n::tr;
    let fmt = crate::i18n::format;
    let tokens = match draft.tokens.trim().parse::<u64>() {
        Ok(0) | Err(_) => return Some(tr("Enter a pool total allowance above 0, for example 0.25.").into()),
        Ok(tokens) => tokens,
    };
    if let Some(remaining) = bounds.remaining.filter(|remaining| tokens > *remaining) {
        return Some(fmt("The pool total allowance can be at most {max}.", &[("max", amount(Some(remaining)))]));
    }
    if tokens < bounds.minimum {
        return Some(fmt(
            "It can’t be lower than the {held} held by running Agents.",
            &[("held", amount(Some(bounds.minimum)))],
        ));
    }
    let managers = managers_of(&draft.managers);
    if managers.is_empty() {
        return Some(tr("Add at least one person who can request.").into());
    }
    if managers.len() > 64 {
        return Some(tr("At most 64 people can be on the list.").into());
    }
    if managers.iter().any(|id| !crate::host::hagency::grants::matrix_user(id)) {
        return Some(tr("Someone on the list isn’t a valid account. Remove them and add them again.").into());
    }
    if !bounds.keep_managers.iter().all(|id| managers.contains(id)) {
        return Some(tr("This month’s list can only grow: keep everyone already on it.").into());
    }
    let expiry = chrono::DateTime::parse_from_rfc3339(draft.expires_utc.trim())
        .ok()
        .filter(|at| at.offset().local_minus_utc() == 0)
        .and_then(|at| u64::try_from(at.timestamp_millis()).ok());
    match expiry {
        None => Some(tr("Choose when sharing ends under Sharing Period.").into()),
        Some(at) if at <= now.max(bounds.expiry_after_ms) || at > bounds.expiry_until_ms => Some(match renewing {
            Some(month) => fmt("Sharing must end within {month}.", &[("month", month.to_owned())]),
            None => tr("Sharing must end within this month.").into(),
        }),
        Some(_) => None,
    }
}
/// Sentences joined the way each language writes them.
fn sentences(parts: &[String]) -> String {
    let separator = if crate::i18n::language() == crate::i18n::Language::Chinese { "" } else { " " };
    parts.iter().filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>().join(separator)
}
/// Codes refused before anything was written: capacity and the list stay.
fn unchanged_failure(code: &str) -> bool {
    matches!(
        code,
        "invalid_grant_budget"
            | "invalid_grant_expiry"
            | "invalid_grant_calendar"
            | "invalid_grant_manager"
            | "owner_grant_capacity_unavailable"
            | "management_grant_succession_required"
            | "pool_grant_succession_unavailable"
            | "owner_grant_predecessor_required"
            | "grant_predecessor_not_effective"
            | "owner_grant_succession_pending"
            | "owner_grant_successor_exists"
            | "grant_successor_exists"
            | "owner_grant_renewal_window_closed"
            | "grant_succession_window_closed"
            | "owner_grant_below_floor"
            | "grant_successor_below_reserved"
            | "owner_grant_manager_removed"
            | "management_monthly_regrant_required"
            | "management_admission_receipts_required"
            | "owner_grant_prior_intent_required"
            | "owner_grant_new_admission_required"
            | "management_features_invalid"
    )
}
/// Refusals the owner can fix in this window by choosing again.
fn fixable_failure(code: &str) -> bool {
    matches!(
        code,
        "invalid_grant_budget"
            | "invalid_grant_expiry"
            | "invalid_grant_calendar"
            | "invalid_grant_manager"
            | "owner_grant_capacity_unavailable"
            | "owner_grant_below_floor"
            | "grant_successor_below_reserved"
            | "owner_grant_manager_removed"
    )
}
/// Failures whose outcome isn't known: the record must be checked first.
fn unknown_failure(code: &str) -> bool {
    !unchanged_failure(code)
        && !matches!(
            code,
            "owner_grant_original_review_expired"
                | "daemon_review_expired"
                | "daemon_grant_review_expired"
                | "daemon_bridge_lease_expired"
                | "owner_grant_publication_review_expired"
                | "native_grant_not_publishable"
                | "owner_grant_publication_history_unavailable"
                | "owner_grant_publication_review_changed"
                | "owner_grant_review_changed"
                | "management_enrollment_changed"
                | "contribution_not_admitted"
                | "management_scope_changed"
                | "management_access_required"
        )
}
/// The collapsed technical details: who and where, for a server
/// administrator. Internal IDs, revisions, local profiles and UTC month
/// bounds stay out; everything the owner decides on is said above.
fn technical_rows(s: &GrantState, summary: Option<&OwnerGrantSummary>) -> String {
    let mut rows: Vec<(&str, String)> = vec![("Your account", s.account.to_owned())];
    let server = match s.phase {
        Phase::PublicationReview(review) => Some(review.server_origin().to_owned()),
        Phase::CancellationReview(review) => Some(review.server_origin().to_owned()),
        _ => s.context().map(|c| c.server_origin.clone()),
    };
    if let Some(server) = server {
        rows.push(("Server", server));
    }
    let pool = summary
        .map(|summary| summary.contribution_id.clone())
        .or_else(|| s.successor.map(|form| form.contribution_id.clone()))
        .or_else(|| s.form.map(|form| form.contribution_id.clone()));
    if let Some(pool) = pool {
        rows.push(("Pool ID", pool));
    }
    // An ended publish: what its Agents kept and used, as last read.
    if let Phase::Progress(p) = s.phase {
        if let Some(closure) = &p.closure {
            rows.push(("Kept by Agents", amount(Some(closure.retained_tokens))));
            rows.push(("Confirmed use", amount(Some(closure.known_consumed_tokens))));
            rows.push(("Not yet confirmed", amount(Some(closure.unresolved_tokens))));
        }
    }
    rows.into_iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(label, value)| format!("{}\n{}", crate::i18n::tr(label), value))
        .collect::<Vec<_>>()
        .join("\n\n")
}
/// The people a sheet lists: people already on the published list first,
/// locked because the list only grows, then the people this window adds,
/// which the owner may still take off before confirming.
fn listed_people(keep: &[String], managers: &[String], successor: bool, editable: bool) -> Vec<Person> {
    let mut rows: Vec<Person> = keep
        .iter()
        .filter(|id| managers.contains(id))
        .map(|id| Person { id: id.clone(), name: person(id).to_owned(), added: false, removable: false })
        .collect();
    for id in managers.iter().filter(|id| !keep.contains(id)) {
        rows.push(Person { id: id.clone(), name: person(id).to_owned(), added: successor, removable: editable });
    }
    rows
}
/// The people a Choose sheet lists, in the order their rows are drawn.
fn choose_people(s: &GrantState) -> Vec<Person> {
    let managers = managers_of(&s.draft.managers);
    let keep = s.bounds().map(|b| b.keep_managers).unwrap_or_default();
    listed_people(&keep, &managers, s.successor.is_some(), s.actionable())
}
/// A person typed into the add field, as the full account the form commits:
/// "lisi", "@lisi" or "@lisi:server" on the owner's own server.
pub(super) fn applicant_from_input(text: &str, account: &str, current: &[String]) -> Result<String, String> {
    let tr = crate::i18n::tr;
    let server = account.split_once(':').map_or("", |(_, server)| server);
    let typed = text.trim().trim_start_matches('@');
    if typed.is_empty() {
        return Err(tr("Type a username first, for example lisi.").into());
    }
    let (name, on) = typed.split_once(':').unwrap_or((typed, server));
    if on != server {
        return Err(crate::i18n::format(
            "Only people on your server ({server}) can be added.",
            &[("server", server.to_owned())],
        ));
    }
    // Usernames are lower case, so "LiSi" can only mean @lisi.
    let name = name.to_lowercase();
    if name.is_empty()
        || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._=-/+".contains(&b))
    {
        return Err(tr("A username uses lowercase letters, digits and . _ = - / + only.").into());
    }
    let id = format!("@{name}:{server}");
    if !crate::host::hagency::grants::matrix_user(&id) {
        return Err(tr("That isn’t a valid username.").into());
    }
    if current.contains(&id) {
        return Err(crate::i18n::format("{person} is already on the list.", &[("person", format!("@{name}"))]));
    }
    if current.len() >= 64 {
        return Err(tr("At most 64 people can be on the list.").into());
    }
    Ok(id)
}
/// Facts about the pool the Choose sheet shows beside the change. Each fact
/// is said once: when the change applies is in the hero's sentence, and the
/// slider's bounds sit under the slider.
fn choose_facts(kind: GrantKind, s: &GrantState, bounds: Option<&Bounds>, month: Option<&str>) -> Vec<Fact> {
    let tr = crate::i18n::tr;
    let fmt = crate::i18n::format;
    let mut facts = Vec::new();
    // The monthly maximum bounds a publish. A change this month is bounded
    // by what's left, shown as how high it can go, and next month's lead
    // already says the maximum carries over.
    if let Some(total) = s.context().and_then(|c| c.resource_total_tokens).filter(|_| matches!(kind, GrantKind::Publish | GrantKind::Record)) {
        let value = if kind == GrantKind::Renew {
            fmt("{amount} (carried over)", &[("amount", amount(Some(total)))])
        } else {
            amount(Some(total))
        };
        facts.push(Fact::new(tr("Monthly maximum"), value));
    }
    let remaining = bounds.and_then(|b| b.remaining);
    match (kind, s.successor) {
        (GrantKind::AddPeople, Some(form)) => facts.push(Fact::new(
            tr("Pool total allowance"),
            fmt("{amount} (unchanged)", &[("amount", amount(Some(form.predecessor.allocated_tokens)))]),
        )),
        // Next month's capacity leads the hero; the row stays only to say it isn't known.
        (GrantKind::Renew, _) if remaining.is_some() => {}
        (GrantKind::Renew, _) => facts.push(Fact::new(
            fmt("Available in {month}", &[("month", month.unwrap_or_default().to_owned())]),
            amount(remaining),
        )),
        (GrantKind::Amend, _) => facts.push(
            Fact::new(tr("Most it can be"), amount(remaining)).note(tr("What this month can still give out.")),
        ),
        _ => facts.push(Fact::new(tr("Left this month"), amount(remaining))),
    }
    // What running Agents hold: the floor of a new limit, and what carries
    // into next month. Adding people changes neither.
    if let Some(form) = s.successor.filter(|_| matches!(kind, GrantKind::Amend | GrantKind::Renew)) {
        let fact = Fact::new(tr("Held by running Agents"), held_value(form.floor.retained_tokens, form.floor.children));
        facts.push(match kind {
            GrantKind::Amend if form.floor.retained_tokens > 0 => fact.note(tr("The pool total allowance can’t go below it.")),
            GrantKind::Renew if form.floor.children > 0 => fact.note(fmt(
                "They carry on into {month} without requesting again.",
                &[("month", month.unwrap_or_default().to_owned())],
            )),
            _ => fact,
        });
    }
    facts
}
/// When sharing ends, in the owner's own time zone: "11 月 1 日 08:00 前".
fn shared_until(bounds: &Bounds, expires_utc: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(expires_utc.trim()) {
        Ok(_) => ends_at(draft_end(bounds, expires_utc)),
        Err(_) => crate::i18n::tr("Not chosen").into(),
    }
}
/// Everything one rendering of the panel shows. Pure: it reads native state
/// only, so the owner sees the same words in the panel and in its preview.
fn grant_view(s: &GrantState, now: u64) -> GrantView {
    let tr = crate::i18n::tr;
    let fmt = crate::i18n::format;
    let kind = grant_kind(s);
    let summary = phase_summary(s.phase);
    let month = grant_month(s, summary.as_ref(), now);
    let choosing = matches!(s.phase, Phase::Choose);
    let bounds = s.bounds();
    let mut v = GrantView {
        glyph: kind_glyph(kind),
        pool: pool_line(s),
        choosing,
        more_label: tr("Technical Details").into(),
        more_open: s.disclosure.more,
        sharing_open: s.disclosure.sharing,
        cancel: tr(if choosing || matches!(s.phase, Phase::Review(_) | Phase::PublicationReview(_) | Phase::CancellationReview(_)) || (matches!(s.phase, Phase::Loading) && !s.submitting) {
            "Cancel"
        } else {
            "Close"
        })
        .into(),
        recover: tr(if s.original { "Check Again" } else { "Refresh" }).into(),
        recover_visible: s.can_recover(),
        cancel_intent_visible: s.can_cancel_intent(),
        technical: if matches!(s.phase, Phase::Loading) { String::new() } else { technical_rows(s, summary.as_ref()) },
        primary_enabled: s.can_primary(now),
        ..GrantView::default()
    };
    let current = s.successor.map(|form| form.predecessor.allocated_tokens);
    let base: Vec<String> = s.successor.map(|form| form.predecessor.eligible_managers.clone()).unwrap_or_default();
    let this_month = s.successor.map(|form| ms_month(form.predecessor.period_start_ms)).unwrap_or_else(|| ms_month(now));
    let renew_month = month.clone().filter(|_| kind == GrantKind::Renew);
    // What next month can share, for a renewal's hero, when its form is known.
    let renew_capacity = s.successor.filter(|_| kind == GrantKind::Renew).and_then(|form| successor_bounds(form).remaining);
    let month_text = month.clone().unwrap_or_else(|| tr("next month").into());
    let mut primary = "Publish";
    let mut primary_text: Option<String> = None;
    match s.phase {
        Phase::Loading => {
            v.tone = SheetTone::Working;
            v.status = if s.submitting {
                sentences(&[
                    tr(match kind {
                        GrantKind::Publish | GrantKind::Renew | GrantKind::Record => "Publishing…",
                        _ => "Saving your change…",
                    })
                    .into(),
                    tr("Closing this window doesn’t cancel it.").into(),
                ])
            } else {
                tr("Reading this pool…").into()
            };
            // While saving, the change being saved stays in view.
            if s.submitting {
                let tokens = s.draft.tokens.trim().parse::<u64>().ok();
                let sharing_only = s.successor.is_some_and(|form| {
                    sharing_moved(form, s.draft) && tokens == current && added_people(form, &managers_of(&s.draft.managers)).is_empty()
                });
                match kind {
                    GrantKind::AddPeople => {}
                    _ if sharing_only => {
                        v.caption = tr("Shared until").into();
                        v.after = s.bounds().map(|b| Figure::new(local_day_time(draft_end(&b, &s.draft.expires_utc)), tr("Being saved")));
                    }
                    _ if tokens.is_some() => {
                        v.caption = tr("Pool total allowance").into();
                        v.before = current.filter(|c| Some(*c) != tokens).map(|c| Figure::new(amount(Some(c)), tr("Now")));
                        v.after = Some(Figure::new(amount(tokens), tr("Being saved")));
                    }
                    _ => {}
                }
            } else if kind == GrantKind::AddPeople && !s.applicants.is_empty() {
                // While reading, what the window is about stays in view.
                v.caption = tr("People who can request").into();
                v.lead = fmt("Adds {people} once you confirm.", &[("people", people_text(s.applicants))]);
            } else if let Some(proposed) = s.proposal.filter(|_| kind != GrantKind::AddPeople) {
                v.caption = tr("Pool total allowance").into();
                v.after = Some(Figure::new(amount(Some(proposed)), tr("You chose")));
            }
        }
        Phase::Choose => {
            let managers = managers_of(&s.draft.managers);
            let tokens = s.draft.tokens.trim().parse::<u64>().ok();
            let added = s.successor.map(|form| added_people(form, &managers)).unwrap_or_default();
            let capacity_known = bounds.as_ref().is_some_and(|b| b.total_known && b.remaining.is_some());
            // A change of when sharing ends, and the new end in the owner's time.
            let moved = s.successor.is_some_and(|form| sharing_moved(form, s.draft));
            let new_end = bounds.as_ref().map(|b| draft_end(b, &s.draft.expires_utc));
            // What running Agents hold is all this month can give out: one amount only.
            let fixed = bounds.as_ref().and_then(fixed_amount).filter(|_| kind == GrantKind::Amend);
            // The page's value, raised or lowered into range when the form opened.
            let adjusted = s
                .proposal
                .zip(bounds.as_ref())
                .filter(|(page, b)| in_range(*page, b) != *page && tokens == Some(in_range(*page, b)))
                .map(|(page, _)| page)
                .filter(|_| matches!(kind, GrantKind::Amend | GrantKind::Renew));
            // The hero: what this window changes.
            match kind {
                GrantKind::Publish | GrantKind::Record => {
                    v.caption = tr("Pool total allowance").into();
                    v.before = Some(Figure::new(tr("Not published"), tr("Now")));
                    v.after = Some(Figure::new(amount(tokens), tr("After you confirm")));
                    let until = bounds.as_ref().map(|b| local_time(draft_end(b, &s.draft.expires_utc)));
                    v.lead = match until {
                        Some(when) => fmt(
                            "People on the list can request up to {amount} at a time, before {when}.",
                            &[("amount", amount(tokens)), ("when", when)],
                        ),
                        None => fmt("People on the list can request up to {amount} at a time.", &[("amount", amount(tokens))]),
                    };
                }
                GrantKind::Amend => {
                    v.caption = tr("Pool total allowance").into();
                    let also_ends = match new_end.filter(|_| moved) {
                        Some(end) => fmt("Sharing then ends before {when}.", &[("when", local_time(end))]),
                        None => String::new(),
                    };
                    if tokens == current && added.is_empty() && moved {
                        // Only when sharing ends changes: the hero says that end.
                        v.title = tr("Change Sharing Period").into();
                        v.caption = tr("Shared until").into();
                        v.after = new_end.map(|end| Figure::new(local_day_time(end), tr("After you confirm")));
                        let was = s.successor.map(|form| form.predecessor.delegation_expires_at_ms).unwrap_or_default();
                        v.lead = fmt(
                            "Now until {now}. Once you confirm, people on the list can request until {when}; the pool total allowance and what running Agents hold stay as they are.",
                            &[("now", local_time(was)), ("when", local_time(new_end.unwrap_or(was)))],
                        );
                    } else if tokens == current && added.is_empty() {
                        v.after = Some(Figure::new(amount(current), tr("Now")));
                        v.lead = match (fixed, adjusted) {
                            (Some(all), Some(page)) => fmt(
                                "You chose {page} on the pool’s page, but running Agents hold all {amount} this month can give out, so the pool total allowance can only be {amount} now. To give more, raise the monthly maximum on the pool’s page.",
                                &[("page", amount(Some(page))), ("amount", amount(Some(all)))],
                            ),
                            (Some(all), None) => fmt(
                                "Running Agents hold all {amount} this month can give out, so the pool total allowance can only be {amount} now. To give more, raise the monthly maximum on the pool’s page.",
                                &[("amount", amount(Some(all)))],
                            ),
                            (None, Some(page)) if bounds.as_ref().is_some_and(|b| page < b.minimum) => fmt(
                                "You chose {page} on the pool’s page, but it can’t be lower than the {held} held by running Agents. Choose a larger amount below, or add people.",
                                &[("page", amount(Some(page))), ("held", amount(bounds.as_ref().map(|b| b.minimum)))],
                            ),
                            (None, Some(page)) => fmt(
                                "You chose {page} on the pool’s page; at most {max} can be given out, so it starts at {max}.",
                                &[("page", amount(Some(page))), ("max", amount(tokens))],
                            ),
                            (None, None) => tr("Choose a new amount below, or add people.").into(),
                        };
                    } else {
                        v.before = Some(Figure::new(amount(current), tr("Now")));
                        v.after = Some(Figure::new(amount(tokens), tr("After you confirm")));
                        v.lead = sentences(&[
                            tr("Applies this month as soon as you confirm. What running Agents hold stays as it is.").into(),
                            if added.is_empty() {
                                String::new()
                            } else {
                                fmt("Also adds {people} to this month’s list.", &[("people", people_text(&added))])
                            },
                            also_ends,
                        ]);
                    }
                }
                GrantKind::AddPeople => {
                    v.caption = tr("People who can request").into();
                    if added.is_empty() {
                        v.after = Some(Figure::new(people_count(base.len()), tr("Now")));
                        v.lead = if s.applicants.is_empty() {
                            tr("Add someone below.").into()
                        } else {
                            fmt("{people} are already on this month’s list.", &[("people", people_text(s.applicants))])
                        };
                    } else {
                        v.before = Some(Figure::new(people_count(base.len()), tr("Now")));
                        v.after = Some(Figure::new(people_count(base.len() + added.len()), tr("After you confirm")));
                        v.lead = sentences(&[
                            fmt(
                                "Adds {people} to {month}’s list. They can request as soon as you confirm.",
                                &[("people", people_text(&added)), ("month", month_text.clone())],
                            ),
                            match new_end.filter(|_| moved) {
                                Some(end) => fmt("Sharing then ends before {when}.", &[("when", local_time(end))]),
                                None => String::new(),
                            },
                        ]);
                    }
                }
                GrantKind::Renew => {
                    let capacity = bounds.as_ref().and_then(|b| b.remaining);
                    renew_hero(&mut v, capacity, current, tokens, &this_month, &month_text);
                    let changed = tokens.is_some_and(|n| current.is_some_and(|c| n != c));
                    v.lead = sentences(&[
                        match (s.successor, capacity) {
                            (Some(form), Some(capacity)) if changed => fmt(
                                "{month} capacity is {amount}. Starts {when}; the list and the monthly maximum carry over.",
                                &[("month", month_text.clone()), ("amount", amount(Some(capacity))), ("when", local_time(form.period_start_ms))],
                            ),
                            (Some(form), Some(_)) => fmt(
                                "Starts {when}. The list, the monthly maximum and the pool total allowance carry over.",
                                &[("when", local_time(form.period_start_ms))],
                            ),
                            (Some(form), None) => fmt(
                                "Starts {when}. The list and the monthly maximum carry over.",
                                &[("when", local_time(form.period_start_ms))],
                            ),
                            (None, _) => tr("The list and the monthly maximum carry over.").into(),
                        },
                        if added.is_empty() {
                            String::new()
                        } else {
                            fmt("Also adds {people}.", &[("people", people_text(&added))])
                        },
                    ]);
                    v.caution = fmt(
                        "Once {month} is published ahead, this month’s pool total allowance and list can’t change any more, and it can’t be withdrawn.",
                        &[("month", month_text.clone())],
                    );
                }
            }
            if kind == GrantKind::Amend && tokens.is_some_and(|n| current.is_some_and(|c| n < c)) {
                v.caution = fmt(
                    "Waiting requests above {new} can’t be approved after this.",
                    &[("new", amount(tokens))],
                );
            }
            if !capacity_known {
                v.problem = capacity_problem(s);
            } else if bounds.as_ref().is_some_and(|b| b.remaining == Some(0)) {
                v.problem = capacity_problem(s);
            } else if let Some(problem) = bounds.as_ref().and_then(|b| draft_problem(b, s.draft, renew_month.as_deref(), now)) {
                v.problem = problem;
            }
            if !capacity_known || bounds.as_ref().is_some_and(|b| b.remaining == Some(0)) {
                v.tone = SheetTone::Failed;
                v.title = tr(if matches!(kind, GrantKind::Publish | GrantKind::Renew | GrantKind::Record) { "Cannot Publish" } else { "Cannot Save Changes" }).into();
                v.status = tr("Nothing has been published or changed.").into();
                v.lead.clear();
                v.caution.clear();
                if let Some(after) = &mut v.after { after.tag = tr("You chose").into(); }
            }
            // An amount that can't be confirmed isn't described as if it would apply.
            if !v.problem.is_empty() && kind == GrantKind::Amend && tokens != current && allowed_amount(bounds.as_ref(), tokens).is_none() {
                v.after = Some(Figure::new(amount(tokens), tr("You chose")));
                v.lead.clear();
                v.caution.clear();
            }
            v.facts_label = tr("This pool").into();
            v.facts = choose_facts(kind, s, bounds.as_ref(), month.as_deref());
            v.people_visible = true;
            v.people = choose_people(s);
            v.people_label = fmt(
                "People who can request · {count}",
                &[("count", people_count(v.people.len()))],
            );
            // A change that keeps the list as it is folds it into one row;
            // the add field opens with it.
            v.people_collapsible = matches!(kind, GrantKind::Amend | GrantKind::Renew)
                && added.is_empty()
                && s.applicants.is_empty()
                && s.add_error.is_empty();
            v.people_open = !v.people_collapsible || s.disclosure.people;
            v.people_summary = people_count(v.people.len());
            v.quota_label = tr("Pool total allowance").into();
            v.people_note = match kind {
                GrantKind::Renew => tr("The list carries over. You can add people, not remove them."),
                GrantKind::Amend | GrantKind::AddPeople => tr("This month’s list only grows: people already on it can’t be removed."),
                _ => "",
            }
            .into();
            v.people_editable = s.actionable();
            v.add_placeholder = tr("Add someone: type a username, for example lisi").into();
            v.add_error = s.add_error.to_owned();
            let server = s.account.split_once(':').map_or("", |(_, server)| server);
            v.add_hint = if managers.is_empty() {
                tr("No one yet. Add at least one person.").into()
            } else {
                fmt("Only people on {server} can be added.", &[("server", server.to_owned())])
            };
            // The unchanged limit never shows in a window that only adds people,
            // and an amount that can't move shows no slider: the hero says why.
            v.quota_visible = kind != GrantKind::AddPeople && fixed.is_none();
            if let Some(b) = &bounds {
                // One short line under the slider; the facts above say why. A value
                // the pool's page chose is said as such, so this reads as a check.
                let from_page = s
                    .proposal
                    .filter(|page| Some(*page) == tokens && kind != GrantKind::AddPeople && allowed_amount(Some(b), tokens).is_some());
                v.quota_hint = match b.remaining {
                    Some(remaining) if b.minimum > 1 => fmt(
                        "Choose {min} – {max}.",
                        &[("min", amount(Some(b.minimum))), ("max", amount(Some(remaining)))],
                    ),
                    Some(remaining) => fmt("At most {max}.", &[("max", amount(Some(remaining)))]),
                    None => String::new(),
                };
                if let Some(page) = from_page {
                    v.quota_hint = sentences(&[
                        fmt("You chose {amount} on the pool’s page; you can still adjust it here.", &[("amount", amount(Some(page)))]),
                        v.quota_hint.clone(),
                    ]);
                }
                // The page's value was outside what the form allows: it starts at the nearest end.
                if let Some(page) = adjusted {
                    v.quota_hint = if page < b.minimum {
                        fmt(
                            "You chose {page} on the pool’s page; it can’t be lower than the {held} held by running Agents, so it starts at {held}.",
                            &[("page", amount(Some(page))), ("held", amount(Some(b.minimum)))],
                        )
                    } else {
                        fmt(
                            "You chose {page} on the pool’s page; at most {max} can be given out, so it starts at {max}.",
                            &[("page", amount(Some(page))), ("max", amount(tokens))],
                        )
                    };
                }
                // When sharing ends is a row of its own; its choice opens under it.
                v.sharing_visible = true;
                v.expiry = shared_until(b, &s.draft.expires_utc);
                let mut notes = vec![fmt("Sharing ends before {when} at the latest.", &[("when", local_time(b.expiry_until_ms))])];
                let left = b.expiry_until_ms.saturating_sub(now.max(b.expiry_after_ms));
                if left < 7 * 86_400_000 {
                    notes.push(tr("Fewer than 7 days are left in it, so 7 days isn’t offered.").into());
                }
                if left < 86_400_000 {
                    notes.push(tr("Less than a day is left, so 1 day isn’t offered.").into());
                }
                v.expiry_note = sentences(&notes);
            }
            // "Change to X" only names an amount this form can confirm; a value
            // outside its range keeps the button's plain name, disabled, with
            // the reason beside the slider.
            let allowed = allowed_amount(bounds.as_ref(), tokens);
            primary = match kind {
                GrantKind::Publish | GrantKind::Record => "Publish",
                GrantKind::Amend if added.is_empty() && !moved && tokens != current => match allowed {
                    Some(n) => {
                        primary_text = Some(fmt("Change to {amount}", &[("amount", amount(Some(n)))]));
                        ""
                    }
                    None => "Confirm Changes",
                },
                GrantKind::Amend => "Confirm Changes",
                GrantKind::AddPeople => "Add to This Month’s List",
                GrantKind::Renew => "Publish Ahead",
            };
        }
        Phase::Review(review) => {
            let s2 = review.summary();
            summary_hero(&mut v, kind, &s2, &base, &this_month, false, renew_capacity);
            if review.capacity_available() {
                v.status = tr("Not published yet. Publish to finish.").into();
            } else {
                v.tone = SheetTone::Failed;
                v.title = tr("Cannot Publish").into();
                v.problem = capacity_problem(s);
                v.status = tr("Nothing has been published or changed.").into();
            }
            if kind == GrantKind::Renew { primary = "Publish Ahead"; }
        }
        Phase::PublicationReview(review) => {
            let s2 = review.summary();
            summary_hero(&mut v, kind, &s2, &base, &this_month, false, renew_capacity);
            v.status = tr("Not published yet. Publish to finish.").into();
            if kind == GrantKind::Renew { primary = "Publish Ahead"; }
        }
        Phase::Progress(p) => {
            if p.closure.is_some() {
                v.tone = SheetTone::Unsure;
                v.title = tr("The Previous Publish Ended").into();
                v.lead = tr("The previous publish has ended.").into();
                v.status = tr("What was used and given out still counts. To publish again, the server administrator must approve this pool again.").into();
                primary = "Publish Again";
            } else {
                match p.state {
                    OwnerGrantState::Recorded => {
                        summary_hero(&mut v, kind, &p.summary, &base, &this_month, false, renew_capacity);
                        v.status = tr("Saved on this computer but not published yet. Publish to finish.").into();
                        if kind == GrantKind::Renew { primary = "Publish Ahead"; }
                    }
                    OwnerGrantState::ReviewRequired => {
                        v.tone = SheetTone::Unsure;
                        v.title = tr("Unfinished Publish").into();
                        v.lead = tr("This publish isn’t finished.").into();
                        v.status = tr("Confirm it again, or discard it.").into();
                        primary = "Continue";
                    }
                    OwnerGrantState::OutcomeUnknown => {
                        v.tone = SheetTone::Unsure;
                        v.title = tr("Not Confirmed Yet").into();
                        v.lead = tr("It isn’t certain yet whether this was published.").into();
                        v.status = tr("Tap Check Again; don’t publish it a second time.").into();
                    }
                    OwnerGrantState::Declined => {
                        v.tone = SheetTone::Unsure;
                        v.title = tr("Publish Discarded").into();
                        v.lead = tr("This publish was discarded; no capacity is held.").into();
                        primary = "Choose Again";
                    }
                    OwnerGrantState::ReviewExpired => {
                        v.tone = SheetTone::Failed;
                        v.title = tr("This Publish Can’t Continue").into();
                        v.lead = tr("This publish belongs to a month that has ended, so it can’t continue.").into();
                    }
                }
            }
        }
        Phase::PublicationProgress(p) => {
            let started = p.summary.period_start_ms <= now;
            match (&p.state, p.server_state.as_deref()) {
                (OwnerGrantPublicationState::ServerObserved, Some("active")) => {
                    v.tone = SheetTone::Done;
                    summary_hero(&mut v, kind, &p.summary, &base, &this_month, true, renew_capacity);
                    let start = local_time(p.summary.period_start_ms);
                    let limit = amount(Some(p.summary.allocated_tokens));
                    let added: Vec<String> = if base.is_empty() {
                        Vec::new()
                    } else {
                        p.summary.eligible_managers.iter().filter(|id| !base.contains(id)).cloned().collect()
                    };
                    // A change that only moved when sharing ends says so, not a new limit.
                    let sharing_saved = s.submitted
                        && kind == GrantKind::Amend
                        && added.is_empty()
                        && s.successor.is_some_and(|form| {
                            !renewal(form)
                                && p.summary.allocated_tokens == form.predecessor.allocated_tokens
                                && p.summary.delegation_expires_at_ms / 1000 != form.predecessor.delegation_expires_at_ms / 1000
                        });
                    if sharing_saved {
                        v.title = tr("Sharing Period Saved").into();
                        v.caption = tr("Shared until").into();
                        v.before = None;
                        v.after = Some(Figure::new(local_day_time(p.summary.delegation_expires_at_ms.min(p.summary.period_end_ms)), tr("Now")));
                    }
                    v.lead = match (s.submitted, kind, started) {
                        _ if sharing_saved => fmt(
                            "People on the list can request until {when}. The pool total allowance stays {amount}.",
                            &[("when", local_time(p.summary.delegation_expires_at_ms.min(p.summary.period_end_ms))), ("amount", limit.clone())],
                        ),
                        (true, GrantKind::AddPeople, _) => fmt(
                            "Added {people} to {month}’s list. They can request now.",
                            &[("people", people_text(&added)), ("month", month_text.clone())],
                        ),
                        (true, GrantKind::Amend, _) => sentences(&[
                            fmt("New requests now get up to {amount}. What running Agents hold stays as it is.", &[("amount", limit.clone())]),
                            if added.is_empty() {
                                String::new()
                            } else {
                                fmt("Also added {people} to this month’s list.", &[("people", people_text(&added))])
                            },
                        ]),
                        (_, _, false) => fmt("Starts {when}. Running Agents carry on without requesting again.", &[("when", start.clone())]),
                        (true, _, true) => fmt(
                            "People on the list can request now, up to {amount} at a time.",
                            &[("amount", limit.clone())],
                        ),
                        (false, _, true) => fmt(
                            "{month} is published: up to {amount} in total.",
                            &[("month", month_text.clone()), ("amount", limit.clone())],
                        ),
                    };
                    if !started {
                        v.status = tr("This month’s pool total allowance and list can’t change any more.").into();
                    }
                    if s.form.is_some_and(|form| form.awaiting_admission) {
                        v.title = tr("Submitted for Approval").into();
                        v.lead = tr("Your quota, period and access list are submitted. Administrator approval makes this pool available without another confirmation.").into();
                    }
                    v.facts_label = tr("This pool").into();
                    let until = p.summary.delegation_expires_at_ms.min(p.summary.period_end_ms);
                    v.facts = vec![Fact::new(tr("Shared until"), ends_at(until))];
                    if let Some(succession) = p.summary.succession.as_ref().filter(|_| kind != GrantKind::AddPeople) {
                        v.facts.push(Fact::new(
                            tr("Held by running Agents"),
                            held_value(succession.carried_retained_tokens, succession.carried_children),
                        ));
                    }
                }
                (OwnerGrantPublicationState::ServerObserved, Some(state)) if matches!(state, "revoked" | "expired" | "superseded") => {
                    v.tone = SheetTone::Unsure;
                    v.title = tr("No Longer in Effect").into();
                    v.lead = tr(match state {
                        "revoked" => "This publish was withdrawn and no longer applies.",
                        "expired" => "This publish has expired.",
                        _ => "A newer change replaced this publish.",
                    })
                    .into();
                }
                (OwnerGrantPublicationState::ServerObserved, _) => {
                    v.tone = SheetTone::Unsure;
                    v.title = tr("Not Confirmed Yet").into();
                    v.lead = tr("The server’s status isn’t known yet.").into();
                }
                (OwnerGrantPublicationState::OutcomeUnknown, _) => {
                    v.tone = SheetTone::Unsure;
                    v.title = tr("Not Confirmed Yet").into();
                    v.lead = tr("It isn’t certain yet whether this was published.").into();
                    v.status = tr("Tap Check Again; don’t publish it a second time.").into();
                }
                (OwnerGrantPublicationState::Rejected, _) => {
                    v.tone = SheetTone::Failed;
                    v.lead = tr("The server didn’t accept this publish.").into();
                    v.status = tr("Nothing was changed.").into();
                }
            }
            let done = matches!(p.state, OwnerGrantPublicationState::ServerObserved)
                && p.server_state.as_deref() == Some("active");
            // A record that is in effect needs no check: the window ends with Done.
            if done {
                v.recover_visible = false;
            }
        }
        Phase::CancellationReview(_) => {
            v.lead = tr("Discard this unfinished publish?").into();
            v.status = tr("Only this unfinished publish on this computer is discarded; the pool and earlier publishes stay as they are. Closing the window keeps it.").into();
            primary = "Discard This Publish";
        }
        Phase::Failed(code) => {
            v.tone = if unknown_failure(code) { SheetTone::Unsure } else { SheetTone::Failed };
            if unknown_failure(code) {
                unknown_hero(&mut v, kind, s, current, &base);
            } else {
                failure_hero(&mut v, code, s, bounds.as_ref());
            }
            if unchanged_failure(code) {
                v.status = tr("Nothing was changed.").into();
            }
            if unknown_failure(code) {
                v.title = tr("Not Confirmed Yet").into();
            }
        }
    }
    if v.title.is_empty() {
        v.title = grant_title(kind, v.tone, s, month.as_deref());
    }
    // People: who the change affects. Results list the people now on it,
    // folded into one row unless this window added someone.
    if let Some(summary) = summary.as_ref().filter(|_| !choosing) {
        let people = listed_people(&base, &summary.eligible_managers, s.successor.is_some(), false);
        v.people_visible = !people.is_empty();
        v.people_label = fmt("People who can request · {count}", &[("count", people_count(people.len()))]);
        // A record opened to read the list shows it; a finished change that
        // kept the list folds it.
        v.people_collapsible = kind != GrantKind::Record && !people.iter().any(|person| person.added);
        v.people_open = !v.people_collapsible || s.disclosure.people;
        v.people_summary = people_count(people.len());
        v.people = people;
        // This month's record can add people right here, in an amendment that
        // keeps the pool total allowance; elsewhere the pool's page adds them.
        v.people_add_visible = (kind == GrantKind::Record || !s.can_choose_new_limits)
            && s.actionable()
            && s.succession_supported == Some(true)
            && live_grant_tokens(s.phase, now).is_some();
        v.people_note = if v.people_add_visible {
            tr("This month’s list only grows: people already on it can’t be removed.").into()
        } else if (kind == GrantKind::Record || !s.can_choose_new_limits)
            && s.succession_supported == Some(false)
            && live_grant_tokens(s.phase, now).is_some()
        {
            tr("This server can’t change a published month or publish ahead yet. Ask its administrator to update the server.").into()
        } else if kind == GrantKind::Record || !s.can_choose_new_limits {
            tr("To add someone, close this window and use Who Can Request on the pool’s page.").into()
        } else {
            String::new()
        };
    }
    if ended_grant(s.phase, now) {
        v.tone = SheetTone::Proposal;
        v.glyph = SheetGlyph::Publish;
        v.title = fmt("Publish {month}", &[("month", ms_month(now))]);
        v.caption.clear();
        v.before = None;
        v.after = None;
        v.lead = tr(ENDED_LEAD).into();
        v.status = tr(ENDED_STATUS).into();
        primary = "Update Pool Allowance";
        primary_text = None;
    }
    let live = live_grant_tokens(s.phase, now);
    if published_grant(s.phase, now) && s.amendable() && live.is_some() {
        // A proposal beside this month's live publish: open the change form.
        v.tone = SheetTone::Proposal;
        v.glyph = SheetGlyph::Change;
        v.title = tr("Change Pool Limits").into();
        v.caption = tr("Pool total allowance").into();
        v.before = Some(Figure::new(amount(live), tr("Now")));
        v.after = Some(Figure::new(amount(s.proposal), tr("Proposed")));
        v.lead = tr("Not applied yet. Continue to choose the amount; what running Agents hold stays as it is.").into();
        primary = "Change Pool Limits";
        primary_text = None;
    }
    if !s.active {
        v.problem = tr("The account or session changed. Sign in again, then open this again.").into();
    }
    // Beside a usable form, Refresh would only reload what is shown; keep it
    // for an unknown or used-up capacity, where reading again can help.
    if choosing && bounds.as_ref().is_some_and(|b| b.total_known && b.remaining.is_some_and(|n| n > 0)) {
        v.recover_visible = false;
    }
    v.notice = if choosing || v.before.is_some() {
        None
    } else {
        proposal_notice(s.phase, s.proposal, &s.draft.tokens, s.succession_supported == Some(true), now)
    };
    v.primary = primary_text.unwrap_or_else(|| tr(primary).into());
    v.primary_visible = choosing || v.primary_enabled;
    // A finished change ends with one primary Done.
    if v.tone == SheetTone::Done && !v.primary_enabled {
        v.primary = tr("Done").into();
        v.primary_visible = true;
        v.primary_enabled = true;
        v.primary_closes = true;
        v.cancel.clear();
    }
    // A refusal offers one way forward as its primary button: back to the
    // form when the owner can fix it here, a check when the outcome isn't
    // known, and otherwise just OK.
    if let Phase::Failed(code) = s.phase {
        let fixable = fixable_failure(code) && (s.successor.is_some() || s.form.is_some());
        if v.primary_enabled {
            // An expired or changed confirmation: read the record again.
            v.primary = tr("Check Again").into();
        } else if (fixable || unknown_failure(code)) && v.recover_visible {
            v.primary = match v.fix_tokens.filter(|_| fixable) {
                Some(tokens) => fmt("Change to {amount}", &[("amount", amount(Some(tokens)))]),
                None => tr(if fixable { "Choose Again" } else { "Check Again" }).into(),
            };
            // With the amount that works one tap away, choosing another one stays a row of its own.
            if v.fix_tokens.is_some() && fixable {
                v.recover = tr("Choose Again").into();
                v.recover_secondary = true;
            }
            v.primary_recovers = true;
            v.primary_enabled = true;
        } else {
            v.primary = tr("OK").into();
            v.primary_closes = true;
            v.primary_enabled = true;
            v.recover_visible = false;
            v.cancel.clear();
        }
        v.primary_visible = true;
    }
    // Elsewhere, where nothing else can go forward, the recovery step is
    // the primary action, such as checking an unknown outcome.
    if !v.primary_visible && v.recover_visible {
        v.primary_recovers = true;
        v.primary_visible = true;
        v.primary_enabled = true;
        v.primary = v.recover.clone();
    }
    let calendar = if choosing {s.draft.calendar.as_ref()} else {summary.as_ref().and_then(|summary| summary.calendar.as_ref())};
    if let Some(calendar) = calendar {
        let mode = if choosing {s.draft.renewal_mode} else {summary.as_ref().and_then(|s|s.renewal_mode)}.unwrap_or(RenewalMode::Automatic);
        v.renewal_mode = choosing.then_some(mode);
        if !choosing { v.facts.push(Fact::new(tr("Pool renewal"), tr(if mode == RenewalMode::Automatic {"Automatic"} else {"Manual"}))); }
        v.calendar = (choosing && s.successor.is_none() && s.form.is_none_or(|form| form.calendar.is_none())).then(|| calendar.clone());
        v.quota_label = tr("Tokens per period").into();
        v.sharing_visible = false;
        for text in [&mut v.title,&mut v.lead,&mut v.problem,&mut v.status,&mut v.quota_hint,&mut v.people_note] {*text=crate::host::hagency::resource_edit::calendar_words(text);}
        for fact in &mut v.facts {fact.label=crate::host::hagency::resource_edit::calendar_words(&fact.label);fact.note=crate::host::hagency::resource_edit::calendar_words(&fact.note);}
        if !choosing && v.tone == SheetTone::Done {
            v.title = tr("Recurring Capacity Published").into();
            v.caption = tr("Tokens per period").into();
            v.lead = tr(if mode == RenewalMode::Automatic {"The pool renews automatically. Approved Agents receive their allowance each new period."} else {"Publish the next period when ready. Approved Agents then receive their allowance together."}).into();
        }
        if !choosing || s.successor.is_some() || s.form.is_some_and(|form| form.calendar.is_some()) {
            v.facts.push(calendar_fact(calendar,summary.as_ref().map(|summary|summary.period_end_ms),now));
        }
        if choosing {
            v.caption = tr("Tokens per period").into();
            v.before = None;
            v.after = Some(Figure::new(amount(s.draft.tokens.trim().parse().ok()), tr("Tokens per period")));
            // Budget facts remain visible in every locale. Their labels above
            // use period wording; translated text must never control visibility.
            if v.tone != SheetTone::Failed {
                v.title = tr("Publish Recurring Capacity").into();
                v.lead = tr("Set renewal once for this pool. All approved Agents follow your choice.").into();
                v.status.clear();
            }
            if calendar.period_at(now).is_err() {v.primary_enabled = false;}
        }
    }
    if let Some(limit) = if choosing {s.draft.per_request.parse::<u64>().ok()} else {summary.as_ref().and_then(|s|s.per_request_tokens)} {
        if !choosing { v.facts.insert(0, Fact::new(tr("Maximum per Agent per period"), amount(Some(limit)))); }
        if choosing && (limit == 0 || s.draft.tokens.parse::<u64>().is_ok_and(|total| limit > total)) {
            v.problem = tr("The Agent limit must be above zero and no greater than the pool total.").into();
            v.primary_enabled = false;
        }
    }
    v
}
/// Publishing next month ahead: the hero leads with that month's capacity and
/// says the pool total allowance under it. The limit's before → after shows only
/// when it changes, never an arrow between two equal amounts.
fn renew_hero(v: &mut GrantView, capacity: Option<u64>, before: Option<u64>, after: Option<u64>, this_month: &str, month: &str) {
    let tr = crate::i18n::tr;
    let fmt = crate::i18n::format;
    v.before = None;
    match (before, after, capacity) {
        (Some(old), Some(new), _) if old != new => {
            v.caption = tr("Pool total allowance").into();
            v.before = Some(Figure::new(amount(Some(old)), this_month.to_owned()));
            v.after = Some(Figure::new(amount(Some(new)), month.to_owned()));
        }
        (_, _, Some(capacity)) => {
            v.caption = fmt("{month} capacity", &[("month", month.to_owned())]);
            v.after = Some(Figure::new(amount(Some(capacity)), fmt("Up to {amount} in total", &[("amount", amount(after))])));
        }
        _ => {
            v.caption = tr("Pool total allowance").into();
            v.after = Some(Figure::new(amount(after), month.to_owned()));
        }
    }
}
/// The hero of a saved, reviewed or published record: its per-request
/// limit, the previous one for a change, or the list for added people.
fn summary_hero(v: &mut GrantView, kind: GrantKind, summary: &OwnerGrantSummary, base: &[String], this_month: &str, done: bool, capacity: Option<u64>) {
    let tr = crate::i18n::tr;
    let added = if base.is_empty() { 0 } else { summary.eligible_managers.iter().filter(|id| !base.contains(id)).count() };
    let after_tag = tr(if done { "Now" } else { "After you confirm" });
    match kind {
        GrantKind::AddPeople if added > 0 => {
            v.caption = tr("People who can request").into();
            v.before = Some(Figure::new(people_count(summary.eligible_managers.len() - added), tr(if done { "Before" } else { "Now" })));
            v.after = Some(Figure::new(people_count(summary.eligible_managers.len()), after_tag));
        }
        GrantKind::Renew => renew_hero(
            v,
            capacity,
            summary.succession.as_ref().map(|s| s.predecessor_allocated_tokens),
            Some(summary.allocated_tokens),
            this_month,
            &key_month(&summary.period_key).unwrap_or_default(),
        ),
        _ => {
            v.caption = tr("Pool total allowance").into();
            v.before = summary
                .succession
                .as_ref()
                .filter(|s| s.predecessor_allocated_tokens != summary.allocated_tokens)
                .map(|s| Figure::new(amount(Some(s.predecessor_allocated_tokens)), tr(if done { "Before" } else { "Now" })));
            v.after = Some(Figure::new(amount(Some(summary.allocated_tokens)), if v.before.is_some() { after_tag } else { tr("Now") }));
        }
    }
}
/// An outcome that isn't known yet: what was tried, kept in view as the
/// proposal showed it, and plainly that it may or may not have applied and
/// that checking only reads the result.
fn unknown_hero(v: &mut GrantView, kind: GrantKind, s: &GrantState, current: Option<u64>, base: &[String]) {
    let tr = crate::i18n::tr;
    let fmt = crate::i18n::format;
    let tokens = s.draft.tokens.trim().parse::<u64>().ok();
    let managers = managers_of(&s.draft.managers);
    let added = s.successor.map(|form| added_people(form, &managers)).unwrap_or_default();
    let mut lead = Vec::new();
    match kind {
        GrantKind::AddPeople if !added.is_empty() => {
            v.caption = tr("People who can request").into();
            v.before = Some(Figure::new(people_count(base.len()), tr("Before")));
            v.after = Some(Figure::new(people_count(base.len() + added.len()), tr("You chose")));
            lead.push(fmt("You were adding {people}.", &[("people", people_text(&added))]));
        }
        GrantKind::AddPeople | GrantKind::Record => {}
        _ if tokens.is_some() => {
            v.caption = tr("Pool total allowance").into();
            v.before = current.filter(|c| Some(*c) != tokens).map(|c| Figure::new(amount(Some(c)), tr("Before")));
            v.after = Some(Figure::new(amount(tokens), tr("You chose")));
        }
        _ => {}
    }
    lead.push(tr("It may or may not have taken effect. Check Again only looks at the result; it won’t make the change again.").into());
    v.lead = sentences(&lead);
}
/// A refusal says why and, where the window knows it, the amount that works.
fn failure_hero(v: &mut GrantView, code: &str, s: &GrantState, bounds: Option<&Bounds>) {
    let tr = crate::i18n::tr;
    let fmt = crate::i18n::format;
    let chosen = s.draft.tokens.trim().parse::<u64>().ok();
    match code {
        "owner_grant_below_floor" | "grant_successor_below_reserved" if bounds.is_some_and(|b| b.minimum > 1) => {
            let held = bounds.map_or(0, |b| b.minimum);
            v.caption = tr("The pool total allowance must be at least").into();
            v.after = Some(Figure::new(amount(Some(held)), tr("Held by running Agents")));
            v.fix_tokens = Some(held);
            v.lead = match chosen {
                Some(chosen) => fmt(
                    "You chose {chosen}, which is less than the {held} held by running Agents. Choose {held} or more.",
                    &[("chosen", amount(Some(chosen))), ("held", amount(Some(held)))],
                ),
                None => fmt("Choose {held} or more.", &[("held", amount(Some(held)))]),
            };
        }
        // Next month is published ahead: say when this month's list (or
        // limit) can change again, at the start of next month.
        "owner_grant_successor_exists" | "grant_successor_exists" => {
            let next = s.successor.map_or_else(
                || publication_period_end().timestamp_millis() as u64,
                |form| if renewal(form) { form.period_start_ms } else { form.period_end_ms },
            );
            let when = local_time(next);
            v.lead = match grant_kind(s) {
                GrantKind::AddPeople => fmt(
                    "Next month is already published ahead, so this month’s list can’t change. You can add people again from {when}.",
                    &[("when", when)],
                ),
                GrantKind::Renew => tr("Next month is already published ahead.").into(),
                _ => fmt(
                    "Next month is already published ahead, so this month can’t change. You can change it again from {when}.",
                    &[("when", when)],
                ),
            };
        }
        "owner_grant_capacity_unavailable" if bounds.and_then(|b| b.remaining).is_some() => {
            let most = bounds.and_then(|b| b.remaining);
            v.caption = tr("The pool total allowance can be at most").into();
            v.after = Some(Figure::new(amount(most), tr("Left this month")));
            v.fix_tokens = most.filter(|most| *most > 0 && chosen.is_some_and(|chosen| chosen > *most));
            v.lead = match chosen {
                Some(chosen) => fmt(
                    "You chose {chosen}, which is more than is left this month. Choose {most} or less.",
                    &[("chosen", amount(Some(chosen))), ("most", amount(most))],
                ),
                None => tr(failure_message(code)).into(),
            };
        }
        _ => v.lead = tr(failure_message(code)).into(),
    }
}
/// Native text for a script's proposed pool total allowance: the current limit,
/// the amount this screen would commit, and why a phase does not use it.
fn proposal_notice(
    phase: &Phase,
    proposal: Option<u64>,
    draft_tokens: &str,
    succession_supported: bool,
    now: u64,
) -> Option<String> {
    let proposal = proposal?;
    let (new, note) = match phase {
        Phase::Loading => return None,
        // The form commits only what its tokens field holds now.
        Phase::Choose => (draft_tokens.trim().parse::<u64>().ok(), None),
        phase if ended_grant(phase, now) => (
            Some(proposal),
            Some("This amount is filled in when you set this month’s capacity."),
        ),
        phase if published_grant(phase, now) && !succession_supported => {
            (Some(proposal), Some(PUBLISHED_NOTE))
        }
        phase if scheduled_grant(phase, now) => (Some(proposal), Some(SCHEDULED_NOTE)),
        phase if published_grant(phase, now) && live_grant_tokens(phase, now).is_some() => {
            (Some(proposal), Some(AMENDABLE_NOTE))
        }
        phase if published_grant(phase, now) => (Some(proposal), Some(NOT_AMENDABLE_NOTE)),
        Phase::Progress(p)
            if matches!(p.state, OwnerGrantState::Declined)
                || (matches!(p.state, OwnerGrantState::Recorded) && p.closure.is_some()) =>
        {
            (Some(proposal), Some("This amount is filled in when you choose again."))
        }
        Phase::Progress(p)
            if matches!(p.state, OwnerGrantState::ReviewRequired | OwnerGrantState::ReviewExpired) =>
        {
            (Some(proposal), Some(PENDING_NOTE))
        }
        // Reviews of an existing, unattempted original; submitting the form
        // clears the proposal before its own review appears.
        Phase::Review(_) | Phase::CancellationReview(_) => (Some(proposal), Some(PENDING_NOTE)),
        Phase::Failed(_) => (Some(proposal), Some("This amount wasn’t applied.")),
        _ => (
            Some(proposal),
            Some("This amount doesn’t change the existing publish; it keeps its own amounts."),
        ),
    };
    // A failure says only that the amount was not used: what is in force
    // is not known from it, so no current amount is claimed.
    if matches!(phase, Phase::Failed(_)) {
        return note.map(|note| crate::i18n::tr(note).to_owned());
    }
    let current = live_grant_tokens(phase, now)
        .map(|tokens| amount(Some(tokens)))
        .unwrap_or_else(|| crate::i18n::tr("Not in effect").into());
    let mut text = crate::i18n::format(
        "Pool total allowance: {current} → {new}",
        &[("current", current), ("new", amount(new))],
    );
    if let Some(note) = note {
        text.push('\n');
        text.push_str(crate::i18n::tr(note));
    }
    Some(text)
}
fn same_publication_resource(form: &OwnerGrantForm, review: &OwnerGrantReview) -> bool {
    let c=review.context();let s=review.summary();
    form.contribution_id==s.contribution_id && form.resource_id==s.resource_id
        && form.context.server_origin==c.server_origin && form.context.profile_id==c.profile_id
        && form.context.resource_revision==c.resource_revision
        && form.context.framework==c.framework && form.context.model==c.model
}
/// The successor review continues one owner submission only if it is the
/// change of the same resource, kind and predecessor the owner was shown.
fn same_successor_resource(form: &OwnerGrantSuccessionForm, review: &OwnerGrantReview) -> bool {
    let c=review.context();let s=review.summary();
    form.contribution_id==s.contribution_id && form.resource_id==s.resource_id
        && form.context.server_origin==c.server_origin && form.context.profile_id==c.profile_id
        && form.context.resource_revision==c.resource_revision
        && form.context.framework==c.framework && form.context.model==c.model
        && s.succession.as_ref().is_some_and(|succession| succession.kind==form.kind
            && succession.predecessor_grant_id==form.predecessor.grant_id)
}
/// Only exact reviewed native codes may enter diagnostics. A prefix match would
/// allow arbitrary provider errors, paths or credentials to escape into logs.
fn grant_failure_code(code: &str) -> &'static str {
    if let Some(code)=crate::host::hagency::errors::known_code(code){return code;}
    match code {
        "management_access_required" => "management_access_required",
        "management_admission_receipts_required" => "management_admission_receipts_required",
        "management_features_invalid" => "management_features_invalid",
        "owner_grant_prior_intent_required" => "owner_grant_prior_intent_required",
        "owner_grant_new_admission_required" => "owner_grant_new_admission_required",
        "management_monthly_regrant_required" => "management_monthly_regrant_required",
        "management_backing_changed" => "management_backing_changed",
        "management_capacity" => "management_capacity",
        "management_drain_outcome_unknown" => "management_drain_outcome_unknown",
        "management_drain_unavailable" => "management_drain_unavailable",
        "management_enrollment_changed" => "management_enrollment_changed",
        "management_enrollment_history_full" => "management_enrollment_history_full",
        "management_enrollment_history_required" => "management_enrollment_history_required",
        "management_model_unqualified" => "management_model_unqualified",
        "management_not_found" => "management_not_found",
        "management_outcome_unknown" => "management_outcome_unknown",
        "management_profile_mismatch" => "management_profile_mismatch",
        "management_resource_in_use" => "management_resource_in_use",
        "management_revision_changed" => "management_revision_changed",
        "management_revision_conflict" => "management_revision_conflict",
        "management_scope_changed" => "management_scope_changed",
        "management_state_unavailable" => "management_state_unavailable",
        "management_storage_failed" => "management_storage_failed",
        "management_unavailable" => "management_unavailable",
        "management_permission_required" => "management_permission_required",
        "management_session_required" => "management_session_required",
        "management_command_conflict" => "management_command_conflict",
        "management_receipt_capacity" => "management_receipt_capacity",
        "native_grant_not_publishable" => "native_grant_not_publishable",
        "native_grant_receipt_required" => "native_grant_receipt_required",
        "native_owner_state_unavailable" => "native_owner_state_unavailable",
        "native_random_unavailable" => "native_random_unavailable",
        "owner_authorization_required" => "owner_authorization_required",
        "owner_grant_already_attempted" => "owner_grant_already_attempted",
        "owner_grant_binding_changed" => "owner_grant_binding_changed",
        "owner_grant_capacity_unavailable" => "owner_grant_capacity_unavailable",
        "owner_grant_history_full" => "owner_grant_history_full",
        "owner_grant_history_unavailable" => "owner_grant_history_unavailable",
        "owner_grant_intent_missing" => "owner_grant_intent_missing",
        "owner_grant_original_intent_required" => "owner_grant_original_intent_required",
        "owner_grant_original_review_expired" => "owner_grant_original_review_expired",
        "owner_grant_publication_confirmation_required" => {
            "owner_grant_publication_confirmation_required"
        }
        "owner_grant_publication_history_unavailable" => {
            "owner_grant_publication_history_unavailable"
        }
        "owner_grant_publication_review_changed" => "owner_grant_publication_review_changed",
        "owner_grant_publication_review_expired" => "owner_grant_publication_review_expired",
        "owner_grant_review_changed" => "owner_grant_review_changed",
        "current_profile_required" => "current_profile_required",
        "provider_account_changed" => "provider_account_changed",
        "provider_closing" => "provider_closing",
        "provider_directory_unavailable" => "provider_directory_unavailable",
        "provider_identity_timeout" => "provider_identity_timeout",
        "provider_identity_unavailable" => "provider_identity_unavailable",
        "provider_profile_mismatch" => "provider_profile_mismatch",
        "local_codex_account_not_signed_in" => "local_codex_account_not_signed_in",
        "local_provider_not_signed_in" => "local_provider_not_signed_in",
        "contribution_not_admitted" => "contribution_not_admitted",
        "contribution_executor_changed" => "contribution_executor_changed",
        "contribution_resource_changed" => "contribution_resource_changed",
        "allocation_enrollment_changed" => "allocation_enrollment_changed",
        "invalid_grant_budget" => "invalid_grant_budget",
        "invalid_grant_expiry" => "invalid_grant_expiry",
        "invalid_grant_calendar" => "invalid_grant_calendar",
        "invalid_grant_manager" => "invalid_grant_manager",
        "management_grant_succession_required" => "management_grant_succession_required",
        "pool_grant_succession_unavailable" => "pool_grant_succession_unavailable",
        "owner_grant_predecessor_required" => "owner_grant_predecessor_required",
        "owner_grant_succession_pending" => "owner_grant_succession_pending",
        "owner_grant_successor_exists" => "owner_grant_successor_exists",
        "owner_grant_renewal_window_closed" => "owner_grant_renewal_window_closed",
        "owner_grant_below_floor" => "owner_grant_below_floor",
        "owner_grant_manager_removed" => "owner_grant_manager_removed",
        "grant_predecessor_not_effective" => "grant_predecessor_not_effective",
        "grant_successor_exists" => "grant_successor_exists",
        "grant_successor_below_reserved" => "grant_successor_below_reserved",
        "grant_succession_window_closed" => "grant_succession_window_closed",
        "invalid_grant_succession" => "invalid_grant_succession",
        _ => "owner_grant_unclassified_failure",
    }
}
fn failure_message(code: &str) -> &'static str {
        if let Some(message)=crate::host::hagency::errors::message(code){return message;}
    match code {
        "owner_grant_original_review_expired" | "owner_grant_publication_review_expired" => {
            "This confirmation expired. Check the record again."
        }
        "native_grant_not_publishable" => {
            "This saved publish can’t be published any more. Its record is kept."
        }
        "owner_grant_publication_history_unavailable" => {
            "This computer’s publish history is incomplete, so nothing new can be created. The saved record is kept."
        }
        "owner_grant_publication_review_changed" => {
            "This pool changed on the server. Close this window and refresh."
        }
        "owner_grant_review_changed" => "This confirmation changed or expired. Check the record again.",
        "management_enrollment_changed" | "contribution_not_admitted" => {
            "The server’s approval of this pool changed. Check the record; an unfinished publish can still be discarded."
        }
        "owner_grant_prior_intent_required" => "Finish or discard earlier publishes first.",
        "management_admission_receipts_required" => {
            "The server isn’t accepting publishes for this pool. Ask its administrator."
        }
        "management_monthly_regrant_required" => {
            "This server can’t take a new month for this pool yet. Ask its administrator to update the server."
        }
        "management_features_invalid" => {
            "Couldn’t confirm what the server supports. Refresh, then try again."
        }
        "management_scope_changed" | "management_access_required" => {
            "The account or session changed. Sign in again, then open this again."
        }
        "invalid_grant_expiry" => "Choose when sharing ends under Sharing Period.",
        "invalid_grant_calendar" => "Enter a valid IANA time zone, such as America/Los_Angeles.",
        "owner_grant_new_admission_required" => {
            "The server administrator must approve this pool again before it can be published."
        }
        "owner_grant_capacity_unavailable" => {
            "There isn’t enough capacity left, or it isn’t known yet. Nothing was published."
        }
        "invalid_grant_budget" => {
            "Enter a pool total allowance above 0 and at least one person who can request."
        }
        "invalid_grant_manager" => "Someone on the list isn’t an account on your server. Remove them and add them again.",
        "grant_person_not_found" => "An account on the list no longer exists. Remove it and search for the person again.",
        "grant_people_unavailable" => "Couldn’t verify people with the server. Your choices are kept. Try again when the connection is available.",
        "management_grant_succession_required" | "pool_grant_succession_unavailable" => {
            "This server can’t change a published month or publish ahead yet. Ask its administrator to update the server."
        }
        "owner_grant_predecessor_required" | "grant_predecessor_not_effective" => {
            "This pool has nothing published this month to change. Publish it first."
        }
        "owner_grant_succession_pending" => {
            "An earlier change to this pool isn’t finished. Finish or discard it first."
        }
        "owner_grant_successor_exists" | "grant_successor_exists" => {
            "Next month is already published ahead, so this month can’t change now."
        }
        "owner_grant_renewal_window_closed" | "grant_succession_window_closed" => {
            "Next month can be published ahead only in the last 7 days of this month."
        }
        "owner_grant_below_floor" | "grant_successor_below_reserved" => {
            "The new limit is less than what running Agents hold. Choose a larger amount."
        }
        "owner_grant_manager_removed" => {
            "This month’s list can only grow: keep everyone already on it."
        }
        _ => "The result isn’t confirmed yet. Tap Check Again before trying anything else.",
    }
}
/// Values the native fields are refilled with when the panel is presented.
/// The list and the expiry are native state the owner edits through the
/// sheet's own rows and buttons; only the amount is a typed field.
pub(super) struct GrantInputs {
    per_request: String,
    tokens: String,
    minimum: u64,
    maximum: Option<u64>,
    presets: [bool; 3],
    /// The preset that matches the sharing period chosen now (1 day, 7 days, to month end).
    selected: [bool; 3],
}
impl GrantInputs {
    fn of(s: &GrantState, now: u64) -> Self {
        let bounds = s.bounds();
        let presets = [Some(1), Some(7), None].map(|days| match s.successor {
            Some(form) => successor_preset(form, days, now).is_some(),
            None => publication_preset_available_at(days, now),
        });
        // The draft's end against each preset's: to month end exactly, else 1 or 7 days from now.
        let end = bounds.as_ref().map(|b| draft_end(b, &s.draft.expires_utc));
        let period_end = bounds.as_ref().map(|b| b.expiry_until_ms);
        let near = |at: Option<u64>, target: u64, slack: u64| at.is_some_and(|at| at.abs_diff(target) <= slack);
        let month = end.is_some() && end == period_end;
        let selected = [
            !month && near(end, now + 86_400_000, 3_600_000),
            !month && near(end, now + 7 * 86_400_000, 3_600_000),
            month,
        ];
        Self {
            tokens: s.draft.tokens.clone(),
            per_request: s.draft.per_request.clone(),
            minimum: bounds.as_ref().map_or(1, |b| b.minimum),
            maximum: bounds.as_ref().map(|b| b.remaining.unwrap_or(0)),
            presets,
            selected,
        }
    }
}
impl GrantPrompt {
    /// The native facts this panel shows, with the flow's facts copied in.
    fn state(&self) -> GrantState<'_> {
        GrantState {
            phase: &self.phase,
            draft: &self.draft,
            proposal: self.proposal,
            form: self.form.as_ref(),
            successor: self.successor.as_ref(),
            requested: self.flow.requested_succession(),
            applicants: self.flow.applicants(),
            account: self.flow.account(),
            original: self.original.is_some(),
            in_flight: self.receiver.is_some(),
            submitted: self.submitted,
            submitting: self.submit_publication,
            publishing: self.publishing,
            cancelling: self.cancelling,
            regrant: self.regrant,
            active: self.flow.active(),
            can_choose_new_limits: self.flow.can_choose_new_limits(),
            succession_supported: self.flow.succession_supported(),
            disclosure: self.disclosure,
            add_error: &self.add_error,
            add_people: self.add_people,
        }
    }
    fn actionable(&self) -> bool {
        self.state().actionable()
    }
    /// The owner submits the terms shown natively for reservation and
    /// publication; a script proposal plays no further part in this window.
    fn submit(&mut self) {
        self.submit_publication = true;
        self.submitted = true;
        self.proposal = None;
    }
    /// A proposal beside this month's published grant may open the native
    /// amendment form, but only once the server is known to support it.
    fn amendable(&self) -> bool {
        self.state().amendable()
    }
    fn can_primary(&self) -> bool {
        self.state().can_primary(now_ms())
    }
    fn can_recover(&self) -> bool {
        self.state().can_recover()
    }
    fn can_cancel_intent(&self) -> bool {
        self.state().can_cancel_intent()
    }
}
/// The panel's own argument for the native successor kind.
fn succession_arg(kind: OwnerGrantSuccessionKind) -> Succession {
    match kind {
        OwnerGrantSuccessionKind::Renewal => Succession::Renewal,
        _ => Succession::Amendment,
    }
}
impl MiniAppsPanel {
    pub(super) fn begin_grant_confirmation(
        &mut self,
        cx: &mut Cx,
        args: Value,
        reply: (usize, u64),
    ) -> Result<(), String> {
        if self.execution_confirmation.is_some()
            || self.grant_confirmation.is_some()
            || self.adoption_confirmation.is_some()
            || self.provider_confirmation.is_some()
            || self.account_login.is_some()
            || self.reauthentication.is_some()
            || self.project_room_reply.is_some()
            || self.pending.len() >= 16
        {
            return Err("provider_confirmation_busy".into());
        }
        let host = self.hagency.as_ref().ok_or("management_session_required")?;
        let flow = host.grant_handoff(
            self.lease.clone().ok_or("management_session_required")?,
            args,
        )?;
        let proposal = flow.proposed_tokens();
        self.grant_confirmation = Some(GrantPrompt {
            flow: flow.clone(),
            reply,
            phase: Phase::Loading,
            original: None,
            receiver: None,
            draft: { let mut draft = initial_draft(proposal, flow.applicants()); draft.per_request = flow.proposed_per_request_tokens().map(|n| n.to_string()).unwrap_or_default(); draft },
            publishing: false,
            cancelling: false,
            form: None,
            submit_publication: false,
            proposal,
            regrant: false,
            successor: None,
            submitted: false,
            disclosure: Disclosure { people: flow.choose_people(), ..Default::default() },
            add_error: String::new(),
            add_people: flow.choose_people() && flow.requested_succession() == Some(Succession::Amendment),
            people_search: PeopleSearch::default(),
        });
        self.view.text_input(cx, ids!(grant_confirmation.sheet_scroll.people.list.add.add_name)).set_text(cx, "");
        self.grant_job(async move { flow.open().await });
        self.grant_add_person(cx);
        self.present_grant_confirmation(cx);
        cx.set_key_focus(
            self.view
                .button(cx, ids!(grant_confirmation.buttons.cancel))
                .area(),
        );
        Ok(())
    }
    fn grant_job(
        &mut self,
        task: impl std::future::Future<Output = Result<GrantResult, String>> + Send + 'static,
    ) {
        let Some(prompt) = &mut self.grant_confirmation else {
            return;
        };
        let (tx, receiver) = mpsc::sync_channel(1);
        prompt.receiver = Some(receiver);
        prompt.phase = Phase::Loading;
        crate::sliding_sync::spawn_async_task(async move {
            let result = task.await;
            if let Err(code) = &result {
                let diagnostic = grant_failure_code(code);
                makepad_widgets::warning!("Native owner grant operation failed: {diagnostic}");
            }
            let _ = tx.try_send(result);
            SignalToUI::set_ui_signal();
        });
    }
    /// A dropdown that is open holds the window's pointer lock and gives it
    /// back when it closes. Once its sheet is hidden it receives no event, so
    /// it never closes: every later click in the window went nowhere until
    /// the app was restarted (Escape with the account list open did this).
    /// Close them whenever a sheet goes away; closing a closed one is a no-op.
    pub(super) fn close_sheet_dropdowns(&mut self, cx: &mut Cx) {
        let paths: [&[LiveId]; 3] = [
            ids!(grant_confirmation.scroll.people.list.add.directory.candidates),
            ids!(grant_confirmation.scroll.calendar.zones),
            ids!(project_room_picker.rooms),
        ];
        for path in paths {
            if let Some(mut dropdown) = self.view.drop_down(cx, path).borrow_mut() {
                dropdown.set_closed(cx);
            }
        }
    }
    pub(super) fn present_grant_confirmation(&mut self, cx: &mut Cx) {
        let visible = self.grant_confirmation.is_some();
        if !visible {
            self.close_sheet_dropdowns(cx);
        }
        self.view
            .view(cx, ids!(grant_confirmation))
            .set_visible(cx, visible);
        let Some(prompt) = &self.grant_confirmation else {
            return;
        };
        let now = now_ms();
        self.view.view(cx, ids!(app_content)).set_visible(cx, false);
        self.view.view(cx, ids!(approval)).set_visible(cx, false);
        let state = prompt.state();
        let view = grant_view(&state, now);
        let inputs = GrantInputs::of(&state, now);
        self.apply_grant_view(cx, &view, Some(&inputs));
        self.present_grant_people(cx);
        self.request_confirmation_redraw(cx);
    }
    /// Write one computed view into the panel. `inputs`, when given, also
    /// refills the native amount field; the draft is always read back from it.
    fn apply_grant_view(&mut self, cx: &mut Cx, view: &GrantView, inputs: Option<&GrantInputs>) {
        self.apply_sheet(cx, id!(grant_confirmation), view);
        self.view.view(cx, ids!(grant_confirmation.sheet_scroll.request_quota)).set_visible(cx, view.choosing);
        self.view.label(cx, ids!(grant_confirmation.sheet_scroll.request_quota.label)).set_text(cx, crate::i18n::tr("Maximum per Agent per period"));
        self.view.label(cx, ids!(grant_confirmation.sheet_scroll.request_quota.hint)).set_text(cx, crate::i18n::tr("Each Agent can request up to this amount. All Agents share the pool total above."));
        self.apply_sheet_disclosure(cx, id!(grant_confirmation), view.more_open);
        self.view.view(cx, ids!(grant_confirmation.sheet_scroll.pool_renewal)).set_visible(cx, view.renewal_mode.is_some());
        if let Some(mode) = view.renewal_mode {
            self.view.label(cx, ids!(grant_confirmation.sheet_scroll.pool_renewal.title)).set_text(cx, crate::i18n::tr("Pool renewal"));
            for (path, value, label) in [
                (ids!(grant_confirmation.sheet_scroll.pool_renewal.choices.automatic), RenewalMode::Automatic, "Automatic"),
                (ids!(grant_confirmation.sheet_scroll.pool_renewal.choices.manual), RenewalMode::Owner, "Manual"),
            ] {self.view.widget(cx,path).set_text(cx,&format!("{}{}",crate::i18n::tr(label),if value==mode {" ✓"} else {""}));}
            self.view.label(cx, ids!(grant_confirmation.sheet_scroll.pool_renewal.note)).set_text(cx,crate::i18n::tr(if mode==RenewalMode::Automatic {"At each new period, the pool renews and approved Agents receive their allowance. Unused tokens do not carry over."} else {"The pool pauses at period end. Publish the next period to renew all approved Agents together."}));
        }
        self.view.view(cx, ids!(grant_confirmation.sheet_scroll.calendar)).set_visible(cx, view.calendar.is_some());
        if let Some(calendar) = &view.calendar {
            let tr = crate::i18n::tr;
            self.view.label(cx, ids!(grant_confirmation.sheet_scroll.calendar.title)).set_text(cx, tr("Authorization calendar"));
            self.view.label(cx, ids!(grant_confirmation.sheet_scroll.calendar.zone_label)).set_text(cx, tr("Owner time zone"));
            for (path, cadence, label) in [
                (ids!(grant_confirmation.sheet_scroll.calendar.cadence.daily), Cadence::Daily, "Daily"),
                (ids!(grant_confirmation.sheet_scroll.calendar.cadence.weekly), Cadence::Weekly, "Weekly"),
                (ids!(grant_confirmation.sheet_scroll.calendar.cadence.monthly), Cadence::Monthly, "Monthly"),
            ] {
                self.view.widget(cx, path).set_text(cx, &format!("{}{}", tr(label), if cadence == calendar.cadence {" ✓"} else {""}));
            }
            if inputs.is_some() {self.view.text_input(cx, ids!(grant_confirmation.sheet_scroll.calendar.zone)).set_text(cx, &calendar.time_zone);}
            let zones = self.view.drop_down(cx, ids!(grant_confirmation.sheet_scroll.calendar.zones));
            zones.set_labels(cx, std::iter::once(tr("Choose a time zone").to_owned()).chain(CALENDAR_ZONES.iter().map(|zone| (*zone).to_owned())).collect());
            zones.set_selected_item(cx, CALENDAR_ZONES.iter().position(|zone| *zone == calendar.time_zone).map_or(0, |i| i + 1));
            let note = calendar_note(calendar, now_ms());
            self.view.label(cx, ids!(grant_confirmation.sheet_scroll.calendar.note)).set_text(cx, &note);
            let mut note_widget=self.view.widget(cx, ids!(grant_confirmation.sheet_scroll.calendar.note));
            if calendar.period_at(now_ms()).is_ok() {script_apply_eval!(cx, note_widget, {draw_text +: {color: mod.widgets.RINX_MUTED}});}
            else {script_apply_eval!(cx, note_widget, {draw_text +: {color: mod.widgets.RINX_DANGER_FG}});}

        }

        if let Some(inputs) = inputs.filter(|_| view.choosing) {
            for (index, (id, on)) in [
                (ids!(grant_confirmation.sheet_scroll.sharing.period.expiry_presets.day), ids!(grant_confirmation.sheet_scroll.sharing.period.expiry_presets.day_on)),
                (ids!(grant_confirmation.sheet_scroll.sharing.period.expiry_presets.week), ids!(grant_confirmation.sheet_scroll.sharing.period.expiry_presets.week_on)),
                (ids!(grant_confirmation.sheet_scroll.sharing.period.expiry_presets.month), ids!(grant_confirmation.sheet_scroll.sharing.period.expiry_presets.month_on)),
            ]
            .into_iter()
            .enumerate()
            {
                // The chosen period shows selected, like the page's chosen preset.
                self.view.button(cx, id).set_disabled(cx, !inputs.presets[index]);
                self.view.widget(cx, id).set_visible(cx, !inputs.selected[index]);
                self.view.widget(cx, on).set_visible(cx, inputs.selected[index]);
            }
            let limit = self.view.widget(cx, ids!(grant_confirmation.sheet_scroll.request_quota.limit));
            if let Some(mut editor) = limit.borrow_mut::<crate::miniapps::design::MiniAppNumberInput>() {
                editor.set_minimum(cx, 1);
                editor.set_maximum(cx, inputs.tokens.parse().unwrap_or(0));
            }
            self.view.widget(cx, ids!(grant_confirmation.sheet_scroll.request_quota.limit)).set_text(cx, &inputs.per_request);
            let widget = self.view.widget(cx, ids!(grant_confirmation.sheet_scroll.quota.tokens));
            if let Some(mut editor) = widget.borrow_mut::<crate::miniapps::design::MiniAppNumberInput>() {
                editor.set_minimum(cx, inputs.minimum);
                if let Some(maximum) = inputs.maximum {
                    editor.set_maximum(cx, maximum);
                }
            }
            self.view
                .widget(cx, ids!(grant_confirmation.sheet_scroll.quota.tokens))
                .set_text(cx, &inputs.tokens);
        }
    }
    fn calendar_input(&self, cx: &Cx, actions: &Actions, original: &Calendar) -> Option<Calendar> {
        let mut calendar = original.clone();
        for (path, cadence) in [
            (ids!(grant_confirmation.sheet_scroll.calendar.cadence.daily), Cadence::Daily),
            (ids!(grant_confirmation.sheet_scroll.calendar.cadence.weekly), Cadence::Weekly),
            (ids!(grant_confirmation.sheet_scroll.calendar.cadence.monthly), Cadence::Monthly),
        ] { if crate::miniapps::design::key_button_clicked(&self.view.widget(cx, path), actions) {calendar.cadence = cadence;} }
        if let Some(zone) = self.view.text_input(cx, ids!(grant_confirmation.sheet_scroll.calendar.zone)).changed(actions) {calendar.time_zone = zone.trim().into();}
        if let Some(index) = self.view.drop_down(cx, ids!(grant_confirmation.sheet_scroll.calendar.zones)).selected(actions) {
            if let Some(zone) = index.checked_sub(1).and_then(|i| CALENDAR_ZONES.get(i)) {calendar.time_zone = (*zone).into();}
        }
        (calendar != *original).then_some(calendar)
    }
    pub(super) fn grant_renewal_actions(&mut self, cx: &mut Cx, actions: &Actions) -> bool {
        if !self.grant_confirmation.as_ref().is_some_and(|p|matches!(p.phase,Phase::Choose) && p.draft.calendar.is_some()) {return false;}
        for (path, mode) in [(ids!(grant_confirmation.sheet_scroll.pool_renewal.choices.automatic),RenewalMode::Automatic),(ids!(grant_confirmation.sheet_scroll.pool_renewal.choices.manual),RenewalMode::Owner)] {
            if crate::miniapps::design::key_button_clicked(&self.view.widget(cx,path),actions) {
                self.capture_grant_choices(cx);
                if let Some(p)=self.grant_confirmation.as_mut(){p.draft.renewal_mode=Some(mode);}
                self.present_grant_confirmation(cx);
                return true;
            }
        }
        false
    }
    pub(super) fn grant_calendar_actions(&mut self, cx: &mut Cx, actions: &Actions) -> bool {
        let Some(calendar) = self.grant_confirmation.as_ref().filter(|p| matches!(p.phase, Phase::Choose) && p.successor.is_none() && p.form.as_ref().is_none_or(|form| form.calendar.is_none())).and_then(|p| p.draft.calendar.as_ref()) else {return false};
        let Some(calendar) = self.calendar_input(cx, actions, calendar) else {return false};
        let typing=self.view.text_input(cx, ids!(grant_confirmation.sheet_scroll.calendar.zone)).changed(actions).is_some();
        if let Some(prompt) = &mut self.grant_confirmation {prompt.draft.calendar = Some(calendar);}
        if typing {
            if let Some(prompt)=&self.grant_confirmation {let view=grant_view(&prompt.state(),now_ms());self.apply_grant_view(cx,&view,None);}
            self.request_confirmation_redraw(cx);
        } else {self.present_grant_confirmation(cx);}
        true
    }
    /// Open or close the technical details; the draft is read back first.
    pub(super) fn grant_disclose(&mut self, cx: &mut Cx) {
        self.capture_grant_choices(cx);
        let Some(prompt) = &mut self.grant_confirmation else {
            return;
        };
        prompt.disclosure.more = !prompt.disclosure.more;
        self.present_grant_confirmation(cx);
    }
    /// Open or close the sharing period's choice under its row.
    pub(super) fn grant_sharing_toggle(&mut self, cx: &mut Cx) {
        self.capture_grant_choices(cx);
        let Some(prompt) = &mut self.grant_confirmation else {
            return;
        };
        prompt.disclosure.sharing = !prompt.disclosure.sharing;
        self.present_grant_confirmation(cx);
    }
    /// From this month's record: add people in this same window. It opens
    /// the amendment form of the record's own pool, which keeps the
    /// pool total allowance; only that form's own confirmation commits anything.
    pub(super) fn grant_add_people(&mut self, cx: &mut Cx) {
        let Some(prompt) = &mut self.grant_confirmation else {
            return;
        };
        if !grant_view(&prompt.state(), now_ms()).people_add_visible {
            return;
        }
        let Some(contribution) = phase_summary(&prompt.phase).map(|summary| summary.contribution_id) else {
            return;
        };
        let Ok(flow) = prompt.flow.for_record_pool(contribution) else {
            return;
        };
        prompt.flow = flow.clone();
        prompt.add_people = true;
        prompt.original = None;
        prompt.publishing = false;
        prompt.cancelling = false;
        prompt.submitted = false;
        prompt.submit_publication = false;
        prompt.proposal = None;
        prompt.form = None;
        prompt.successor = None;
        prompt.draft = initial_draft(None, &[]);
        prompt.disclosure = Disclosure::default();
        prompt.add_error.clear();
        self.grant_job(async move { flow.choose_successor(Succession::Amendment).await });
        self.present_grant_confirmation(cx);
    }
    /// Open or fold a list of people this window doesn't change.
    pub(super) fn grant_people_toggle(&mut self, cx: &mut Cx) {
        self.capture_grant_choices(cx);
        let Some(prompt) = &mut self.grant_confirmation else {
            return;
        };
        prompt.disclosure.people = !prompt.disclosure.people;
        self.present_grant_confirmation(cx);
    }
    /// Search the actual homeserver. The field is never directly added to
    /// the whitelist; the owner selects a returned account instead.
    pub(super) fn grant_add_person(&mut self, cx: &mut Cx) {
        self.capture_grant_choices(cx);
        let input = self.view.text_input(cx, ids!(grant_confirmation.sheet_scroll.people.list.add.add_name));
        let query = input.text().trim().to_owned();
        let Some(prompt) = &mut self.grant_confirmation else {
            return;
        };
        if !prompt.flow.active() || !matches!(prompt.phase, Phase::Choose | Phase::Loading) {
            return;
        }
        let (tx, receiver) = mpsc::sync_channel(1);
        prompt.people_search = PeopleSearch { query: query.clone(), receiver: Some(receiver), ..Default::default() };
        prompt.add_error.clear();
        let flow = prompt.flow.clone();
        crate::sliding_sync::spawn_async_task(async move {
            let _ = tx.try_send(flow.search_people(query).await);
            SignalToUI::set_ui_signal();
        });
        self.present_grant_confirmation(cx);
    }
    pub(super) fn grant_select_person(&mut self, cx: &mut Cx, index: usize) {
        self.capture_grant_choices(cx);
        let query = self.view.text_input(cx, ids!(grant_confirmation.sheet_scroll.people.list.add.add_name)).text();
        let Some(prompt) = &mut self.grant_confirmation else { return };
        if !matches!(prompt.phase, Phase::Choose) || !prompt.actionable()
            || prompt.people_search.query != query.trim() || prompt.people_search.receiver.is_some() {
            return;
        }
        let managers = managers_of(&prompt.draft.managers);
        let candidates = prompt.people_search.candidates(&managers);
        prompt.people_search.selected = index.checked_sub(1).and_then(|i| candidates.get(i)).map(|person| person.id.clone());
        self.present_grant_people(cx);
        self.request_confirmation_redraw(cx);
    }
    pub(super) fn grant_add_selected_person(&mut self, cx: &mut Cx) {
        self.capture_grant_choices(cx);
        let query = self.view.text_input(cx, ids!(grant_confirmation.sheet_scroll.people.list.add.add_name)).text();
        let Some(prompt) = &mut self.grant_confirmation else { return };
        if !matches!(prompt.phase, Phase::Choose) || !prompt.actionable()
            || prompt.people_search.query != query.trim() || prompt.people_search.receiver.is_some() {
            return;
        }
        let Some(selected) = prompt.people_search.selected.take() else { return };
        let mut managers = managers_of(&prompt.draft.managers);
        let candidates = prompt.people_search.candidates(&managers);
        let Some(person) = candidates.iter().find(|person| person.id == selected) else { return };
        match applicant_from_input(&person.id, prompt.flow.account(), &managers) {
            Ok(id) => {
                managers.push(id);
                prompt.draft.managers = managers_of(&managers.join(" ")).join(" ");
                prompt.add_error.clear();
            }
            Err(reason) => prompt.add_error = reason,
        }
        self.present_grant_confirmation(cx);
    }
    fn present_grant_people(&mut self, cx: &mut Cx) {
        let Some(prompt) = &self.grant_confirmation else { return };
        let picker = &prompt.people_search;
        let candidates = picker.candidates(&managers_of(&prompt.draft.managers));
        let tr = crate::i18n::tr;
        let status = if picker.receiver.is_some() { tr("Searching server accounts…") }
            else if picker.error.is_some() { tr("Couldn’t load accounts. Press Search to try again.") }
            else if picker.result.is_none() { tr("Press Search to find matching accounts.") }
            else if candidates.is_empty() { tr("No additional local accounts found. Try an exact username.") }
            else if picker.limited(&managers_of(&prompt.draft.managers)) { tr("More accounts are available. Search by name to narrow the list.") }
            else { tr("Choose a verified account to add. Already selected people are hidden.") };
        let searching = picker.receiver.is_some();
        let selected = picker.selected.as_ref().and_then(|id| candidates.iter().position(|p| &p.id == id)).map_or(0, |i| i + 1);
        self.apply_grant_people(cx, &candidates, selected, status, searching);
    }
    fn apply_grant_people(&mut self, cx: &mut Cx, candidates: &[crate::host::hagency::grant_people::Person], selected: usize, status: &str, searching: bool) {
        let tr = crate::i18n::tr;
        let dropdown = self.view.drop_down(cx, ids!(grant_confirmation.sheet_scroll.people.list.add.directory.candidates));
        dropdown.set_labels(cx, std::iter::once(tr("Select a person to add").to_owned())
            .chain(candidates.iter().map(|p| p.label())).collect());
        dropdown.set_selected_item(cx, selected);
        dropdown.set_visible(cx, !candidates.is_empty());
        let add = self.view.button(cx, ids!(grant_confirmation.sheet_scroll.people.list.add.directory.add_selected));
        add.set_visible(cx, !candidates.is_empty());
        add.set_enabled(cx, selected > 0 && !searching);
        let identity = selected.checked_sub(1).and_then(|i| candidates.get(i)).map(|p| p.label());
        self.view.label(cx, ids!(grant_confirmation.sheet_scroll.people.list.add.directory.status)).set_text(cx, identity.as_deref().unwrap_or(status));
        self.view.view(cx, ids!(grant_confirmation.sheet_scroll.people.list.add.directory)).set_visible(cx, true);
        self.view.text_input(cx, ids!(grant_confirmation.sheet_scroll.people.list.add.add_name))
            .set_empty_text(cx, tr("Search by name or username").to_owned());
        self.view.button(cx, ids!(grant_confirmation.sheet_scroll.people.list.add.add_button))
            .set_enabled(cx, !searching);
    }
    /// Take someone this window added off the list again. People already on
    /// the published list stay: their rows have no remove button.
    pub(super) fn grant_remove_person(&mut self, cx: &mut Cx, index: usize) {
        self.capture_grant_choices(cx);
        let Some(prompt) = &mut self.grant_confirmation else {
            return;
        };
        if !matches!(prompt.phase, Phase::Choose) || !prompt.actionable() {
            return;
        }
        let Some(person) = choose_people(&prompt.state()).into_iter().nth(index).filter(|p| p.removable) else {
            return;
        };
        let managers: Vec<String> = managers_of(&prompt.draft.managers).into_iter().filter(|id| *id != person.id).collect();
        prompt.draft.managers = managers.join(" ");
        prompt.add_error.clear();
        self.present_grant_confirmation(cx);
    }
    /// Typing in the add field clears the reason shown for the last attempt.
    pub(super) fn grant_add_typing(&mut self, cx: &mut Cx) {
        let Some(prompt) = &mut self.grant_confirmation else {
            return;
        };
        prompt.add_error.clear();
        prompt.people_search = PeopleSearch::default();
        let view = grant_view(&prompt.state(), now_ms());
        self.apply_grant_view(cx, &view, None);
        self.present_grant_people(cx);
        self.request_confirmation_redraw(cx);
    }
    pub(super) fn poll_grant_confirmation(&mut self, cx: &mut Cx) {
        let Some(prompt) = &mut self.grant_confirmation else {
            return;
        };
        let result = prompt
            .receiver
            .as_ref()
            .and_then(|receiver| match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("management_outcome_unknown".into()))
                }
                Err(_) => None,
            });
        let mut changed = false;
        let people = prompt.people_search.receiver.as_ref().and_then(|receiver| match receiver.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Disconnected) => Some(Err("grant_people_unavailable".into())),
            Err(_) => None,
        });
        if let Some(result) = people {
            prompt.people_search.receiver = None;
            if prompt.flow.active() {
                match result {
                    Ok(people) => prompt.people_search.result = Some(people),
                    Err(error) => prompt.people_search.error = Some(error),
                }
            } else {
                prompt.people_search = PeopleSearch::default();
            }
            changed = true;
        }
        // Adding people from the record: the name field is ready once the form is.
        let mut focus_add = false;
        if let Some(result) = result {
            prompt.receiver = None;
            prompt.phase = match result {
                Ok(GrantResult::Choose(form)) => {
                    prompt.original = None;
                    prompt.publishing = false;
                    prompt.cancelling = false;
                    prompt.submit_publication = false;
                    prompt.submitted = false;
                    prompt.successor = None;
                    if prompt.draft.calendar.is_none() { prompt.draft.calendar = Some(default_calendar()); }
                    fill_form_defaults(&mut prompt.draft, &form);
                    prompt.draft.coordinator=prompt.flow.account().into();
                    prompt.form=Some(form);
                    Phase::Choose
                }
                Ok(GrantResult::ChooseSuccessor(form)) => {
                    prompt.original = None;
                    prompt.publishing = false;
                    prompt.cancelling = false;
                    prompt.submit_publication = false;
                    prompt.submitted = false;
                    focus_add = prompt.add_people;
                    fill_successor_defaults(&mut prompt.draft, &form, now_ms());
                    prompt.draft.coordinator=prompt.flow.account().into();
                    prompt.form=None;
                    prompt.successor=Some(*form);
                    Phase::Choose
                }
                Ok(GrantResult::Review(review)) => {
                    prompt.original = Some(review.summary().intent_id);
                    prompt.cancelling = false;
                    let same=match (&prompt.successor,&prompt.form) {
                        (Some(form),_)=>same_successor_resource(form,&review),
                        (None,Some(form))=>same_publication_resource(form,&review),
                        _=>true,
                    };
                    if prompt.submit_publication && !same {
                        prompt.submit_publication=false;
                    }
                    Phase::Review(review)
                }
                Ok(GrantResult::Progress(progress)) => {
                    prompt.original = Some(progress.summary.intent_id.clone());
                    prompt.cancelling = false;
                    Phase::Progress(*progress)
                }
                Ok(GrantResult::CancellationReview(review)) => {
                    prompt.original = Some(review.summary().intent_id);
                    prompt.cancelling = true;
                    Phase::CancellationReview(review)
                }
                Ok(GrantResult::PublicationReview(review)) => {
                    prompt.original = Some(review.summary().intent_id);
                    prompt.publishing = true;
                    Phase::PublicationReview(review)
                }
                Ok(GrantResult::PublicationProgress(progress)) => {
                    prompt.original = Some(progress.summary.intent_id.clone());
                    prompt.publishing = true;
                    Phase::PublicationProgress(*progress)
                }
                Err(code) if matches!(code.as_str(), "grant_person_not_found" | "grant_people_unavailable") => {
                    prompt.submit_publication = false;
                    prompt.submitted = false;
                    prompt.add_error = crate::i18n::tr(failure_message(&code)).into();
                    Phase::Choose
                }
                Err(code) => Phase::Failed(code),
            };
            changed = true;
        }
        if matches!(&prompt.phase,Phase::Review(review) if review.expired()) {
            prompt.phase = Phase::Failed("owner_grant_original_review_expired".into());
            changed = true;
        }
        if matches!(&prompt.phase,Phase::PublicationReview(review) if review.expired()) {
            prompt.phase = Phase::Failed("owner_grant_publication_review_expired".into());
            changed = true;
        }
        if matches!(&prompt.phase,Phase::CancellationReview(review) if review.expired()) {
            prompt.phase = Phase::Failed("owner_grant_review_changed".into());
            changed = true;
        }
        if changed {
            self.continue_grant_publication();
            self.present_grant_confirmation(cx);
            if focus_add {
                let field = self.view.text_input(cx, ids!(grant_confirmation.sheet_scroll.people.list.add.add_name)).area();
                cx.set_key_focus(field);
            }
        }
    }
    /// Advance only the exact native reviews covered by this window's owner
    /// submission. Unknown outcomes, rejection, closing, and reopen all stop it.
    fn continue_grant_publication(&mut self) {
        let Some(prompt)=&mut self.grant_confirmation else{return};
        if !prompt.submit_publication || !prompt.actionable(){return}
        let flow=prompt.flow.clone();
        match std::mem::replace(&mut prompt.phase,Phase::Loading) {
            Phase::Review(review) if !review.expired() && review.capacity_available()=> {
                self.grant_job(async move {flow.confirm(*review).await});
            }
            Phase::Progress(progress) if matches!(progress.state,OwnerGrantState::Recorded) && progress.closure.is_none()=> {
                prompt.publishing=true;
                self.grant_job(async move {flow.publication_review(progress.summary.intent_id).await});
            }
            Phase::PublicationReview(review) if !review.expired()=> {
                self.grant_job(async move {flow.publication_confirm(*review).await});
            }
            phase=> {prompt.phase=phase;prompt.submit_publication=false;}
        }
    }
    /// The sharing-period preset tapped in these actions, if any: 1 day, 7
    /// days or to month end. The presets are key buttons, so their taps are
    /// found by widget uid; `ButtonRef::clicked` never sees them.
    pub(super) fn grant_preset_clicked(&self, cx: &Cx, actions: &Actions) -> Option<Option<i64>> {
        let presets: [(&[LiveId], Option<i64>); 3] = [
            (ids!(grant_confirmation.sheet_scroll.sharing.period.expiry_presets.day), Some(1)),
            (ids!(grant_confirmation.sheet_scroll.sharing.period.expiry_presets.week), Some(7)),
            (ids!(grant_confirmation.sheet_scroll.sharing.period.expiry_presets.month), None),
        ];
        presets
            .into_iter()
            .find(|(path, _)| crate::miniapps::design::key_button_clicked(&self.view.widget(cx, path), actions))
            .map(|(_, days)| days)
    }
    pub(super) fn grant_expiry_preset(&mut self,cx:&mut Cx,days:Option<i64>){
        self.capture_grant_choices(cx);
        let Some(prompt)=&mut self.grant_confirmation else{return};
        if !matches!(prompt.phase,Phase::Choose){return}
        if let Some(form)=&prompt.successor {
            let Some(value)=successor_preset(form,days,now_ms()) else{return};
            prompt.draft.expires_utc=value;
        } else {
            if !publication_preset_available(days){return}
            prompt.draft.expires_utc=publication_expiry(days);
        }
        self.present_grant_confirmation(cx);
    }
    pub(super) fn capture_grant_choices(&mut self, cx: &mut Cx) {
        let Some(prompt) = &mut self.grant_confirmation else {
            return;
        };
        if !matches!(prompt.phase, Phase::Choose) {
            return;
        }
        // The amount is the one typed field. The list and the expiry are
        // native state changed only by this sheet's add, remove and preset
        // controls, so they are kept as they are.
        let draft = GrantChoices { per_request: self.view.widget(cx, ids!(grant_confirmation.sheet_scroll.request_quota.limit)).text(),
            coordinator: prompt.flow.account().to_owned(),
            managers: prompt.draft.managers.clone(),
            tokens: self
                .view
                .widget(cx, ids!(grant_confirmation.sheet_scroll.quota.tokens))
                .text(),
            expires_utc: prompt.draft.expires_utc.clone(),
            self_approval: false,
            calendar: prompt.draft.calendar.clone(),
            renewal_mode: prompt.draft.renewal_mode,
        };
        let changed = draft.tokens != prompt.draft.tokens || draft.per_request != prompt.draft.per_request;
        prompt.draft = draft;
        // Keep every sentence, the list and the confirm button equal to what
        // the fields would commit. The fields themselves are left untouched.
        if changed {
            let view = grant_view(&prompt.state(), now_ms());
            self.apply_grant_view(cx, &view, None);
            self.request_confirmation_redraw(cx);
        }
    }
    pub(super) fn grant_primary(&mut self, cx: &mut Cx) {
        self.capture_grant_choices(cx);
        self.poll_grant_confirmation(cx);
        let Some(prompt) = &mut self.grant_confirmation else {
            return;
        };
        // Done closes a finished change; after a refusal or an unknown
        // outcome the primary button is the recovery step itself.
        let view = grant_view(&prompt.state(), now_ms());
        if view.primary_closes {
            self.close_grant_confirmation(cx);
            return;
        }
        if view.primary_recovers {
            // "Change to 0.6 M": back to the form with that amount in it.
            if let Some(tokens) = view.fix_tokens {
                prompt.draft.tokens = tokens.to_string();
            }
            self.grant_recover(cx);
            return;
        }
        if !prompt.can_primary() {
            return;
        }
        let flow = prompt.flow.clone();
        let now = now_ms();
        match std::mem::replace(&mut prompt.phase, Phase::Loading) {
            // Before any other action on a recorded grant: its month ended, so
            // the only step is this month's grant, never publishing the old one.
            phase if ended_grant(&phase, now) => {
                prompt.regrant = true;
                self.grant_job(async move { flow.choose_new_limits().await });
            }
            // A proposal beside this month's active grant opens the native
            // amendment form; nothing is reserved or published until the
            // owner submits that form.
            phase
                if published_grant(&phase, now)
                    && prompt.amendable()
                    && live_grant_tokens(&phase, now).is_some() =>
            {
                self.grant_job(async move { flow.choose_successor(Succession::Amendment).await });
            }
            Phase::Choose => {
                let choices = prompt.draft.clone();
                if let Err(code) = choices.clone().parse() {
                    if let Some(prompt) = &mut self.grant_confirmation {
                        prompt.phase = Phase::Choose;
                    }
                    self.present_grant_confirmation(cx);
                    self.view
                        .label(cx, ids!(grant_confirmation.sheet_scroll.status))
                        .set_text(cx, crate::i18n::tr(failure_message(&code)));
                    return;
                }
                // Validate native input before consuming the one possible prepare.
                let kind = prompt.successor.as_ref().map(|form| succession_arg(form.kind));
                prompt.submit();
                match kind {
                    Some(kind) => {
                        self.grant_job(async move { flow.prepare_successor(kind, choices).await })
                    }
                    None => self.grant_job(async move { flow.prepare(choices).await }),
                }
            }
            Phase::Review(review) => {
                prompt.submit();
                self.grant_job(async move { flow.confirm(*review).await });
            }
            Phase::PublicationReview(review) => {
                prompt.submit();
                self.grant_job(async move { flow.publication_confirm(*review).await })
            }
            Phase::CancellationReview(review) => {
                self.grant_job(async move { flow.cancellation_confirm(*review).await })
            }
            Phase::Progress(progress) if matches!(progress.state, OwnerGrantState::Declined) => {
                self.grant_job(async move { flow.choose_new_limits().await });
            }
            Phase::Progress(progress)
                if matches!(progress.state, OwnerGrantState::Recorded)
                    && progress.closure.is_some() =>
            {
                self.grant_job(async move { flow.choose_new_limits().await });
            }
            Phase::Progress(progress) if matches!(progress.state, OwnerGrantState::Recorded) => {
                prompt.publishing = true;
                prompt.submit();
                self.grant_job(
                    async move { flow.publication_review(progress.summary.intent_id).await },
                );
            }
            _ => {
                let id = prompt.original.clone();
                let publishing = prompt.publishing;
                let cancelling = prompt.cancelling;
                self.grant_job(async move {
                    if cancelling {
                        flow.cancellation_review(id.ok_or("owner_grant_original_intent_required")?)
                            .await
                    } else if publishing {
                        flow.publication_review(id.ok_or("owner_grant_original_intent_required")?)
                            .await
                    } else {
                        flow.review(id).await
                    }
                });
            }
        }
        self.present_grant_confirmation(cx);
    }
    pub(super) fn grant_cancel_intent(&mut self, cx: &mut Cx) {
        let Some(prompt) = &mut self.grant_confirmation else {
            return;
        };
        if !prompt.can_cancel_intent() {
            return;
        }
        let Some(id) = prompt.original.clone() else {
            return;
        };
        let flow = prompt.flow.clone();
        prompt.cancelling = true;
        self.grant_job(async move { flow.cancellation_review(id).await });
        self.present_grant_confirmation(cx);
    }
    pub(super) fn grant_recover(&mut self, cx: &mut Cx) {
        let Some(prompt) = &self.grant_confirmation else {
            return;
        };
        if !prompt.can_recover() {
            return;
        }
        let flow = prompt.flow.clone();
        let id = prompt.original.clone();
        let publishing = prompt.publishing;
        // This month's form refreshes its capacity, e.g. once last month's
        // Agents have reported their final usage; a successor form refreshes
        // its headroom and carried floor.
        let regrant = prompt.regrant;
        let successor = prompt.successor.as_ref().map(|form| succession_arg(form.kind));
        self.grant_job(async move {
            match id {
                Some(id) if publishing => flow.publication_review(id).await,
                Some(id) => flow.recover(id).await,
                None if successor.is_some() => {
                    flow.choose_successor(successor.unwrap_or(Succession::Amendment)).await
                }
                None if regrant => flow.choose_new_limits().await,
                None => flow.open().await,
            }
        });
        self.present_grant_confirmation(cx);
    }
    pub(super) fn close_grant_confirmation(&mut self, cx: &mut Cx) {
        let Some(prompt) = self.grant_confirmation.take() else {
            return;
        };
        // Close never mutates the private journal. An in-flight effect keeps
        // its original native custody even when the screen/reply disappears.
        let reply = close_reply(
            &prompt.phase,
            prompt.proposal,
            prompt.receiver.is_some(),
            prompt.original.as_deref(),
            prompt.flow.succession_supported() == Some(true),
            now_ms(),
        );
        splash_host_respond(
            cx,
            prompt.reply.0,
            prompt.reply.1,
            reply.as_ref().map(String::as_str).map_err(String::as_str),
        );
        self.present_grant_confirmation(cx);
        self.show_approval(cx);
        self.view.view(cx, ids!(app_content)).set_visible(cx, true);
        self.reply_redraw = cx.new_next_frame();
        self.view.redraw(cx);
    }
    pub(super) fn grant_key(&mut self, cx: &mut Cx, event: &Event) -> bool {
        let Some(prompt) = &self.grant_confirmation else {
            return false;
        };
        let Event::KeyDown(key) = event else {
            return false;
        };
        if key.key_code == KeyCode::Escape {
            self.close_grant_confirmation(cx);
            return true;
        }
        let view = grant_view(&prompt.state(), now_ms());
        let area = |this: &Self, cx: &mut Cx, path: &[LiveId]| this.view.button(cx, path).area();
        let cancel = area(self, cx, ids!(grant_confirmation.buttons.cancel));
        let primary = area(self, cx, ids!(grant_confirmation.buttons.primary));
        let recover = area(self, cx, ids!(grant_confirmation.sheet_scroll.actions.recover));
        let cancel_intent = area(self, cx, ids!(grant_confirmation.sheet_scroll.actions.cancel_intent));
        let more = area(self, cx, ids!(grant_confirmation.sheet_scroll.more.more_row.more_toggle));
        let sharing = area(self, cx, ids!(grant_confirmation.sheet_scroll.sharing.sharing_row.sharing_toggle));
        let open_add = area(self, cx, ids!(grant_confirmation.sheet_scroll.people.list.open_add.open_add_button));
        let add = area(self, cx, ids!(grant_confirmation.sheet_scroll.people.list.add.add_button));
        let add_selected = area(self, cx, ids!(grant_confirmation.sheet_scroll.people.list.add.directory.add_selected));
        let people_toggle = area(self, cx, ids!(grant_confirmation.sheet_scroll.people.list.people_toggle_row.people_toggle));
        let folded = view.people_collapsible && !view.people_open;
        let presets = [
            (ids!(grant_confirmation.sheet_scroll.sharing.period.expiry_presets.day), Some(1)),
            (ids!(grant_confirmation.sheet_scroll.sharing.period.expiry_presets.week), Some(7)),
            (ids!(grant_confirmation.sheet_scroll.sharing.period.expiry_presets.month), None),
        ];
        let shown = super::sheet_ui::people_shown(view.people.len());
        let removes: Vec<(usize, Area)> = view
            .people
            .iter()
            .enumerate()
            .filter(|(index, person)| *index < shown && person.removable)
            .map(|(index, _)| {
                let path = super::sheet_ui::sheet_path(
                    id!(grant_confirmation),
                    &[id!(sheet_scroll), id!(people), id!(list), super::sheet_ui::PEOPLE[index], id!(body), id!(remove)],
                );
                (index, self.view.button(cx, &path).area())
            })
            .collect();
        let mut targets = Vec::new();
        if view.people_visible && view.people_collapsible {
            targets.push(people_toggle);
        }
        if view.people_add_visible {
            targets.push(open_add);
        }
        if view.choosing {
            targets.extend(removes.iter().map(|(_, area)| *area));
            if view.people_editable && !folded {
                targets.push(self.view.text_input(cx, ids!(grant_confirmation.sheet_scroll.people.list.add.add_name)).area());
                targets.push(add);
                let candidates = self.view.drop_down(cx, ids!(grant_confirmation.sheet_scroll.people.list.add.directory.candidates)).area();
                if !prompt.people_search.candidates(&managers_of(&prompt.draft.managers)).is_empty() {
                    targets.push(candidates);
                    if prompt.people_search.selected.is_some() { targets.push(add_selected); }
                }
            }
            if view.renewal_mode.is_some() {
                targets.push(self.view.widget(cx,ids!(grant_confirmation.sheet_scroll.pool_renewal.choices.automatic)).area());
                targets.push(self.view.widget(cx,ids!(grant_confirmation.sheet_scroll.pool_renewal.choices.manual)).area());
            }
            if view.calendar.is_some() {
                for path in [ids!(grant_confirmation.sheet_scroll.calendar.cadence.daily), ids!(grant_confirmation.sheet_scroll.calendar.cadence.weekly), ids!(grant_confirmation.sheet_scroll.calendar.cadence.monthly)] {
                    targets.push(self.view.widget(cx, path).area());
                }
                targets.push(self.view.text_input(cx, ids!(grant_confirmation.sheet_scroll.calendar.zone)).area());
                targets.push(self.view.drop_down(cx, ids!(grant_confirmation.sheet_scroll.calendar.zones)).area());
            }
            if view.quota_visible {
                if let Some(slider) = self
                    .view
                    .widget(cx, ids!(grant_confirmation.sheet_scroll.quota.tokens.slider_wrap.amount_slider))
                    .borrow::<crate::miniapps::design::MiniAppAmountSlider>()
                {
                    targets.push(slider.focus_area());
                }
                targets.push(self.view.text_input(cx, ids!(grant_confirmation.sheet_scroll.quota.tokens.input)).area());
                targets.push(self.view.text_input(cx, ids!(grant_confirmation.sheet_scroll.request_quota.limit.input)).area());
            }
        }
        if view.sharing_visible {
            targets.push(sharing);
            if view.sharing_open {
                for (id, _) in presets {
                    let button = self.view.button(cx, id);
                    if !button.disabled(cx) {
                        targets.push(button.area());
                    }
                }
            }
        }
        let recover_row = view.recover_row();
        if recover_row {
            targets.push(recover);
        }
        if view.cancel_intent_visible {
            targets.push(cancel_intent);
        }
        if view.more_visible() {
            targets.push(more);
        }
        if !view.cancel.is_empty() {
            targets.push(cancel);
        }
        if view.primary_visible && view.primary_enabled {
            targets.push(primary);
        }
        if key.key_code == KeyCode::Tab {
            let next = focus_next(
                targets.iter().position(|area| cx.has_key_focus(*area)),
                targets.len(),
                key.modifiers.shift,
            );
            cx.set_key_focus(targets[next]);
            return true;
        }
        if matches!(key.key_code, KeyCode::ReturnKey | KeyCode::Space) {
            if view.sharing_visible && cx.has_key_focus(sharing) {
                self.grant_sharing_toggle(cx);
                cx.set_key_focus(sharing);
                return true;
            }
            if view.people_add_visible && cx.has_key_focus(open_add) {
                self.grant_add_people(cx);
                return true;
            }
            if view.renewal_mode.is_some() {
                for (path, mode) in [(ids!(grant_confirmation.sheet_scroll.pool_renewal.choices.automatic),RenewalMode::Automatic),(ids!(grant_confirmation.sheet_scroll.pool_renewal.choices.manual),RenewalMode::Owner)] {
                    if cx.has_key_focus(self.view.widget(cx,path).area()) {
                        self.capture_grant_choices(cx);
                        if let Some(p)=self.grant_confirmation.as_mut(){p.draft.renewal_mode=Some(mode);}
                        self.present_grant_confirmation(cx);
                        return true;
                    }
                }
            }
            if view.calendar.is_some() {
                for (path, cadence) in [
                    (ids!(grant_confirmation.sheet_scroll.calendar.cadence.daily), Cadence::Daily),
                    (ids!(grant_confirmation.sheet_scroll.calendar.cadence.weekly), Cadence::Weekly),
                    (ids!(grant_confirmation.sheet_scroll.calendar.cadence.monthly), Cadence::Monthly),
                ] {
                    if cx.has_key_focus(self.view.widget(cx, path).area()) {
                        if let Some(calendar) = self.grant_confirmation.as_mut().and_then(|p| p.draft.calendar.as_mut()) {calendar.cadence = cadence;}
                        self.present_grant_confirmation(cx);
                        return true;
                    }
                }
            }
            if view.choosing {
                if view.people_editable && !folded && cx.has_key_focus(add_selected) {
                    self.grant_add_selected_person(cx);
                    return true;
                }
                if view.sharing_visible && view.sharing_open {
                    for (id, days) in presets {
                        if cx.has_key_focus(self.view.button(cx, id).area()) {
                            self.grant_expiry_preset(cx, days);
                            return true;
                        }
                    }
                }
                for (index, target) in &removes {
                    if cx.has_key_focus(*target) {
                        self.grant_remove_person(cx, *index);
                        return true;
                    }
                }
                if view.people_editable && !folded && cx.has_key_focus(add) {
                    self.grant_add_person(cx);
                    cx.set_key_focus(add);
                    return true;
                }
            }
            if view.people_visible && view.people_collapsible && cx.has_key_focus(people_toggle) {
                self.grant_people_toggle(cx);
                cx.set_key_focus(people_toggle);
                return true;
            }
            if view.more_visible() && cx.has_key_focus(more) {
                self.grant_disclose(cx);
                cx.set_key_focus(more);
                return true;
            }
            if !view.cancel.is_empty() && cx.has_key_focus(cancel) {
                self.close_grant_confirmation(cx);
                return true;
            }
            if view.primary_visible && view.primary_enabled && cx.has_key_focus(primary) {
                self.grant_primary(cx);
                return true;
            }
            if recover_row && cx.has_key_focus(recover) {
                self.grant_recover(cx);
                return true;
            }
            if view.cancel_intent_visible && cx.has_key_focus(cancel_intent) {
                self.grant_cancel_intent(cx);
                return true;
            }
        }
        false
    }
}
pub(super) fn focus_next(current: Option<usize>, count: usize, reverse: bool) -> usize {
    match current {
        None => {
            if reverse {
                count - 1
            } else {
                0
            }
        }
        Some(index) => {
            if reverse {
                (index + count - 1) % count
            } else {
                (index + 1) % count
            }
        }
    }
}
/// An offline preview of the panel: phases built from fixture JSON and shown
/// through the production view model and layout. There is no flow, so the
/// preview can never reserve, publish or reply to a script.
#[cfg(any(test, feature = "onboarding-test-support"))]
pub(super) struct GrantPreview {
    phase: Phase,
    draft: GrantChoices,
    proposal: Option<u64>,
    form: Option<OwnerGrantForm>,
    successor: Option<OwnerGrantSuccessionForm>,
    requested: Option<Succession>,
    applicants: Vec<String>,
    account: String,
    original: bool,
    in_flight: bool,
    submitted: bool,
    submitting: bool,
    can_choose_new_limits: bool,
    disclosure: Disclosure,
    add_error: String,
    add_people: bool,
    /// The preview's clock: now, or `hoursLeft` before this UTC month ends,
    /// so a review can show the presets a given time of the month offers.
    now: u64,
}
#[cfg(any(test, feature = "onboarding-test-support"))]
impl GrantPreview {
    fn state(&self) -> GrantState<'_> {
        GrantState {
            phase: &self.phase,
            draft: &self.draft,
            proposal: self.proposal,
            form: self.form.as_ref(),
            successor: self.successor.as_ref(),
            requested: self.requested,
            applicants: &self.applicants,
            account: &self.account,
            original: self.original,
            in_flight: self.in_flight,
            submitted: self.submitted,
            submitting: self.submitting,
            publishing: false,
            cancelling: false,
            regrant: false,
            active: true,
            can_choose_new_limits: self.can_choose_new_limits,
            succession_supported: Some(true),
            disclosure: self.disclosure,
            add_error: &self.add_error,
            add_people: self.add_people,
        }
    }
    /// `state`: publish, amend, add_people, renew, published, record, loading
    /// or failed. Amounts are tokens; people are full Matrix IDs. `typed` is
    /// what the owner typed into the add field, checked as the sheet checks it.
    /// `adding`: the add-people form opened from this month's record, with
    /// nobody chosen yet. `sharing`: the sharing period's choice is open.
    pub(super) fn from_fixture(f: &Value, now: u64) -> Result<Self, String> {
        use chrono::Datelike;
        let text = |key: &str, default: &str| f[key].as_str().unwrap_or(default).to_owned();
        let list = |key: &str| -> Vec<String> {
            f[key].as_array().map(|items| items.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default()
        };
        let account = text("account", "@reviewer:example.test");
        let pool = text("pool", "pool_original");
        let resource = text("resource", "resource_original");
        let current = f["current"].as_u64().unwrap_or(200_000);
        let managers = list("managers");
        let added = list("added");
        let state = text("state", "publish");
        let kind = match (state.as_str(), f["kind"].as_str()) {
            ("amend" | "add_people", _) | (_, Some("amendment" | "add_people")) => Some(OwnerGrantSuccessionKind::Amendment),
            ("renew", _) | (_, Some("renewal")) => Some(OwnerGrantSuccessionKind::Renewal),
            _ => None,
        };
        let month_start = |ms: u64| -> u64 {
            let at = chrono::DateTime::from_timestamp_millis(ms as i64).unwrap_or_default().date_naive();
            at.with_day(1).and_then(|d| d.and_hms_opt(0, 0, 0)).map_or(0, |d| d.and_utc().timestamp_millis() as u64)
        };
        let start = month_start(now);
        let end = month_start(start + 32 * 86_400_000);
        let now = f["hoursLeft"].as_u64().map_or(now, |hours| end.saturating_sub(hours * 3_600_000).max(start));
        let next_end = month_start(end + 32 * 86_400_000);
        let key = |ms: u64| chrono::DateTime::from_timestamp_millis(ms as i64).unwrap_or_default().format("%Y-%m").to_string();
        let context = ::hagency::native_owner::OwnerGrantReviewContext {
            server_origin: text("server", "https://matrix.example.test"),
            profile_id: text("profile", "profile_owner"),
            resource_revision: "c".repeat(64),
            framework: "codex".into(),
            model: text("model", "coding-model"),
            remaining_tokens: f["remaining"].as_u64(),
            resource_total_tokens: Some(f["total"].as_u64().unwrap_or(1_000_000)),
            shared_account_total_tokens: f["accountLimit"].as_u64(),
            capacity_blocker: f["blockedReason"].as_str().map(str::to_owned),
            observed_at_ms: now,
        };
        let summary = |tokens: u64, people: Vec<String>, period: (String, u64, u64), succession: Option<::hagency::native_owner::OwnerGrantSuccessionSummary>| OwnerGrantSummary { renewal_mode: serde_json::from_value(f["renewalMode"].clone()).ok(), per_request_tokens: f["perRequest"].as_u64(),
            intent_id: "intent_preview".into(),
            grant_id: if succession.is_some() { "grant_preview_2".into() } else { "grant_preview_1".into() },
            owner_mxid: account.clone(),
            contribution_id: pool.clone(),
            resource_id: resource.clone(),
            coordinator_mxid: account.clone(),
            allocated_tokens: tokens,
            eligible_managers: people,
            allow_self_approval: false,
            delegation_expires_at_ms: period.2,
            period_key: period.0,
            period_start_ms: period.1,
            period_end_ms: period.2,
            calendar: serde_json::from_value(f["calendar"].clone()).ok(),
            succession,
        };
        let this_period = (key(start), start, end);
        let successor = kind.map(|kind| {
            let renewing = kind == OwnerGrantSuccessionKind::Renewal;
            OwnerGrantSuccessionForm {
                kind,
                contribution_id: pool.clone(),
                resource_id: resource.clone(),
                label: text("label", "Research"),
                context: context.clone(),
                predecessor: summary(current, managers.clone(), this_period.clone(), None),
                floor: ::hagency::native_owner::OwnerGrantFloor {
                    children: f["running"].as_u64().unwrap_or(0),
                    retained_tokens: f["held"].as_u64().unwrap_or(0),
                },
                period_key: if renewing { key(end) } else { key(start) },
                period_start_ms: if renewing { end } else { start },
                period_end_ms: if renewing { next_end } else { end },
                window_opens_at_ms: end.saturating_sub(7 * 86_400_000),
                window_closes_at_ms: end,
            }
        });
        let form = kind.is_none().then(|| OwnerGrantForm {
            awaiting_admission: f["awaitingAdmission"].as_bool().unwrap_or(false),
            calendar: serde_json::from_value(f["boundCalendar"].clone()).ok(),
            contribution_id: pool.clone(),
            resource_id: resource.clone(),
            label: text("label", "Research"),
            context: context.clone(),
        });
        // As the mini app asks: a new limit, or the current one with people or
        // a renewal. Submitting in the window clears it, as `submit` does.
        let proposal = match state.as_str() {
            "amend" | "publish" | "loading" => f["proposed"].as_u64(),
            "add_people" | "renew" => Some(current),
            _ => None,
        };
        // A refused change keeps what the owner typed in the fields; `chosen` is the amount
        // the owner moved the window's slider to.
        let typed = f["chosen"].as_u64().or(if state == "failed" { f["proposed"].as_u64() } else { proposal });
        let mut draft = initial_draft(typed, if kind.is_some() { &added } else { &managers });
        draft.per_request = f["perRequest"].as_u64().map(|n|n.to_string()).unwrap_or_default();
        match (&successor, &form) {
            (Some(successor), _) => fill_successor_defaults(&mut draft, successor, now),
            (None, Some(form)) => fill_form_defaults(&mut draft, form),
            _ => {}
        }
        if let Ok(calendar) = serde_json::from_value::<Calendar>(f["calendar"].clone()) {
            draft.calendar = Some(calendar);
            draft.renewal_mode = Some(serde_json::from_value(f["renewalMode"].clone()).unwrap_or(RenewalMode::Automatic));
        }
        draft.coordinator = account.clone();
        let requested = kind.map(succession_arg);
        let published_tokens = f["proposed"].as_u64().unwrap_or(current);
        let mut people = managers.clone();
        people.extend(added.iter().cloned());
        let people = managers_of(&people.join(" "));
        let succession_summary = |kind| ::hagency::native_owner::OwnerGrantSuccessionSummary {
            kind,
            predecessor_grant_id: "grant_preview_1".into(),
            predecessor_allocated_tokens: current,
            carried_children: f["running"].as_u64().unwrap_or(0),
            carried_retained_tokens: f["held"].as_u64().unwrap_or(0),
        };
        let progress = |summary| {
            Phase::PublicationProgress(OwnerGrantPublicationProgress {
                summary,
                state: OwnerGrantPublicationState::ServerObserved,
                server_state: Some("active".into()),
                server_revision: Some(2),
                observed_at_ms: Some(now),
            })
        };
        let phase = match state.as_str() {
            "publish" | "amend" | "add_people" | "renew" => Phase::Choose,
            "published" => progress(match kind {
                Some(OwnerGrantSuccessionKind::Renewal) => summary(published_tokens, people.clone(), (key(end), end, next_end), Some(succession_summary(OwnerGrantSuccessionKind::Renewal))),
                Some(kind) => summary(published_tokens, people.clone(), this_period.clone(), Some(succession_summary(kind))),
                None => summary(published_tokens, people.clone(), this_period.clone(), None),
            }),
            "record" => progress(summary(current, managers.clone(), this_period.clone(), None)),
            "loading" => Phase::Loading,
            "failed" => Phase::Failed(text("code", "owner_grant_below_floor")),
            other => return Err(format!("unknown preview state {other}")),
        };
        // A record opened by itself, or a window still reading, has no form yet.
        let record = state == "record";
        let formless = record || (state == "loading" && !f["submitting"].as_bool().unwrap_or(false));
        let add_error = match f["typed"].as_str() {
            Some(typed) => applicant_from_input(typed, &account, &managers_of(&draft.managers)).err().unwrap_or_default(),
            None => String::new(),
        };
        Ok(Self {
            phase,
            draft,
            proposal,
            form: if formless { None } else { form },
            successor: if formless { None } else { successor },
            requested: if record { None } else { requested },
            applicants: if state == "add_people" || f["kind"].as_str() == Some("add_people") { added.clone() } else { Vec::new() },
            account,
            original: matches!(state.as_str(), "published" | "record"),
            in_flight: state == "loading",
            submitted: state == "published",
            submitting: f["submitting"].as_bool().unwrap_or(false),
            can_choose_new_limits: !record,
            disclosure: Disclosure {
                more: ["more", "technical"].iter().any(|key| f[*key].as_bool() == Some(true)),
                people: f["people"].as_bool() == Some(true),
                sharing: f["sharing"].as_bool() == Some(true),
            },
            add_error,
            add_people: f["adding"].as_bool() == Some(true),
            now,
        })
    }
}
#[cfg(any(test, feature = "onboarding-test-support"))]
thread_local! {
    /// The preview on screen, so a headless review can tap its presets, open
    /// its sharing row and type its amount through the real window's
    /// dispatch. It has no flow: nothing can be confirmed or sent.
    static GRANT_PREVIEW: std::cell::RefCell<Option<GrantPreview>> = const { std::cell::RefCell::new(None) };
}
#[cfg(any(test, feature = "onboarding-test-support"))]
impl MiniAppsPanel {
    /// Show one preview phase in the real panel. Nothing here can be confirmed.
    pub(super) fn preview_grant(&mut self, cx: &mut Cx, fixture: &Value) -> Result<(), String> {
        let preview = GrantPreview::from_fixture(fixture, now_ms())?;
        let now = preview.now;
        let state = preview.state();
        let view = grant_view(&state, now);
        let inputs = GrantInputs::of(&state, now);
        self.view.view(cx, ids!(grant_confirmation)).set_visible(cx, true);
        self.apply_grant_view(cx, &view, Some(&inputs));
        // What the owner typed into the add field, beside its inline reason.
        self.view
            .text_input(cx, ids!(grant_confirmation.sheet_scroll.people.list.add.add_name))
            .set_text(cx, fixture["typed"].as_str().unwrap_or_default());
        let candidates: Vec<_> = fixture["directory"].as_array().into_iter().flatten()
            .filter_map(|person| Some(crate::host::hagency::grant_people::Person {
                id: person["id"].as_str()?.to_owned(),
                display_name: person["name"].as_str().map(str::to_owned),
            })).collect();
        self.apply_grant_people(cx, &candidates, fixture["selected"].as_u64().unwrap_or(0) as usize, crate::i18n::tr("Choose a verified account to add. Already selected people are hidden."), false);
        GRANT_PREVIEW.with(|cell| *cell.borrow_mut() = Some(preview));
        Ok(())
    }
    /// Taps and typing in a preview's form: the presets choose when sharing
    /// ends, the row opens or folds them, and the amount follows the field, as
    /// in the real window. Returns whether the preview changed.
    pub(super) fn preview_grant_actions(&mut self, cx: &mut Cx, actions: &Actions) -> bool {
        let preset = self.grant_preset_clicked(cx, actions);
        let toggle = self.view.button(cx, ids!(grant_confirmation.sheet_scroll.sharing.sharing_row.sharing_toggle)).clicked(actions);
        let typed = self.view.widget(cx, ids!(grant_confirmation.sheet_scroll.quota.tokens)).text();
        let per_request = self.view.widget(cx, ids!(grant_confirmation.sheet_scroll.request_quota.limit)).text();
        let now = GRANT_PREVIEW.with(|cell| cell.borrow().as_ref().map(|p| p.now)).unwrap_or_else(now_ms);
        let calendar = GRANT_PREVIEW.with(|cell| cell.borrow().as_ref().and_then(|p| p.draft.calendar.as_ref().and_then(|c| self.calendar_input(cx, actions, c))));
        let calendar_typing=self.view.text_input(cx, ids!(grant_confirmation.sheet_scroll.calendar.zone)).changed(actions).is_some();
        let renewal = if crate::miniapps::design::key_button_clicked(&self.view.widget(cx, ids!(grant_confirmation.sheet_scroll.pool_renewal.choices.automatic)), actions) {
            Some(RenewalMode::Automatic)
        } else if crate::miniapps::design::key_button_clicked(&self.view.widget(cx, ids!(grant_confirmation.sheet_scroll.pool_renewal.choices.manual)), actions) {
            Some(RenewalMode::Owner)
        } else { None };
        let (changed, refill) = GRANT_PREVIEW.with(|cell| {
            let mut cell = cell.borrow_mut();
            let Some(preview) = cell.as_mut().filter(|p| matches!(p.phase, Phase::Choose)) else {
                return (false, false);
            };
            let mut changed = false;
            if let Some(calendar) = calendar {preview.draft.calendar = Some(calendar); changed = true;}
            if let Some(mode) = renewal {preview.draft.renewal_mode = Some(mode); changed = true;}
            if let Some(days) = preset {
                let value = match &preview.successor {
                    Some(form) => successor_preset(form, days, now),
                    None => publication_preset_available_at(days, now).then(|| publication_expiry_at(days, now)),
                };
                if let Some(value) = value {
                    preview.draft.expires_utc = value;
                    changed = true;
                }
            }
            if toggle {
                preview.disclosure.sharing = !preview.disclosure.sharing;
                changed = true;
            }
            let refill = changed && !calendar_typing;
            if per_request != preview.draft.per_request {
                preview.draft.per_request = per_request;
                changed = true;
            }
            if typed != preview.draft.tokens {
                preview.draft.tokens = typed;
                changed = true;
            }
            (changed, refill)
        });
        if changed {
            GRANT_PREVIEW.with(|cell| {
                if let Some(preview) = cell.borrow().as_ref() {
                    let state = preview.state();
                    let view = grant_view(&state, now);
                    let inputs = GrantInputs::of(&state, now);
                    // While typing, the field is left as it is, as the real window does.
                    self.apply_grant_view(cx, &view, refill.then_some(&inputs));
                }
            });
            self.view.redraw(cx);
        }
        changed
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn recurring_publication_discloses_standing_consent_and_owner_calendar() {
        let fixture=json!({"state":"publish","total":2_000_000,"remaining":2_000_000,"proposed":500_000,"managers":["@chris:test"],"calendar":{"cadence":"weekly","timeZone":"America/Los_Angeles"}});
        let preview=GrantPreview::from_fixture(&fixture,now_ms()).unwrap();
        let view=grant_view(&preview.state(),now_ms());
        assert_eq!(view.title,"Publish Recurring Capacity");
        assert!(view.lead.contains("All approved Agents follow your choice"));
        assert_eq!(view.renewal_mode, Some(RenewalMode::Automatic));
        assert!(view.facts.iter().any(|fact| fact.label == "Period maximum" && fact.value == "2 M"));
        assert!(view.facts.iter().any(|fact| fact.label == "Left this period" && fact.value == "2 M"));
        assert!(!view.sharing_visible && view.calendar.is_some());
        assert!(calendar_note(view.calendar.as_ref().unwrap(),now_ms()).contains("America/Los_Angeles"));
        let mut manual = fixture.clone();
        manual["renewalMode"] = json!("owner");
        let preview = GrantPreview::from_fixture(&manual, now_ms()).unwrap();
        assert_eq!(grant_view(&preview.state(), preview.now).renewal_mode, Some(RenewalMode::Owner));
        let mut bound=fixture.clone();
        bound["boundCalendar"]=bound["calendar"].clone();
        let preview=GrantPreview::from_fixture(&bound,now_ms()).unwrap();
        let view=grant_view(&preview.state(),now_ms());
        assert!(view.calendar.is_none());
        assert!(view.facts.iter().any(|fact| fact.note.contains("America/Los_Angeles")));
        let mut blocked=fixture;
        blocked["remaining"]=json!(0);
        blocked["blockedReason"]=json!("pool_limit_reached");
        let preview=GrantPreview::from_fixture(&blocked,now_ms()).unwrap();
        let view=grant_view(&preview.state(),now_ms());
        assert_eq!(view.title,"Cannot Publish");
        assert!(!view.primary_enabled && view.lead.is_empty());
        assert!(view.problem.contains("no capacity left"));
        assert_eq!(view.caption,"Tokens per period");
        assert!(view.before.is_none());
    }
    #[test]
    fn blocked_publication_explains_the_reason_and_keeps_publish_disabled() {
        for (remaining, reason, message) in [
            (Value::Null, "usage_unsettled", "Final usage is still unsettled"),
            (json!(0), "pool_limit_reached", "This pool has no capacity left"),
            (Value::Null, "pool_limit_missing", "Set a monthly maximum"),
        ] {
            let fixture = json!({"state":"publish","total":20_000_000,"remaining":remaining,"proposed":5_000_000,"managers":["@chris:test"],"blockedReason":reason});
            let preview = GrantPreview::from_fixture(&fixture, now_ms()).unwrap();
            let view = grant_view(&preview.state(), preview.now);
            assert_eq!(view.tone, SheetTone::Failed);
            assert_eq!(view.title, "Cannot Publish");
            assert!(view.problem.starts_with(message), "{}", view.problem);
            assert!(!view.primary_enabled);
            assert!(view.lead.is_empty(), "a blocked choice must not promise publication");
            assert_eq!(view.status, "Nothing has been published or changed.");
        }
        let preview = GrantPreview::from_fixture(&json!({"state":"publish","total":20_000_000,"remaining":20_000_000,"proposed":5_000_000,"managers":["@chris:test"]}), now_ms()).unwrap();
        let view = grant_view(&preview.state(), preview.now);
        assert_eq!(view.tone, SheetTone::Proposal);
        assert!(view.problem.is_empty());
        assert!(view.primary_enabled);
        let unknown = GrantPreview::from_fixture(&json!({"state":"failed","code":"management_outcome_unknown"}), now_ms()).unwrap();
        assert_eq!(grant_view(&unknown.state(), unknown.now).tone, SheetTone::Unsure);
    }
    use super::*;
    #[test]
    fn directory_hides_selected_people_and_bounds_the_visible_menu() {
        let picker = PeopleSearch { result: Some(crate::host::hagency::grant_people::People {
            items: (0..8).map(|i| crate::host::hagency::grant_people::Person {
                id: format!("@person{i}:test"), display_name: None,
            }).collect(), limited: false,
        }), ..Default::default() };
        let selected = vec!["@person0:test".into(), "@person1:test".into()];
        assert!(picker.limited(&[]));
        assert!(!picker.limited(&selected));
        let candidates = picker.candidates(&selected);
        assert_eq!(candidates.len(), 6);
        assert_eq!(candidates[0].id, "@person2:test");
        assert_eq!(candidates[5].id, "@person7:test");
    }
    #[test]
    fn keyboard_cycle_keeps_footer_reachable_both_directions() {
        assert_eq!(focus_next(Some(3), 6, false), 4);
        assert_eq!(focus_next(Some(5), 6, false), 0);
        assert_eq!(focus_next(Some(0), 6, true), 5);
        assert_eq!(focus_next(None, 2, true), 1);
    }
    #[test]
    fn local_times_name_their_zone_and_unknown_is_not_zero() {
        // Every local time says which zone it is in, as the pool's page says it.
        let zone = crate::host::hagency::local_zone::label(Some(1793491200000));
        assert!(local_time(1793491200000).contains(&format!("({zone})")), "{}", local_time(1793491200000));
        assert_eq!(ends_at(1793491200000), format!("Before {}", local_time(1793491200000)));
        assert_ne!(amount(None), "0");
        assert_eq!(amount(Some(0)), "0 M");
    }
    #[test]
    fn publication_form_requires_known_capacity_and_expiry_in_current_period() {
        let mut form=OwnerGrantForm {awaiting_admission:false,calendar:None,contribution_id:"c".into(),resource_id:"r".into(),label:"Pool".into(),
            context: ::hagency::native_owner::OwnerGrantReviewContext {
                server_origin:"https://matrix.test".into(),profile_id:"profile".into(),resource_revision:"revision".into(),
                framework:"codex".into(),model:"model".into(),remaining_tokens:Some(1_000_001),
                resource_total_tokens:Some(2_000_000),shared_account_total_tokens:Some(9_000_000),capacity_blocker:None,observed_at_ms:1,
            }};
        let mut draft=GrantChoices { renewal_mode: None, per_request: String::new(),coordinator:"@owner:test".into(),managers:"@manager:test".into(),tokens:"1000001".into(),expires_utc:publication_expiry(None),self_approval:false,calendar:None};
        assert!(form_choices_valid(&form,&draft));
        draft.tokens="1000002".into();assert!(!form_choices_valid(&form,&draft));
        draft.tokens="1000001".into();
        form.context.remaining_tokens=None;assert!(!form_choices_valid(&form,&draft));
        form.context.remaining_tokens=Some(0);assert!(!form_choices_valid(&form,&draft));
        form.context.remaining_tokens=Some(1_000_001);
        draft.expires_utc=(publication_period_end()+chrono::Duration::seconds(1)).to_rfc3339();
        assert!(!form_choices_valid(&form,&draft));
        for days in [Some(1),Some(7),None] {
            draft.expires_utc=publication_expiry(days);
            assert!(form_choices_valid(&form,&draft));
        }
        draft.managers.clear();assert!(!form_choices_valid(&form,&draft));
    }
    fn pool_form(remaining: u64) -> OwnerGrantForm {
        OwnerGrantForm {awaiting_admission:false,calendar:None,contribution_id:"pool_1".into(),resource_id:"r".into(),label:"Pool".into(),
            context: ::hagency::native_owner::OwnerGrantReviewContext {
                server_origin:"https://matrix.test".into(),profile_id:"profile".into(),resource_revision:"revision".into(),
                framework:"codex".into(),model:"model".into(),remaining_tokens:Some(remaining),
                resource_total_tokens:Some(20_000_000),shared_account_total_tokens:None,capacity_blocker:None,observed_at_ms:1,
            }}
    }
    fn summary(tokens: u64) -> OwnerGrantSummary {
        OwnerGrantSummary { renewal_mode: None, per_request_tokens: None,
            intent_id: "original".into(), grant_id: "grant_original".into(), owner_mxid: "@owner:test".into(),
            contribution_id: "pool_1".into(), resource_id: "resource".into(),
            coordinator_mxid: "@owner:test".into(), allocated_tokens: tokens,
            eligible_managers: vec!["@manager:test".into()], allow_self_approval: false,
            delegation_expires_at_ms: 1793491200000, period_key: "2026-10".into(),
            period_start_ms: 1790812800000, period_end_ms: 1793491200000,
            calendar: None, succession: None,
        }
    }
    fn publication(state: OwnerGrantPublicationState, server: Option<&str>) -> Phase {
        Phase::PublicationProgress(OwnerGrantPublicationProgress {
            summary: summary(2_000_000), state, server_state: server.map(str::to_owned),
            server_revision: server.map(|_| 1), observed_at_ms: server.map(|_| 1),
        })
    }
    fn progress(state: OwnerGrantState, closed: bool) -> Phase {
        Phase::Progress(OwnerGrantProgress {
            summary: summary(2_000_000), state, succession_state: None,
            closure: closed.then_some(::hagency::native_owner::OwnerGrantClosure {
                retained_tokens: 0, known_consumed_tokens: 0, unresolved_tokens: 0, observed_at_ms: 1,
            }),
        })
    }
    /// Inside the fixtures' 2026-10 month, and that month's end (2026-11-01).
    const OCTOBER: u64 = 1_792_000_000_000;
    const NOVEMBER: u64 = 1_793_491_200_000;
    #[test]
    fn proposal_prefills_only_the_native_tokens_field_and_is_never_lowered() {
        let form = pool_form(10_000_000);
        let mut draft = initial_draft(Some(3_000_000), &[]);
        assert_eq!(draft.tokens, "3000000");
        fill_form_defaults(&mut draft, &form);
        assert_eq!(draft.tokens, "3000000");
        // Applicants are still typed natively; expiry keeps today's default.
        assert!(draft.managers.is_empty());
        assert_eq!(draft.expires_utc, publication_expiry(None));
        let mut default = initial_draft(None, &[]);
        fill_form_defaults(&mut default, &form);
        assert_eq!(default.tokens, "20000000");
        assert_eq!(default.per_request, "20000000");
        // Above the available capacity the proposal stays visible and unconfirmable.
        let mut above = initial_draft(Some(12_000_000), &[]);
        fill_form_defaults(&mut above, &form);
        above.coordinator = "@owner:test".into();
        above.managers = "@manager:test".into();
        assert_eq!(above.tokens, "12000000");
        assert!(!form_choices_valid(&form, &above));
        above.tokens = "3000000".into();
        assert!(!form_choices_valid(&form, &above), "Agent cap cannot exceed the lowered total");
        above.per_request = "2000000".into();
        assert!(form_choices_valid(&form, &above));
        // A reopened form keeps what the owner typed over the proposal.
        let mut edited = initial_draft(Some(3_000_000), &[]);
        edited.tokens = "2500000".into();
        fill_form_defaults(&mut edited, &form);
        assert_eq!(edited.tokens, "2500000");
    }
    #[test]
    fn proposal_notice_shows_the_current_and_the_committed_amount() {
        // Only a pool without an issued grant reaches the form; the new amount
        // is whatever the native tokens field holds now.
        for (field, shown) in [("3000000", "3 M"), ("2500000", "2.5 M"), ("2.5", "Unavailable")] {
            assert_eq!(
                proposal_notice(&Phase::Choose, Some(3_000_000), field, false, OCTOBER).as_deref(),
                Some(format!("Pool total allowance: Not in effect → {shown}").as_str())
            );
        }
        assert!(proposal_notice(&Phase::Choose, None, "3000000", false, OCTOBER).is_none());
        assert!(proposal_notice(&Phase::Loading, Some(3_000_000), "", false, OCTOBER).is_none());
        let declined = proposal_notice(&progress(OwnerGrantState::Declined, false), Some(3_000_000), "", false, OCTOBER).unwrap();
        assert_eq!(declined, "Pool total allowance: Not in effect → 3 M\nThis amount is filled in when you choose again.");
        let closed = proposal_notice(&progress(OwnerGrantState::Recorded, true), Some(3_000_000), "", false, OCTOBER).unwrap();
        assert!(closed.ends_with("filled in when you choose again."), "{closed}");
        let failed = proposal_notice(&Phase::Failed("owner_grant_prior_intent_required".into()), Some(3_000_000), "", false, OCTOBER).unwrap();
        assert_eq!(failed, "This amount wasn’t applied.");
    }
    #[test]
    fn published_grant_is_shown_read_only_and_never_replaced_in_its_month() {
        let live = publication(OwnerGrantPublicationState::ServerObserved, Some("active"));
        assert_eq!(live_grant_tokens(&live, OCTOBER), Some(2_000_000));
        assert_eq!(
            proposal_notice(&live, Some(3_000_000), "", false, OCTOBER).unwrap(),
            format!("Pool total allowance: 2 M → 3 M\n{PUBLISHED_NOTE}")
        );
        // A grant revoked, or expired by its delegation, within its month still
        // used this month of the admission: no current limit, and the proposal
        // still cannot become a second grant this month.
        for server in ["revoked", "expired"] {
            let used = publication(OwnerGrantPublicationState::ServerObserved, Some(server));
            assert_eq!(live_grant_tokens(&used, OCTOBER), None);
            assert_eq!(
                proposal_notice(&used, Some(3_000_000), "", false, OCTOBER).unwrap(),
                format!("Pool total allowance: Not in effect → 3 M\n{PUBLISHED_NOTE}")
            );
        }
        // No proposal, applicant or selector flag offers a commit beside a
        // published grant: nothing is revoked, reserved or published from here.
        let form = pool_form(10_000_000);
        let mut draft = initial_draft(Some(3_000_000), &[]);
        fill_form_defaults(&mut draft, &form);
        draft.coordinator = "@owner:test".into();
        draft.managers = "@manager:test".into();
        assert!(primary_available(
            &Phase::Choose,
            Some(&root_bounds(&form)),
            &draft,
            false,
            true,
            false, OCTOBER
        ));
        for server in ["active", "revoked", "expired"] {
            let phase = publication(OwnerGrantPublicationState::ServerObserved, Some(server));
            for has_original in [false, true] {
                for can_choose_new_limits in [false, true] {
                    assert!(!primary_available(
                        &phase,
                        Some(&root_bounds(&form)),
                        &draft,
                        has_original,
                        can_choose_new_limits,
                        false, OCTOBER
                    ));
                }
            }
        }
    }
    #[test]
    fn ended_month_grant_no_longer_blocks_a_proposal_and_offers_this_month() {
        const ENDED_NOTE: &str =
            "This amount is filled in when you set this month’s capacity.";
        let form = pool_form(10_000_000);
        let draft = initial_draft(Some(3_000_000), &[]);
        // Once its UTC month has ended, a published grant (the server reports
        // it expired) or an unpublished recorded one no longer blocks: the
        // pool has no limit and the panel offers this month's limits instead.
        for phase in [
            publication(OwnerGrantPublicationState::ServerObserved, Some("expired")),
            publication(OwnerGrantPublicationState::ServerObserved, Some("active")),
            publication(OwnerGrantPublicationState::ServerObserved, Some("revoked")),
            publication(OwnerGrantPublicationState::OutcomeUnknown, None),
            publication(OwnerGrantPublicationState::Rejected, None),
            progress(OwnerGrantState::Recorded, false),
        ] {
            assert!(ended_grant(&phase, NOVEMBER) && !ended_grant(&phase, NOVEMBER - 1));
            assert!(!published_grant(&phase, NOVEMBER));
            assert_eq!(live_grant_tokens(&phase, NOVEMBER), None);
            assert_eq!(
                proposal_notice(&phase, Some(3_000_000), "", false, NOVEMBER).unwrap(),
                format!("Pool total allowance: Not in effect → 3 M\n{ENDED_NOTE}")
            );
            assert!(primary_available(
                &phase,
                Some(&root_bounds(&form)),
                &draft,
                true,
                true,
                false, NOVEMBER
            ));
            assert!(!primary_available(
                &phase,
                Some(&root_bounds(&form)),
                &draft,
                true,
                false,
                false, NOVEMBER
            ));
            // Closing keeps the public progress, never pool_grant_already_published.
            let reply: serde_json::Value = serde_json::from_str(
                &close_reply(&phase, Some(3_000_000), false, Some("original"), false, NOVEMBER).unwrap(),
            )
            .unwrap();
            assert_eq!(reply["summary"]["periodKey"], "2026-10");
        }
        // Unresolved, declined and closed originals keep their own handling.
        for phase in [
            progress(OwnerGrantState::OutcomeUnknown, false),
            progress(OwnerGrantState::ReviewExpired, false),
            progress(OwnerGrantState::Declined, false),
            progress(OwnerGrantState::Recorded, true),
        ] {
            assert!(!ended_grant(&phase, NOVEMBER));
        }
        assert!(!primary_available(
            &progress(OwnerGrantState::OutcomeUnknown, false),
            None,
            &draft,
            true,
            true,
            false, NOVEMBER
        ));
    }
    #[test]
    fn other_originals_keep_their_amounts_and_their_own_actions() {
        const EXISTING: &str = "Pool total allowance: Not in effect → 3 M\nThis amount doesn’t change the existing publish; it keeps its own amounts.";
        for phase in [
            publication(OwnerGrantPublicationState::Rejected, None),
            publication(OwnerGrantPublicationState::OutcomeUnknown, None),
            progress(OwnerGrantState::Recorded, false),
            progress(OwnerGrantState::OutcomeUnknown, false),
        ] {
            assert_eq!(live_grant_tokens(&phase, OCTOBER), None);
            assert_eq!(
                proposal_notice(&phase, Some(3_000_000), "", false, OCTOBER).unwrap(),
                EXISTING
            );
        }
        // An unattempted original reserved nothing; cancelling it reopens the form.
        for state in [OwnerGrantState::ReviewRequired, OwnerGrantState::ReviewExpired] {
            assert_eq!(
                proposal_notice(&progress(state, false), Some(3_000_000), "", false, OCTOBER).unwrap(),
                format!("Pool total allowance: Not in effect → 3 M\n{PENDING_NOTE}")
            );
        }
        // A proposal adds or removes no action on an existing original.
        let draft = initial_draft(Some(3_000_000), &[]);
        assert!(primary_available(
            &progress(OwnerGrantState::Recorded, false),
            None,
            &draft,
            true,
            true,
            false, OCTOBER
        ));
        assert!(!primary_available(
            &progress(OwnerGrantState::OutcomeUnknown, false),
            None,
            &draft,
            true,
            true,
            false, OCTOBER
        ));
    }
    /// The real UTC month (start, end): `GrantChoices::parse` reads the real clock.
    fn this_month() -> (u64, u64) {
        use chrono::Datelike;
        let first = chrono::Utc::now().date_naive().with_day(1).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc();
        (first.timestamp_millis() as u64, publication_period_end().timestamp_millis() as u64)
    }
    fn month_after(start: u64) -> u64 {
        use chrono::Datelike;
        let at = chrono::DateTime::from_timestamp_millis(start as i64).unwrap().date_naive();
        (at + chrono::Duration::days(32)).with_day(1).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis() as u64
    }
    fn successor_form(kind: OwnerGrantSuccessionKind) -> OwnerGrantSuccessionForm {
        let (start, end) = this_month();
        let mut predecessor = summary(2_000_000);
        predecessor.period_start_ms = start;
        predecessor.period_end_ms = end;
        predecessor.delegation_expires_at_ms = end;
        let (key, period_start, period_end) = if kind == OwnerGrantSuccessionKind::Renewal {
            let at = chrono::DateTime::from_timestamp_millis(end as i64).unwrap();
            (at.format("%Y-%m").to_string(), end, month_after(end))
        } else {
            let at = chrono::DateTime::from_timestamp_millis(start as i64).unwrap();
            (at.format("%Y-%m").to_string(), start, end)
        };
        OwnerGrantSuccessionForm {
            kind,
            contribution_id: "pool_1".into(),
            resource_id: "resource".into(),
            label: "Pool".into(),
            context: pool_form(5_000_000).context,
            predecessor,
            floor: ::hagency::native_owner::OwnerGrantFloor { children: 2, retained_tokens: 600_000 },
            period_key: key,
            period_start_ms: period_start,
            period_end_ms: period_end,
            window_opens_at_ms: end.saturating_sub(7 * 86_400_000),
            window_closes_at_ms: end,
        }
    }
    fn facts(view: &GrantView) -> Vec<(String, String)> {
        view.facts.iter().map(|fact| (fact.label.clone(), fact.value.clone())).collect()
    }
    fn names(view: &GrantView) -> Vec<(String, bool, bool)> {
        view.people.iter().map(|p| (p.name.clone(), p.added, p.removable)).collect()
    }
    #[test]
    fn an_amendment_keeps_running_agents_and_only_adds_applicants() {
        let form = successor_form(OwnerGrantSuccessionKind::Amendment);
        let bounds = successor_bounds(&form);
        assert_eq!(bounds.minimum, 600_000);
        assert_eq!(bounds.keep_managers, ["@manager:test"]);
        // The proposal fills the tokens field; applicants are added to the
        // current ones; the current delegation is kept.
        let mut draft = initial_draft(Some(800_000), &["@new:test".to_owned()]);
        assert_eq!(draft.managers, "@new:test");
        fill_successor_defaults(&mut draft, &form, now_ms());
        assert_eq!(draft.tokens, "800000");
        assert_eq!(draft.managers, "@manager:test @new:test");
        assert_eq!(draft.expires_utc, rfc3339(form.predecessor.delegation_expires_at_ms));
        draft.coordinator = "@owner:test".into();
        assert!(choices_valid(&bounds, &draft));
        // Lowering is allowed down to what running Agents were given, never below.
        for (tokens, valid) in [("600000", true), ("599999", false), ("5000000", true), ("5000001", false)] {
            let mut d = draft.clone();
            d.tokens = tokens.into();
            d.per_request = "100000".into();
            assert_eq!(choices_valid(&bounds, &d), valid, "{tokens}");
        }
        // Removing a current applicant is never committed from this form.
        let mut removed = draft.clone();
        removed.managers = "@new:test".into();
        assert!(!choices_valid(&bounds, &removed));
        // The delegation cannot leave the month being amended.
        let mut late = draft.clone();
        late.expires_utc = rfc3339(form.period_end_ms + 1000);
        assert!(!choices_valid(&bounds, &late));
        // Without a proposal the field starts at the current limit.
        let mut plain = initial_draft(None, &[]);
        fill_successor_defaults(&mut plain, &form, now_ms());
        assert_eq!(plain.tokens, "2000000");
        assert_eq!(plain.managers, "@manager:test");
        // The hero shows before → after.
        let applicants = ["@new:test".to_owned()];
        let view = preview_view(&Phase::Choose, &draft, Some(&form), None, &applicants, Disclosure::default());
        assert_eq!(view.tone, SheetTone::Proposal);
        assert_eq!(view.glyph, SheetGlyph::Change);
        assert_eq!(view.title, "Change Pool Limits");
        assert_eq!(view.caption, "Pool total allowance");
        assert_eq!(view.before, Some(Figure::new("2 M", "Now")));
        assert_eq!(view.after, Some(Figure::new("0.8 M", "After you confirm")));
        assert_eq!(view.lead, "Applies this month as soon as you confirm. What running Agents hold stays as it is. Also adds @new to this month’s list.");
        // When it applies is the lead's; the rows say how high it can go and
        // what running Agents hold, the floor.
        assert_eq!(facts(&view), [
            ("Most it can be".to_owned(), "5 M".to_owned()),
            ("Held by running Agents".to_owned(), "0.6 M · 2 Agents".to_owned()),
        ]);
        assert_eq!(view.facts[0].note, "What this month can still give out.");
        assert_eq!(view.facts[1].note, "The pool total allowance can’t go below it.");
        assert_eq!(view.primary, "Confirm Changes");
        assert!(view.primary_enabled && !view.primary_closes && !view.primary_recovers);
        // Listed people are locked; the person this window adds can be removed.
        assert_eq!(names(&view), [("@manager".to_owned(), false, false), ("@new".to_owned(), true, true)]);
        assert_eq!(view.quota_hint, "Choose 0.6 M – 5 M.");
        // Someone is being added, so the list shows with its add field.
        assert!(!view.people_collapsible && view.people_open);
        assert_eq!(view.caution, "Waiting requests above 0.8 M can’t be approved after this.");
        assert!(view.problem.is_empty() && view.quota_visible);
        // Unknown capacity keeps the form unconfirmable.
        let mut unknown = form.clone();
        unknown.context.remaining_tokens = None;
        assert!(!choices_valid(&successor_bounds(&unknown), &draft));
    }
    #[test]
    fn a_renewal_starts_next_month_and_its_delegation_reaches_into_it() {
        let form = successor_form(OwnerGrantSuccessionKind::Renewal);
        let bounds = successor_bounds(&form);
        let mut draft = initial_draft(None, &[]);
        fill_successor_defaults(&mut draft, &form, now_ms());
        // Q12: a renewal's delegation defaults to the end of the new month.
        assert_eq!(draft.expires_utc, rfc3339(form.period_end_ms));
        assert_eq!(draft.tokens, "2000000");
        draft.coordinator = "@owner:test".into();
        assert!(choices_valid(&bounds, &draft));
        for (expiry, valid) in [
            (form.period_start_ms, false),
            (form.period_start_ms + 1000, true),
            (form.period_end_ms, true),
            (form.period_end_ms + 1000, false),
        ] {
            let mut d = draft.clone();
            d.expires_utc = rfc3339(expiry);
            assert_eq!(choices_valid(&bounds, &d), valid, "{expiry}");
        }
        // Presets only offer expiries inside the new month.
        assert_eq!(successor_preset(&form, None, now_ms()), Some(rfc3339(form.period_end_ms)));
        for days in [Some(1), Some(7)] {
            if let Some(value) = successor_preset(&form, days, now_ms()) {
                let mut d = draft.clone();
                d.expires_utc = value;
                assert!(choices_valid(&bounds, &d));
            }
        }
        // The window names next month, says it locks this month's limit and
        // can't be withdrawn here, and that the list and maximum carry over.
        let view = preview_view(&Phase::Choose, &draft, Some(&form), None, &[], Disclosure::default());
        let month = key_month(&form.period_key).unwrap();
        let this = ms_month(form.predecessor.period_start_ms);
        assert_eq!(view.title, format!("Publish {month} Ahead"));
        assert_eq!(view.glyph, SheetGlyph::Renew);
        // The hero leads with next month's capacity and the pool total allowance under it; no arrow
        // between two equal limits.
        assert_eq!(view.caption, format!("{month} capacity"));
        assert_eq!(view.before, None);
        assert_eq!(view.after, Some(Figure::new("5 M", "Up to 2 M in total")));
        assert_eq!(view.lead, format!("Starts {}. The list, the monthly maximum and the pool total allowance carry over.", local_time(form.period_start_ms)));
        assert_eq!(view.caution, format!("Once {month} is published ahead, this month’s pool total allowance and list can’t change any more, and it can’t be withdrawn."));
        // The list carries over unchanged: one row that opens in place.
        assert!(view.people_collapsible && !view.people_open);
        assert_eq!(view.people_summary, "1 person");
        // The hero says what next month has, so the rows don't repeat it.
        assert!(!facts(&view).iter().any(|(label, _)| label == "Monthly maximum" || label.starts_with("Available in")));
        assert_eq!(view.primary, "Publish Ahead");
        // Moving the slider shows the change: this month's limit → next month's.
        let mut raised = draft.clone();
        raised.tokens = "3000000".into();
        let view = preview_view(&Phase::Choose, &raised, Some(&form), None, &[], Disclosure::default());
        assert_eq!(view.caption, "Pool total allowance");
        assert_eq!(view.before, Some(Figure::new("2 M", this)));
        assert_eq!(view.after, Some(Figure::new("3 M", month.clone())));
        assert!(view.lead.starts_with(&format!("{month} capacity is 5 M. Starts ")), "{}", view.lead);
    }
    #[test]
    fn a_proposal_beside_an_active_grant_opens_the_amendment_when_the_server_supports_it() {
        let live = publication(OwnerGrantPublicationState::ServerObserved, Some("active"));
        assert_eq!(
            proposal_notice(&live, Some(3_000_000), "", true, OCTOBER).unwrap(),
            format!("Pool total allowance: 2 M → 3 M\n{AMENDABLE_NOTE}")
        );
        let draft = initial_draft(Some(3_000_000), &[]);
        assert!(primary_available(&live, None, &draft, true, true, true, OCTOBER));
        // Only the native amendment form can commit; without support or a
        // pool selector nothing is offered.
        assert!(!primary_available(&live, None, &draft, true, true, false, OCTOBER));
        assert!(!primary_available(&live, None, &draft, true, false, true, OCTOBER));
        // The reply is the public progress, not a refusal.
        let reply: serde_json::Value = serde_json::from_str(
            &close_reply(&live, Some(3_000_000), false, Some("original"), true, OCTOBER).unwrap(),
        )
        .unwrap();
        assert_eq!(reply["serverState"], "active");
        assert_eq!(reply["summary"]["grantId"], "grant_original");
        // A revoked, superseded or expired grant cannot be amended.
        for server in ["revoked", "superseded", "expired"] {
            let used = publication(OwnerGrantPublicationState::ServerObserved, Some(server));
            assert_eq!(
                proposal_notice(&used, Some(3_000_000), "", true, OCTOBER).unwrap(),
                format!("Pool total allowance: Not in effect → 3 M\n{NOT_AMENDABLE_NOTE}")
            );
            assert!(!primary_available(&used, None, &draft, true, true, true, OCTOBER));
        }
    }
    #[test]
    fn a_scheduled_renewal_is_shown_but_never_amended_from_here() {
        let mut p = OwnerGrantPublicationProgress {
            summary: summary(2_000_000),
            state: OwnerGrantPublicationState::ServerObserved,
            server_state: Some("active".into()),
            server_revision: Some(1),
            observed_at_ms: Some(1),
        };
        p.summary.period_key = "2026-11".into();
        p.summary.period_start_ms = NOVEMBER;
        p.summary.period_end_ms = 1_796_083_200_000;
        let scheduled = Phase::PublicationProgress(p);
        assert!(published_grant(&scheduled, OCTOBER) && scheduled_grant(&scheduled, OCTOBER));
        assert_eq!(live_grant_tokens(&scheduled, OCTOBER), None);
        assert_eq!(
            proposal_notice(&scheduled, Some(3_000_000), "", true, OCTOBER).unwrap(),
            format!("Pool total allowance: Not in effect → 3 M\n{SCHEDULED_NOTE}")
        );
        let draft = initial_draft(Some(3_000_000), &[]);
        assert!(!primary_available(&scheduled, None, &draft, true, true, true, OCTOBER));
        // In its own month it is the active grant and can be amended.
        assert!(!scheduled_grant(&scheduled, NOVEMBER));
        assert_eq!(live_grant_tokens(&scheduled, NOVEMBER), Some(2_000_000));
        assert!(primary_available(&scheduled, None, &draft, true, true, true, NOVEMBER));
    }
    #[test]
    fn closing_beside_a_published_grant_names_why_the_proposal_was_not_applied() {
        for server in ["active", "revoked", "expired"] {
            let phase = publication(OwnerGrantPublicationState::ServerObserved, Some(server));
            assert_eq!(
                close_reply(&phase, Some(3_000_000), false, Some("original"), false, OCTOBER),
                Err("pool_grant_already_published".into())
            );
            // Without a proposal the reply stays the public publication status.
            let reply: serde_json::Value =
                serde_json::from_str(&close_reply(&phase, None, false, Some("original"), false, OCTOBER).unwrap()).unwrap();
            assert_eq!(reply["state"], "server_observed");
            assert_eq!(reply["serverState"], server);
            assert_eq!(reply["summary"]["allocatedTokens"], 2_000_000);
        }
        // Other outcomes keep their public progress so the app can recover them.
        let reserved: serde_json::Value = serde_json::from_str(
            &close_reply(&progress(OwnerGrantState::Recorded, false), Some(3_000_000), false, Some("original"), false, OCTOBER).unwrap(),
        ).unwrap();
        assert_eq!(reserved["state"], "recorded");
        assert_eq!(reserved["summary"]["allocatedTokens"], 2_000_000);
        let unknown = close_reply(
            &publication(OwnerGrantPublicationState::OutcomeUnknown, None),
            Some(3_000_000),
            false,
            Some("original"),
            false,
            OCTOBER,
        );
        assert!(unknown.unwrap().contains("\"state\":\"outcome_unknown\""));
        // Closing the form itself applies nothing and names no grant.
        let parsed = |reply: Result<String, String>| serde_json::from_str::<serde_json::Value>(&reply.unwrap()).unwrap();
        assert_eq!(
            parsed(close_reply(
                &Phase::Choose,
                Some(3_000_000),
                false,
                None,
                false, OCTOBER
            )),
            json!({"state":"review_closed","intentId":null})
        );
        assert_eq!(
            parsed(close_reply(
                &Phase::Loading,
                Some(3_000_000),
                true,
                None,
                false, OCTOBER
            )),
            json!({"state":"outcome_unknown","intentId":null})
        );
    }
    /// The view of one native state, with an active flow that supports
    /// successors, as the shipped panel computes it.
    fn preview_view(
        phase: &Phase,
        draft: &GrantChoices,
        successor: Option<&OwnerGrantSuccessionForm>,
        form: Option<&OwnerGrantForm>,
        applicants: &[String],
        disclosure: Disclosure,
    ) -> GrantView {
        let state = GrantState {
            phase,
            draft,
            proposal: None,
            form,
            successor,
            requested: successor.map(|form| succession_arg(form.kind)),
            applicants,
            account: "@owner:test",
            original: false,
            in_flight: false,
            submitted: false,
            submitting: false,
            publishing: false,
            cancelling: false,
            regrant: false,
            active: true,
            can_choose_new_limits: true,
            succession_supported: Some(true),
            disclosure,
            add_error: "",
            add_people: false,
        };
        grant_view(&state, now_ms())
    }
    #[test]
    fn adding_people_names_only_them_and_keeps_the_unchanged_limit_out_of_the_way() {
        let form = successor_form(OwnerGrantSuccessionKind::Amendment);
        // As the mini app sends it: the current limit and the people to add.
        let applicants = ["@lisi:test".to_owned()];
        let mut draft = initial_draft(Some(2_000_000), &applicants);
        fill_successor_defaults(&mut draft, &form, now_ms());
        draft.coordinator = "@owner:test".into();
        let view = preview_view(&Phase::Choose, &draft, Some(&form), None, &applicants, Disclosure::default());
        assert_eq!(view.title, "Add People Who Can Request");
        assert_eq!(view.glyph, SheetGlyph::People);
        assert_eq!(view.caption, "People who can request");
        assert_eq!(view.before, Some(Figure::new("1 person", "Now")));
        assert_eq!(view.after, Some(Figure::new("2 people", "After you confirm")));
        assert!(view.lead.starts_with("Adds @lisi to "), "{}", view.lead);
        assert_eq!(view.people_note, "This month’s list only grows: people already on it can’t be removed.");
        assert_eq!(view.primary, "Add to This Month’s List");
        assert!(view.primary_enabled && view.people_editable);
        assert_eq!(names(&view), [("@manager".to_owned(), false, false), ("@lisi".to_owned(), true, true)]);
        // The unchanged limit is a row, never a slider and never "2 M → 2 M".
        let open = preview_view(&Phase::Choose, &draft, Some(&form), None, &applicants, Disclosure { sharing: true, ..Disclosure::default() });
        assert!(!view.quota_visible && !open.quota_visible && open.sharing_visible && open.sharing_open && !view.sharing_open);
        assert_eq!(facts(&view), [("Pool total allowance".to_owned(), "2 M (unchanged)".to_owned())]);
        // Someone already on the list: nothing to confirm.
        let already = ["@manager:test".to_owned()];
        let mut same = initial_draft(Some(2_000_000), &already);
        fill_successor_defaults(&mut same, &form, now_ms());
        same.coordinator = "@owner:test".into();
        let view = preview_view(&Phase::Choose, &same, Some(&form), None, &already, Disclosure::default());
        assert_eq!(view.lead, "@manager are already on this month’s list.");
        assert!(!view.primary_enabled);
        // Removing someone already on the list is refused beside the list.
        let mut removed = draft.clone();
        removed.managers = "@lisi:test".into();
        let view = preview_view(&Phase::Choose, &removed, Some(&form), None, &applicants, Disclosure::default());
        assert_eq!(view.problem, "This month’s list can only grow: keep everyone already on it.");
        assert!(!view.primary_enabled);
    }
    #[test]
    fn the_list_takes_one_person_at_a_time_on_the_owners_server() {
        let current = ["@manager:test".to_owned()];
        for (typed, added) in [("lisi", "@lisi:test"), ("@lisi", "@lisi:test"), (" @lisi:test ", "@lisi:test"), ("wang.wu_2", "@wang.wu_2:test"), ("LiSi", "@lisi:test")] {
            assert_eq!(applicant_from_input(typed, "@owner:test", &current).as_deref(), Ok(added), "{typed}");
        }
        for (typed, reason) in [
            ("", "Type a username first, for example lisi."),
            ("@lisi:elsewhere.org", "Only people on your server (test) can be added."),
            ("Li Si", "A username uses lowercase letters, digits and . _ = - / + only."),
            ("manager", "@manager is already on the list."),
        ] {
            assert_eq!(applicant_from_input(typed, "@owner:test", &current).unwrap_err(), reason, "{typed}");
        }
        let full: Vec<String> = (0..64).map(|n| format!("@p{n}:test")).collect();
        assert_eq!(applicant_from_input("lisi", "@owner:test", &full).unwrap_err(), "At most 64 people can be on the list.");
        // A first publish lists everyone as removable, with no "new" tag.
        let rows = listed_people(&[], &["@a:test".to_owned(), "@b:test".to_owned()], false, true);
        assert!(rows.iter().all(|p| p.removable && !p.added));
        // A reason beside the field shows in the view until the owner types.
        let form = successor_form(OwnerGrantSuccessionKind::Amendment);
        let mut draft = initial_draft(None, &[]);
        fill_successor_defaults(&mut draft, &form, now_ms());
        let state = GrantState {
            phase: &Phase::Choose, draft: &draft, proposal: None, form: None, successor: Some(&form),
            requested: Some(Succession::Amendment), applicants: &[], account: "@owner:test", original: false,
            in_flight: false, submitted: false, submitting: false, publishing: false, cancelling: false, regrant: false,
            active: true, can_choose_new_limits: true, succession_supported: Some(true), disclosure: Disclosure::default(),
            add_error: "@manager is already on the list.", add_people: false,
        };
        assert_eq!(grant_view(&state, now_ms()).add_error, "@manager is already on the list.");
    }
    #[test]
    fn an_unchanged_amendment_cannot_be_confirmed() {
        let form = successor_form(OwnerGrantSuccessionKind::Amendment);
        let mut draft = initial_draft(None, &[]);
        fill_successor_defaults(&mut draft, &form, now_ms());
        draft.coordinator = "@owner:test".into();
        assert!(choices_valid(&successor_bounds(&form), &draft));
        let view = preview_view(&Phase::Choose, &draft, Some(&form), None, &[], Disclosure::default());
        assert_eq!(view.lead, "Choose a new amount below, or add people.");
        assert_eq!((view.before.clone(), view.after.clone()), (None, Some(Figure::new("2 M", "Now"))));
        assert!(!view.primary_enabled && view.primary_visible);
    }
    /// A Choose sheet as the window computes it, with the page's proposal.
    fn proposal_state<'a>(draft: &'a GrantChoices, form: &'a OwnerGrantSuccessionForm, proposal: Option<u64>) -> GrantState<'a> {
        GrantState {
            phase: &Phase::Choose,
            draft,
            proposal,
            form: None,
            successor: Some(form),
            requested: Some(succession_arg(form.kind)),
            applicants: &[],
            account: "@owner:test",
            original: false,
            in_flight: false,
            submitted: false,
            submitting: false,
            publishing: false,
            cancelling: false,
            regrant: false,
            active: true,
            can_choose_new_limits: true,
            succession_supported: Some(true),
            disclosure: Disclosure::default(),
            add_error: "",
            add_people: false,
        }
    }
    /// The owner's pool on 30 October: a pool total allowance of 9 M, and one
    /// running Agent holding all 9 M this month can give out.
    fn held_pool() -> OwnerGrantSuccessionForm {
        let mut form = successor_form(OwnerGrantSuccessionKind::Amendment);
        form.predecessor.allocated_tokens = 9_000_000;
        form.context.remaining_tokens = Some(9_000_000);
        form.floor = ::hagency::native_owner::OwnerGrantFloor { children: 1, retained_tokens: 9_000_000 };
        form
    }
    #[test]
    fn a_page_value_below_what_running_agents_hold_is_never_offered_as_the_change() {
        let now = now_ms();
        // The page proposed 5 M; the form starts at the 9 M running Agents hold.
        let form = held_pool();
        let mut draft = initial_draft(Some(5_000_000), &[]);
        fill_successor_defaults(&mut draft, &form, now);
        draft.coordinator = "@owner:test".into();
        assert_eq!(draft.tokens, "9000000");
        let state = proposal_state(&draft, &form, Some(5_000_000));
        let view = grant_view(&state, now);
        assert!(!view.primary.contains("5 M"), "{}", view.primary);
        assert_eq!(view.primary, "Confirm Changes");
        assert!(view.primary_visible && !view.primary_enabled);
        // One possible amount: no slider, the reason and the way forward in the hero.
        assert!(!view.quota_visible);
        assert_eq!(view.after, Some(Figure::new("9 M", "Now")));
        assert_eq!(view.lead, "You chose 5 M on the pool’s page, but running Agents hold all 9 M this month can give out, so the pool total allowance can only be 9 M now. To give more, raise the monthly maximum on the pool’s page.");
        let inputs = GrantInputs::of(&state, now);
        assert_eq!((inputs.minimum, inputs.maximum, inputs.tokens.as_str()), (9_000_000, Some(9_000_000), "9000000"));
        // Typed below the floor in the window: the button keeps its plain name, disabled, beside the reason.
        let mut typed = draft.clone();
        typed.tokens = "5000000".into();
        typed.per_request = "5000000".into();
        let view = grant_view(&proposal_state(&typed, &form, Some(5_000_000)), now);
        assert_eq!(view.primary, "Confirm Changes");
        assert!(!view.primary_enabled);
        assert_eq!(view.problem, "It can’t be lower than the 9 M held by running Agents.");
        assert_eq!(view.after, Some(Figure::new("5 M", "You chose")));
        // Nor does the line under the slider present it as the page's choice to adjust.
        assert!(!view.quota_hint.contains("you can still adjust it"), "{}", view.quota_hint);
        // With room above the floor the slider shows, starting at the floor, and says why.
        let mut roomy = held_pool();
        roomy.context.remaining_tokens = Some(20_000_000);
        let mut draft = initial_draft(Some(5_000_000), &[]);
        fill_successor_defaults(&mut draft, &roomy, now);
        draft.coordinator = "@owner:test".into();
        let view = grant_view(&proposal_state(&draft, &roomy, Some(5_000_000)), now);
        assert!(view.quota_visible);
        assert_eq!(view.quota_hint, "You chose 5 M on the pool’s page; it can’t be lower than the 9 M held by running Agents, so it starts at 9 M.");
        assert_eq!(view.lead, "You chose 5 M on the pool’s page, but it can’t be lower than the 9 M held by running Agents. Choose a larger amount below, or add people.");
        draft.tokens = "12000000".into();
        let view = grant_view(&proposal_state(&draft, &roomy, Some(5_000_000)), now);
        assert_eq!(view.primary, "Change to 12 M");
        assert!(view.primary_enabled);
        // Above what is left: lowered to it, said the same way.
        let mut draft = initial_draft(Some(25_000_000), &[]);
        fill_successor_defaults(&mut draft, &roomy, now);
        assert_eq!(draft.tokens, "20000000");
        let view = grant_view(&proposal_state(&draft, &roomy, Some(25_000_000)), now);
        assert_eq!(view.quota_hint, "You chose 25 M on the pool’s page; at most 20 M can be given out, so it starts at 20 M.");
        assert_eq!(view.primary, "Change to 20 M");
    }
    #[test]
    fn a_chosen_sharing_period_shows_selected_and_is_what_the_amendment_commits() {
        let now = now_ms();
        let form = successor_form(OwnerGrantSuccessionKind::Amendment);
        let mut draft = initial_draft(None, &[]);
        fill_successor_defaults(&mut draft, &form, now);
        draft.coordinator = "@owner:test".into();
        // Opened: sharing ends with the month, as it does now, so nothing can be confirmed yet.
        let state = proposal_state(&draft, &form, None);
        assert_eq!(GrantInputs::of(&state, now).selected, [false, false, true]);
        assert!(!state.changes_something() && !state.can_primary(now));
        // 1 day (a run on the month's last day has no day left to choose).
        let Some(day) = successor_preset(&form, Some(1), now) else { return };
        draft.expires_utc = day;
        let state = proposal_state(&draft, &form, None);
        assert_eq!(GrantInputs::of(&state, now).selected, [true, false, false]);
        assert!(state.changes_something() && state.can_primary(now));
        let view = grant_view(&state, now);
        assert_eq!(view.title, "Change Sharing Period");
        assert_eq!(view.caption, "Shared until");
        assert_eq!(view.primary, "Confirm Changes");
        assert!(view.primary_enabled && view.problem.is_empty());
        assert!(view.lead.starts_with("Now until "), "{}", view.lead);
        // What the amendment commits is the period chosen, to the second.
        let choices = draft.clone().parse().unwrap();
        assert!(choices.delegation_expires_at_ms.abs_diff(now + 86_400_000) < 2_000);
        assert_eq!(choices.allocated_tokens, form.predecessor.allocated_tokens);
        // Back to the month's end: nothing changes again.
        draft.expires_utc = successor_preset(&form, None, now).unwrap();
        assert!(!proposal_state(&draft, &form, None).changes_something());
    }
    #[test]
    fn publishing_names_the_month_and_keeps_ids_in_technical_details() {
        let form = pool_form(5_000_000);
        let mut draft = initial_draft(Some(500_000), &["@applicant:test".to_owned()]);
        fill_form_defaults(&mut draft, &form);
        draft.coordinator = "@owner:test".into();
        let view = preview_view(&Phase::Choose, &draft, None, Some(&form), &[], Disclosure::default());
        let month = ms_month(now_ms());
        assert_eq!(view.title, format!("Publish {month}"));
        assert_eq!(view.glyph, SheetGlyph::Publish);
        assert_eq!(view.before, Some(Figure::new("Not published", "Now")));
        assert_eq!(view.after, Some(Figure::new("0.5 M", "After you confirm")));
        // When sharing ends is one local time, the same in every state.
        let end = local_time(publication_period_end().timestamp_millis() as u64);
        assert_eq!(view.lead, format!("People on the list can request up to 0.5 M at a time, before {end}."));
        // The lead says when sharing ends; the rows don't repeat it.
        assert_eq!(facts(&view), [
            ("Monthly maximum".to_owned(), "20 M".to_owned()),
            ("Left this month".to_owned(), "5 M".to_owned()),
        ]);
        assert_eq!(view.primary, "Publish");
        assert_eq!(names(&view), [("@applicant".to_owned(), false, true)]);
        // When sharing ends is a visible row of its own, its choice folded under it.
        assert!(view.sharing_visible && !view.sharing_open);
        assert_eq!(view.expiry, format!("Before {end}"));
        assert!(!view.people_collapsible, "a publish chooses its list here");
        for primary in [&view.title, &view.pool, &view.lead, &view.status, &view.quota_hint, &view.expiry, &view.expiry_note] {
            let primary = without_zone(primary);
            assert!(!primary.contains("UTC") && !primary.contains("matrix.test") && !primary.contains("pool_1"), "{primary}");
        }
        // Details are for an administrator: who and where, no revisions or profiles.
        assert_eq!(view.more_label, "Technical Details");
        assert_eq!(view.technical, "Your account\n@owner:test\n\nServer\nhttps://matrix.test\n\nPool ID\npool_1");
        assert!(!view.more_open);
        // Nobody chosen yet: the add row says what is missing.
        let mut empty = draft.clone();
        empty.managers.clear();
        let view = preview_view(&Phase::Choose, &empty, None, Some(&form), &[], Disclosure::default());
        assert!(view.people_editable && !view.primary_enabled);
        assert_eq!(view.problem, "Add at least one person who can request.");
    }
    fn finished(phase: &Phase, kind: Option<&OwnerGrantSuccessionForm>, form: Option<&OwnerGrantForm>) -> GrantView {
        let draft = initial_draft(None, &[]);
        let state = GrantState {
            phase, draft: &draft, proposal: None, form, successor: kind,
            requested: kind.map(|form| succession_arg(form.kind)), applicants: &[], account: "@owner:test",
            original: true, in_flight: false, submitted: true, submitting: false, publishing: true, cancelling: false,
            regrant: false, active: true, can_choose_new_limits: true, succession_supported: Some(true),
            disclosure: Disclosure::default(), add_error: "", add_people: false,
        };
        grant_view(&state, OCTOBER)
    }
    #[test]
    fn a_finished_publish_says_what_now_applies_and_offers_done() {
        let live = publication(OwnerGrantPublicationState::ServerObserved, Some("active"));
        let form = pool_form(5_000_000);
        let view = finished(&live, None, Some(&form));
        assert_eq!(view.tone, SheetTone::Done);
        assert_eq!(view.title, "Published October");
        assert_eq!(view.after, Some(Figure::new("2 M", "Now")));
        assert_eq!(view.lead, "People on the list can request now, up to 2 M at a time.");
        assert_eq!((view.primary.as_str(), view.cancel.as_str()), ("Done", ""));
        assert!(view.primary_closes && view.primary_enabled && view.primary_visible);
        assert_eq!(names(&view), [("@manager".to_owned(), false, false)]);
        // The list was chosen in the proposal, so the result folds it, and a
        // finished publish offers no re-check row beside Done.
        assert!(view.people_collapsible && !view.people_open && !view.recover_visible);
        assert_eq!(facts(&view), [("Shared until".to_owned(), format!("Before {}", local_time(1793491200000)))]);
    }
    #[test]
    fn every_state_says_when_sharing_ends_in_the_same_local_words() {
        // The proposal's row and the finished window's row for one month end.
        let form = pool_form(5_000_000);
        let mut draft = initial_draft(Some(500_000), &["@applicant:test".to_owned()]);
        fill_form_defaults(&mut draft, &form);
        draft.coordinator = "@owner:test".into();
        let proposal = preview_view(&Phase::Choose, &draft, None, Some(&form), &[], Disclosure::default());
        let end = publication_period_end().timestamp_millis() as u64;
        let mut live = summary(500_000);
        live.period_end_ms = end;
        live.delegation_expires_at_ms = end;
        let done = Phase::PublicationProgress(OwnerGrantPublicationProgress {
            summary: live, state: OwnerGrantPublicationState::ServerObserved, server_state: Some("active".into()),
            server_revision: Some(1), observed_at_ms: Some(1),
        });
        let finished = finished(&done, None, Some(&form));
        let row = |view: &GrantView| view.facts.iter().find(|f| f.label == "Shared until").map(|f| f.value.clone());
        assert_eq!(row(&finished), Some(format!("Before {}", local_time(end))));
        assert!(proposal.lead.contains(&format!("before {}", local_time(end))), "{}", proposal.lead);
        assert_eq!(proposal.expiry, format!("Before {}", local_time(end)));
        // The time names its zone, as the pool's page does; no second, UTC date is needed beside it.
        let zone = crate::host::hagency::local_zone::label(Some(end));
        assert!(local_time(end).ends_with(&format!("({zone})")), "{}", local_time(end));
        assert!(finished.facts[0].note.is_empty());
        assert!(!finished.technical.contains("UTC") && !finished.technical.contains("Shared until"));
        for view in [&proposal, &finished] {
            for text in [&view.lead, &view.status, &view.expiry, &view.expiry_note] {
                let text = without_zone(text);
                assert!(!text.contains("End of") && !text.contains("UTC"), "{text}");
            }
        }
    }
    /// Local times name the device's zone; where it isn't Beijing time that is
    /// an offset ("(UTC-7)"), which is a zone, not UTC jargon.
    fn without_zone(text: &str) -> String {
        let mut out = text.to_owned();
        for (open, close) in [("(UTC", ")"), ("（UTC", "）")] {
            while let Some(start) = out.find(open) {
                let Some(len) = out[start..].find(close) else { break };
                out.replace_range(start..start + len + close.len(), "");
            }
        }
        out
    }
    #[test]
    fn a_list_the_window_keeps_folds_into_one_row_that_opens_in_place() {
        let form = successor_form(OwnerGrantSuccessionKind::Amendment);
        let mut draft = initial_draft(Some(800_000), &[]);
        fill_successor_defaults(&mut draft, &form, now_ms());
        draft.coordinator = "@owner:test".into();
        let folded = preview_view(&Phase::Choose, &draft, Some(&form), None, &[], Disclosure::default());
        assert!(folded.people_collapsible && !folded.people_open && folded.people_editable);
        assert_eq!(folded.people_summary, "1 person");
        let open = preview_view(&Phase::Choose, &draft, Some(&form), None, &[], Disclosure { people: true, ..Disclosure::default() });
        assert!(open.people_collapsible && open.people_open);
        assert_eq!(names(&open), [("@manager".to_owned(), false, false)]);
        // A record opened to read the list shows it.
        let record = GrantState {
            phase: &publication(OwnerGrantPublicationState::ServerObserved, Some("active")), draft: &draft, proposal: None,
            form: None, successor: None, requested: None, applicants: &[], account: "@owner:test", original: true,
            in_flight: false, submitted: false, submitting: false, publishing: true, cancelling: false, regrant: false,
            active: true, can_choose_new_limits: false, succession_supported: Some(true), disclosure: Disclosure::default(),
            add_error: "", add_people: false,
        };
        let view = grant_view(&record, OCTOBER);
        assert!(!view.people_collapsible && view.people_open && !view.recover_visible);
        // This month's record adds people right here, not by a trip to the page.
        assert!(view.people_add_visible);
        assert_eq!(view.people_note, "This month’s list only grows: people already on it can’t be removed.");
        // A server that can't change a published month says so; unknown support keeps the page's way.
        for (support, note) in [
            (Some(false), "This server can’t change a published month or publish ahead yet. Ask its administrator to update the server."),
            (None, "To add someone, close this window and use Who Can Request on the pool’s page."),
        ] {
            let view = grant_view(&GrantState { succession_supported: support, ..record }, OCTOBER);
            assert!(!view.people_add_visible);
            assert_eq!(view.people_note, note);
        }
        // An ended month, or one the server no longer holds, offers nothing to add to.
        assert!(!grant_view(&record, NOVEMBER).people_add_visible);
        let revoked = publication(OwnerGrantPublicationState::ServerObserved, Some("revoked"));
        assert!(!grant_view(&GrantState { phase: &revoked, ..record }, OCTOBER).people_add_visible);
        // Chosen from the record: the add form keeps the limit and names nobody yet.
        let form = successor_form(OwnerGrantSuccessionKind::Amendment);
        let mut adding = initial_draft(None, &[]);
        fill_successor_defaults(&mut adding, &form, now_ms());
        let state = GrantState {
            phase: &Phase::Choose, draft: &adding, successor: Some(&form), requested: Some(Succession::Amendment),
            original: false, publishing: false, can_choose_new_limits: true, add_people: true, ..record
        };
        let view = grant_view(&state, now_ms());
        assert_eq!(view.title, "Add People Who Can Request");
        assert_eq!(view.glyph, SheetGlyph::People);
        assert_eq!(view.lead, "Add someone below.");
        assert!(view.people_editable && !view.people_collapsible && !view.quota_visible);
        assert!(!view.primary_enabled, "nobody added yet");
        // While the form loads, the window already says what it is for.
        let loading = GrantState { phase: &Phase::Loading, successor: None, in_flight: true, ..state };
        assert_eq!(grant_view(&loading, now_ms()).title, "Add People Who Can Request");
    }
    #[test]
    fn adding_people_after_next_month_is_published_says_when_it_works_again() {
        let form = successor_form(OwnerGrantSuccessionKind::Amendment);
        let applicants = ["@lisi:test".to_owned()];
        let mut draft = initial_draft(Some(2_000_000), &applicants);
        fill_successor_defaults(&mut draft, &form, now_ms());
        let view = preview_view(&Phase::Failed("owner_grant_successor_exists".into()), &draft, Some(&form), None, &applicants, Disclosure::default());
        assert_eq!(view.title, "No One Was Added");
        assert_eq!(view.lead, format!(
            "Next month is already published ahead, so this month’s list can’t change. You can add people again from {}.",
            local_time(form.period_end_ms)
        ));
        assert_eq!(view.status, "Nothing was changed.");
        // Opened without a form (the server refused before it loaded): the
        // next month's start comes from the clock.
        let early = preview_view(&Phase::Failed("grant_successor_exists".into()), &draft, None, None, &applicants, Disclosure::default());
        assert!(early.lead.ends_with(&format!("from {}.", local_time(publication_period_end().timestamp_millis() as u64))), "{}", early.lead);
    }
    #[test]
    fn proposal_success_and_failure_look_different_and_failure_names_what_works() {
        let form = successor_form(OwnerGrantSuccessionKind::Amendment);
        let mut draft = initial_draft(Some(500_000), &[]);
        fill_successor_defaults(&mut draft, &form, now_ms());
        // The form starts at the floor; 0.5 M was submitted before the floor rose to 0.6 M.
        assert_eq!(draft.tokens, "600000");
        draft.tokens = "500000".into();
        let proposal = preview_view(&Phase::Choose, &draft, Some(&form), None, &[], Disclosure::default());
        let failed = preview_view(&Phase::Failed("owner_grant_below_floor".into()), &draft, Some(&form), None, &[], Disclosure::default());
        assert_eq!(proposal.tone, SheetTone::Proposal);
        assert_eq!(failed.tone, SheetTone::Failed);
        assert_ne!(proposal.title, failed.title);
        assert_eq!(failed.title, "Pool Limits Not Saved");
        assert_eq!(failed.caption, "The pool total allowance must be at least");
        assert_eq!(failed.after, Some(Figure::new("0.6 M", "Held by running Agents")));
        assert_eq!(failed.lead, "You chose 0.5 M, which is less than the 0.6 M held by running Agents. Choose 0.6 M or more.");
        assert_eq!(failed.status, "Nothing was changed.");
        // The way forward is the primary button: back to the form.
        assert!(failed.primary_recovers && failed.primary_enabled);
        // The fix is a known amount, so the button offers it in the verb that commits it.
        assert_eq!((failed.primary.as_str(), failed.cancel.as_str()), ("Change to 0.6 M", "Close"));
        assert_eq!(failed.fix_tokens, Some(600_000));
        // Choosing another amount stays one row away.
        assert!(failed.recover_row() && failed.recover == "Choose Again");
        // Publishing more than is left: the most that works, one tap away, as in the other windows.
        let publish = pool_form(2_000_000);
        let mut over = initial_draft(Some(2_500_000), &[]);
        fill_form_defaults(&mut over, &publish);
        let refused = preview_view(&Phase::Failed("owner_grant_capacity_unavailable".into()), &over, None, Some(&publish), &[], Disclosure::default());
        assert_eq!(refused.after, Some(Figure::new("2 M", "Left this month")));
        assert_eq!((refused.primary.as_str(), refused.cancel.as_str()), ("Change to 2 M", "Close"));
        assert_eq!(refused.fix_tokens, Some(2_000_000));
        assert!(refused.primary_recovers && refused.recover_row() && refused.recover == "Choose Again");
        // An unknown outcome is amber and asks to check before anything else.
        let unsure = preview_view(&Phase::Failed("management_outcome_unknown".into()), &draft, Some(&form), None, &[], Disclosure::default());
        assert_eq!(unsure.tone, SheetTone::Unsure);
        assert_eq!(unsure.title, "Not Confirmed Yet");
        assert!(unsure.primary_recovers);
        // A finished change is green with one Done.
        let mut summary = summary(800_000);
        summary.succession = Some(::hagency::native_owner::OwnerGrantSuccessionSummary {
            kind: OwnerGrantSuccessionKind::Amendment, predecessor_grant_id: "grant_original".into(),
            predecessor_allocated_tokens: 2_000_000, carried_children: 2, carried_retained_tokens: 600_000,
        });
        let done = Phase::PublicationProgress(OwnerGrantPublicationProgress {
            summary, state: OwnerGrantPublicationState::ServerObserved, server_state: Some("active".into()),
            server_revision: Some(2), observed_at_ms: Some(1),
        });
        let view = finished(&done, Some(&form), None);
        assert_eq!(view.tone, SheetTone::Done);
        assert_eq!(view.title, "Pool Limits Saved");
        assert_eq!((view.before.clone().unwrap().value, view.after.clone().unwrap().value), ("2 M".into(), "0.8 M".into()));
        assert!(view.lead.starts_with("New requests now get up to 0.8 M."), "{}", view.lead);
        assert!(facts(&view).contains(&("Held by running Agents".to_owned(), "0.6 M · 2 Agents".to_owned())));
        // The list didn't change: folded into one row, and no re-check row.
        assert!(view.people_collapsible && !view.people_open && !view.recover_visible);
    }
    #[test]
    fn every_preview_state_renders_through_the_shipped_view() {
        let now = now_ms();
        for (state, kind, code) in [
            ("publish", "", ""), ("amend", "", ""), ("add_people", "", ""), ("renew", "", ""),
            ("published", "", ""), ("published", "amendment", ""), ("published", "add_people", ""), ("published", "renewal", ""),
            ("record", "", ""), ("loading", "amendment", ""), ("add_people", "", "adding"),
            ("failed", "", "owner_grant_capacity_unavailable"), ("failed", "amendment", "owner_grant_below_floor"),
            ("failed", "add_people", "invalid_grant_manager"), ("failed", "renewal", "owner_grant_renewal_window_closed"),
            ("failed", "amendment", "management_outcome_unknown"),
        ] {
            let fixture = json!({
                "panel": "grant", "state": state, "label": "Research", "model": "coding-model",
                "account": "@reviewer:example.test", "current": 200000, "proposed": 250000,
                "remaining": 800000, "held": 100000, "running": 1,
                "managers": ["@applicant:example.test", "@designer:example.test"],
                "added": if code == "adding" { json!([]) } else { json!(["@lisi:example.test"]) },
                "kind": kind, "code": code, "typed": "Li Si", "adding": code == "adding", "sharing": true,
            });
            let preview = GrantPreview::from_fixture(&fixture, now).unwrap();
            let view = grant_view(&preview.state(), now);
            assert!(!view.title.is_empty(), "{state}");
            for primary in [&view.title, &view.lead, &view.status, &view.caption, &view.problem] {
                let primary = without_zone(primary);
                assert!(!primary.contains(":example.test") && !primary.contains("UTC") && !primary.contains("pool_original"), "{state}: {primary}");
            }
            // Details name who and where; internal IDs and revisions stay out.
            for internal in ["Resource revision", "Local profile", "Resource ID", "Grant ID", "Record ID", "cccc", "profile_owner"] {
                assert!(!view.technical.contains(internal), "{state}: {}", view.technical);
            }
            for fact in &view.facts {
                assert!(!fact.label.is_empty() && !fact.value.contains(":example.test"), "{state}: {fact:?}");
            }
            assert!(view.primary_visible || !view.cancel.is_empty(), "{state} has a way out");
        }
    }
    /// String literals in Rust source, unescaped as the compiler would.
    fn literals(source: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut chars = source.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '"' {
                continue;
            }
            let mut text = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' => match chars.next() {
                        Some('n') => text.push('\n'),
                        Some(other) => text.push(other),
                        None => break,
                    },
                    c => text.push(c),
                }
            }
            out.push(text);
        }
        out
    }
    /// The native windows speak the mini app's words: what running Agents
    /// hold is 正在运行的 Agent 已拿到 (the per-request floor), and the
    /// monthly maximum's floor is 已用和已分出. 已承诺 is retired.
    #[test]
    fn native_pool_windows_use_the_mini_apps_words_in_chinese() {
        use crate::i18n::{Language, text_for};
        const BANNED: [&str; 32] = [
            "每月总上限", "本设备", "智能体", "令牌", "授权意向", "准入", "协调者", "发布额度",
            "M tokens", "申请者", "已贡献", "结算周期", "资源池发布",
            // Retired words for what Agents hold and the committed total.
            "已分到", "已占用", "预留", "已计入", "承诺给", "已有额度", "保留已有", "已占",
            "已承诺", "承诺", "所占的部分", "已分给正在运行的",
            // Month ends are one local time ("11 月 1 日 08:00 前"), never "10 月底".
            "{month}底",
            // Round 8: one verb, one section name and one saving line in both windows, the
            // page's name for the list, and an option named rather than hidden as "advanced".
            "不变的部分", "正在保存…", "可以申请的人 ·", "加入本月名单", "高级选项", "每次申请上限的下限",
        ];
        let cut = |source: &'static str, from: &str, to: &str| -> &'static str {
            let start = source.find(from).unwrap_or(0);
            let end = source[start..].find(to).map_or(source.len(), |n| start + n);
            &source[start..end]
        };
        let sources = [
            cut(include_str!("grant_ui.rs"), "//!", "#[cfg(test)]\nmod tests {"),
            cut(include_str!("sheet_ui.rs"), "//!", "#[cfg(test)]"),
            cut(include_str!("resource_edit_ui.rs"), "//!", "#[cfg(test)]"),
            cut(include_str!("../host/hagency/resource_edit.rs"), "//!", "#[cfg(test)]"),
            cut(include_str!("ui.rs"), "let SheetGroup = RoundedView", "mod.widgets.MiniAppsPanel"),
            cut(include_str!("resource_enrollment_ui.rs"), "Self::Create(v) if v.recovering()", "pub(super) async fn accept"),
        ];
        let mut checked = 0;
        for source in sources {
            for english in literals(source) {
                // One verb commits a new value in both windows: "Change to {amount}".
                assert!(!english.eq_ignore_ascii_case("Save Monthly Maximum"), "a window still says {english:?}");
                let chinese = text_for(Language::Chinese, &english);
                if chinese == english {
                    continue;
                }
                checked += 1;
                for word in BANNED {
                    assert!(!chinese.contains(word), "{english:?} is shown as {chinese:?}, which says {word}");
                }
            }
        }
        assert!(checked > 200, "only {checked} translated strings were checked");
        // The window titles and the two hold words use the tab's own terms.
        for (english, chinese) in [
            ("Publish {month}", "发布 {month}额度"),
            ("Change Pool Limits", "修改资源池额度"),
            ("Add People Who Can Request", "添加可以申请的人"),
            ("Publish {month} Ahead", "提前发布 {month}额度"),
            ("Pool total allowance", "资源池总额度"),
            ("Change the Monthly Maximum", "修改每月上限"),
            ("Held by running Agents", "正在运行的 Agent 已拿到"),
            ("Used and given out this month", "本月已用和已分出"),
            ("Most it can be", "最多可设为"),
            ("What this month can still give out.", "本月还能分出的额度。"),
            ("Nothing was changed.", "什么都没有改动。"),
            // The time ends with its zone,「11 月 1 日 08:00（北京时间）」, so 前 follows the bracket.
            ("Before {when}", "{when}前"),
            ("Monthly Maximum Saved", "已保存每月上限"),
            ("Pool Limits Not Saved", "资源池额度未保存"),
            // One verb commits a new value in both windows and on the page; one saving line; one
            // section name; the page's own name for the list of people.
            ("Change to {amount}", "改成 {amount}"),
            ("Saving your change…", "正在保存更改…"),
            ("This pool", "这个资源池"),
            ("People who can request", "谁可以申请"),
            ("Add to This Month’s List", "添加到本月名单"),
        ] {
            assert_eq!(text_for(Language::Chinese, english), chinese);
        }
        // Every term and verb the windows share with the pool's page is the page's own text.
        let page = include_str!("../../apps/hagency/bundle/main.splash");
        for english in [
            "Monthly maximum", "Held by running Agents", "Used and given out this month",
            "This month’s list only grows: people already on it can’t be removed.", "People who can request",
            "Add People Who Can Request", "Add to This Month’s List", "This pool", "Choose Again", "Publish Ahead",
            "Account limit", "Change to {amount}",
        ] {
            let chinese = text_for(Language::Chinese, english).replace("{amount}", "");
            assert!(page.contains(&chinese), "the windows say {chinese:?} for {english:?}; the pool's page doesn't");
        }
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use makepad_widgets::makepad_draw::cx_draw::CxDraw;
    /// Draw one sheet at a size and return the panel, so rects can be read.
    fn draw_sheet(width: f64, height: f64, view: &GrantView, inputs: Option<&GrantInputs>, root: LiveId) -> (Cx, MiniAppsPanel) {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::i18n::install(vm);
            makepad_code_editor::script_mod(vm);
            crate::shared::script_mod(vm);
            vm.bx.captured_errors = Some(Vec::new());
            crate::miniapps::script_mod(vm);
            let value = vm.eval(script! {use mod.prelude.widgets.* use mod.widgets.* MiniAppsPanel{}});
            let panel = MiniAppsPanel::script_from_value(vm, value);
            let errors = vm.take_errors();
            assert!(errors.is_empty(), "native sheet script errors: {errors:?}");
            panel
        });
        for id in [ids!(import_form), ids!(app_content), ids!(notice), ids!(approval)] {
            panel.view.widget(&cx, id).set_visible(&mut cx, false);
        }
        panel.view.widget(&cx, &[root]).set_visible(&mut cx, true);
        if root == id!(grant_confirmation) {
            panel.apply_grant_view(&mut cx, view, inputs);
        } else {
            panel.apply_sheet(&mut cx, root, view);
        }
        let pass = DrawPass::new(&mut cx);
        pass.set_size(&mut cx, dvec2(width, height));
        let mut list = DrawList2d::new(&mut cx);
        let event = DrawEvent::default();
        let mut draw = CxDraw::new(&mut cx, &event);
        let mut cx2d = Cx2d::new(&mut draw);
        cx2d.begin_pass(&pass, None);
        list.begin_always(&mut cx2d);
        cx2d.begin_root_turtle(dvec2(width, height), Layout::flow_overlay());
        panel.draw_all(&mut cx2d, &mut Scope::empty());
        cx2d.end_pass_sized_turtle();
        list.end(&mut cx2d);
        cx2d.end_pass(&pass);
        drop(cx2d);
        drop(draw);
        (cx, panel)
    }
    fn long_view(cn: bool, choosing: bool) -> GrantView {
        let text = |en: &str, zh: &str| if cn { zh.to_owned() } else { en.to_owned() };
        let people: Vec<Person> = (0..12)
            .map(|n| Person { id: format!("@person{n}:example.org"), name: format!("@person{n}"), added: n % 2 == 1, removable: n % 2 == 1 })
            .collect();
        GrantView {
            tone: if choosing { SheetTone::Proposal } else { SheetTone::Done },
            glyph: SheetGlyph::Change,
            title: text("Change Pool Limits", "修改资源池额度"),
            pool: "Research · coding-model".into(),
            caption: text("Pool total allowance", "资源池总额度"),
            before: Some(Figure::new("0.2 M", text("Now", "现在"))),
            after: Some(Figure::new("0.25 M", text("After you confirm", "确认后"))),
            lead: text("Applies this month as soon as you confirm. What running Agents hold stays as it is.", "确认后本月立即生效；正在运行的 Agent 已拿到的额度不变。").repeat(3),
            caution: text("Waiting requests above 0.25 M can’t be approved after this.", "改完后，超过 0.25 M 的待批准申请不能再批准。"),
            facts: (0..6).map(|n| Fact::new(format!("Fact {n}"), "0.8 M").note(if n == 2 { "note" } else { "" })).collect(),
            facts_label: text("This pool", "这个资源池"),
            people_visible: true,
            people_label: text("People who can request · 12 people", "可以申请的人 · 12 人"),
            people,
            people_note: text("This month’s list only grows.", "本月名单只能加人。"),
            people_editable: choosing,
            add_placeholder: text("Add someone: type a username, for example lisi", "添加：输入用户名，例如 lisi"),
            add_error: text("Only people on your server (example.org) can be added.", "只能添加你所在服务器（example.org）上的人。"),
            choosing,
            quota_visible: choosing,
            quota_hint: text("Choose 0.1 M – 0.8 M.", "可选 0.1 M – 0.8 M。"),
            more_label: text("Technical Details", "技术详情"),
            more_open: true,
            sharing_visible: choosing,
            sharing_open: true,
            expiry: text("Before November 1, 8:00 AM (Beijing time)", "11 月 1 日 08:00（北京时间）前"),
            expiry_note: text("Sharing ends before November 1, 8:00 AM (Beijing time) at the latest.", "最晚在 11 月 1 日 08:00（北京时间）前结束共享。"),
            technical: text("Your account\n@reviewer:example.org\n\n", "你的账号\n@reviewer:example.org\n\n").repeat(40),
            recover: text("Check Again", "再查一次"),
            recover_visible: true,
            cancel_intent_visible: true,
            primary: text("Confirm Changes", "确认更改"),
            primary_visible: true,
            primary_enabled: true,
            cancel: text("Cancel", "取消"),
            ..GrantView::default()
        }
    }
    /// The sharing-period presets are key buttons (`MiniAppButton`).
    /// `ButtonRef::clicked` can't see their taps, so before this the window
    /// never heard 1 day, 7 days or to month end, and nothing changed.
    #[test]
    fn sharing_period_taps_reach_the_window() {
        let view = long_view(false, true);
        let inputs = GrantInputs { per_request: "200000".into(), tokens: "250000".into(), minimum: 100_000, maximum: Some(800_000), presets: [true, true, true], selected: [false, false, true] };
        let (mut cx, panel) = draw_sheet(840.0, 700.0, &view, Some(&inputs), id!(grant_confirmation));
        for (path, days) in [
            (ids!(grant_confirmation.sheet_scroll.sharing.period.expiry_presets.day), Some(1)),
            (ids!(grant_confirmation.sheet_scroll.sharing.period.expiry_presets.week), Some(7)),
            (ids!(grant_confirmation.sheet_scroll.sharing.period.expiry_presets.month), None),
        ] {
            let uid = panel.view.widget(&cx, path).widget_uid();
            let actions = cx.capture_actions(|cx| cx.widget_action(uid, ButtonAction::Clicked(KeyModifiers::default())));
            assert!(!panel.view.button(&cx, path).clicked(&actions), "ButtonRef now sees key buttons; the uid lookup can go");
            assert_eq!(panel.grant_preset_clicked(&cx, &actions), Some(days));
        }
        // Anything else tapped is no preset.
        let other = panel.view.widget(&cx, ids!(grant_confirmation.buttons.cancel)).widget_uid();
        let actions = cx.capture_actions(|cx| cx.widget_action(other, ButtonAction::Clicked(KeyModifiers::default())));
        assert_eq!(panel.grant_preset_clicked(&cx, &actions), None);
        // The chosen period shows selected: its tinted twin replaces it.
        let selected = GrantInputs { selected: [true, false, false], ..inputs };
        let (cx, panel) = draw_sheet(840.0, 700.0, &view, Some(&selected), id!(grant_confirmation));
        let shown = |name: &[LiveId]| panel.view.widget(&cx, name).visible();
        assert!(!shown(ids!(grant_confirmation.sheet_scroll.sharing.period.expiry_presets.day)));
        assert!(shown(ids!(grant_confirmation.sheet_scroll.sharing.period.expiry_presets.day_on)));
        assert!(shown(ids!(grant_confirmation.sheet_scroll.sharing.period.expiry_presets.month)));
        assert!(!shown(ids!(grant_confirmation.sheet_scroll.sharing.period.expiry_presets.month_on)));
    }
    #[test]
    fn native_sheets_keep_a_full_width_footer_in_view_with_long_text() {
        for (width, height) in [(360.0, 640.0), (840.0, 700.0)] {
            for cn in [false, true] {
                for choosing in [true, false] {
                    for root in [id!(grant_confirmation), id!(ceiling_confirmation)] {
                        let view = long_view(cn, choosing);
                        let inputs = GrantInputs { per_request: "200000".into(), tokens: "250000".into(), minimum: 100_000, maximum: Some(800_000), presets: [true, false, true], selected: [false, false, true] };
                        let (cx, panel) = draw_sheet(width, height, &view, Some(&inputs), root);
                        let at = |rest: &[LiveId]| super::super::sheet_ui::sheet_path(root, rest);
                        let cancel = panel.view.widget(&cx, &at(&[id!(buttons), id!(cancel)])).area().rect(&cx);
                        let primary = panel.view.widget(&cx, &at(&[id!(buttons), id!(primary)])).area().rect(&cx);
                        for rect in [cancel, primary] {
                            assert!(
                                rect.size.x > 0.0 && rect.size.y >= 44.0 && rect.pos.x >= 0.0 && rect.pos.y >= 0.0
                                    && rect.pos.x + rect.size.x <= width + 0.5 && rect.pos.y + rect.size.y <= height + 0.5,
                                "footer clipped at {width}x{height}, cn={cn}, choosing={choosing}: {rect:?}"
                            );
                        }
                        // Side by side, sharing the width.
                        assert!((cancel.pos.y - primary.pos.y).abs() < 0.5 && cancel.pos.x < primary.pos.x, "{cancel:?} {primary:?}");
                        assert!((cancel.size.x - primary.size.x).abs() < 1.0, "{cancel:?} {primary:?}");
                    }
                    if choosing {
                        // The value box sits beside the slider, as on the new-pool sheet.
                        let short = GrantView { facts: Vec::new(), caution: String::new(), lead: String::new(), people: Vec::new(), ..long_view(cn, true) };
                        let inputs = GrantInputs { per_request: "200000".into(), tokens: "250000".into(), minimum: 100_000, maximum: Some(800_000), presets: [true, false, true], selected: [false, false, true] };
                        let (cx, panel) = draw_sheet(width, height, &short, Some(&inputs), id!(grant_confirmation));
                        let slider = panel.view.widget(&cx, ids!(grant_confirmation.sheet_scroll.quota.tokens.slider_wrap.amount_slider)).area().rect(&cx);
                        let input = panel.view.widget(&cx, ids!(grant_confirmation.sheet_scroll.quota.tokens.input)).area().rect(&cx);
                        assert!(slider.size.x >= 120.0 && slider.size.y >= 44.0, "{slider:?}");
                        assert!(input.size.x <= 100.0 && input.pos.x > slider.pos.x && (input.pos.y - slider.pos.y).abs() < 1.0, "{input:?} {slider:?}");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod diagnostic_tests {
    #[test]
    fn grant_diagnostics_only_accept_exact_static_codes() {
        for code in [
            "management_access_required",
            "management_monthly_regrant_required",
            "management_backing_changed",
            "management_capacity",
            "management_drain_outcome_unknown",
            "management_drain_unavailable",
            "management_enrollment_changed",
            "management_enrollment_history_full",
            "management_enrollment_history_required",
            "management_model_unqualified",
            "management_not_found",
            "management_outcome_unknown",
            "management_profile_mismatch",
            "management_resource_in_use",
            "management_revision_changed",
            "management_revision_conflict",
            "management_scope_changed",
            "management_state_unavailable",
            "management_storage_failed",
            "management_unavailable",
            "management_permission_required",
            "management_session_required",
            "management_command_conflict",
            "management_receipt_capacity",
            "native_grant_not_publishable",
            "native_grant_receipt_required",
            "native_owner_state_unavailable",
            "native_random_unavailable",
            "owner_authorization_required",
            "owner_grant_already_attempted",
            "owner_grant_binding_changed",
            "owner_grant_capacity_unavailable",
            "owner_grant_history_full",
            "owner_grant_history_unavailable",
            "owner_grant_intent_missing",
            "owner_grant_original_intent_required",
            "owner_grant_original_review_expired",
            "owner_grant_publication_confirmation_required",
            "owner_grant_publication_history_unavailable",
            "owner_grant_publication_review_changed",
            "owner_grant_publication_review_expired",
            "owner_grant_review_changed",
            "current_profile_required",
            "provider_account_changed",
            "provider_closing",
            "provider_directory_unavailable",
            "provider_identity_timeout",
            "provider_identity_unavailable",
            "provider_profile_mismatch",
            "local_codex_account_not_signed_in",
            "local_provider_not_signed_in",
            "contribution_not_admitted",
            "contribution_executor_changed",
            "contribution_resource_changed",
            "allocation_enrollment_changed",
            "invalid_grant_budget",
            "invalid_grant_expiry",
            "invalid_grant_manager",
            "management_grant_succession_required",
            "pool_grant_succession_unavailable",
            "owner_grant_predecessor_required",
            "owner_grant_succession_pending",
            "owner_grant_successor_exists",
            "owner_grant_renewal_window_closed",
            "owner_grant_below_floor",
            "owner_grant_manager_removed",
            "grant_predecessor_not_effective",
            "grant_successor_exists",
            "grant_successor_below_reserved",
            "grant_succession_window_closed",
            "invalid_grant_succession",
        ] {
            assert_eq!(super::grant_failure_code(code), code);
        }
        for untrusted in [
            "",
            "/private/profile/provider.json",
            "bearer secret",
            "owner_grant_history_unavailable: secret",
            "provider_identity_unavailable\ncredential=secret",
            "{\"error\":\"owner_authorization_required\",\"token\":\"secret\"}",
        ] {
            assert_eq!(
                super::grant_failure_code(untrusted),
                "owner_grant_unclassified_failure"
            );
        }
    }
}
