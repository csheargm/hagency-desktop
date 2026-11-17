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

/// Shows this request's runs as the Agent's message, or hides the row.
pub fn populate(cx: &mut Cx, item: &WidgetRef, entries: &[Entry]) {
    let row = item.view(cx, ids!(agent_runs));
    row.set_visible(cx, !entries.is_empty());
    for (path, shown) in card_paths().into_iter().zip(super::agent_run_pager::slot_entries(entries)) {
        item.agent_run_card(cx, path).set_entries(cx, shown);
    }
    let Some(first) = entries.first() else { return };
    let name = row_name(entries);
    item.label(cx, ids!(agent_runs.agent_name)).set_text(cx, &name);
    item.avatar(cx, ids!(agent_runs.agent_avatar))
        .show_text(cx, Some(crate::shared::avatar::avatar_color(&first.agent)), None, &name);
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
        populate(&mut cx, &widget, std::slice::from_ref(&entry));
        assert!(widget.view(&mut cx, ids!(agent_runs)).visible());
        assert_eq!(widget.label(&mut cx, ids!(agent_runs.agent_name)).text(), "Claude");
        assert!(widget.widget(&mut cx, ids!(agent_runs.agent_run_card.answer)).visible());
        populate(&mut cx, &widget, &[]);
        assert!(!widget.view(&mut cx, ids!(agent_runs)).visible());
    }
}
