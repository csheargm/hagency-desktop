use hagency_runtime::codex::tree::TurnTree;
use serde_json::{Value, json};

fn started(thread: &str, turn: &str, root: &str) -> Value {
    json!({"threadId":thread,"turn":{"id":turn,"rootTurnId":root,"status":"inProgress"}})
}
fn ended(thread: &str, turn: &str) -> Value {
    json!({"threadId":thread,"turn":{"id":turn,"rootTurnId":"root-turn","status":"completed"}})
}
fn receipt(thread: &str, turn: &str, response: &str) -> Value {
    json!({"threadId":thread,"turnId":turn,"responseId":response,"usage":{
        "inputTokens":10,"outputTokens":4,"cachedInputTokens":2,"cacheWriteInputTokens":3,
        "reasoningOutputTokens":1,"totalTokens":14}})
}
#[test]
fn root_answer_waits_for_children_and_deduplicates_exact_receipts() {
    let mut tree = TurnTree::new("root", "root-turn").unwrap();
    tree.observe(
        "turn/started",
        &started("child", "child-turn", "root-turn"),
        false,
    )
    .unwrap();
    let root = receipt("root", "root-turn", "response-root");
    for _ in 0..2 {
        tree.observe("rawResponse/completed", &root, false).unwrap();
    }
    tree.observe("turn/completed", &ended("root", "root-turn"), false)
        .unwrap();
    assert!(!tree.is_complete());
    assert!(tree.usage().is_none());
    let callback = tree
        .observe(
            "item/commandExecution/requestApproval",
            &json!({"threadId":"child","turnId":"child-turn"}),
            true,
        )
        .unwrap()
        .unwrap();
    assert!(!callback.root);
    // A fork's entire inherited history is not consumption for this call.
    tree.observe("thread/tokenUsage/updated", &json!({"threadId":"child","turnId":"child-turn","tokenUsage":{"total":{"totalTokens":9999}}}), false).unwrap();
    tree.observe(
        "rawResponse/completed",
        &receipt("child", "child-turn", "response-child"),
        false,
    )
    .unwrap();
    tree.observe("turn/completed", &ended("child", "child-turn"), false)
        .unwrap();
    assert!(tree.is_complete());
    let total = tree.usage().unwrap();
    assert_eq!(
        (
            total.input,
            total.output,
            total.cached_input,
            total.cache_write_input
        ),
        (20, 8, 4, 6)
    );
}
#[test]
fn root_completion_cannot_outrun_child_listener_attachment() {
    let mut tree = TurnTree::new("root", "root-turn").unwrap();
    tree.observe(
        "rawResponse/completed",
        &receipt("root", "root-turn", "r"),
        false,
    )
    .unwrap();
    tree.observe(
        "item/completed",
        &json!({"threadId":"root","turnId":"root-turn","item":{
        "id":"spawn","type":"collabAgentToolCall","tool":"spawnAgent","status":"completed",
        "receiverThreadIds":["child"],"agentsStates":{"child":{"status":"pendingInit"}}}}),
        false,
    )
    .unwrap();
    tree.observe("turn/completed", &ended("root", "root-turn"), false)
        .unwrap();
    assert!(!tree.is_complete());
    tree.observe(
        "thread/status/changed",
        &json!({"threadId":"child","status":{"type":"idle"}}),
        false,
    )
    .unwrap();
    assert!(!tree.is_complete());
    tree.observe(
        "turn/started",
        &started("child", "child-turn", "root-turn"),
        false,
    )
    .unwrap();
    tree.observe(
        "rawResponse/completed",
        &receipt("child", "child-turn", "c"),
        false,
    )
    .unwrap();
    tree.observe("turn/completed", &ended("child", "child-turn"), false)
        .unwrap();
    assert!(tree.is_complete() && tree.usage().is_some());
}
#[test]
fn foreign_turns_and_unbound_callbacks_never_gain_a_scope() {
    let mut tree = TurnTree::new("root", "root-turn").unwrap();
    assert!(
        tree.observe(
            "turn/started",
            &started("foreign", "foreign-turn", "foreign-root"),
            false
        )
        .is_err()
    );
    assert!(
        tree.observe(
            "item/permissions/requestApproval",
            &json!({"threadId":"foreign","turnId":"foreign-turn"}),
            true
        )
        .is_err()
    );
    assert!(
        tree.observe(
            "item/completed",
            &json!({"threadId":"root","turnId":"previous"}),
            false
        )
        .is_err()
    );
    tree.observe("turn/completed", &ended("root", "root-turn"), false)
        .unwrap();
    assert!(
        tree.observe(
            "item/permissions/requestApproval",
            &json!({"threadId":"root","turnId":"root-turn"}),
            true
        )
        .is_err()
    );
}
#[test]
fn missing_conflicting_or_malformed_child_usage_stays_unknown() {
    for mode in [
        "missing",
        "conflict",
        "foreign-response",
        "malformed",
        "future-counter",
    ] {
        let mut tree = TurnTree::new("root", "root-turn").unwrap();
        tree.observe(
            "turn/started",
            &started("child", "child-turn", "root-turn"),
            false,
        )
        .unwrap();
        tree.observe(
            "rawResponse/completed",
            &receipt("root", "root-turn", "r"),
            false,
        )
        .unwrap();
        if mode != "missing" {
            let mut child = receipt(
                "child",
                "child-turn",
                if mode == "foreign-response" { "r" } else { "c" },
            );
            if mode == "malformed" {
                child["usage"]["totalTokens"] = 1.into();
            }
            if mode == "future-counter" {
                child["usage"]["futureBillableTokens"] = 99.into();
            }
            tree.observe("rawResponse/completed", &child, false)
                .unwrap();
            if mode == "conflict" {
                child["usage"]["inputTokens"] = 11.into();
                child["usage"]["totalTokens"] = 15.into();
                tree.observe("rawResponse/completed", &child, false)
                    .unwrap();
            }
        }
        tree.observe("turn/completed", &ended("root", "root-turn"), false)
            .unwrap();
        tree.observe("turn/completed", &ended("child", "child-turn"), false)
            .unwrap();
        assert!(tree.is_complete());
        assert!(tree.usage().is_none(), "{mode}");
    }
}

#[test]
fn collaboration_in_flight_or_fast_child_completion_cannot_end_measurement_early() {
    let mut tree = TurnTree::new("root", "root-turn").unwrap();
    let mut item = json!({"threadId":"root","turnId":"root-turn","item":{
        "id":"spawn","type":"collabAgentToolCall","tool":"spawnAgent","status":"inProgress",
        "receiverThreadIds":[],"agentsStates":{}}});
    tree.observe("item/started", &item, false).unwrap();
    tree.observe(
        "rawResponse/completed",
        &receipt("root", "root-turn", "r"),
        false,
    )
    .unwrap();
    assert!(tree.awaiting_collaboration() && tree.has_descendants());
    item["item"]["status"] = "completed".into();
    item["item"]["receiverThreadIds"] = json!(["child"]);
    item["item"]["agentsStates"] = json!({"child":{"status":"completed"}});
    tree.observe("item/completed", &item, false).unwrap();
    tree.observe("turn/completed", &ended("root", "root-turn"), false)
        .unwrap();
    assert!(!tree.is_complete() && tree.usage().is_none());
    tree.observe(
        "turn/started",
        &started("child", "child-turn", "root-turn"),
        false,
    )
    .unwrap();
    tree.observe(
        "rawResponse/completed",
        &receipt("child", "child-turn", "c"),
        false,
    )
    .unwrap();
    tree.observe("turn/completed", &ended("child", "child-turn"), false)
        .unwrap();
    assert!(tree.is_complete());
    assert_eq!(tree.usage().unwrap().input, 20);
}
