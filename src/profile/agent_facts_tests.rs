use super::*;
use matrix_sdk::ruma::user_id;
use serde_json::json;

#[test]
fn own_status_reads_running_offline_and_paused() {
    assert_eq!(own_status("active", None), None, "unread service: nothing claimed yet");
    assert_eq!(own_status("active", Some("running")), Some("Running"));
    assert_eq!(own_status("active", Some("processing")), Some("Running"));
    assert_eq!(own_status("active", Some("stopped")), Some("Paused"));
    assert_eq!(own_status("active", Some("paused")), Some("Paused"));
    assert_eq!(own_status("active", Some("no_host")), Some("Offline"));
    assert_eq!(own_status("active", Some("host_restarting")), Some("Offline"));
    assert_eq!(own_status("active", Some("unknown")), None, "an unread service is not claimed Offline");
    assert_eq!(own_status("active", Some("elsewhere")), Some("On another device"));
    assert_eq!(own_status("active", Some("claude_sign_in")), Some("Claude sign-in needed — run `claude login`"));
    assert_eq!(own_status("active", Some("codex_sign_in")), Some("Codex sign-in needed — run `codex login`"));
    assert_eq!(own_status("suspended", Some("running")), Some("Paused"), "a suspended Agent is paused whatever its service says");
}

#[test]
fn the_roster_names_owners_and_pauses_only() {
    let rows = roster_rows(&json!([
        {"puppetMxid": "@_hagency_agt_1:s", "ownerMxid": "@betty:s", "bindingState": "active"},
        {"puppetMxid": "@_hagency_agt_2:s", "ownerMxid": "@betty:s", "bindingState": "suspended"},
        {"puppetMxid": "@_hagency_agt_3:s"},
        {"puppetMxid": "bad", "ownerMxid": "@betty:s"}
    ]));
    assert_eq!(rows.len(), 2, "rows without an owner or with a malformed ID are left out");
    let first = &rows[user_id!("@_hagency_agt_1:s")];
    assert_eq!(first.owner, user_id!("@betty:s"));
    assert_eq!(roster_status(&first.binding_state), Some("Active in this room"));
    assert_eq!(roster_status(&rows[user_id!("@_hagency_agt_2:s")].binding_state), Some("Paused"));
    assert_eq!(roster_status("left"), None);
}

#[test]
fn own_agents_show_config_facts_and_others_say_who_sees_settings() {
    let own = AgentFacts { own_agent_id: Some("agt_1".into()), owner: None, owner_basis: OwnerBasis::Listed, runtime: Some("Claude"), status: Some("Running"), activity: None, shared: None };
    assert_eq!(fact_lines(&own, Some("Anna"), false), vec!["Owner: Anna", "Runtime: Claude", "Status: Running"]);
    let theirs = AgentFacts { status: Some("Paused"), ..Default::default() };
    assert_eq!(fact_lines(&theirs, Some("Betty"), true), vec!["Agent of Betty", "Status: Paused", "Settings are visible only to Betty"]);
    assert_eq!(fact_lines(&AgentFacts::default(), None, false), vec!["Owner not shared in this room", "Settings are visible only to its owner"]);
}

/// Chris (Nov 17): the hover card says what a shared Agent does for the
/// room's members, for each of its group rooms only.
#[test]
fn an_own_agents_group_rooms_carry_its_shared_choices() {
    let status = serde_json::json!({
        "rooms": [{"roomId": "!dev:s", "name": "dev", "state": "active"}, {"roomId": "!gone:s", "name": "gone", "state": "left"}],
        "approvals": {"roomShares": [{"room": "!dev:s", "answer": true, "review": false, "fix": false}]}
    });
    let shares = super::group_room_shares(&status);
    assert_eq!(shares.len(), 1, "a room it left is not a group room any more");
    let share = shares["!dev:s"];
    assert!(share.answer && !share.review && !share.fix);
    let facts = AgentFacts { own_agent_id: Some("agt_1".into()), shared: Some(share), ..Default::default() };
    assert_eq!(fact_lines(&facts, Some("Betty"), false), vec!["Owner: Betty", "Shared in this room: answers questions; reviews need Betty's OK"]);
}

/// Live finding (Nov 18, Anna's dev room): Betty's federated bird-claude had
/// no roster row, so its profile said "Owner not shared in this room" while
/// its run cards named Betty. A viewer now reads whose it says it is, by name
/// and Matrix ID, its runtime and what it does.
#[test]
fn a_federated_agents_profile_names_its_owner_from_its_run_cards() {
    use super::super::agent_room_cards::CardFacts;
    let cards = CardFacts { owner: Some(user_id!("@bird:beta").to_owned()), owner_conflict: false, runtime: Some("Claude"), working: false };
    let mut facts = others_facts(None, cards.clone());
    assert_eq!(facts.owner.as_deref(), Some(user_id!("@bird:beta")));
    facts.activity = Some("idle".into());
    assert_eq!(fact_lines(&facts, Some("Betty"), true),
        vec!["Agent of Betty (@bird:beta), as stated by the Agent", "Runtime: Claude", "Status: idle", "Settings are visible only to its owner"]);
    let working = others_facts(None, CardFacts { working: true, ..cards.clone() });
    assert_eq!(working.status, Some("Working on a request"));
    let row = RosterRow { owner: user_id!("@betty:alpha").to_owned(), binding_state: "suspended".into(), runtime: Some("codex") };
    let paused = others_facts(Some(row), CardFacts { working: true, ..cards });
    assert_eq!((paused.owner.as_deref(), paused.status, paused.runtime), (Some(user_id!("@betty:alpha")), Some("Paused"), Some("Codex")),
        "the roster wins where both say; a paused binding stays paused");
    assert_eq!(fact_lines(&paused, Some("Betty"), false)[0], "Owner: Betty (@betty:alpha)", "a roster row is established");
}

/// GPT integration review: a federated Agent's own profile field named its
/// owner as fact and silently overrode run cards naming someone else. Both
/// are only the Agent's word: worded so, and no owner when they disagree.
#[test]
fn an_agents_self_claimed_owner_is_worded_as_its_claim_and_conflicts_show_none() {
    use super::super::agent_room_cards::CardFacts;
    let (bird, eve) = (user_id!("@bird:beta").to_owned(), user_id!("@eve:beta").to_owned());
    let stated = others_facts(None, with_profile_owner(CardFacts::default(), Some(bird.clone())));
    assert_eq!((stated.owner.as_deref(), stated.owner_basis), (Some(&*bird), OwnerBasis::StatedByAgent));
    assert_eq!(fact_lines(&stated, Some("Betty"), false),
        vec!["Owner (as stated by the Agent): Betty (@bird:beta)", "Settings are visible only to its owner"]);
    let cards = CardFacts { owner: Some(eve), ..Default::default() };
    let conflict = others_facts(None, with_profile_owner(cards.clone(), Some(bird.clone())));
    assert_eq!((conflict.owner.clone(), conflict.owner_basis), (None, OwnerBasis::Conflicting), "profile and cards disagree: neither wins");
    assert_eq!(fact_lines(&conflict, None, true),
        vec!["Owner unknown: the Agent names different owners", "Settings are visible only to its owner"]);
    let cards_conflict = CardFacts { owner_conflict: true, ..Default::default() };
    assert_eq!(others_facts(None, with_profile_owner(cards_conflict, Some(bird.clone()))).owner, None, "the profile does not settle conflicting cards");
    let agreeing = with_profile_owner(CardFacts { owner: Some(bird.clone()), ..Default::default() }, Some(bird));
    assert_eq!(others_facts(None, agreeing).owner_basis, OwnerBasis::StatedByAgent, "agreeing claims are still claims");
}

/// Nov 18: Anna's profile of bird-claude showed owner and status but no
/// runtime once issue hand-offs stopped posting run cards; the Agent's own
/// profile now names it, ahead of an older card's.
#[test]
fn an_agents_profile_names_its_runtime_without_any_run_card() {
    use super::super::{agent_profile_owner::Published, agent_room_cards::CardFacts};
    let bird = user_id!("@bird:beta").to_owned();
    let published = Published { owner: Some(bird), runtime: Some("Claude") };
    let facts = others_facts(None, with_profile(CardFacts::default(), published.clone()));
    assert_eq!(facts.runtime, Some("Claude"));
    assert_eq!(fact_lines(&facts, Some("Betty"), true)[1], "Runtime: Claude", "shown on the profile and the hover card");
    let old_card = CardFacts { runtime: Some("Codex"), ..Default::default() };
    assert_eq!(with_profile(old_card.clone(), published).runtime, Some("Claude"), "the latest start wins over an old card");
    assert_eq!(with_profile(old_card, Published::default()).runtime, Some("Codex"), "no profile runtime: the cards still say");
}

#[test]
fn the_owner_shows_with_its_matrix_id_unless_the_name_is_the_id() {
    let id = user_id!("@bird:beta");
    assert_eq!(owner_label("Betty", Some(id)), "Betty (@bird:beta)");
    assert_eq!(owner_label("@bird:beta", Some(id)), "@bird:beta", "no name: the ID once");
    assert_eq!(owner_label("Anna", None), "Anna");
}

/// GPT review: a profile opened before a run card arrived kept "Owner not
/// shared"; an admitted card or a room's Status list now re-reads it.
#[test]
fn run_cards_and_room_status_refresh_open_cards() {
    let updates: [makepad_widgets::Action; 4] = [
        Box::new(AgentFactsChanged),
        Box::new(super::own_agents::OwnAgentsChanged),
        Box::new(crate::hagency::pending_questions::PendingQuestionsChanged),
        Box::new(crate::home::room_watches::WatchesChanged { room_id: matrix_sdk::ruma::owned_room_id!("!r:s") }),
    ];
    assert!(updates.iter().all(is_facts_update));
    let other: makepad_widgets::Action = Box::new(());
    assert!(!is_facts_update(&other));
}
