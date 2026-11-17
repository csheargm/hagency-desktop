use super::*;
use serde_json::json;

fn panel(cx: &mut Cx) -> AgentPanel {
    cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::app::register_widgets(vm);
        vm.bx.captured_errors = Some(Vec::new());
        let value = script_eval!(vm, {mod.widgets.HagencyAgentPanel {}});
        let panel = AgentPanel::script_from_value(vm, value);
        assert!(vm.take_errors().is_empty(), "the panel's DSL evaluates without errors");
        panel
    })
}

#[test]
fn context_lists_the_private_chat_and_rooms() {
    let status = json!({"chat": {"roomId": "!dm:s"}, "rooms": [{"name": "Ops", "state": "active"}, {"name": "", "state": "suspended"}]});
    assert_eq!(activity::room_lines(&status), vec!["Private chat with you", "Ops", "Unnamed room · suspended"]);
    assert!(activity::room_lines(&json!({})).is_empty());
}

#[test]
fn loaded_agent_renders_identity_who_can_ask_and_credit() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut panel = panel(&mut cx);
    panel.agent = json!({"id": "agt_1", "displayName": "ann-claude", "puppetMxid": "@_hagency_agt_1:s"});
    panel.status = json!({"here": true, "deviceName": "Mac", "chat": {"roomId": "!dm:s"}, "service": {"state": "stopped"}, "approvals": {"mode": "outside", "rules": [{"tool": "claude.Bash", "directory": "/w", "request": "r", "label": "gh pr list"}]}});
    panel.model = json!({"policy": {"revision": 3, "policy": {"budget": {"limit": {"Tokens": 2_000_000}, "period": "UtcDay"}, "requests": "Deny", "high_risk": "AskOwner"}},
        "usage": {"spent": 800_000, "held": 0}, "modelProfile": {"model": "m", "credential_ref": "claude-managed:x", "workspace_root": "/work", "reasoning_effort": "high"}});
    panel.models = vec![ModelChoice { model: "m".into(), label: "Model M".into(), efforts: vec!["low".into(), "high".into()], default_effort: "low".into() }];
    panel.fill_budget(&mut cx);
    panel.render(&mut cx);
    assert_eq!(panel.view.label(&mut cx, ids!(runs_on)).text(), "Runs on this Mac (Claude)");
    assert_eq!(panel.view.label(&mut cx, ids!(matrix_id)).text(), "@_hagency_agt_1:s", "the Agent's Matrix ID is shown (live finding)");
    assert!(panel.view.button(&mut cx, ids!(copy_matrix_id)).borrow().unwrap().enabled());
    assert_eq!(panel.view.drop_down(&mut cx, ids!(who)).selected_item(), 0, "Deny shows Only me");
    assert_eq!(panel.view.drop_down(&mut cx, ids!(thinking)).selected_item(), 1, "saved Thinking is selected");
    assert_eq!(panel.view.label(&mut cx, ids!(credit_text)).text(), "1.2M of 2M tokens left today");
    assert_eq!(panel.view.drop_down(&mut cx, ids!(approval_mode)).selected_item(), 2, "saved outside mode shows");
    assert!(panel.view.button(&mut cx, ids!(revoke)).borrow().unwrap().enabled(), "a remembered rule can be revoked");
    assert_eq!(panel.view.text_input(&mut cx, ids!(panel_token_limit)).text(), "2000000");
    assert_eq!(panel.view.label(&mut cx, ids!(panel_workspace)).text(), "/work");
    assert_eq!(panel.view.button(&mut cx, ids!(panel_service_toggle)).text(), "Resume");
}

#[test]
fn opening_another_agent_drops_the_previous_one() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut panel = panel(&mut cx);
    panel.status = json!({"here": true});
    panel.model = json!({"policy": {}});
    let before = panel.request;
    panel.open(&mut cx, &json!({"id": "agt_2", "displayName": "Second"}));
    assert_ne!(panel.request, before, "late replies for the earlier Agent are ignored");
    assert!(panel.status.is_null() && panel.model.is_null());
    assert_eq!(panel.view.label(&mut cx, ids!(name)).text(), "Second");
    assert!(panel.view.view(&mut cx, ids!(identity_page)).visible());
}

#[test]
fn activity_rows_show_waiting_approvals_then_requests_newest_first() {
    let value = json!({
        "approvals": [{"proposalId": "p1", "argsDigest": "d", "proposal": {"dispatch": "d2", "tool": "Bash", "arguments": {"command": "gh pr list"}}}],
        "requests": [
            {"dispatchId": "d2", "roomId": "!abcdefghijklmnop:s", "requester": "@bob:s", "receivedAt": 20, "outcome": "running", "tokens": null},
            {"dispatchId": "d1", "roomId": "!ops:s", "roomName": "Ops", "requester": "@ann:s", "requesterName": "Ann", "receivedAt": 10, "outcome": "answered", "tokens": 1500},
            {"dispatchId": "d0", "roomId": "!ops:s", "roomName": "Ops", "requester": "@ann:s", "receivedAt": 5, "outcome": "declined", "tokens": 0},
        ]});
    let lines = activity_rows::activity_lines(&value, |t| format!("t{t}"));
    assert_eq!(lines, vec![
        "Waiting for your approval: Bash · gh pr list",
        "t20 · @bob:s · !abcdefghi… · Waiting for approval",
        "t10 · Ann · Ops · Answered · 1500 tokens",
        "t5 · @ann:s · Ops · Declined",
    ]);
    assert_eq!(activity_rows::outcome_label("running", false), "Running");
    assert_eq!(activity_rows::outcome_label("failed", false), "Failed");
    assert!(activity_rows::activity_lines(&json!({}), |_| String::new()).is_empty());
}

#[test]
fn activity_tab_says_where_activity_lives_and_when_it_is_empty() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut panel = panel(&mut cx);
    panel.status = json!({"here": false});
    panel.render_activity(&mut cx);
    assert_eq!(panel.view.label(&mut cx, ids!(activity)).text(), "Activity is kept on the device that runs this agent.");
    panel.status = json!({"here": true});
    panel.activity = json!({"requests": [], "approvals": null});
    panel.render_activity(&mut cx);
    assert_eq!(panel.view.label(&mut cx, ids!(activity)).text(), "No requests yet.");
}


#[test]
fn approvals_card_shows_and_saves_pull_request_merges() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut panel = panel(&mut cx);
    panel.status = json!({"here": true, "approvals": {"mode": "remember", "rules": [], "autoMerge": true}});
    panel.render(&mut cx);
    assert_eq!(panel.view.drop_down(&mut cx, ids!(auto_merge)).selected_item(), 1, "saved autoMerge shows Agents merge");
    panel.status["approvals"]["autoMerge"] = json!(false);
    panel.render(&mut cx);
    assert_eq!(panel.view.drop_down(&mut cx, ids!(auto_merge)).selected_item(), 0, "off shows People merge");
    assert_eq!(mapping::auto_merge_change(1), Some(json!({"autoMerge": true})), "choosing Agents merge posts autoMerge");
    assert_eq!(mapping::auto_merge_change(0), Some(json!({"autoMerge": false})));
    assert_eq!(mapping::auto_merge_change(2), None);
}

/// On screen: one Agent saved "sonnet", another "Claude Sonnet"; both Model
/// dropdowns read "Claude Sonnet", before and after the choices load.
#[test]
fn model_dropdown_shows_one_label_for_alias_and_display_name() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut panel = panel(&mut cx);
    panel.status = json!({"here": true});
    for saved in ["sonnet", "Claude Sonnet"] {
        panel.model = json!({"modelProfile": {"model": saved, "credential_ref": "claude-managed:x"}});
        panel.models.clear();
        panel.render(&mut cx);
        assert_eq!(panel.view.drop_down(&mut cx, ids!(panel_model)).selected_label(), "Claude Sonnet", "{saved} while loading");
        panel.models = vec![ModelChoice { model: "sonnet".into(), label: "Claude Sonnet".into(), efforts: vec![], default_effort: String::new() },
            ModelChoice { model: "opus".into(), label: "Claude Opus".into(), efforts: vec![], default_effort: String::new() }];
        panel.render(&mut cx);
        let model = panel.view.drop_down(&mut cx, ids!(panel_model));
        assert_eq!((model.selected_item(), model.selected_label()), (0, "Claude Sonnet".to_owned()), "{saved} with choices");
    }
}

/// Regression (live 2026-11-16): the status reply's Rooms had no `roomId`, so
/// issue watching offered "No connected Rooms yet." for an Agent in "fix-it room".
#[test]
fn status_rooms_are_offered_to_issue_watching() {
    let binding = json!({"id": "bnd_1", "roomId": "!fix:alpha", "state": "active", "scopeKind": "project"});
    let status = json!({"rooms": [crate::host::hagency::local::room_entry(&binding, "fix-it room".into())]});
    assert_eq!(super::mapping::watch_rooms(&status), vec![("!fix:alpha".to_owned(), "fix-it room".to_owned())]);
}
