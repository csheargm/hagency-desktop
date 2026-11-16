//! "The Agent follows its owner into rooms": join rules and the saved setting.
use super::super::{Follow, RoomFacts, active_agents, follow_on_join, offer_text, prefs::{FILE_NAME, Prefs}};
use super::super::retry::{POWER_LEVELS_UNREADABLE, RETRY_DELAYS_MS, ROOM_NOT_LOADED, begin, bind_settles, delay_after, lookup_retries};
use serde_json::json;

fn group_room() -> RoomFacts {
    RoomFacts { can_invite: true, ..RoomFacts::default() }
}

/// Regression (Chris, fix-it): Betty accepted an invite and her Agent stayed behind.
#[test]
fn joining_a_group_room_brings_an_agent_whose_setting_is_on() {
    assert_eq!(follow_on_join(true, false, group_room()), Follow::Join);
}

#[test]
fn an_agent_with_the_setting_off_is_offered_not_added() {
    assert_eq!(follow_on_join(false, false, group_room()), Follow::Offer);
    assert_eq!(offer_text("betty-claude", "fix-it"), "Bring betty-claude into fix-it?");
}

#[test]
fn dms_spaces_rooms_it_is_in_and_handled_pairs_are_skipped() {
    let room = group_room();
    for facts in [RoomFacts { direct: true, ..room }, RoomFacts { space: true, ..room }, RoomFacts { agent_present: true, ..room }] {
        assert_eq!(follow_on_join(true, false, facts), Follow::Skip, "{facts:?}");
        assert_eq!(follow_on_join(false, false, facts), Follow::Skip, "{facts:?}");
    }
    assert_eq!(follow_on_join(true, true, room), Follow::Skip, "one add per Agent per room");
}

#[test]
fn without_invite_rights_nothing_is_tried_and_the_owner_is_told() {
    let room = RoomFacts { can_invite: false, ..group_room() };
    assert_eq!(follow_on_join(true, false, room), Follow::NoInviteRights);
    assert_eq!(follow_on_join(false, false, room), Follow::NoInviteRights);
}

#[test]
fn every_active_agent_with_an_identity_is_considered() {
    let listing = json!({"agents": [
        {"id": "agt_01a", "state": "active", "puppetMxid": "@a:s", "displayName": "a"},
        {"id": "agt_01b", "state": "retired", "puppetMxid": "@b:s"},
        {"id": "agt_01c", "state": "active", "puppetMxid": "@c:s", "displayName": "c"},
    ]});
    let ids: Vec<String> = active_agents(&listing).into_iter().map(|a| a.id).collect();
    assert_eq!(ids, ["agt_01c", "agt_01a"]);
}

fn temp_file(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("hagency-follow-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir); // a leftover from an earlier run; absent is fine
    dir.join(FILE_NAME)
}

#[test]
fn the_setting_defaults_on_including_existing_agents_and_persists() {
    let path = temp_file("persist");
    let mut prefs = Prefs::load(&path);
    assert!(prefs.joins_rooms("agt_existing"), "no entry (Agent made before the setting) means on");
    prefs.set_joins_rooms("agt_quiet", false);
    assert!(prefs.claim("!fix:s", "agt_quiet").unwrap());
    prefs.save(&path).unwrap();
    let back = Prefs::load(&path);
    assert!(!back.joins_rooms("agt_quiet") && back.joins_rooms("agt_existing"));
    assert!(back.is_handled("!fix:s", "agt_quiet") && !back.is_handled("!fix:s", "agt_existing"));
    let mut again = back;
    assert!(!again.claim("!fix:s", "agt_quiet").unwrap(), "a second claim is refused");
}

#[test]
fn an_unreadable_file_fails_closed_to_asking() {
    let path = temp_file("corrupt");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"{not json").unwrap();
    assert!(!Prefs::load(&path).joins_rooms("agt_any"), "corrupt means ask, never join silently");
    assert!(path.with_extension("json.corrupt").exists());
    assert!(!Prefs::load(&path).claim_catch_up("agt_any").unwrap(), "corrupt never catches up");
    assert!(!Prefs::load(&path).needs_catch_up("agt_any"));
}

#[test]
fn an_agent_catches_up_once_and_it_persists() {
    let path = temp_file("catch_up");
    let mut prefs = Prefs::load(&path);
    assert!(prefs.needs_catch_up("agt_new"));
    assert!(prefs.claim_catch_up("agt_new").unwrap());
    assert!(!prefs.claim_catch_up("agt_new").unwrap());
    assert!(!prefs.needs_catch_up("agt_new"));
    prefs.save(&path).unwrap();
    let mut back = Prefs::load(&path);
    assert!(!back.claim_catch_up("agt_new").unwrap(), "restart does not catch up again");
    assert!(back.claim_catch_up("agt_other").unwrap());
}

/// Review fix: a room not loaded yet when Joined arrives is retried, and the
/// retries are bounded.
#[test]
fn an_unloaded_room_is_retried_a_bounded_number_of_times() {
    assert!(lookup_retries(ROOM_NOT_LOADED) && lookup_retries(POWER_LEVELS_UNREADABLE));
    assert!(!lookup_retries("matrix_sign_in_required"));
    let waits: Vec<_> = (0..10).map_while(delay_after).collect();
    assert_eq!(waits.len(), RETRY_DELAYS_MS.len(), "attempts stop");
    assert!(waits.windows(2).all(|w| w[0] < w[1]), "backoff grows");
}

/// Review fix: a transient bind failure leaves the pair open; only success or
/// a definitive refusal records it as handled.
#[test]
fn only_success_or_a_definitive_refusal_settles_a_pair() {
    assert!(bind_settles(&Ok(true)) && bind_settles(&Ok(false)));
    assert!(bind_settles(&Err("agent_retired".into())));
    assert!(!bind_settles(&Err("network_unavailable".into())), "transient: retry later");
    let first = begin("!r:s agt_1").expect("free");
    assert!(begin("!r:s agt_1").is_none(), "one bind in flight per pair");
    drop(first); // also what an aborted bind task does
    assert!(begin("!r:s agt_1").is_some(), "the pair is free again");
}
