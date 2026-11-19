//! The loop's notices as the room and the Agents read them.
use super::{LoopNotice, Target};
use matrix_sdk::ruma::{OwnedUserId, owned_user_id};
use serde_json::Value;

const HEAD: &str = "2aef4710ec72aa11bb22cc33dd44ee55ff667788";
const URL: &str = "https://github.com/acme/app/pull/132";

fn target() -> Target {
    Target { repo: "acme/app".into(), number: 132, url: URL.into(), head: HEAD.into(), issues: Vec::new() }
}

fn agent() -> OwnedUserId {
    owned_user_id!("@_hagency_agt_01m7k84kgg5a5mtm2epqhbpjb5:localhost:8452")
}

fn sent(notice: &LoopNotice) -> Value {
    serde_json::to_value(notice.content()).expect("the notice encodes")
}

/// The receiving runtime wakes on `m.mentions` (event_batch); the plain body
/// keeps the ID the gate parses; the timeline's HTML pill shows the name.
fn assert_named_mention(notice: &LoopNotice, label: &str) {
    let content = sent(notice);
    assert_eq!(content["m.mentions"]["user_ids"], serde_json::json!([agent().as_str()]), "the Agent is still notified");
    assert_eq!(content["body"], notice.body(), "the plain body the Agents read is unchanged");
    let html = content["formatted_body"].as_str().expect("an HTML body");
    assert_eq!(content["format"], "org.matrix.custom.html");
    let pill = format!("<a href=\"{}\">{label}</a>", agent().matrix_to_uri());
    assert!(html.contains(&pill), "a mention pill: {html}");
    assert_eq!(html.matches(agent().as_str()).count(), 1 + usize::from(label == agent().as_str()), "the ID only in the pill: {html}");
}

#[test]
fn review_request_mentions_the_reviewer_with_a_pill() {
    // A pill whose text is the ID draws the member's display name.
    assert_named_mention(&LoopNotice::ReviewRequested { target: target(), reviewer: agent() }, agent().as_str());
}

#[test]
fn approved_and_mark_ready_show_the_agent_by_name() {
    assert_named_mention(&LoopNotice::Approved { target: target(), fixer: agent(), name: "yue-claude".into() }, "yue-claude");
    let ready = LoopNotice::MarkReady { target: target(), fixer: agent(), name: "yue-claude".into() };
    assert_named_mention(&ready, "yue-claude");
    assert!(ready.body().contains(&format!("yue-claude ({})", agent())), "the gate still reads the ID: {}", ready.body());
}

#[test]
fn a_notice_with_no_agent_stays_plain() {
    let held = LoopNotice::Unreachable { target: target(), step: "hold-unreachable".into() };
    assert!(sent(&held).get("formatted_body").is_none());
}
