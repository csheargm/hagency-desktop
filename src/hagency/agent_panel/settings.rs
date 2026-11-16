//! Settings tab: approval mode and remembered rules (Revoke), issue watching
//! and GitHub access (the int build's own blocks), budget and period, workspace folder, Pause/Resume service and Retire.
use super::mapping;
use super::*;
use crate::hagency::model::AgentAction;

impl AgentPanel {
    fn rules(&self) -> Vec<Value> {
        self.status["approvals"]["rules"].as_array().cloned().unwrap_or_default()
    }
    pub(super) fn render_settings(&mut self, cx: &mut Cx) {
        let (here, idle) = (self.here(), !self.busy);
        self.render_approvals(cx, here && idle);
        self.render_budget(cx, here && idle);
        let workspace = self.model["modelProfile"]["workspace_root"].as_str().filter(|w| !w.is_empty());
        self.view.label(cx, ids!(workspace)).set_text(cx, workspace.unwrap_or(crate::i18n::tr("Chosen when the agent first runs on this Mac")));
        let state = self.status["service"]["state"].as_str().unwrap_or("");
        let detail = self.status["service"]["detail"].as_str().unwrap_or("");
        let line = crate::i18n::tr(mapping::service_line(state));
        self.view.label(cx, ids!(service_state)).set_text(cx, &if detail.is_empty() { line.to_owned() } else { format!("{line} · {detail}") });
        let toggle = self.view.button(cx, ids!(service_toggle));
        toggle.set_text(cx, crate::i18n::tr(if mapping::service_running(state) { "Pause" } else { "Resume" }));
        toggle.set_enabled(cx, here && idle && !state.is_empty() && state != "unknown");
        let retirable = !matches!(self.agent["state"].as_str(), Some("retiring" | "retired"));
        let confirmed = self.view.check_box(cx, ids!(retire_confirm)).active(cx);
        self.view.button(cx, ids!(retire)).set_enabled(cx, idle && retirable && confirmed);
    }
    fn render_approvals(&mut self, cx: &mut Cx, editable: bool) {
        let mode = self.status["approvals"]["mode"].as_str().unwrap_or("");
        let labels = mapping::approval_mode_labels().iter().map(|l| crate::i18n::tr(l).to_owned()).collect();
        self.view.drop_down(cx, ids!(approval_mode)).set_labels(cx, labels);
        self.view.drop_down(cx, ids!(approval_mode)).set_selected_item(cx, mapping::approval_mode_index(mode));
        let loaded = self.status["approvals"].is_object();
        self.view.widget(cx, ids!(approval_mode)).set_disabled(cx, !editable || !loaded);
        let hint = if !self.here() && !self.status.is_null() {
            "Approvals are kept on the device that runs this agent."
        } else {
            mapping::approval_mode_hint(mode)
        };
        self.view.label(cx, ids!(approval_hint)).set_text(cx, crate::i18n::tr(hint));
        let rules = self.rules();
        let labels: Vec<String> = if rules.is_empty() {
            vec![crate::i18n::tr("No remembered approvals").into()]
        } else { rules.iter().map(mapping::rule_label).collect() };
        let selected = self.view.drop_down(cx, ids!(rules)).selected_item().min(labels.len() - 1);
        self.view.drop_down(cx, ids!(rules)).set_labels(cx, labels);
        self.view.drop_down(cx, ids!(rules)).set_selected_item(cx, selected);
        self.view.widget(cx, ids!(rules)).set_disabled(cx, rules.is_empty());
        self.view.button(cx, ids!(revoke)).set_enabled(cx, editable && !rules.is_empty());
        let merge = mapping::auto_merge_labels().iter().map(|l| crate::i18n::tr(l).to_owned()).collect();
        self.view.drop_down(cx, ids!(auto_merge)).set_labels(cx, merge);
        let on = self.status["approvals"]["autoMerge"] == true;
        self.view.drop_down(cx, ids!(auto_merge)).set_selected_item(cx, usize::from(on));
        self.view.widget(cx, ids!(auto_merge)).set_disabled(cx, !editable || !loaded);
    }
    fn render_budget(&mut self, cx: &mut Cx, editable: bool) {
        let budget = &self.model["policy"]["policy"]["budget"];
        let limited = self.view.drop_down(cx, ids!(limit_mode)).selected_item() == 1;
        self.view.view(cx, ids!(token_holder)).set_visible(cx, limited);
        self.view.widget(cx, ids!(period)).set_visible(cx, limited);
        let editable = editable && budget.is_object();
        for id in [ids!(limit_mode), ids!(token_limit), ids!(period)] {
            self.view.widget(cx, id).set_disabled(cx, !editable);
        }
        self.view.button(cx, ids!(save_budget)).set_enabled(cx, editable);
    }

    /// Puts the saved budget into the controls; only when a policy reply arrives,
    /// so a later re-render never overwrites what the owner is typing.
    pub(super) fn fill_budget(&mut self, cx: &mut Cx) {
        let budget = &self.model["policy"]["policy"]["budget"];
        let tokens = budget["limit"]["Tokens"].as_u64();
        self.view.drop_down(cx, ids!(limit_mode)).set_selected_item(cx, usize::from(tokens.is_some()));
        self.view.text_input(cx, ids!(token_limit)).set_text(cx, &tokens.map(|t| t.to_string()).unwrap_or_default());
        self.view.drop_down(cx, ids!(period)).set_selected_item(cx, mapping::period_index(budget["period"].as_str().unwrap_or("")));
    }

    /// Hands this Agent and its Rooms to the issue-watch and GitHub access
    /// blocks; each keeps its own state and saves through its own owner call.
    pub(super) fn fill_settings_sections(&mut self, cx: &mut Cx) {
        use crate::hagency::issue_watch::section::IssueWatchSectionWidgetExt;
        use crate::hagency::github_access::GithubAccessSectionWidgetExt;
        let agent = self.agent_id();
        let rooms = mapping::watch_rooms(&self.status);
        // `None` until the status arrives, so the block says it is waiting instead of greying out silently.
        let editable = self.status.is_object().then(|| self.here());
        self.view.issue_watch_section(cx, ids!(issue_watch)).set_context(cx, &agent, editable, rooms);
        self.view.github_access_section(cx, ids!(github_access)).set_context(cx, &agent);
    }

    pub(super) fn handle_settings_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if let Some(change) = self.view.drop_down(cx, ids!(approval_mode)).selected(actions).and_then(mapping::approval_mode_change) {
            self.save_approvals(cx, change);
        }
        if let Some(change) = self.view.drop_down(cx, ids!(auto_merge)).selected(actions).and_then(mapping::auto_merge_change) {
            self.save_approvals(cx, change);
        }
        if self.view.button(cx, ids!(revoke)).clicked(actions)
            && let Some(rule) = self.rules().get(self.view.drop_down(cx, ids!(rules)).selected_item()).cloned() {
            self.save_approvals(cx, mapping::forget_change(&rule));
        }
        if self.view.drop_down(cx, ids!(limit_mode)).selected(actions).is_some() {
            let limited = self.view.drop_down(cx, ids!(limit_mode)).selected_item() == 1;
            self.view.view(cx, ids!(token_holder)).set_visible(cx, limited);
            self.view.widget(cx, ids!(period)).set_visible(cx, limited);
            self.view.redraw(cx);
        }
        if self.view.button(cx, ids!(save_budget)).clicked(actions) { self.save_budget(cx); }
        if self.view.button(cx, ids!(service_toggle)).clicked(actions) {
            let running = mapping::service_running(self.status["service"]["state"].as_str().unwrap_or(""));
            self.note(cx, if running { "Pausing…" } else { "Resuming…" });
            self.call(Load::Status, true, Call::Service(self.agent_id(), !running));
            self.render(cx);
        }
        if self.view.check_box(cx, ids!(retire_confirm)).changed(actions).is_some() { self.render(cx); }
        if self.view.button(cx, ids!(retire)).clicked(actions) && self.view.check_box(cx, ids!(retire_confirm)).active(cx) {
            self.note(cx, "Retiring…");
            self.command(Load::Retired, true, Command::Agent { agent_id: self.agent_id(), action: AgentAction::Retire });
            self.render(cx);
        }
    }
    fn save_approvals(&mut self, cx: &mut Cx, change: Value) {
        self.note(cx, "Saving…");
        self.call(Load::Status, true, Call::Approvals(self.agent_id(), change));
        self.render(cx);
    }
    fn save_budget(&mut self, cx: &mut Cx) {
        let limited = self.view.drop_down(cx, ids!(limit_mode)).selected_item() == 1;
        let tokens = self.view.text_input(cx, ids!(token_limit)).text();
        let period = self.view.drop_down(cx, ids!(period)).selected_item();
        let budget = match mapping::budget_value(limited, &tokens, period) {
            Ok(budget) => budget,
            Err(reason) => return self.note(cx, reason),
        };
        match mapping::policy_change(&self.model, "budget", budget) {
            Some(change) => {
                self.note(cx, "Saving…");
                self.local(Load::Model, LocalAction::SaveAgentPolicy, Some(change));
            }
            None => self.note(cx, "Load this agent's policy before changing it."),
        }
        self.render(cx);
    }
}
