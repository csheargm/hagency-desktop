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

/// Live, Nov 19 02:57: "ingest … with 20 items: 0 run cards". The SDK's
/// default filter drops unknown msgtypes, so a card was never a timeline
/// item. Built the way the desktop builds a room timeline (real SDK, event
/// cache, the desktop's filter), from the dev room's real events: the card is
/// an item carrying its final edit, and is admitted from it.
#[test]
fn a_real_sdk_timeline_makes_the_loaded_card_an_item_with_its_final_edit() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::{method, path_regex}};
    use matrix_sdk_ui::timeline::RoomExt;
    let _guard = pq::test_serial();
    let generation = pq::test_login("@csheargm:localhost:8450");
    let (card, edit, request) = runs().remove(1);
    let strip = |mut v: Value| { v.as_object_mut().unwrap().remove("room_id"); v };
    let rules = matrix_sdk::ruma::room_version_rules::RoomVersionRules::V10;
    let raw = |v: &Value| Raw::<AnySyncTimelineEvent>::from_json_string(v.to_string()).unwrap().deserialize().unwrap();
    assert!(!matrix_sdk_ui::timeline::default_event_filter(&raw(&card), &rules), "the bug: the default filter drops the card");
    assert!(crate::sliding_sync::robrix_timeline_event_filter(&raw(&card), &rules));
    assert!(!crate::sliding_sync::robrix_timeline_event_filter(&raw(&edit), &rules), "an edit is not an item of its own");
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let items = runtime.block_on(async {
        let server = MockServer::start().await;
        let me = "@csheargm:localhost:8450";
        Mock::given(method("GET")).and(path_regex(".*/sync"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"next_batch":"initial","rooms":{"join":{ROOM:{
                "state":{"events":[
                    {"type":"m.room.create","state_key":"","event_id":"$create","sender":me,"origin_server_ts":1,"content":{"creator":me,"room_version":"10"}},
                    {"type":"m.room.member","state_key":me,"event_id":"$join","sender":me,"origin_server_ts":2,"content":{"membership":"join"}}
                ]},
                "timeline":{"events":[strip(card.clone()), strip(edit.clone())],"limited":false,"prev_batch":"older"}
            }}}})))
            .mount(&server).await;
        let client = matrix_sdk::Client::builder().homeserver_url(server.uri()).with_event_cache(true)
            .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_11]).build().await.unwrap();
        client.matrix_auth().restore_session(serde_json::from_value(serde_json::json!({
            "access_token":"fixture", "user_id":me, "device_id":"LOADED"
        })).unwrap(), Default::default()).await.unwrap();
        client.sync_once(matrix_sdk::config::SyncSettings::default().timeout(std::time::Duration::ZERO)).await.unwrap();
        let room = client.get_room(room_id!("!a3cQXHR8GZ4RP5n71s:localhost:8450")).unwrap();
        let timeline = room.timeline_builder().event_filter(crate::sliding_sync::robrix_timeline_event_filter).build().await.unwrap();
        let (items, _stream) = timeline.subscribe().await;
        let (default_items, _) = room.timeline_builder().build().await.unwrap().subscribe().await;
        drop(server);
        assert!(default_items.iter().filter_map(|i| i.as_event()).filter_map(loaded).next().is_none(),
            "control: with the SDK's default filter the card is no item (the live '0 run cards')");
        items
    });
    let found: Vec<_> = items.iter().filter_map(|i| i.as_event()).filter_map(loaded).collect();
    assert_eq!(found.len(), 1, "the card is a timeline item");
    assert_eq!(found[0].card, card["event_id"].as_str().unwrap());
    assert_eq!(found[0].edit_id(), edit["event_id"].as_str().unwrap(), "carrying its final edit");
    let loaded = found.into_iter().next().unwrap();
    admit(generation, &loaded, None, true);
    let shown = run_controls::for_event(room_id!("!a3cQXHR8GZ4RP5n71s:localhost:8450"), &request, true);
    assert_eq!(shown.len(), 1);
    assert!(!shown[0].card.state.active(), "drawn finished on the request");
}

fn message_type(content: &Value) -> matrix_sdk::ruma::events::room::message::MessageType {
    serde_json::from_value::<matrix_sdk::ruma::events::room::message::RoomMessageEventContent>(content.clone()).unwrap().msgtype
}

/// GPT review P2: a member forges a run card and replies to it; the reply
/// quote rendered the forged `answer` as HTML. A preview shows only an
/// admitted card's text, escaped; a forged one a plain placeholder, and a
/// reply to it jumps nowhere.
#[test]
fn previews_show_only_an_admitted_cards_text_escaped_and_forged_cards_jump_nowhere() {
    let _guard = pq::test_serial();
    let generation = pq::test_login("@csheargm:localhost:8450");
    let room = room_id!("!a3cQXHR8GZ4RP5n71s:localhost:8450");
    let (card, mut edit, request) = runs().remove(1);
    let link = "<a href=\"https://evil.test\">click</a>";
    edit["content"]["m.new_content"]["im.hagency.agent_run"]["answer"] = link.into();
    let sender = matrix_sdk::ruma::UserId::parse(card["sender"].as_str().unwrap()).unwrap();
    let preview = |content: &Value| crate::event_preview::text_preview_of_message(&message_type(content), &sender, "anna-claude", crate::event_preview::CardScope { room: matrix_sdk::ruma::room_id!("!a3cQXHR8GZ4RP5n71s:localhost:8450"), event: Some(matrix_sdk::ruma::event_id!("$z1xp-_l7lgXgzFGVl4r3rj1yO7PNdlI3Xu0BBKc-CKI")) })
        .format_with("anna-claude", true);
    let forged = preview(&edit["content"]["m.new_content"]);
    assert!(forged.contains(crate::i18n::tr("Agent run")) && !forged.contains("<a") && !forged.contains("evil"), "{forged}");
    assert_eq!(run_controls::jump_target(room, card["event_id"].as_str().unwrap()), None, "an unadmitted card: no jump");
    for value in [&card, &edit] {
        assert!(pq::test_accept(generation, ROOM, SERVER, value, None, true, false));
    }
    // The stored card's answer is shown, escaped, whatever the quoted content says.
    let mut other = edit["content"]["m.new_content"].clone();
    other["im.hagency.agent_run"]["answer"] = "forged later".into();
    let admitted = preview(&other);
    assert!(admitted.contains("&lt;a href=") && !admitted.contains("<a ") && !admitted.contains("forged later"), "{admitted}");
    assert_eq!(run_controls::jump_target(room, card["event_id"].as_str().unwrap()).as_deref(), Some(request.as_str()), "to the request it is drawn on");
}

/// GPT review P2: ids in log lines come from the card's sender.
#[test]
fn only_plain_matrix_ids_reach_the_log() {
    assert_eq!(loggable("$z1xp-_l7lgXgzFGVl4r3rj1yO7PNdlI3Xu0BBKc-CKI"), "$z1xp-_l7lgXgzFGVl4r3rj1yO7PNdlI3Xu0BBKc-CKI");
    assert_eq!(loggable(ROOM), ROOM);
    assert_eq!(loggable("$abc\n[I] forged log line"), "<invalid>");
    assert_eq!(loggable("not an id"), "<invalid>");
}

/// GPT review P2: a sign-out during a retry pause aborted the task before
/// it settled its claims, and old logins' claims were never pruned.
#[test]
fn an_aborted_admission_settles_its_claims_and_old_logins_are_pruned() {
    let _guard = pq::test_serial();
    let (card, edit, _) = runs().remove(1);
    let fresh = || vec![cards_of(card.clone(), Some(edit.clone()), None).unwrap()];
    let old = pq::test_login("@csheargm:localhost:8450");
    let claimed = claim(old, fresh());
    assert_eq!(claimed.len(), 1);
    drop(Unsettled { generation: old, room: ROOM.into(), cards: claimed });
    assert_eq!(claim(old, fresh()).len(), 1, "settled as failed on drop, so tried again, not stuck admitting");
    let new = pq::test_login("@csheargm:localhost:8450");
    assert_eq!(claim(new, fresh()).len(), 1);
    assert!(HANDED.lock().unwrap().keys().all(|k| k.0 == new), "the old login's claims are gone");
}
