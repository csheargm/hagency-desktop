//! Routing between the Agents page and the native agent panel
//! (`hagency::agent_panel`): opening an Agent shows the panel; its Advanced
//! request opens the full detail page (every int setting stays there), and
//! Retired refreshes the list.
use super::*;
use crate::hagency::agent_panel::{AgentPanelAction, AgentPanelWidgetRefExt};

impl HagencyPanel {
    // TODO(id-collision): limit_mode, model, period, retire_confirm, token_limit,
    // workspace and service_toggle are also bare ids in both pages; scope them too.
    /// The detail page's own issue-watch block. A bare `ids!(issue_watch)`
    /// finds the native panel's block first and overwrote its context.
    pub(super) fn detail_issue_watch(&self, cx: &mut Cx) -> WidgetRef {
        self.view.widget(cx, ids!(model_section)).widget(cx, ids!(issue_watch))
    }
    /// The detail page's own GitHub access block (same collision).
    pub(super) fn detail_github_access(&self, cx: &mut Cx) -> WidgetRef {
        self.view.widget(cx, ids!(model_section)).widget(cx, ids!(github_access))
    }
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

    /// Back from an Agent opened from My Agents: close management so the
    /// Hagency app (still on My Agents) shows again. False means the usual
    /// Back to the Local agents list.
    pub(super) fn leave_for_my_agents(&mut self, cx: &mut Cx) -> bool {
        if !self.back_to_my_agents || !self.view.widget(cx, ids!(agent_panel)).visible() { return false; }
        self.back_to_my_agents = false;
        self.list_page(cx);
        cx.action(HagencyAction::Close);
        true
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

    /// The user flow: My Agents card -> `OpenAgent` -> panel; Back closes
    /// management (back to My Agents) instead of showing the Local agents list.
    #[test]
    fn back_from_an_agent_opened_from_my_agents_returns_to_my_agents() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = panel(&mut cx);
        panel.identity = "owner".into();
        panel.agents = vec![json!({"id": "a", "displayName": "First"})];
        panel.back_to_my_agents = true; // what `action(OpenAgent)` records
        panel.set_page(&mut cx, Page::Agents);
        panel.open_agent_panel(&mut cx, 0);
        let emitted = cx.capture_actions(|cx| assert!(panel.leave_for_my_agents(cx)));
        assert!(emitted.iter().any(|a| matches!(a.downcast_ref(), Some(HagencyAction::Close))), "Back closes management");
        assert!(!panel.view.widget(&mut cx, ids!(agent_panel)).visible());

        // Opened from the Local agents list instead: Back stays the list's Back.
        panel.open_agent_panel(&mut cx, 0);
        let emitted = cx.capture_actions(|cx| assert!(!panel.leave_for_my_agents(cx)));
        assert!(emitted.is_empty());
    }

    /// Regression: the detail page's own issue-watch/GitHub blocks were looked
    /// up by bare id, which found the native panel's copies first (the panel
    /// sits earlier in the tree). Every `sync_navigation` then reset the open
    /// panel's issue watching to "not on this device" with no Rooms.
    #[test]
    fn detail_page_settings_blocks_are_not_the_open_panels() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = panel(&mut cx);
        panel.identity = "owner".into();
        panel.agents = vec![json!({"id": "a", "displayName": "First"})];
        panel.set_page(&mut cx, Page::Agents);
        panel.open_agent_panel(&mut cx, 0);
        let inner = panel.view.widget(&mut cx, ids!(agent_panel));
        // The collision itself: a bare id resolves into the native panel.
        assert_eq!(panel.view.widget(&mut cx, ids!(issue_watch)).widget_uid(), inner.widget(&mut cx, ids!(issue_watch)).widget_uid());
        for id in [ids!(issue_watch), ids!(github_access)] {
            let theirs = inner.widget(&mut cx, id).widget_uid();
            assert_ne!(panel.view.widget(&mut cx, ids!(model_section)).widget(&mut cx, id).widget_uid(), theirs);
        }
        assert_ne!(panel.detail_issue_watch(&mut cx).widget_uid(), inner.widget(&mut cx, ids!(issue_watch)).widget_uid());
        assert_ne!(panel.detail_github_access(&mut cx).widget_uid(), inner.widget(&mut cx, ids!(github_access)).widget_uid());
    }
}
