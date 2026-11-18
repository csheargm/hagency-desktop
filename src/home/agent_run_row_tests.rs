use super::*;
use crate::hagency::run_controls::fixture;
use ::hagency::agent_run_controls::State;

fn room() -> OwnedRoomId { OwnedRoomId::try_from("!dev:alpha").unwrap() }

/// Live finding (Nov 18, Anna's dev room): her own Agent's answer had no
/// "agent" tag, its avatar did nothing on click, and a new reply showed
/// the raw `_hagency_agt_…` ID first. The row now reads like the Agent's
/// own message: its name, the tag, and an avatar that opens its profile.
#[cfg(feature = "agent_chat")]
#[test]
fn the_answer_row_is_tagged_named_and_its_avatar_opens_the_profile() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let widget = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::app::register_widgets(vm);
        let value = script_eval!(vm, {mod.widgets.OwnMessage {}});
        let widget = WidgetRef::script_from_value(vm, value);
        widget.register_as_ui_root(vm);
        widget
    });
    let mut entry = fixture("$a", State::Running, "");
    entry.agent = "@_hagency_agt_a:alpha".into();
    entry.name = "_hagency_agt_a".into();
    populate(&mut cx, &widget, &room(), std::slice::from_ref(&entry), None);
    assert_eq!(widget.label(&mut cx, ids!(agent_runs.agent_name)).text(), crate::i18n::tr("Agent"), "never the raw ID");
    assert!(widget.widget(&mut cx, ids!(agent_runs.agent_badge)).visible(), "tagged like every Agent's message");
    assert_eq!(widget.label(&mut cx, ids!(agent_runs.agent_badge.agent_badge_label)).text(), crate::i18n::tr("agent"));
    entry.name = "anna-claude".into();
    populate(&mut cx, &widget, &room(), std::slice::from_ref(&entry), None);
    assert_eq!(widget.label(&mut cx, ids!(agent_runs.agent_name)).text(), "anna-claude");
    let avatar = widget.avatar(&mut cx, ids!(agent_runs.agent_avatar));
    let info = avatar.profile_info().expect("a click shows this Agent, not nothing");
    assert_eq!((info.user_id.as_str(), info.room_id.as_str()), ("@_hagency_agt_a:alpha", "!dev:alpha"));
    let mut other = entry.clone();
    (other.key, other.agent, other.name) = ("$b".into(), "@_hagency_agt_b:beta".into(), "bird-codex".into());
    populate(&mut cx, &widget, &room(), &[entry, other], None);
    assert_eq!(widget.label(&mut cx, ids!(agent_runs.agent_name)).text(), "anna-claude · bird-codex");
    let info = widget.avatar(&mut cx, ids!(agent_runs.agent_avatar)).profile_info().expect("the first Agent's profile");
    assert_eq!((info.user_id.as_str(), info.username.as_deref()), ("@_hagency_agt_a:alpha", Some("anna-claude")), "its own name, not both");
}

#[test]
fn the_row_names_each_answering_agent_once() {
    let mut a = fixture("$a", State::Completed, "x");
    a.name = "Claude".into();
    let mut b = a.clone();
    b.key = "$b".into();
    let mut c = a.clone();
    c.name = "Codex".into();
    assert_eq!(row_name(&[a.clone(), b]), "Claude");
    assert_eq!(row_name(&[a, c]), "Claude · Codex");
    assert_eq!(row_name(&[]), "");
}

/// Live finding: the answer drew inside the asker's right-side bubble. It
/// now sits in its own left-side row, outside every bubble.
#[cfg(feature = "agent_chat")]
#[test]
fn the_answer_is_its_own_left_side_message_not_inside_the_request_bubble() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let widget = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::app::register_widgets(vm);
        let value = script_eval!(vm, {mod.widgets.OwnMessage {}});
        let widget = WidgetRef::script_from_value(vm, value);
        widget.register_as_ui_root(vm);
        widget
    });
    assert!(widget.widget(&mut cx, ids!(content.agent_run_card)).is_empty(), "no run card inside the bubble");
    assert!(widget.widget(&mut cx, ids!(bubble.agent_run_card)).is_empty(), "no run card inside the bubble");
    let mut entry = fixture("$a", State::Completed, "42");
    entry.name = "Claude".into();
    populate(&mut cx, &widget, &room(), std::slice::from_ref(&entry), None);
    assert!(widget.view(&mut cx, ids!(agent_runs)).visible());
    assert_eq!(widget.label(&mut cx, ids!(agent_runs.agent_name)).text(), "Claude");
    assert!(widget.widget(&mut cx, ids!(agent_runs.agent_run_card.answer)).visible());
    populate(&mut cx, &widget, &room(), &[], None);
    assert!(!widget.view(&mut cx, ids!(agent_runs)).visible());
}

/// Quiet replies on the request's real row: the answer shows with no
/// per-reply "Agent · runtime" header and no "Done" footer; "What I did"
/// lists the steps; the progress thread is an "N updates" link that opens
/// the updates inside the card; a working run keeps one compact status line.
#[cfg(feature = "agent_chat")]
#[test]
fn an_answered_run_is_quiet_with_what_i_did_and_an_updates_link() {
    use ::hagency::agent_run_controls::Progress;
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let widget = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::app::register_widgets(vm);
        let value = script_eval!(vm, {mod.widgets.OwnMessage {}});
        let widget = WidgetRef::script_from_value(vm, value);
        widget.register_as_ui_root(vm);
        widget
    });
    let card = ids!(agent_runs.agent_run_card);
    let at = |path: &[LiveId]| [card.as_slice(), path].concat();
    let mut entry = fixture("$a", State::Completed, "42");
    entry.name = "bird-claude".into();
    entry.card.progress = Some(Progress { step: "running gh pr create".into(), steps: 3, elapsed_secs: Some(4) });
    populate(&mut cx, &widget, &room(), std::slice::from_ref(&entry), None);
    assert!(widget.widget(&mut cx, &at(ids!(title))).is_empty(), "no per-reply header");
    assert!(!widget.view(&mut cx, &at(ids!(footer))).visible(), "a successful answer has no status line");
    assert!(widget.button(&mut cx, &at(ids!(links.details))).visible());
    assert!(widget.label(&mut cx, &at(ids!(what_i_did))).text().contains("running gh pr create"));
    assert!(!widget.button(&mut cx, &at(ids!(links.updates))).visible());
    set_updates(&mut cx, &widget, 5);
    let updates = widget.button(&mut cx, &at(ids!(links.updates)));
    assert!(updates.visible() && updates.text().contains('5'), "{}", updates.text());
    // ADR 0024: the link opens the updates in place, no thread to open.
    let click = |cx: &mut Cx| {
        let uid = widget.button(cx, &at(ids!(links.updates))).widget_uid();
        let actions = cx.capture_actions(|cx| cx.widget_action(uid, ButtonAction::Clicked(Default::default())));
        let card_ref = widget.widget(cx, card);
        cx.capture_actions(|cx| card_ref.handle_event(cx, &Event::Actions(actions), &mut Scope::empty()))
    };
    let sent = click(&mut cx);
    assert!(!sent.iter().any(|a| a.downcast_ref::<crate::app::AppStateAction>().is_some()), "the room does not navigate");
    set_updates(&mut cx, &widget, 5);
    assert_eq!(widget.button(&mut cx, &at(ids!(links.updates))).text(), crate::i18n::tr("Hide updates"));
    let list = widget.label(&mut cx, &at(ids!(updates_list)));
    assert!(list.visible() && !list.text().is_empty(), "the updates show inside the card");
    click(&mut cx);
    set_updates(&mut cx, &widget, 5);
    assert!(!widget.label(&mut cx, &at(ids!(updates_list))).visible(), "closed again");
    assert!(widget.button(&mut cx, &at(ids!(links.updates))).text().contains('5'));
    entry.card.progress = None;
    populate(&mut cx, &widget, &room(), std::slice::from_ref(&entry), None);
    assert!(!widget.button(&mut cx, &at(ids!(links.details))).visible(), "no steps: no What I did");
    let mut running = fixture("$b", State::Running, "");
    running.card.progress = Some(Progress { step: "running ls".into(), steps: 1, elapsed_secs: None });
    populate(&mut cx, &widget, &room(), std::slice::from_ref(&running), None);
    assert!(widget.view(&mut cx, &at(ids!(footer))).visible());
    assert!(widget.label(&mut cx, &at(ids!(footer.status))).text().contains("running ls"));
    assert!(!widget.button(&mut cx, &at(ids!(footer.open_approval))).visible());
}

/// Chris's test (Nov 17), Anna's window: she asks Betty's Agent. At once
/// her message shows the Agent's row with "⏳ Waiting for bird-claude to
/// start… (N s)" and nothing else; when the run card arrives waiting for
/// Betty's approval it reads "⏳ Waiting for <owner>'s approval (N s)",
/// with no "Open approval" (Anna cannot decide); then the answer.
#[cfg(feature = "agent_chat")]
#[test]
fn the_requester_sees_waiting_then_the_owners_approval_then_the_answer() {
    use ::hagency::agent_run_controls::{APPROVAL_WAIT_STEP, Progress};
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let widget = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::app::register_widgets(vm);
        let value = script_eval!(vm, {mod.widgets.OwnMessage {}});
        let widget = WidgetRef::script_from_value(vm, value);
        widget.register_as_ui_root(vm);
        widget
    });
    let card = ids!(agent_runs.agent_run_card);
    let at = |path: &[LiveId]| [card.as_slice(), path].concat();
    let sent = std::time::Instant::now().checked_sub(std::time::Duration::from_secs(3)).unwrap();
    let pending = super::super::agent_run_requester::PendingRequest { agent: OwnedUserId::try_from("@_hagency_agt_b:beta").unwrap(), name: "bird-claude".into(), sent };
    populate(&mut cx, &widget, &room(), &[], Some(pending.clone()));
    assert!(widget.view(&mut cx, ids!(agent_runs)).visible(), "shown at once, before any card");
    assert_eq!(widget.label(&mut cx, ids!(agent_runs.agent_name)).text(), "bird-claude");
    let status = widget.label(&mut cx, &at(ids!(footer.status))).text();
    assert!(status.starts_with("⏳") && status.contains("bird-claude"), "{status}");
    for hidden in [ids!(input), ids!(buttons), ids!(links), ids!(preview)] {
        assert!(!widget.widget(&mut cx, &at(hidden)).visible(), "only the waiting line");
    }
    let mut blocked = fixture("$run", State::Running, "");
    blocked.card.progress = Some(Progress { step: APPROVAL_WAIT_STEP.into(), steps: 1, elapsed_secs: None });
    populate(&mut cx, &widget, &room(), std::slice::from_ref(&blocked), Some(pending));
    let status = widget.label(&mut cx, &at(ids!(footer.status))).text();
    assert!(status.starts_with("⏳") && status.contains("owner"), "the owner's name (here its ID's local part): {status}");
    assert!(!widget.button(&mut cx, &at(ids!(footer.open_approval))).visible(), "the requester cannot decide");
    let done = fixture("$run", State::Completed, "4 open issues");
    populate(&mut cx, &widget, &room(), std::slice::from_ref(&done), None);
    assert!(widget.widget(&mut cx, &at(ids!(answer))).visible());
    assert!(!widget.view(&mut cx, &at(ids!(footer))).visible());
}
