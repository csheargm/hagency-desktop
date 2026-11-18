use super::*;
use crate::inbox::Limits;
const OWNER: &str = "@alice:test";

fn dispatch(id: &str, agent: &str, binding: &str) -> Dispatch {
    Dispatch {
        id: id.into(),
        binding_id: binding.into(),
        agent_id: agent.into(),
        event_id: format!("${id}:test"),
        room_id: format!("!{binding}:test"),
        requester_mxid: "@bob:test".into(),
        thread_root: "$root:test".into(),
        body: "secret request text".into(),
        state: "offered".into(),
        binding_generation: 1,
        dispatch_epoch: Some(1),
        dispatch_device_id: Some("device-a".into()),
        execution_id: None,
        outcome: None,
    }
}
/// Receive one request at `at`, then force its state and an optional call row.
fn record(l: &mut Ledger, d: &Dispatch, at: i64, state: &str, usage: Option<(&str, &str)>) {
    l.register_binding(OWNER, &d.scope()).unwrap();
    l.receive_dispatch(OWNER, d, Limits::default(), at).unwrap();
    let execution = format!("exec-{}", d.id);
    l.db.execute("UPDATE local_inbox SET state=?,execution_id=? WHERE id=?", (state, &execution, &d.id)).unwrap();
    if let Some((call_state, usage)) = usage {
        let usage = (call_state == "settled").then_some(usage);
        l.db.execute("INSERT INTO calls VALUES (?,?,?,'d',0,'[]',?,?)",
            (&execution, &d.binding_id, crate::json(&d.scope()).unwrap(), call_state, usage)).unwrap();
    }
}
fn usage(input: u64, output: u64, version: &str) -> String {
    format!(r#"{{"input":{input},"output":{output},"cached_input":0,"reasoning_output":0,"accounting_version":"{version}"}}"#)
}

#[test]
fn newest_first_limited_to_this_agent_with_outcomes_and_tokens() {
    let temp = tempfile::tempdir().unwrap();
    let mut l = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
    let measured = usage(120, 30, "provider-v1");
    let unreported = usage(0, 0, "unreported-zero-v1");
    record(&mut l, &dispatch("d1", "agent-a", "b1"), 10, "replied", Some(("settled", &measured)));
    record(&mut l, &dispatch("d2", "agent-a", "b1"), 20, "failed", Some(("settled", &unreported)));
    record(&mut l, &dispatch("d3", "agent-a", "b2"), 30, "rejected", None);
    record(&mut l, &dispatch("d4", "agent-a", "b2"), 40, "running", Some(("pending", "")));
    record(&mut l, &dispatch("other", "agent-b", "b3"), 50, "replied", None);
    let rows = l.recent_activity(OWNER, "agent-a", 20).unwrap();
    let ids: Vec<&str> = rows.iter().map(|r| r.dispatch_id.as_str()).collect();
    assert_eq!(ids, ["d4", "d3", "d2", "d1"]);
    let outcomes: Vec<Outcome> = rows.iter().map(|r| r.outcome).collect();
    assert_eq!(outcomes, [Outcome::Running, Outcome::Declined, Outcome::Failed, Outcome::Answered]);
    let tokens: Vec<Option<u64>> = rows.iter().map(|r| r.tokens).collect();
    assert_eq!(tokens, [None, None, None, Some(150)]);
    assert_eq!((rows[3].room_id.as_str(), rows[3].requester.as_str(), rows[3].received_at), ("!b1:test", "@bob:test", 10));
    let encoded = serde_json::to_string(&rows).unwrap();
    assert!(!encoded.contains("secret request text"), "request text must not leave the ledger");
    assert!(encoded.contains(r#""outcome":"declined""#));
    let newest = l.recent_activity(OWNER, "agent-a", 2).unwrap();
    assert_eq!(newest.iter().map(|r| r.dispatch_id.as_str()).collect::<Vec<_>>(), ["d4", "d3"]);
}

#[test]
fn rejects_another_owner_and_out_of_range_limits() {
    let temp = tempfile::tempdir().unwrap();
    let l = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
    assert!(matches!(l.recent_activity("@mallory:test", "agent-a", 20), Err(Error::Unauthorized)));
    assert!(l.recent_activity(OWNER, "agent-a", 0).is_err());
    assert!(l.recent_activity(OWNER, "agent-a", MAX_RECENT + 1).is_err());
    assert!(l.recent_activity(OWNER, "agent-a", 20).unwrap().is_empty());
}

#[test]
fn every_inbox_state_maps_to_an_outcome() {
    for (state, outcome) in [("received", Outcome::Running), ("acknowledged", Outcome::Running),
        ("prepared", Outcome::Running), ("running", Outcome::Running), ("reply_ready", Outcome::Answered),
        ("replied", Outcome::Answered), ("rejected", Outcome::Declined), ("failed", Outcome::Failed),
        ("unknown", Outcome::Failed)] {
        assert_eq!(Outcome::of_state(state), outcome, "{state}");
    }
}

#[test]
fn a_hand_off_names_its_issue_and_other_requests_name_none() {
    assert_eq!(handoff_issue("🔔 New GitHub issue #46 in csheargm/hagency-demo-sandbox — @bird please fix it"), Some(IssueRef { repo: "csheargm/hagency-demo-sandbox".into(), number: 46 }));
    assert_eq!(handoff_issue("context\n  🔔 New GitHub issue #7 in a/b — x"), Some(IssueRef { repo: "a/b".into(), number: 7 }));
    for other in ["secret request text", "🔔 New GitHub issue #x in a/b", "🔔 New GitHub issue #0 in a/b", "🔔 New GitHub issue #3 in not-a-repo", ""] {
        assert_eq!(handoff_issue(other), None, "{other}");
    }
    let temp = tempfile::tempdir().unwrap();
    let mut l = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
    let mut d = dispatch("h", "agent", "bind");
    d.body = "🔔 New GitHub issue #46 in a/b — @bird please fix it".into();
    record(&mut l, &d, 10, "failed", None);
    let json = serde_json::to_value(l.recent_activity(OWNER, "agent", 5).unwrap()).unwrap();
    assert_eq!(json[0]["issue"], serde_json::json!({"repo": "a/b", "number": 46}));
    assert!(!json.to_string().contains("please fix"), "no request text");
}

#[test]
fn a_run_a_restart_left_unknown_is_marked_interrupted() {
    let temp = tempfile::tempdir().unwrap();
    let mut l = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
    record(&mut l, &dispatch("lost", "agent", "bind"), 10, "running", None);
    l.recover_local_executions(OWNER).unwrap();
    record(&mut l, &dispatch("bad", "agent", "bind2"), 20, "failed", None);
    let json = serde_json::to_value(l.recent_activity(OWNER, "agent", 5).unwrap()).unwrap();
    assert_eq!((json[0]["dispatchId"].as_str(), json[0].get("interrupted")), (Some("bad"), None), "a real failure is not");
    assert_eq!((json[1]["dispatchId"].as_str(), json[1]["interrupted"].as_bool(), json[1]["outcome"].as_str()), (Some("lost"), Some(true), Some("failed")));
}
