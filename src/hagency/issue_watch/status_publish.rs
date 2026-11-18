//! Publishes one watched repository's tracker states into its report room
//! (ADR 0023) as this owner's own `io.hagency.issue_status` state event
//! (state key = the owner's Matrix ID, which the homeserver lets no one else
//! write). Only room-shareable fields leave the ledger; an end-to-end
//! encrypted room gets nothing, since room state is plaintext to the server.
use ::hagency::issue_watch::issue_track::Track;
use ::hagency::repo_status::{ISSUE_STATUS_STATE_TYPE, Published, REFRESH_SECS, TrackState, publish_content};
use matrix_sdk::ruma::{OwnedRoomId, api::client::state::get_state_event_for_key::v3 as get_state, events::StateEventType};
use serde_json::Value;
use std::collections::HashMap;

/// The ledger's records of one repository as published entries.
pub(crate) fn entries(agent_mxid: &str, repo: &str, tracks: &[Track], titles: &HashMap<i64, String>, holds: &[(i64, String)], now: i64) -> Result<Vec<Published>, String> {
    let mut out = Vec::new();
    for track in tracks.iter().filter(|t| t.repo.eq_ignore_ascii_case(repo)) {
        // The ledger and the room share the wire names; one that does not parse is a bug, not data.
        let state: TrackState = serde_json::to_value(track.state).and_then(serde_json::from_value).map_err(|e| format!("Unpublishable issue state: {e}"))?;
        out.push(Published {
            agent: agent_mxid.to_owned(), repo: repo.to_owned(), number: track.number, title: titles.get(&track.number).cloned().unwrap_or_default(), state,
            hold: holds.iter().find(|(n, _)| *n == track.number).map(|(_, h)| h.clone()), attempts: track.attempts, pr: track.pr, updated_at: now,
        });
    }
    Ok(out)
}

/// Keeps titles and `updatedAt` of issues whose published state did not change,
/// so a check that changes nothing writes nothing; a not-finished issue's
/// time is refreshed every [`REFRESH_SECS`] so readers can tell it from an
/// abandoned one (they drop it after `STALE_SECS`).
pub(crate) fn carry_over(current: &Value, fresh: &mut [Published]) {
    let before: Vec<Published> = current["issues"].as_array().into_iter().flatten().filter_map(|i| serde_json::from_value(i.clone()).ok()).collect();
    for issue in fresh.iter_mut() {
        let Some(old) = before.iter().find(|o| o.agent == issue.agent && o.repo.eq_ignore_ascii_case(&issue.repo) && o.number == issue.number) else { continue };
        if issue.title.is_empty() {
            issue.title = old.title.clone();
        }
        let due = !issue.state.is_terminal() && issue.updated_at.saturating_sub(old.updated_at) >= REFRESH_SECS;
        if (old.state, old.attempts, old.pr, &old.hold) == (issue.state, issue.attempts, issue.pr, &issue.hold) && !due {
            issue.updated_at = old.updated_at;
        }
    }
}

/// Serializes read-modify-write of this owner's one event per room.
static WRITING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What was last published per (room, Agent, repository), times left out,
/// and when: an unchanged check needs no server read until a refresh is due.
type Last = HashMap<(String, String, String), (Vec<Published>, i64)>;
static LAST: std::sync::LazyLock<std::sync::Mutex<Last>> = std::sync::LazyLock::new(Default::default);

fn timeless(fresh: &[Published]) -> Vec<Published> {
    fresh.iter().map(|p| Published { updated_at: 0, ..p.clone() }).collect()
}

/// Publish `fresh` (one Agent's issues on one repository) into `room`. A
/// watch that moved to another room has its issues withdrawn from the room
/// this process last published them to (GPT integration review); readers
/// also drop not-finished issues nobody refreshed.
pub(crate) async fn publish(room_id: &OwnedRoomId, agent_mxid: &str, repo: &str, fresh: Vec<Published>, now: i64) -> Result<(), String> {
    // The digest's "needs you" is this follow's holds, whether or not the room takes them.
    super::digest_gate::note(agent_mxid, repo, &fresh);
    let key = (room_id.to_string(), agent_mxid.to_owned(), repo.to_ascii_lowercase());
    let (unchanged, moved) = {
        // A poisoned lock still holds a whole map: inserts are single operations.
        let last = LAST.lock().unwrap_or_else(|p| p.into_inner());
        let unchanged = last.get(&key).is_some_and(|(sent, at)| *sent == timeless(&fresh) && now.saturating_sub(*at) < REFRESH_SECS);
        let moved: Vec<String> = last.keys().filter(|(room, agent, r)| *agent == key.1 && *r == key.2 && *room != key.0).map(|(room, _, _)| room.clone()).collect();
        (unchanged, moved)
    };
    for old in moved {
        let Ok(old_room) = OwnedRoomId::try_from(old.as_str()) else { continue };
        write(&old_room, agent_mxid, repo, Vec::new(), now).await?;
        LAST.lock().unwrap_or_else(|p| p.into_inner()).remove(&(old, key.1.clone(), key.2.clone()));
    }
    if unchanged {
        return Ok(());
    }
    let written = timeless(&fresh);
    write(room_id, agent_mxid, repo, fresh, now).await?;
    LAST.lock().unwrap_or_else(|p| p.into_inner()).insert(key, (written, now));
    Ok(())
}

async fn write(room_id: &OwnedRoomId, agent_mxid: &str, repo: &str, mut fresh: Vec<Published>, now: i64) -> Result<(), String> {
    let _writing = WRITING.lock().await;
    let client = crate::sliding_sync::get_client().ok_or("Signed out")?;
    let me = client.user_id().ok_or("Signed out")?.to_owned();
    let room = client.get_room(room_id).ok_or_else(|| format!("This desktop has not joined room {room_id}"))?;
    let security = room.latest_encryption_state().await.map_err(|e| format!("Could not read room {room_id}: {e}"))?;
    if security.is_unknown() {
        return Err(format!("Could not read room {room_id}'s encryption"));
    }
    if security.is_encrypted() {
        // Deliberately nothing: plaintext state would show titles to the homeserver (ADR 0023).
        return Ok(());
    }
    let request = get_state::Request::new(room_id.clone(), StateEventType::from(ISSUE_STATUS_STATE_TYPE), me.to_string());
    let current: Value = match client.send(request).await {
        Ok(reply) => serde_json::from_str(reply.event_or_content.get()).map_err(|e| format!("Unreadable published status: {e}"))?,
        Err(error) if error.client_api_error_kind() == Some(&matrix_sdk::ruma::api::error::ErrorKind::NotFound) => Value::Null,
        Err(error) => return Err(format!("Could not read the published status: {error}")),
    };
    carry_over(&current, &mut fresh);
    let Some(content) = publish_content(&current, agent_mxid, repo, &fresh, now) else { return Ok(()) };
    let Err(error) = room.send_state_event_raw(ISSUE_STATUS_STATE_TYPE, me.as_str(), content).await else { return Ok(()) };
    let forbidden = error.client_api_error_kind() == Some(&matrix_sdk::ruma::api::error::ErrorKind::Forbidden);
    let refusal = match room.power_levels().await {
        Ok(l) if forbidden && !l.user_can_send_state(&me, StateEventType::from(ISSUE_STATUS_STATE_TYPE)) => {
            Some(if l.events.contains_key(&ISSUE_STATUS_STATE_TYPE.into()) { Refusal::Restricted } else { Refusal::NotYetAllowed })
        }
        _ => None,
    };
    Err(share_failure(refusal, error.to_string()))
}

/// Why the room refused for want of power: that is the room's setting, so
/// the watch panel says who can change it instead of the server's raw 403.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Refusal {
    /// No entry for the event: an admin linking a repository adds it.
    NotYetAllowed,
    /// An admin set the event's level on purpose; linking keeps it (GPT review).
    Restricted,
}

pub(crate) const NEEDS_ADMIN: &str = "Issue status can't be shared in this room yet — a room admin needs to allow it (linking a repository does this).";
pub(crate) const RESTRICTED: &str = "Issue status can't be shared in this room — a room admin restricted it in the room's permissions.";

/// The watch panel's line for a failed status write.
fn share_failure(refusal: Option<Refusal>, raw: String) -> String {
    match refusal {
        Some(Refusal::NotYetAllowed) => crate::i18n::tr(NEEDS_ADMIN).to_owned(),
        Some(Refusal::Restricted) => crate::i18n::tr(RESTRICTED).to_owned(),
        None => crate::i18n::format("Issue status not shared with the room: {error}", &[("error", raw)]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::hagency::issue_watch::issue_track::State;

    fn track(number: i64, state: State) -> Track {
        Track { repo: "o/r".into(), number, state, attempts: 1, handed_at: 5, pr: Some(9), notified: false, free_retries: 0, changed_at: 0 }
    }

    #[test]
    fn a_refusal_for_power_reads_as_a_plain_admin_step_in_both_languages() {
        let raw = "M_FORBIDDEN: sender does not have enough power to send event".to_owned();
        assert_eq!(share_failure(Some(Refusal::NotYetAllowed), raw.clone()), crate::i18n::tr(NEEDS_ADMIN));
        assert_eq!(share_failure(Some(Refusal::Restricted), raw), crate::i18n::tr(RESTRICTED), "linking would not help there");
        assert_ne!(crate::i18n::text_for(crate::i18n::Language::Chinese, RESTRICTED), RESTRICTED);
        assert!(!crate::i18n::tr(NEEDS_ADMIN).contains("M_FORBIDDEN"));
        let zh = crate::i18n::text_for(crate::i18n::Language::Chinese, NEEDS_ADMIN);
        assert_ne!(zh, NEEDS_ADMIN, "the Chinese catalog translates it");
        assert!(share_failure(None, "timed out".into()).ends_with("timed out"), "other failures keep their reason");
    }

    #[test]
    fn every_ledger_state_publishes_and_only_this_repository() {
        let all = [State::HandedOff, State::PrOpened, State::Reviewed, State::Merged, State::Closed, State::Failed, State::NeedsHuman];
        let mut tracks: Vec<Track> = all.iter().enumerate().map(|(i, s)| track(i as i64 + 1, *s)).collect();
        tracks.push(Track { repo: "o/other".into(), ..track(99, State::HandedOff) });
        let titles = HashMap::from([(1, "First".to_owned())]);
        let got = entries("@_hagency_agt_b:x", "o/r", &tracks, &titles, &[(3, "green CI".into())], 100).unwrap();
        assert_eq!(got.len(), all.len());
        assert_eq!(got[0].title, "First");
        assert_eq!(got[2].hold.as_deref(), Some("green CI"));
        assert!(got.iter().all(|p| p.updated_at == 100 && p.pr == Some(9)));
    }

    #[test]
    fn an_unchanged_issue_keeps_its_title_and_time_so_nothing_is_rewritten() {
        let first = entries("@_hagency_agt_b:x", "o/r", &[track(1, State::Reviewed)], &HashMap::from([(1, "T".to_owned())]), &[], 100).unwrap();
        let current = publish_content(&Value::Null, "@_hagency_agt_b:x", "o/r", &first, 100).unwrap();
        // Next check: the issue left the open list (no title) and nothing changed.
        let mut again = entries("@_hagency_agt_b:x", "o/r", &[track(1, State::Reviewed)], &HashMap::new(), &[], 200).unwrap();
        carry_over(&current, &mut again);
        assert_eq!(publish_content(&current, "@_hagency_agt_b:x", "o/r", &again, 200), None);
        // A transition is written with its new time.
        let mut moved = entries("@_hagency_agt_b:x", "o/r", &[track(1, State::Merged)], &HashMap::new(), &[], 300).unwrap();
        carry_over(&current, &mut moved);
        let next = publish_content(&current, "@_hagency_agt_b:x", "o/r", &moved, 300).unwrap();
        assert_eq!((next["issues"][0]["state"].as_str(), next["issues"][0]["updatedAt"].as_i64(), next["issues"][0]["title"].as_str()), (Some("merged"), Some(300), Some("T")));
    }

    /// A long wait in one state is refreshed, so readers never take it for an abandoned watch.
    #[test]
    fn an_unchanged_open_issue_is_refreshed_before_readers_call_it_stale() {
        let first = entries("@_hagency_agt_b:x", "o/r", &[track(1, State::Reviewed)], &HashMap::new(), &[], 100).unwrap();
        let current = publish_content(&Value::Null, "@_hagency_agt_b:x", "o/r", &first, 100).unwrap();
        let later = 100 + REFRESH_SECS;
        let mut again = entries("@_hagency_agt_b:x", "o/r", &[track(1, State::Reviewed)], &HashMap::new(), &[], later).unwrap();
        carry_over(&current, &mut again);
        assert_eq!(again[0].updated_at, later);
        assert!(REFRESH_SECS < ::hagency::repo_status::STALE_SECS);
        let mut done = entries("@_hagency_agt_b:x", "o/r", &[track(1, State::Merged)], &HashMap::new(), &[], 100).unwrap();
        let merged = publish_content(&Value::Null, "@_hagency_agt_b:x", "o/r", &done, 100).unwrap();
        done[0].updated_at = later;
        carry_over(&merged, &mut done);
        assert_eq!(done[0].updated_at, 100, "a finished issue keeps its time and ages out");
    }
}
