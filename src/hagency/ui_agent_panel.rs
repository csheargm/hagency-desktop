//! Routing between the Agents page and the native agent panel
//! (`hagency::agent_panel`): opening an Agent shows the panel; its Advanced
//! request opens the full detail page (every int setting stays there), and
//! Retired refreshes the list.
use super::*;
use crate::hagency::agent_panel::{AgentPanelAction, AgentPanelWidgetRefExt};

impl HagencyPanel {
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

    /// Opens the Agent an `OpenAgent` / `OpenAgentSettings` asked for, on the tab it asked for.
    pub(super) fn open_wanted_agent_panel(&mut self, cx: &mut Cx, index: usize) {
        self.open_agent_panel(cx, index);
        if std::mem::take(&mut self.wanted_agent_settings) {
            self.view.widget(cx, ids!(agent_panel)).as_agent_panel().show_settings(cx);
        }
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
                Some(AgentPanelAction::Renamed { agent_id, display_name }) => {
                    if let Some(agent) = self.agents.iter_mut().find(|a| string(a, "id") == agent_id) {
                        agent["displayName"] = serde_json::Value::from(display_name.as_str());
                    }
                    // Only the panel's own Agent emits this, so its heading follows.
                    if self.view.widget(cx, ids!(agent_panel)).visible() {
                        self.view.label(cx, ids!(heading)).set_text(cx, display_name);
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
        let renamed: ActionsBuf = vec![Box::new(AgentPanelAction::Renamed { agent_id: "a".into(), display_name: "Second".into() })];
        panel.handle_agent_panel(&mut cx, &renamed);
        assert_eq!(panel.view.label(&mut cx, ids!(heading)).text(), "Second", "a rename updates the heading");
        assert_eq!(panel.agents[0]["displayName"], "Second", "and the list");

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

    /// The user flow: Chats menu -> Settings in an Agent's private chat ->
    /// `OpenAgentSettings` -> once the Agents list arrives, the panel shows
    /// that Agent on its Settings tab; a later plain open is on Identity again.
    #[test]
    fn open_agent_settings_opens_the_panel_on_settings() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = panel(&mut cx);
        panel.identity = "owner".into();
        panel.set_page(&mut cx, Page::Agents);
        // What `action(OpenAgentSettings {agent_id: "a"})` records before the listing.
        (panel.wanted_agent, panel.wanted_agent_settings, panel.back_to_my_agents) = (Some("a".into()), true, true);
        panel.apply(&mut cx, Target::Agents, &json!({"agents": [{"id": "a", "displayName": "yue-claude"}], "projects": []}));
        let inner = panel.view.widget(&mut cx, ids!(agent_panel));
        assert!(inner.visible(), "the Agent's panel opens");
        assert!(inner.view(&mut cx, ids!(settings_page)).visible(), "on its Settings tab");
        assert!(!inner.view(&mut cx, ids!(identity_page)).visible());
        assert!(!panel.wanted_agent_settings, "the tab request is used once");
        panel.open_wanted_agent_panel(&mut cx, 0);
        assert!(panel.view.widget(&mut cx, ids!(agent_panel)).view(&mut cx, ids!(identity_page)).visible());
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

    /// Regression: every id the detail page looks up bare that the native
    /// panel also defined resolved into the panel (it sits earlier in the
    /// tree), so the detail page read and wrote the open panel's controls.
    /// The panel's copies now carry a `panel_` prefix.
    #[test]
    fn detail_page_bare_ids_never_reach_the_open_panel() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = panel(&mut cx);
        panel.identity = "owner".into();
        panel.agents = vec![json!({"id": "a", "displayName": "First"})];
        panel.set_page(&mut cx, Page::Agents);
        panel.open_agent_panel(&mut cx, 0);
        let inner = panel.view.widget(&mut cx, ids!(agent_panel));
        let detail = panel.view.widget(&mut cx, ids!(agent_detail));
        let shared = [
            (ids!(limit_mode), ids!(panel_limit_mode)), (ids!(model), ids!(panel_model)),
            (ids!(period), ids!(panel_period)), (ids!(retire_confirm), ids!(panel_retire_confirm)),
            (ids!(token_limit), ids!(panel_token_limit)), (ids!(workspace), ids!(panel_workspace)),
            (ids!(service_toggle), ids!(panel_service_toggle)),
        ];
        for (bare, renamed) in shared {
            let found = panel.view.widget(&mut cx, bare);
            assert!(!found.is_empty(), "{bare:?} missing from the detail page");
            assert_eq!(found.widget_uid(), detail.widget(&mut cx, bare).widget_uid(), "{bare:?} left the detail page");
            assert!(inner.widget(&mut cx, bare).is_empty(), "{bare:?} still defined in the native panel");
            assert!(!inner.widget(&mut cx, renamed).is_empty(), "{renamed:?} missing from the native panel");
        }
    }
}
