//! Routing between the Agents page and the native agent panel
//! (`hagency::agent_panel`): opening an Agent shows the panel; its Advanced
//! request opens the full detail page (every int setting stays there), and
//! Retired refreshes the list.
use super::*;
use crate::hagency::agent_panel::{AgentPanelAction, AgentPanelWidgetRefExt};

impl HagencyPanel {
    /// Opens the Agent at `index` in the native agent panel.
    pub(super) fn open_agent_panel(&mut self, cx: &mut Cx, index: usize) {
        let Some(a) = self.agents.get(index).cloned() else { return };
        self.enter_detail(cx, "agent_panel", string(&a, "id"));
        self.view.drop_down(cx, ids!(agents)).set_selected_item(cx, index + 1);
        for id in [ids!(agent_list_holder), ids!(agent_create), ids!(agent_detail)] {
            self.view.view(cx, id).set_visible(cx, false);
        }
        let mut heading = self.view.label(cx, ids!(heading));
        script_apply_eval!(cx, heading, {i18n_text: ""});
        heading.set_text(cx, string(&a, "displayName"));
        let panel = self.view.widget(cx, ids!(agent_panel));
        panel.set_visible(cx, true);
        panel.as_agent_panel().open(cx, &a);
        self.sync_navigation(cx);
    }

    /// The panel's own requests back to the Agents page.
    pub(super) fn handle_agent_panel(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions {
            match action.downcast_ref::<AgentPanelAction>() {
                Some(AgentPanelAction::Advanced { agent_id }) => {
                    if let Some(index) = self.agents.iter().position(|a| string(a, "id") == agent_id) {
                        self.agent_detail(cx, index);
                    }
                }
                Some(AgentPanelAction::Retired { .. }) => {
                    self.list_page(cx);
                    self.run(cx, Target::Agents, Command::Agents);
                }
                None => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel(cx: &mut Cx) -> HagencyPanel {
        cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let value = script_eval!(vm, {mod.widgets.HagencyPanel {}});
            let panel = HagencyPanel::script_from_value(vm, value);
            assert!(vm.take_errors().is_empty());
            panel
        })
    }

    /// The user flow: open an Agent from the list -> the native panel shows;
    /// Advanced settings -> the full detail page (every int setting) shows instead.
    #[test]
    fn opening_an_agent_shows_the_panel_and_advanced_opens_the_full_detail() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = panel(&mut cx);
        panel.identity = "owner".into();
        panel.agents = vec![json!({"id": "a", "displayName": "First"})];
        panel.set_page(&mut cx, Page::Agents);
        panel.open_agent_panel(&mut cx, 0);
        assert!(panel.view.widget(&mut cx, ids!(agent_panel)).visible());
        assert!(!panel.view.view(&mut cx, ids!(agent_detail)).visible());
        assert_eq!(panel.view.label(&mut cx, ids!(heading)).text(), "First");

        let advanced: ActionsBuf = vec![Box::new(AgentPanelAction::Advanced { agent_id: "a".into() })];
        panel.handle_agent_panel(&mut cx, &advanced);
        assert!(panel.view.view(&mut cx, ids!(agent_detail)).visible(), "Advanced keeps the full detail page reachable");
        assert!(!panel.view.widget(&mut cx, ids!(agent_panel)).visible());

        panel.list_page(&mut cx);
        assert!(!panel.view.widget(&mut cx, ids!(agent_panel)).visible(), "back to the list hides the panel");
    }
}
