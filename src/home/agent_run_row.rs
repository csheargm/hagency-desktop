//! The Agent's answer to a request, drawn as the Agent's own message on the
//! left (its avatar and name), under the requester's bubble. Before this the
//! run card sat inside the requester's bubble, so the answer read as if the
//! asker had written it.
use makepad_widgets::*;
use crate::hagency::run_controls::Entry;
use crate::shared::avatar::AvatarWidgetRefExt;
use super::agent_run_card::AgentRunCardWidgetRefExt;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.AgentRunRow = View {
        visible: false
        width: Fill height: Fit flow: Right spacing: 10
        padding: Inset{top: 0 bottom: 10 left: (1.5 * mod.widgets.RINX_GUTTER) right: (1.5 * mod.widgets.RINX_GUTTER)}
        agent_profile := View {
            width: 49.0 height: Fit flow: Down align: Align{x: 0.5 y: 0.0} margin: Inset{top: 4.5}
            agent_avatar := Avatar {width: 32 height: 32}
        }
        agent_column := View {
            width: Fill height: Fit flow: Down
            agent_name := Label {
                width: Fill height: Fit padding: Inset{top: 8 bottom: 4}
                draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: 11}}
            }
            agent_run_card := mod.widgets.AgentRunCard {width: Fill{max: mod.widgets.RINX_READING_WIDTH}}
            agent_run_card_1 := mod.widgets.AgentRunCard {width: Fill{max: mod.widgets.RINX_READING_WIDTH}}
            agent_run_card_2 := mod.widgets.AgentRunCard {width: Fill{max: mod.widgets.RINX_READING_WIDTH}}
        }
    }
}

/// The card slots inside the row, in page order.
pub fn card_paths() -> [&'static [LiveId]; 3] {
    [
        ids!(agent_runs.agent_run_card).as_slice(),
        ids!(agent_runs.agent_run_card_1).as_slice(),
        ids!(agent_runs.agent_run_card_2).as_slice(),
    ]
}

/// The name shown over the answer: one Agent's name, or every distinct one.
pub fn row_name(entries: &[Entry]) -> String {
    let mut names: Vec<&str> = Vec::new();
    for entry in entries {
        let name = if entry.name.is_empty() { entry.agent.as_str() } else { entry.name.as_str() };
        if !names.contains(&name) { names.push(name); }
    }
    names.join(" · ")
}

/// Puts the progress thread's reply count on the answer, as "N updates".
pub fn set_updates(cx: &mut Cx, item: &WidgetRef, count: usize) {
    for path in card_paths() {
        item.agent_run_card(cx, path).set_updates(cx, count);
    }
}

/// Shows this request's runs as the Agent's message, or hides the row. With
/// no run yet, a request the viewer sent (`pending`) shows the Agent's row
/// with its waiting line (`agent_run_requester`).
pub fn populate(cx: &mut Cx, item: &WidgetRef, entries: &[Entry], pending: Option<super::agent_run_requester::PendingRequest>) {
    let pending = pending.filter(|_| entries.is_empty());
    let row = item.view(cx, ids!(agent_runs));
    row.set_visible(cx, !entries.is_empty() || pending.is_some());
    for (index, (path, shown)) in card_paths().into_iter().zip(super::agent_run_pager::slot_entries(entries)).enumerate() {
        let card = item.agent_run_card(cx, path);
        card.set_pending(cx, if index == 0 { pending.clone() } else { None });
        card.set_entries(cx, shown);
    }
    let (name, color_key) = match (entries.first(), &pending) {
        (Some(first), _) => (row_name(entries), first.agent.clone()),
        (None, Some(p)) => (p.name.clone(), p.name.clone()),
        (None, None) => return,
    };
    item.label(cx, ids!(agent_runs.agent_name)).set_text(cx, &name);
    item.avatar(cx, ids!(agent_runs.agent_avatar))
        .show_text(cx, Some(crate::shared::avatar::avatar_color(&color_key)), None, &name);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hagency::run_controls::fixture;
    use ::hagency::agent_run_controls::State;

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
        populate(&mut cx, &widget, std::slice::from_ref(&entry), None);
        assert!(widget.view(&mut cx, ids!(agent_runs)).visible());
        assert_eq!(widget.label(&mut cx, ids!(agent_runs.agent_name)).text(), "Claude");
        assert!(widget.widget(&mut cx, ids!(agent_runs.agent_run_card.answer)).visible());
        populate(&mut cx, &widget, &[], None);
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
        populate(&mut cx, &widget, std::slice::from_ref(&entry), None);
        assert!(widget.widget(&mut cx, &at(ids!(title))).is_empty(), "no per-reply header");
        assert!(!widget.view(&mut cx, &at(ids!(footer))).visible(), "a successful answer has no status line");
        assert!(widget.button(&mut cx, &at(ids!(links.details))).visible());
        assert!(widget.label(&mut cx, &at(ids!(what_i_did))).text().contains("running gh pr create"));
        assert!(!widget.button(&mut cx, &at(ids!(links.updates))).visible());
        set_updates(&mut cx, &widget, 5);
        let updates = widget.button(&mut cx, &at(ids!(links.updates)));
        assert!(updates.visible() && updates.text().contains('5'), "{}", updates.text());
        // ADR 0023: the link opens the updates in place, no thread to open.
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
        populate(&mut cx, &widget, std::slice::from_ref(&entry), None);
        assert!(!widget.button(&mut cx, &at(ids!(links.details))).visible(), "no steps: no What I did");
        let mut running = fixture("$b", State::Running, "");
        running.card.progress = Some(Progress { step: "running ls".into(), steps: 1, elapsed_secs: None });
        populate(&mut cx, &widget, std::slice::from_ref(&running), None);
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
        let pending = super::super::agent_run_requester::PendingRequest { name: "bird-claude".into(), sent };
        populate(&mut cx, &widget, &[], Some(pending.clone()));
        assert!(widget.view(&mut cx, ids!(agent_runs)).visible(), "shown at once, before any card");
        assert_eq!(widget.label(&mut cx, ids!(agent_runs.agent_name)).text(), "bird-claude");
        let status = widget.label(&mut cx, &at(ids!(footer.status))).text();
        assert!(status.starts_with("⏳") && status.contains("bird-claude"), "{status}");
        for hidden in [ids!(input), ids!(buttons), ids!(links), ids!(preview)] {
            assert!(!widget.widget(&mut cx, &at(hidden)).visible(), "only the waiting line");
        }
        let mut blocked = fixture("$run", State::Running, "");
        blocked.card.progress = Some(Progress { step: APPROVAL_WAIT_STEP.into(), steps: 1, elapsed_secs: None });
        populate(&mut cx, &widget, std::slice::from_ref(&blocked), Some(pending));
        let status = widget.label(&mut cx, &at(ids!(footer.status))).text();
        assert!(status.starts_with("⏳") && status.contains("owner"), "the owner's name (here its ID's local part): {status}");
        assert!(!widget.button(&mut cx, &at(ids!(footer.open_approval))).visible(), "the requester cannot decide");
        let done = fixture("$run", State::Completed, "4 open issues");
        populate(&mut cx, &widget, std::slice::from_ref(&done), None);
        assert!(widget.widget(&mut cx, &at(ids!(answer))).visible());
        assert!(!widget.view(&mut cx, &at(ids!(footer))).visible());
    }
}
