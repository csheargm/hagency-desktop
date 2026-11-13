//! App-wide awareness of resource admission requests that wait for the server
//! administrator.
//!
//! A resource owner's request to put a pool on the server waits up to seven
//! days for the administrator's decision. It used to show only inside the
//! Hagency mini app's administrator workspace, so an administrator who was
//! chatting never learned that one had arrived. This module reads the
//! authenticated management list on a slow cadence, announces each new request
//! once, and lets the Agent tab show that something waits.
//!
//! Nothing here grants authority: the decision is made in the mini app, where
//! the management server checks the deciding account again.
use crate::shared::popup_list::{PopupKind, enqueue_popup_notification};
use makepad_widgets::*;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Seconds between reads. A request waits for days, so this only bounds how
/// long the reminder trails a new request or a decision.
pub const POLL_SECONDS: u64 = 20;

#[derive(Default)]
struct State {
    identity: String,
    /// Whether this account decides admission on its server. Unknown until
    /// the server has answered once; an account that does not is not asked again.
    administrator: Option<bool>,
    /// Ids of the requests that wait for this account.
    waiting: Vec<String>,
    announced: HashSet<String>,
    asked: Option<Instant>,
}
static STATE: LazyLock<Mutex<State>> = LazyLock::new(Default::default);

/// Posted from the poll task with the raw management answer: the page of
/// waiting requests, or null for an account that is not the administrator.
#[derive(Debug)]
pub struct PendingAdmissionsUpdate {
    pub identity: String,
    pub result: Result<Value, String>,
}

/// Ask the management server which admission requests wait for this account.
/// Called on the app's poll timer; does nothing without a signed-in Hagency
/// identity, between reads, or for an account that is not the administrator.
pub fn poll() {
    let Ok(service) = crate::hagency::backend::Service::for_current() else { return };
    let identity = service.identity();
    if !due(&identity, Instant::now()) {
        return;
    }
    crate::sliding_sync::spawn_async_task(async move {
        let result = service.admission_requests().await;
        Cx::post_action(PendingAdmissionsUpdate { identity, result });
    });
}

/// Whether a read is due for `identity`, recording that it was asked.
fn due(identity: &str, now: Instant) -> bool {
    let Ok(mut state) = STATE.lock() else { return false };
    if state.identity != identity {
        *state = State { identity: identity.to_owned(), ..State::default() };
    }
    if state.administrator == Some(false) {
        return false;
    }
    if state.asked.is_some_and(|at| now.duration_since(at) < Duration::from_secs(POLL_SECONDS)) {
        return false;
    }
    state.asked = Some(now);
    true
}

/// Record a poll answer. Returns true when the set of waiting requests
/// changed, so the caller redraws. A request is announced once.
pub fn apply(cx: &mut Cx, update: &PendingAdmissionsUpdate) -> bool {
    let (changed, announce) = record(update);
    match announce.as_slice() {
        [] => {}
        [(label, requester)] => {
            let title = crate::i18n::tr("New resource admission request");
            let summary = format!("{label} · {requester}");
            enqueue_popup_notification(
                format!("{title}\n{summary}\n{}", crate::i18n::tr("Open Hagency's Server admin workspace to decide.")),
                PopupKind::Info,
                Some(20.0),
            );
            // A no-op on backends without system notifications; iOS shows it.
            crate::jit_compat::show_notification(cx, title, &summary);
        }
        several => {
            let title = crate::i18n::format(
                "{count} resource admission requests wait for your review",
                &[("count", several.len().to_string())],
            );
            let tail = crate::i18n::tr("Open Hagency's Server admin workspace to decide.");
            enqueue_popup_notification(format!("{title}\n{tail}"), PopupKind::Info, Some(20.0));
            crate::jit_compat::show_notification(cx, &title, tail);
        }
    }
    changed
}

/// The state change of `apply`, and the (pool, requester) of each request to announce.
fn record(update: &PendingAdmissionsUpdate) -> (bool, Vec<(String, String)>) {
    // A failed read keeps what was known: the reminder must not blink off and
    // announce everything again when the server is briefly unreachable.
    let Ok(value) = &update.result else { return (false, Vec::new()) };
    let Ok(mut state) = STATE.lock() else { return (false, Vec::new()) };
    // An answer for an account that is no longer the current one.
    if state.identity != update.identity {
        return (false, Vec::new());
    }
    let Some(rows) = value.get("items").and_then(Value::as_array) else {
        state.administrator = Some(false);
        let changed = !state.waiting.is_empty();
        state.waiting.clear();
        return (changed, Vec::new());
    };
    state.administrator = Some(true);
    let rows: Vec<&Value> = rows
        .iter()
        .filter(|row| row["kind"] == "resource_admission" && row["canDecide"] == true)
        .filter(|row| row["id"].as_str().is_some_and(|id| !id.is_empty()))
        .collect();
    let waiting: Vec<String> = rows.iter().map(|row| row["id"].as_str().unwrap_or_default().to_owned()).collect();
    let announce: Vec<(String, String)> = rows
        .iter()
        .filter(|row| !state.announced.contains(row["id"].as_str().unwrap_or_default()))
        .map(|row| {
            (
                row["resourceLabel"].as_str().unwrap_or_default().to_owned(),
                row["requesterMxid"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    state.announced.extend(waiting.iter().cloned());
    let changed = state.waiting != waiting;
    state.waiting = waiting;
    (changed, announce)
}

/// Admission requests that wait for this account, for the Agent tab.
pub fn total() -> u64 {
    STATE.lock().map_or(0, |s| s.waiting.len() as u64)
}

pub fn forget_identity() {
    if let Ok(mut state) = STATE.lock() {
        *state = State::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // One test owns the process-wide state: the steps run in order.
    fn row(id: &str, can: bool) -> Value {
        json!({"id":id,"kind":"resource_admission","contributionId":"con_1","resourceLabel":format!("Pool {id}"),
            "requesterMxid":"@owner:example.test","approverMxid":"@admin:example.test","state":"pending","canDecide":can})
    }
    fn update(identity: &str, result: Result<Value, String>) -> PendingAdmissionsUpdate {
        PendingAdmissionsUpdate { identity: identity.into(), result }
    }

    #[test]
    fn waiting_requests_are_counted_announced_once_and_only_for_the_administrator() {
        forget_identity();
        let start = Instant::now();
        // The first read is due at once, the next only after the interval.
        assert!(due("admin", start));
        assert!(!due("admin", start + Duration::from_secs(POLL_SECONDS - 1)));
        assert!(due("admin", start + Duration::from_secs(POLL_SECONDS)));

        // Two wait; one row this account cannot decide is not counted.
        let page = json!({"items":[row("a", true), row("b", true), row("c", false)],"nextAfter":null});
        let (changed, announce) = record(&update("admin", Ok(page.clone())));
        assert!(changed);
        assert_eq!(announce, vec![("Pool a".to_owned(), "@owner:example.test".to_owned()), ("Pool b".to_owned(), "@owner:example.test".to_owned())]);
        assert_eq!(total(), 2);

        // The same answer again changes nothing and announces nothing.
        assert_eq!(record(&update("admin", Ok(page))), (false, Vec::new()));

        // A failed read keeps the reminder.
        assert_eq!(record(&update("admin", Err("offline".into()))), (false, Vec::new()));
        assert_eq!(total(), 2);

        // One was decided, one is new: only the new one is announced.
        let page = json!({"items":[row("b", true), row("d", true)],"nextAfter":null});
        let (changed, announce) = record(&update("admin", Ok(page)));
        assert!(changed);
        assert_eq!(announce, vec![("Pool d".to_owned(), "@owner:example.test".to_owned())]);
        assert_eq!(total(), 2);

        // Nothing waits: the reminder goes.
        assert_eq!(record(&update("admin", Ok(json!({"items":[],"nextAfter":null})))), (true, Vec::new()));
        assert_eq!(total(), 0);

        // An answer for another account is dropped.
        assert_eq!(record(&update("someone-else", Ok(json!({"items":[row("x", true)]})))), (false, Vec::new()));
        assert_eq!(total(), 0);

        // An account that is not the administrator is asked once, then left alone.
        let later = start + Duration::from_secs(10 * POLL_SECONDS);
        assert!(due("member", later));
        assert_eq!(record(&update("member", Ok(Value::Null))), (false, Vec::new()));
        assert!(!due("member", later + Duration::from_secs(10 * POLL_SECONDS)));
        assert_eq!(total(), 0);

        // Signing in as the administrator again starts over: what waits is announced.
        assert!(due("admin", later));
        let (changed, announce) = record(&update("admin", Ok(json!({"items":[row("b", true)]}))));
        assert!(changed);
        assert_eq!(announce.len(), 1);
        forget_identity();
        assert_eq!(total(), 0);
    }
}
