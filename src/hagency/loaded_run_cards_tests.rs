use super::*;
use matrix_sdk::deserialized_responses::{AlgorithmInfo, VerificationState};
use matrix_sdk::ruma::{OwnedUserId, room_id};
use super::super::{pending_questions as pq, run_controls};

const ROOM: &str = "!a3cQXHR8GZ4RP5n71s:localhost:8450";
const SERVER: &str = "localhost:8450";
const BIRD_AGENT: &str = "@_hagency_agt_01m7k6ws4k2z06ysr6shrxeqzf:localhost:8452";

/// The live dev room's events (Nov 19 01:36-01:39, read from palpo_fa):
/// bird-codex's card + final edit, then anna-claude's card + final edit.
fn fixture() -> Vec<Value> {
    serde_json::from_str(include_str!("loaded_run_cards_fixture.json")).unwrap()
}

/// (card, final edit, the request it answers) for each run in the fixture.
fn runs() -> Vec<(Value, Value, String)> {
    fixture().chunks(2).map(|pair| {
        let request = pair[0]["content"]["im.hagency.agent_run"]["eventId"].as_str().unwrap().to_owned();
        (pair[0].clone(), pair[1].clone(), request)
    }).collect()
}

/// Megolm info as the SDK reports a decrypted event from `sender`.
fn megolm(sender: &str) -> EncryptionInfo {
    EncryptionInfo {
        sender: OwnedUserId::try_from(sender).unwrap(),
        sender_device: Some("AGENT_DEVICE".into()),
        forwarder: None,
        algorithm_info: AlgorithmInfo::MegolmV1AesSha2 {
            curve25519_key: "curve25519".into(),
            sender_claimed_keys: Default::default(),
            session_id: Some("session".into()),
        },
        verification_state: VerificationState::Verified,
    }
}

/// What `ingest` does for `card`, through the sync admission rules, with
/// `fetched` standing in for the event cache's copy of the edit.
fn admit(generation: u64, card: &Loaded, fetched: Option<Admit>, plaintext: bool) {
    let mut events = vec![card.original.clone()];
    events.extend(edit_to_admit(card, fetched));
    for (value, info) in events {
        // bird-codex is on the trusted peer (beta); anna-claude on the room's server.
        let remote = !value["sender"].as_str().unwrap().ends_with(SERVER);
        pq::test_accept(generation, ROOM, SERVER, &value, info.as_ref(), plaintext, remote);
    }
    settle(generation, ROOM, card);
}

/// Regression (Nov 19 02:06): after a restart, replies posted earlier were
/// missing from the dev room. Their cards came from the local cache, never
/// through the sync handler, so no card was drawn on the request while the
/// card's own event stayed hidden. Loaded cards are admitted with their edit.
#[test]
fn replies_loaded_from_the_cache_are_drawn_on_their_request_after_a_restart() {
    let _guard = pq::test_serial();
    let generation = pq::test_login("@csheargm:localhost:8450");
    let room = room_id!("!a3cQXHR8GZ4RP5n71s:localhost:8450");
    let runs = runs();
    assert_eq!(runs.len(), 2);
    for (card, _, request) in &runs {
        assert!(run_controls::for_event(room, request, true).is_empty(), "fresh start: nothing admitted");
        assert!(!run_controls::is_card_event(room, card["event_id"].as_str().unwrap()));
    }
    let loaded: Vec<_> = runs.iter().map(|(card, edit, _)| cards_of(card.clone(), Some(edit.clone()), None).unwrap()).collect();
    for card in claim(generation, loaded) {
        admit(generation, &card, None, true);
    }
    for (card, edit, request) in &runs {
        let shown = run_controls::for_event(room, request, true);
        assert_eq!(shown.len(), 1, "one card on request {request}");
        let answer = edit["content"]["m.new_content"]["im.hagency.agent_run"]["answer"].as_str().unwrap();
        assert_eq!(shown[0].card.answer, answer, "the final edit's answer is drawn");
        assert!(!shown[0].card.state.active(), "completed, not stuck on Working");
        assert!(run_controls::is_card_event(room, card["event_id"].as_str().unwrap()));
    }
    assert!(runs[0].1["content"]["m.new_content"]["body"].as_str().unwrap().starts_with("Checked [PR #135]"));
    assert!(runs[1].1["content"]["m.new_content"]["body"].as_str().unwrap().starts_with("The docs index was proposed"));
}

/// GPT review P2: in an encrypted owner DM the cached edit came without
/// encryption info and was dropped, so a finished run stayed "working".
/// The edit is admitted with the event cache's own info, under the live
/// rules: decrypted from the card's sender, verified enough.
#[test]
fn an_edit_loaded_in_an_encrypted_room_is_admitted_with_its_own_encryption_info() {
    let _guard = pq::test_serial();
    let room = room_id!("!a3cQXHR8GZ4RP5n71s:localhost:8450");
    let (card, edit, request) = runs().remove(1);
    let agent = card["sender"].as_str().unwrap().to_owned();
    let fresh = |generation| claim(generation, vec![cards_of(card.clone(), Some(edit.clone()), Some(megolm(&agent))).unwrap()]).remove(0);
    let state = || run_controls::for_event(room, &request, true).remove(0).card.state;

    let generation = pq::test_login("@csheargm:localhost:8450");
    admit(generation, &fresh(generation), None, false);
    assert!(state().active(), "before: no info for the edit, so it was dropped");

    let generation = pq::test_login("@csheargm:localhost:8450");
    admit(generation, &fresh(generation), Some((edit.clone(), Some(megolm(BIRD_AGENT)))), false);
    assert!(state().active(), "an edit decrypted from another sender is refused");

    let generation = pq::test_login("@csheargm:localhost:8450");
    admit(generation, &fresh(generation), Some((edit.clone(), Some(megolm(&agent)))), false);
    let shown = run_controls::for_event(room, &request, true).remove(0);
    assert!(!shown.card.state.active(), "finished");
    assert_eq!(shown.card.answer, edit["content"]["m.new_content"]["im.hagency.agent_run"]["answer"].as_str().unwrap());
    let other = Some((serde_json::json!({"event_id": "$other"}), Some(megolm(&agent))));
    let loaded = cards_of(card.clone(), Some(edit.clone()), None).unwrap();
    assert_eq!(edit_to_admit(&loaded, other).unwrap().1, None, "another event is never taken for the edit");
}

#[test]
fn a_card_is_handed_over_again_until_it_is_stored_then_never() {
    let _guard = pq::test_serial();
    let generation = pq::test_login("@csheargm:localhost:8450");
    let (card, edit, _) = runs().remove(1);
    let only = || vec![cards_of(card.clone(), None, None).unwrap()];
    for _ in 0..MAX_TRIES {
        let claimed = claim(generation, only());
        assert_eq!(claimed.len(), 1, "not stored yet: handed over again");
        assert!(claim(generation, only()).is_empty(), "not twice while admitting");
        settle(generation, ROOM, &claimed[0]);
    }
    assert!(claim(generation, only()).is_empty(), "gives up after MAX_TRIES");
    let generation = pq::test_login("@csheargm:localhost:8450");
    let claimed = claim(generation, vec![cards_of(card.clone(), Some(edit.clone()), None).unwrap()]);
    admit(generation, &claimed[0], None, true);
    assert!(claim(generation, vec![cards_of(card.clone(), Some(edit.clone()), None).unwrap()]).is_empty(), "stored: never again");
}

/// A running card like the fixture's, as its own run and request.
fn running(n: usize) -> Value {
    let mut v = runs().remove(1).0;
    v["event_id"] = format!("$live{n}").into();
    let run = &mut v["content"]["im.hagency.agent_run"];
    run["id"] = format!("run_live{n}").into();
    run["eventId"] = format!("$ask{n}").into();
    run["threadRoot"] = format!("$ask{n}").into();
    v
}

/// GPT review P2: history must never evict an active run (or one with
/// local input) from a full store, and is skipped instead.
#[test]
fn history_never_evicts_an_active_run_from_a_full_store() {
    let _guard = pq::test_serial();
    let generation = pq::test_login("@csheargm:localhost:8450");
    let room = room_id!("!a3cQXHR8GZ4RP5n71s:localhost:8450");
    for n in 0..256 {
        assert!(pq::test_accept(generation, ROOM, SERVER, &running(n), None, true, false));
    }
    let (card, edit, request) = runs().remove(1);
    let claimed = claim(generation, vec![cards_of(card.clone(), Some(edit.clone()), None).unwrap()]);
    run_controls::as_history_sync(|| admit(generation, &claimed[0], None, true));
    assert!(run_controls::for_event(room, &request, true).is_empty(), "skipped, not stored");
    for n in 0..256 {
        assert!(run_controls::is_card_event(room, &format!("$live{n}")), "live run {n} kept");
    }
    assert_eq!(claim(generation, vec![cards_of(card.clone(), Some(edit.clone()), None).unwrap()]).len(), 1,
        "not marked handed over: tried again");
    // A settled run is what makes room for it.
    let mut done = runs().remove(1).1;
    done["event_id"] = "$live7done".into();
    done["content"]["m.relates_to"]["event_id"] = "$live7".into();
    let run = &mut done["content"]["m.new_content"]["im.hagency.agent_run"];
    run["id"] = "run_live7".into();
    run["eventId"] = "$ask7".into();
    run["threadRoot"] = "$ask7".into();
    assert!(pq::test_accept(generation, ROOM, SERVER, &done, None, true, false), "run 7 finished");
    let claimed = cards_of(card.clone(), Some(edit.clone()), None).unwrap();
    run_controls::as_history_sync(|| admit(generation, &claimed, None, true));
    assert_eq!(run_controls::for_event(room, &request, true).len(), 1, "admitted in the settled run's place");
    assert!(!run_controls::is_card_event(room, "$live7"), "the settled run made room");
    assert!((0..256).filter(|n| *n != 7).all(|n| run_controls::is_card_event(room, &format!("$live{n}"))), "every active run kept");
}

#[test]
fn only_agent_run_status_events_are_cards() {
    let mut message = fixture().remove(0);
    message["content"]["msgtype"] = "m.text".into();
    assert!(cards_of(message, None, None).is_none());
    let (card, edit, _) = runs().remove(0);
    let loaded = cards_of(card, Some(edit), None).unwrap();
    assert_eq!(loaded.original.0["content"]["m.relates_to"]["rel_type"], Value::Null, "the card first");
    assert_eq!(edit_to_admit(&loaded, None).unwrap().0["content"]["m.relates_to"]["rel_type"], "m.replace");
    assert_eq!(loaded.revision(), 5, "the latest edit's revision");
}
