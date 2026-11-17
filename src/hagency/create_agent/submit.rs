//! Create agent: Create and its follow-up (bind rooms, backup model, work
//! list, then open the private chat).
use super::*;

impl CreateAgentDialog {
    /// Create: the "What should it work on?" step is checked on GitHub
    /// first when it lists repositories; otherwise the Agent is created now.
    pub(super) fn create(&mut self, cx: &mut Cx) {
        if self.busy { return; }
        if let Err(reason) = self.state.spec(quick_setup::default_spec) {
            self.view.label(cx, ids!(status)).set_text(cx, crate::i18n::tr(reason));
            return;
        }
        let plan = match self.work_plan(cx) {
            Ok(plan) => plan,
            Err(reason) => { self.view.label(cx, ids!(work_result)).set_text(cx, &reason); return; }
        };
        let checking = !plan.wanted.is_empty();
        self.work = Some(plan.clone());
        self.busy = true;
        self.render_create(cx);
        if checking { self.start_check(cx, &plan); } else { self.create_now(cx); }
    }
    /// Runs one-click creation with the chosen model and the step's budget.
    pub(super) fn create_now(&mut self, cx: &mut Cx) {
        let mut spec = match self.state.spec(quick_setup::default_spec) {
            Ok(spec) => spec,
            Err(reason) => { self.busy = false; self.view.label(cx, ids!(status)).set_text(cx, crate::i18n::tr(reason)); return self.render_create(cx); }
        };
        if let Some(budget) = self.work.as_ref().and_then(|p| p.draft.budget().ok().flatten()) {
            spec.daily_budget = budget;
        }
        let service = match Service::for_current() {
            Ok(service) => service,
            Err(code) => { self.busy = false; self.view.label(cx, ids!(status)).set_text(cx, &code); return self.render_create(cx); }
        };
        self.busy = true;
        self.render_create(cx);
        self.view.label(cx, ids!(status)).set_text(cx, crate::i18n::tr("Creating…"));
        self.timer = cx.start_timeout(0.5);
        let (request, identity) = (self.request, service.identity());
        spawn_async_task(async move {
            let result = quick_setup::create_spec(service, spec).await;
            Cx::post_action(Created { request, identity, result });
        });
    }
    /// Same follow-up as the Agents panel: refresh the tree, then open the private chat.
    pub(super) fn finish(&mut self, cx: &mut Cx, identity: &str, result: &Result<Value, String>) {
        self.busy = false;
        cx.stop_timer(self.timer);
        match result {
            Ok(value) => {
                crate::hagency::agent_presence::set_has_agent(true);
                self.bind_ticked(value);
                // The checklist was the owner's explicit choice of earlier rooms:
                // the catch-up must never add this Agent to rooms left unticked.
                if let Some(id) = value["creation"]["agent"]["id"].as_str() {
                    crate::hagency::bring_agent::skip_catch_up(id);
                }
                self.save_backup(value);
                self.save_work(value);
                cx.action(crate::home::project_tree::ProjectTreeChanged { identity: identity.to_owned() });
                if let Some(room_id) = value["quickSetup"]["ownerDirectRoomId"].as_str().and_then(|id| matrix_sdk::ruma::OwnedRoomId::try_from(id).ok()) {
                    cx.action(crate::hagency::ui::OpenAgentChat { room_id });
                }
                cx.action(CreateAgentAction::Close);
            }
            Err(code) => {
                let step = simple_agent::last_failed_step().unwrap_or("setup");
                let message = super::super::failure_message(step, code);
                self.view.label(cx, ids!(status)).set_text(cx, &message);
                self.render_create(cx);
            }
        }
    }
}
