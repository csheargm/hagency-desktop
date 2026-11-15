//! The watcher's hand-off message: what the Agent reads, what wakes it, and
//! what the timeline shows.
use super::notice::{MARKER, MERGE_POLICY_STEP, Notice};
use ::hagency::issue_watch::Issue;
use matrix_sdk::ruma::UserId;
use serde_json::Value;

fn issue() -> Issue {
    Issue {
        number: 6,
        url: "https://github.com/csheargm/hagency-demo-sandbox/issues/6".into(),
        title: "README: add a 'Getting started'\n section".into(),
        labels: vec![],
        is_pr: false,
        author_association: "OWNER".into(),
        created_at: 10,
    }
}
fn puppet() -> &'static UserId {
    "@demoup-claude:example.org".try_into().unwrap()
}
/// The event content exactly as `room_send::send_routed_with` assembles it
/// (message, then the marker beside it), before routing directives.
fn sent_content(notice: &Notice) -> Value {
    let mut content = serde_json::to_value(notice.content(puppet())).unwrap();
    content.as_object_mut().unwrap().extend(notice.marker());
    content
}

#[test]
fn the_body_names_the_issue_and_asks_the_agent_with_its_url() {
    let notice = Notice::new("csheargm/hagency-demo-sandbox", &issue(), "@demoup-claude");
    assert_eq!(
        notice.body(),
        "🔔 New GitHub issue #6 in csheargm/hagency-demo-sandbox: README: add a 'Getting started' section — @demoup-claude please fix it on a branch and open a draft pull request: https://github.com/csheargm/hagency-demo-sandbox/issues/6"
    );
    let untitled = Issue { title: String::new(), ..issue() };
    assert!(Notice::new("a/b", &untitled, "Bot").body().starts_with("🔔 New GitHub issue #6 in a/b — @Bot please fix it on a branch and open a draft pull request"));
}

/// Dispatch (`hagency-matrix` event_batch + `verified_ingress`) wakes the Agent
/// on an owner message whose msgtype is text and whose `m.mentions` names the
/// Agent's puppet. The hand-off keeps both, so it triggers exactly as the
/// old typed-looking "@Agent please fix <url>" did.
#[test]
fn the_sent_event_still_wakes_the_agent_as_a_request() {
    let notice = Notice::new("csheargm/hagency-demo-sandbox", &issue(), "demoup-claude");
    let content = sent_content(&notice);
    assert_eq!(content["msgtype"], "m.text", "text wakes; a custom msgtype would be Unsupported");
    assert_eq!(content["m.mentions"]["user_ids"], serde_json::json!([puppet().as_str()]));
    assert!(content["body"].as_str().unwrap().contains(&notice.url), "the Agent reads the issue URL");
    let html = content["formatted_body"].as_str().unwrap();
    assert!(html.contains(&format!("href=\"{}\"", puppet().matrix_to_uri())), "a real mention pill: {html}");
    assert!(html.contains("&#39;") || html.contains("'Getting started'"), "title is escaped text: {html}");
    assert_eq!(content[MARKER]["number"], 6);
    assert_eq!(content[MARKER]["repo"], "csheargm/hagency-demo-sandbox");
}

#[test]
fn the_timeline_reads_the_marker_back_and_shows_a_compact_notice() {
    let notice = Notice::new("csheargm/hagency-demo-sandbox", &issue(), "demoup-claude");
    assert_eq!(Notice::from_content(&sent_content(&notice)), Some(notice.clone()));
    let (plain, html) = notice.view();
    assert!(plain.contains("From issue watching"), "{plain}");
    assert!(html.body.contains("Open on GitHub") && html.body.contains(&format!("href=\"{}\"", notice.url)), "{}", html.body);
    assert!(html.body.contains("Asked @demoup-claude to fix it"), "{}", html.body);
}

#[test]
fn an_unmarked_or_malformed_message_is_not_a_hand_off() {
    let typed = serde_json::to_value(matrix_sdk::ruma::events::room::message::RoomMessageEventContent::text_plain("please fix x")).unwrap();
    assert_eq!(Notice::from_content(&typed), None);
    let mut bad = sent_content(&Notice::new("a/b", &issue(), "Bot"));
    bad[MARKER]["url"] = "javascript:alert(1)".into();
    assert_eq!(Notice::from_content(&bad), None, "only https links become the Open on GitHub link");
    bad[MARKER] = Value::String("x".into());
    assert_eq!(Notice::from_content(&bad), None);
}

/// Auto-merge on: the fixer is told to carry the PR on to an Agent review, so
/// the flow continues past the draft with no person's prompt; off: unchanged.
#[test]
fn auto_merge_on_adds_the_merge_policy_step_and_survives_the_marker() {
    let off = Notice::new("a/b", &issue(), "fixer");
    assert!(!off.body().contains("merge policy"), "off: the draft-PR request only");
    let on = off.clone().with_merge_policy(true);
    assert!(on.body().ends_with(&format!("issues/6 {MERGE_POLICY_STEP}")));
    assert!(MERGE_POLICY_STEP.contains("gh pr ready <N>") && MERGE_POLICY_STEP.contains("another Agent in this room to review"));
    let content = sent_content(&on);
    assert!(content["formatted_body"].as_str().unwrap().contains("agent merge policy"));
    assert_eq!(Notice::from_content(&content), Some(on));
    assert_eq!(Notice::from_content(&sent_content(&off)), Some(off), "older notices read as off");
}
