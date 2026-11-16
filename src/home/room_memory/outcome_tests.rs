//! Cross-client sync of proposal cards (live 2026-11-15: Anna accepted, Betty's
//! card stayed "Waiting…", Element saw nothing): the reload trigger, the
//! proposal→entry link, the visible outcome reply, and the card's auto offer.
use super::auto::{Offer, offer};
use super::model::{CardState, accept_plan, card_state, parse_proposal};
use super::outcome::*;
use super::watch::{OUTCOME_RELOAD_GAP, OutcomeDebounce, Stale, stale_by};
use hagency::room_memory::{self, Storage};
use serde_json::json;

#[test]
fn another_members_accept_reaches_this_client_as_a_reload() {
    let state = json!({"type": "io.hagency.memory", "state_key": "mem_1", "content": {"text": "x", "rev": 1}});
    assert_eq!(stale_by(&state), Stale { memory: true, skills: false, from_notice: false });
    assert_eq!(stale_by(&json!({"type": "io.hagency.skill", "state_key": "deploy"})), Stale { memory: false, skills: true, from_notice: false });
    let notice = json!({"type": "m.room.message", "content": {"msgtype": "m.notice", "body": "✅", OUTCOME_KEY: {"proposal": "mem_1"}}});
    assert_eq!(stale_by(&notice), Stale { memory: true, skills: true, from_notice: true });
    assert_eq!(stale_by(&json!({"type": "m.room.message", "content": {"body": "hi"}})), Stale::default());
}

#[test]
fn the_accepted_entry_is_keyed_by_the_proposal_id_so_every_card_flips_to_added() {
    let proposal = parse_proposal(&room_memory::proposal_content("mem_1", "Demo Monday", "asked", "$e", "@a:s")).unwrap();
    let levels = room_memory::PowerLevels { users: [("@anna:alpha".to_string(), 100)].into(), state_default: Some(50), ..Default::default() };
    let write = accept_plan(&proposal, None, "@anna:alpha", &levels, Storage::State).unwrap();
    let event = json!({"state_key": "mem_1", "content": write.content});
    let entries = room_memory::reduce(&[event]);
    let in_memory = entries.iter().any(|e| e.id == proposal.entry.id);
    assert_eq!(card_state(in_memory, false, false, None), CardState::Added, "Betty's card, once reloaded");
}

fn announce(thread: Option<&str>, kind: Kind) -> Announce {
    Announce { origin: Origin { event_id: "$prop".into(), thread_root: thread.map(String::from) }, kind, proposal: "mem_1".into() }
}

#[test]
fn the_outcome_is_a_visible_notice_in_the_proposals_thread() {
    let content = outcome_content(&announce(Some("$root"), Kind::Memory), "Anna");
    assert_eq!(content["msgtype"], "m.notice");
    assert_eq!(content["body"], outcome_text(Kind::Memory, "Anna"));
    assert!(content["body"].as_str().unwrap().contains("Anna"));
    assert_eq!(content["m.relates_to"]["rel_type"], "m.thread");
    assert_eq!(content["m.relates_to"]["event_id"], "$root");
    assert_eq!(content["m.relates_to"]["m.in_reply_to"]["event_id"], "$prop");
    assert_eq!(content[OUTCOME_KEY]["kind"], "memory");
    let plain = outcome_content(&announce(None, Kind::Skill), "Anna");
    assert!(plain["m.relates_to"].get("rel_type").is_none());
    assert_eq!(plain["m.relates_to"]["m.in_reply_to"]["event_id"], "$prop");
    assert_eq!(plain[OUTCOME_KEY]["kind"], "skill");
}

#[test]
fn outcome_text_is_translated() {
    assert_eq!(outcome_text(Kind::Memory, "Anna"), "✅ Added to room memory by Anna");
    assert_eq!(outcome_text(Kind::Skill, "Anna"), "✅ Saved as a team skill by Anna");
    let zh: serde_json::Value = serde_json::from_str(include_str!("../../../resources/i18n/zh-CN.json")).unwrap();
    for key in ["✅ Added to room memory by {name}", "✅ Saved as a team skill by {name}", "Your decision (room moderators):", "Always accept from my Agents", "Auto-accept is on"] {
        assert!(zh[key].as_str().is_some_and(|t| !t.is_empty() && t != key), "{key}");
    }
}

#[test]
fn origin_keeps_the_thread_and_rejects_a_missing_event_id() {
    let threaded = json!({"event_id": "$p", "content": {"m.relates_to": {"rel_type": "m.thread", "event_id": "$r"}}});
    assert_eq!(origin_from(&threaded), Some(Origin { event_id: "$p".into(), thread_root: Some("$r".into()) }));
    assert_eq!(origin_from(&json!({"event_id": "$p", "content": {}})).unwrap().thread_root, None);
    assert_eq!(origin_from(&json!({"content": {}})), None, "a local echo has no event to reply to yet");
}

#[test]
fn only_the_owner_who_can_write_is_offered_auto_accept() {
    assert_eq!(offer(true, Some(true), false, true), Offer::TurnOn);
    assert_eq!(offer(true, Some(true), true, false), Offer::IsOn);
    assert_eq!(offer(true, Some(true), false, false), Offer::Hidden, "nothing to offer on a decided card");
    assert_eq!(offer(false, Some(true), false, true), Offer::Hidden, "cannot write memory");
    assert_eq!(offer(true, Some(false), false, true), Offer::Hidden, "someone else's Agent");
    assert_eq!(offer(true, None, false, true), Offer::Hidden, "ownership unknown");
}

/// Review fix: any member can post the outcome marker, so notices reload at
/// most once per room per gap; other rooms and later notices still reload.
#[test]
fn forged_outcome_notices_are_rate_limited_per_room() {
    let mut debounce = OutcomeDebounce::default();
    let t0 = std::time::Instant::now();
    assert!(debounce.allow("!a:s", t0));
    for i in 1..50 { assert!(!debounce.allow("!a:s", t0 + std::time::Duration::from_millis(i * 10)), "spam {i} suppressed"); }
    assert!(debounce.allow("!b:s", t0), "another room is independent");
    assert!(debounce.allow("!a:s", t0 + OUTCOME_RELOAD_GAP), "a notice after the gap reloads again");
}
