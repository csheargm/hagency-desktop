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

#[test]
fn a_hand_offs_issue_survives_the_body_compaction() {
    let temp = tempfile::tempdir().unwrap();
    let mut l = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
    let mut d = dispatch("h", "agent", "bind");
    d.body = "🔔 New GitHub issue #62 in a/b — @bird please fix it".into();
    record(&mut l, &d, 10, "running", None);
    l.recover_local_executions(OWNER).unwrap(); // unknown, then terminal compaction
    let body: String = l.db.query_row("SELECT json_extract(envelope,'$.body') FROM local_inbox WHERE id='h'", [], |r| r.get(0)).unwrap();
    assert!(body.is_empty(), "the precondition: compaction cleared the body");
    let json = serde_json::to_value(l.recent_activity(OWNER, "agent", 5).unwrap()).unwrap();
    assert_eq!((json[0]["issue"]["number"].as_i64(), json[0]["interrupted"].as_bool()), (Some(62), Some(true)));
}

#[test]
fn a_hand_off_whose_approval_card_went_unanswered_says_so() {
    // e2e Nov 17 23:32: #69's `gh pr create` card ended unanswered; the run
    // replied, and the loop counted it as a failed attempt.
    let temp = tempfile::tempdir().unwrap();
    let mut l = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
    let mut waited = dispatch("waited", "agent", "bind");
    waited.body = "🔔 New GitHub issue #69 in a/b — @bird please fix it".into();
    record(&mut l, &waited, 10, "replied", None);
    let mut done = dispatch("done", "agent", "bind2");
    done.body = "🔔 New GitHub issue #68 in a/b — @bird please fix it".into();
    record(&mut l, &done, 20, "replied", None);
    record(&mut l, &dispatch("plain", "agent", "bind3"), 30, "replied", None);
    l.note_unanswered_approval("waited").unwrap();
    l.note_unanswered_approval("waited").unwrap(); // a second card: same mark
    l.note_unanswered_approval("plain").unwrap(); // not a hand-off: ignored
    let json = serde_json::to_value(l.recent_activity(OWNER, "agent", 5).unwrap()).unwrap();
    assert_eq!((json[0]["dispatchId"].as_str(), json[0].get("unanswered")), (Some("plain"), None));
    assert_eq!((json[1]["dispatchId"].as_str(), json[1].get("unanswered")), (Some("done"), None));
    assert_eq!((json[2]["dispatchId"].as_str(), json[2]["unanswered"].as_bool(), json[2]["outcome"].as_str()), (Some("waited"), Some(true), Some("answered")));
}

/// The live path of a hand-off refused before the model started: receive,
/// prepare, start, then the owner-policy notice instead of a model reply.
fn refused_hand_off(l: &mut Ledger, d: &Dispatch, at: i64, reason: &str) {
    l.register_binding(OWNER, &d.scope()).unwrap();
    l.receive_dispatch(OWNER, d, Limits::default(), at).unwrap();
    l.acknowledge_dispatch(OWNER, &d.id).unwrap();
    let p = l.prepare_execution(OWNER, &d.id).unwrap();
    let mut running = p.dispatch.clone();
    running.state = "running".into();
    running.execution_id = Some(p.execution_id.clone());
    l.confirm_execution_start(OWNER, &crate::inbox::ServerStart { dispatch: running, newly_started: true }).unwrap();
    l.persist_no_provider_reply(OWNER, &d.id, &p.execution_id, reason).unwrap();
}

#[test]
fn three_hand_offs_refused_by_a_used_up_budget_say_no_run_and_the_budget_says_used_up() {
    // Nov 18 08:16-08:19: #93's run took bird-claude's UTC-day spend from
    // 9.98 M to 10.28 M of its 10 M tokens; every later hand-off (#90-#92,
    // three each) was answered "budget reached, model not called" and the
    // loop counted each as an ended attempt.
    let temp = tempfile::tempdir().unwrap();
    let mut l = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
    let now = 1_795_018_600;
    let mut policy = l.agent_policy(OWNER, "agent").unwrap().policy;
    policy.budget = crate::Budget { limit: crate::Limit::Tokens(10_000_000), period: crate::Period::UtcDay };
    let scope = dispatch("x", "agent", "bind").scope();
    l.register_binding(OWNER, &scope).unwrap();
    l.set_policy(OWNER, &scope, crate::Layer::Agent, 0, &policy).unwrap();
    assert!(!l.agent_budget_used_up(OWNER, "agent", now).unwrap());
    let window = crate::Period::UtcDay.window(now).unwrap();
    l.db.execute("INSERT INTO accounts VALUES (?,?,10277116,0)", (crate::json(&("agent", "agent")).unwrap(), &window)).unwrap();
    assert!(l.agent_budget_used_up(OWNER, "agent", now).unwrap());
    assert!(!l.agent_budget_used_up(OWNER, "agent", now + 86_400).unwrap(), "a new UTC day allows again");
    for (i, number) in [90, 91, 92].into_iter().enumerate() {
        let mut d = dispatch(&format!("h{number}"), "agent", "bind");
        d.thread_root = format!("$root{number}:test");
        d.body = format!("🔔 New GitHub issue #{number} in a/b — @bird please fix it");
        let refused = l.begin_postpaid(&d.scope(), &format!("exec-probe-{number}"), &d.id, now);
        assert!(matches!(refused, Err(Error::Budget)), "the precondition: the ledger refuses the run");
        refused_hand_off(&mut l, &d, now + i as i64, "⏸️ budget reached");
    }
    let json = serde_json::to_value(l.recent_activity(OWNER, "agent", 5).unwrap()).unwrap();
    for row in json.as_array().unwrap() {
        assert_eq!((row["outcome"].as_str(), row["noRun"].as_bool()), (Some("answered"), Some(true)), "{row}");
    }
}

#[test]
fn a_model_reply_and_a_declined_request_are_not_no_runs() {
    let temp = tempfile::tempdir().unwrap();
    let mut l = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
    let measured = usage(120, 30, "provider-v1");
    let zero = usage(0, 0, crate::inbox::NO_PROVIDER_CALL);
    record(&mut l, &dispatch("ran", "agent", "b1"), 10, "replied", Some(("settled", &measured)));
    record(&mut l, &dispatch("declined", "agent", "b2"), 20, "rejected", Some(("settled", &zero)));
    record(&mut l, &dispatch("noted", "agent", "b3"), 30, "replied", Some(("settled", &zero)));
    let json = serde_json::to_value(l.recent_activity(OWNER, "agent", 5).unwrap()).unwrap();
    let marks: Vec<_> = json.as_array().unwrap().iter().map(|r| (r["dispatchId"].as_str().unwrap().to_owned(), r.get("noRun").is_some())).collect();
    assert_eq!(marks, [("noted".to_owned(), true), ("declined".to_owned(), false), ("ran".to_owned(), false)]);
}
