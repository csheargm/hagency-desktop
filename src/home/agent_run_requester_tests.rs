//! The waiting line under a request no card answers yet (split from
//! `agent_run_requester.rs`, R4).
use super::*;
use crate::i18n::{Language, text_for};

const ROOM: &str = "!dev:localhost:8452";

#[test]
fn only_an_agent_mention_within_the_window_waits() {
    let people: Vec<OwnedUserId> = ["@anna:x", "@_hagency_agt_01m7k84kgg5a5mtm2epqhbpjb5:y"]
        .iter().map(|u| OwnedUserId::try_from(*u).unwrap()).collect();
    assert_eq!(first_agent(people.iter()).map(|u| u.as_str()), Some("@_hagency_agt_01m7k84kgg5a5mtm2epqhbpjb5:y"));
    assert_eq!(first_agent(people[..1].iter()), None, "a message to a person waits for nobody");
    assert_eq!(age(10_000, 8_000), Some(Duration::from_secs(2)));
    assert_eq!(age(10_000, 12_000), Some(Duration::ZERO), "a clock ahead of ours is no age");
    assert_eq!(age(PENDING_WINDOW.as_millis() as u64 + 1, 0), None, "an old request no longer waits");
}

/// Live PR #109: the loop's "🔎 Review requested" to yue-claude showed
/// "⏳ Waiting for yue-claude to start…" for minutes; its run posts no card.
#[test]
fn the_viewers_own_loop_notice_waits_for_no_card() {
    use crate::hagency::issue_watch::loop_notice::{LoopNotice, Target};
    let reviewer = OwnedUserId::try_from("@_hagency_agt_01m7kd31m0vbgw7ed27chayat9:localhost:8450").unwrap();
    let target = Target { repo: "csheargm/hagency-demo-sandbox".into(), number: 109, url: "https://github.com/csheargm/hagency-demo-sandbox/pull/109".into(), head: "03d8148c63dd54ad69ac1b1435a36b48280b3012".into(), issues: Vec::new() };
    let notice = LoopNotice::ReviewRequested { target, reviewer }.body();
    assert!(waits_for_no_card("@csheargm:localhost:8450", &notice, true), "{notice}");
    assert!(!waits_for_no_card("@csheargm:localhost:8450", "@yue-claude please review PR #109", true), "a typed request still waits");
}

/// Bird's window, Nov 19: the notice to bird-codex (a Codex Agent) kept
/// "⏳ Waiting for bird-codex to start…" because nothing read the listing.
#[test]
fn the_own_codex_agents_review_notice_hides_once_the_listing_is_read() {
    use crate::hagency::issue_watch::loop_notice::{LoopNotice, Target};
    let bird = OwnedUserId::try_from("@bird:localhost:8452").unwrap();
    let codex = OwnedUserId::try_from("@_hagency_bird_bird-codex:localhost:8452").unwrap();
    let target = Target { repo: "csheargm/hagency-demo-sandbox".into(), number: 132, url: "https://github.com/csheargm/hagency-demo-sandbox/pull/132".into(), head: "5d0c3a9e1b2f4a6c8e0d2f4b6a8c0e2d4f6b8a0c".into(), issues: Vec::new() };
    let notice = LoopNotice::ReviewRequested { target, reviewer: codex.clone() }.body();
    assert!(notice.starts_with(&format!("🔎 Review requested — {codex} please review PR #132")), "{notice}");
    let index = crate::home::agent_ask_badge::puppet_index(&serde_json::json!([
        {"id": "agt_bird_codex", "puppetMxid": codex.as_str(), "ownerMxid": bird.as_str(), "harness": "codex"}]), &bird);
    assert!(index.contains_key(&codex), "a Codex Agent is an own Agent like any other");
    let sender = bird.as_str();
    assert_eq!(own_notice_decision(sender, &notice, || None), (false, true), "unknown: the line shows and the listing is read");
    assert_eq!(own_notice_decision(sender, &notice, || Some(index.contains_key(&codex))), (true, false), "then it goes");
    assert_eq!(own_notice_decision(sender, &notice, || Some(false)), (false, false), "another owner's Agent posts its card");
    assert_eq!(own_notice_decision(sender, "@bird-codex please review PR #132", || panic!("not consulted")), (false, false), "a typed request still waits");
}

/// GPT P2, Nov 19: the listing was asked for before the age check, so
/// expired notices kept triggering reads on every clock tick.
#[test]
fn an_expired_notice_never_reads_the_listing() {
    use crate::hagency::issue_watch::loop_notice::{LoopNotice, Target};
    let codex = OwnedUserId::try_from("@_hagency_bird_bird-codex:localhost:8452").unwrap();
    let target = Target { repo: "csheargm/hagency-demo-sandbox".into(), number: 132, url: "https://github.com/csheargm/hagency-demo-sandbox/pull/132".into(), head: "5d0c3a9e1b2f4a6c8e0d2f4b6a8c0e2d4f6b8a0c".into(), issues: Vec::new() };
    let notice = LoopNotice::ReviewRequested { target, reviewer: codex }.body();
    let (sender, expired) = ("@bird:localhost:8452", PENDING_WINDOW.as_millis() as u64 + 1);
    assert_eq!(waiting_age(expired, 0, (ROOM, sender), &notice, || panic!("not consulted"), || panic!("no read")), None);
    let mut reads = 0;
    assert_eq!(waiting_age(5_000, 0, (ROOM, sender), &notice, || None, || reads += 1), Some(Duration::from_secs(5)), "unknown: the line shows");
    assert_eq!(reads, 1, "and the listing is asked for");
    assert_eq!(waiting_age(5_000, 0, (ROOM, sender), &notice, || Some(true), || panic!("no read")), None, "own Agent: no line");
}

/// Bird's window, Nov 18 00:12: "⏳ Waiting for bird-codex to start…"
/// stayed under bird's own review request after bird-codex reviewed. GPT
/// batch review, Nov 19: only that reviewer's recorded review in that room
/// ends it.
#[test]
fn the_waiting_line_goes_once_its_reviewers_review_is_recorded_in_its_room() {
    use crate::hagency::issue_watch::loop_notice::{LoopNotice, Target, note_answered};
    let codex = OwnedUserId::try_from("@_hagency_bird_bird-codex:localhost:8452").unwrap();
    let claude = OwnedUserId::try_from("@_hagency_bird_bird-claude:localhost:8452").unwrap();
    let target = Target { repo: "csheargm/hagency-demo-sandbox".into(), number: 140, url: "https://github.com/csheargm/hagency-demo-sandbox/pull/140".into(), head: "7e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f".into(), issues: Vec::new() };
    let notice = LoopNotice::ReviewRequested { target: target.clone(), reviewer: codex.clone() }.body();
    let (bird, other_room) = ("@bird:localhost:8452", "!other:localhost:8452");
    let (other, same) = (|| Some(false), || None);
    let waits = |room: &str, body: &str| waiting_age(5_000, 0, (room, bird), body, other, || {}).is_some();
    assert!(waits(ROOM, &notice), "requested: the line shows");
    note_answered(ROOM, &target, claude.as_str());
    assert!(waits(ROOM, &notice), "another Agent's review: bird-codex's line still waits");
    note_answered(ROOM, &target, codex.as_str());
    assert_eq!(waiting_age(5_000, 0, (ROOM, bird), &notice, other, || panic!("no read")), None, "recorded: no line");
    assert_eq!(waiting_age(5_000, 0, (ROOM, bird), &notice, same, || panic!("no read")), None, "the same owner's Agent: no line either");
    assert!(waits(other_room, &notice), "the same request in another room is its own");
    let switched = LoopNotice::ReviewRequested { target: Target { number: 141, url: "https://github.com/csheargm/hagency-demo-sandbox/pull/141".into(), ..target.clone() }, reviewer: claude.clone() }.body();
    note_answered(ROOM, &Target { number: 141, url: "https://github.com/csheargm/hagency-demo-sandbox/pull/141".into(), ..target.clone() }, codex.as_str());
    assert!(waits(ROOM, &switched), "switched to bird-claude: the old reviewer's record answers nothing");
    let moved = Target { head: "0123456789abcdef0123456789abcdef01234567".into(), ..target };
    assert!(waits(ROOM, &LoopNotice::ReviewRequested { target: moved, reviewer: codex }.body()), "a new head's request still waits");
}

/// GPT batch review: the loop's review request to another owner's Agent
/// gets that owner's run card, so Anna sees the line until it does.
#[test]
fn a_loop_notice_to_another_owners_agent_still_waits_for_its_card() {
    use crate::hagency::issue_watch::loop_notice::{LoopNotice, Target};
    let reviewer = OwnedUserId::try_from("@_hagency_agt_bird:localhost:8452").unwrap();
    let target = Target { repo: "csheargm/hagency-demo-sandbox".into(), number: 109, url: "https://github.com/csheargm/hagency-demo-sandbox/pull/109".into(), head: "03d8148c63dd54ad69ac1b1435a36b48280b3012".into(), issues: Vec::new() };
    let notice = LoopNotice::ReviewRequested { target, reviewer }.body();
    assert!(!waits_for_no_card("@anna:localhost:8450", &notice, false), "{notice}");
}

/// Nov 18: a new reply's header showed `_hagency_agt_…` for a second
/// before the member list loaded; the mention pill already names it.
#[test]
fn the_agent_is_named_at_once_and_never_by_its_raw_id() {
    let agent = matrix_sdk::ruma::user_id!("@_hagency_agt_01k:beta");
    let html = r#"<a href="https://matrix.to/#/@_hagency_agt_01k:beta">@bird-claude</a> list the open issues"#;
    let pill = pill_name(html, agent);
    assert_eq!(pill.as_deref(), Some("@bird-claude"));
    assert_eq!(pick_agent_name(&[None, pill.as_deref()], agent).as_deref(), Some("bird-claude"), "the pill before the member loads");
    assert_eq!(pick_agent_name(&[Some("Bird Claude"), pill.as_deref()], agent).as_deref(), Some("Bird Claude"), "the room's name wins");
    assert_eq!(pick_agent_name(&[None, Some("_hagency_agt_01k")], agent), None, "a raw ID is never shown");
    assert_eq!(pick_agent_name(&[Some("@_hagency_agt_01k:beta"), Some(" ")], agent), None);
    let encoded = r#"<a href='https://matrix.to/#/%40_hagency_agt_01k%3Abeta'>bird &amp; co</a>"#;
    assert_eq!(pill_name(encoded, agent).as_deref(), Some("bird & co"));
    let other = r#"<a href="https://matrix.to/#/@_hagency_agt_01kx:beta">someone else</a> <a href="https://matrix.to/#/@_hagency_agt_01k:beta"><b>bird</b> &amp;lt;2&amp;gt;</a>"#;
    assert_eq!(pill_name(other, agent).as_deref(), Some("bird &lt;2&gt;"), "a longer ID is another Agent; nested tags drop; no double decoding");
    let fake = r#"<a href="https://example.org/matrix.to/#/@_hagency_agt_01k:beta">not a pill</a> matrix.to/#/@_hagency_agt_01k:beta"#;
    assert_eq!(pill_name(fake, agent), None, "only a matrix.to anchor counts");
}

#[test]
fn the_waiting_line_names_the_agent_and_ticks() {
    let text = pending_text("bird-claude", 12);
    assert!(text.starts_with("⏳") && text.contains("bird-claude") && text.contains(&super::super::agent_run_status::format_elapsed(12)), "{text}");
    let key = "Waiting for {agent} to start… ({t})";
    assert_eq!(text_for(Language::English, key), key);
    let zh = text_for(Language::Chinese, key);
    assert!(zh != key && zh.contains("{agent}") && zh.contains("{t}"), "{zh}");
}
