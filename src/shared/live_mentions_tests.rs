use super::*;
use matrix_sdk::ruma::user_id;

fn members() -> Vec<PlainMentionCandidate<'static>> {
    vec![
        PlainMentionCandidate { user_id: user_id!("@_hagency_agt_fx1:a.server"), display_name: Some("Fixer") },
        PlainMentionCandidate { user_id: user_id!("@alex:a.server"), display_name: Some("Sam") },
        PlainMentionCandidate { user_id: user_id!("@sam:b.server"), display_name: None },
    ]
}
/// Types `typed` at the end of `before`, as a keystroke would.
fn pill(before: &str, typed: &str, declined: &[String]) -> Option<LivePill> {
    let text = format!("{before}{typed}");
    live_pill(&text, text.len(), typed, &members(), declined)
}

#[test]
fn terminator_after_unique_name_converts_to_picker_label() {
    for end in [" ", ",", "!", "?", ")", "\n"] {
        let found = pill("hi @fixer", end, &[]).unwrap_or_else(|| panic!("{end:?}"));
        assert_eq!(found.range, 3..9);
        assert_eq!(found.label, "@Fixer");
        assert_eq!(found.user, user_id!("@_hagency_agt_fx1:a.server"));
        assert_eq!(found.key, "fixer");
    }
}

#[test]
fn non_terminators_and_mid_word_never_convert() {
    for end in ["-", "_", ".", ":", "`", "x"] {
        assert_eq!(pill("@fixer", end, &[]), None, "{end:?}");
    }
    assert_eq!(pill("mail a@fixer", " ", &[]), None);
    assert_eq!(pill("`@fixer", " ", &[]), None);
    // Paste or multi-char input is not a keystroke.
    assert_eq!(pill("@fixer", "  ", &[]), None);
}

#[test]
fn ambiguous_unknown_and_declined_names_stay_text() {
    assert_eq!(pill("@sam", " ", &[]), None);
    assert_eq!(pill("@nobody", " ", &[]), None);
    assert_eq!(pill("@fixer", " ", &["fixer".to_string()]), None);
}

#[test]
fn terminator_typed_mid_text_converts_only_the_name_before_cursor() {
    let text = "@fixer please";
    let found = live_pill("@fixer, please", 7, ",", &members(), &[]).expect("pill");
    assert_eq!(found.range, 0..6);
    assert_eq!(live_pill(text, 7, "p", &members(), &[]), None);
}

#[test]
fn backspacing_into_a_pill_declines_it() {
    let fixer = user_id!("@_hagency_agt_fx1:a.server");
    let mut live = vec![(fixer.to_owned(), "fixer".to_string())];
    assert!(broken_pills(&mut live, &[fixer]).is_empty());
    assert_eq!(broken_pills(&mut live, &[]), vec!["fixer".to_string()]);
    assert!(live.is_empty());
    // Retyping the same name after that never re-pills it.
    assert_eq!(pill("@fixer", " ", &["fixer".to_string()]), None);
}

fn agents() -> Vec<OwnAgent> {
    vec![
        OwnAgent { name: "reviewer".into(), puppet: user_id!("@_hagency_agt_rv1:a.server").to_owned() },
        OwnAgent { name: "Fixer".into(), puppet: user_id!("@_hagency_agt_fx1:a.server").to_owned() },
        OwnAgent { name: "twin".into(), puppet: user_id!("@_hagency_agt_t1:a.server").to_owned() },
        OwnAgent { name: "twin".into(), puppet: user_id!("@_hagency_agt_t2:a.server").to_owned() },
    ]
}

#[test]
fn own_agent_not_in_room_is_offered() {
    let agents = agents();
    let hint = |text: &str| agent_not_in_room(text, &members(), &agents).map(|a| a.name.as_str());
    assert_eq!(hint("ask @Reviewer please"), Some("reviewer"));
    assert_eq!(hint("ask @_hagency_agt_rv1"), Some("reviewer"));
    // Members, ambiguous agents, unknowns and code are never offered.
    assert_eq!(hint("@fixer"), None);
    assert_eq!(hint("@twin"), None);
    assert_eq!(hint("@nobody"), None);
    assert_eq!(hint("`@reviewer`"), None);
}

#[test]
fn own_agents_reads_only_this_owners_active_agents() {
    let owner = user_id!("@me:a.server");
    let listing = serde_json::json!({"ownerMxid": "@me:a.server", "agents": [
        {"id": "agt_2", "state": "active", "displayName": "reviewer", "puppetMxid": "@_hagency_agt_2:a.server"},
        {"id": "agt_1", "state": "active", "displayName": "", "puppetMxid": "@_hagency_agt_1:a.server"},
        {"id": "agt_3", "state": "deleted", "displayName": "gone", "puppetMxid": "@_hagency_agt_3:a.server"},
    ]});
    let names: Vec<_> = own_agents(&listing, owner).into_iter().map(|a| a.name).collect();
    assert_eq!(names, vec!["reviewer", "_hagency_agt_1"]);
    assert!(own_agents(&listing, user_id!("@other:a.server")).is_empty());
}

/// Regression (dev room): "@anna-fixer remember: …" must offer the Agent and,
/// once added, become a real mention instead of plain text.
#[test]
fn typed_agent_name_with_colon_is_offered_and_located_for_the_mention() {
    let anna = OwnAgent { name: "anna-fixer".into(), puppet: user_id!("@_hagency_agt_af1:a.server").to_owned() };
    let agents = vec![anna.clone()];
    let text = "@anna-fixer remember: ship on Friday";
    assert_eq!(agent_not_in_room(text, &members(), &agents), Some(&anna));
    assert_eq!(agent_mention_range(text, &anna), Some(0..11));
    assert_eq!(agent_mention_range("see `@anna-fixer` and @Anna-Fixer!", &anna), Some(22..33), "code spans are skipped");
    assert_eq!(agent_mention_range("hi @_hagency_agt_af1 now", &anna), Some(3..20), "localpart works too");
    assert_eq!(agent_mention_range("hi @anna", &anna), None);
}
