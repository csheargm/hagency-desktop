//! The owner's existing Agent names, read when the Create agent dialog opens,
//! so the suggested name is a free one ("anna-claude-2", not a second
//! "anna-claude").
use super::CreateAgentDialog;
use crate::hagency::{backend::Service, create_agent::agent_names, model::Command};
use crate::sliding_sync::spawn_async_task;
use makepad_widgets::*;

#[derive(Debug)]
pub(super) struct NamesLoaded {
    request: u64,
    names: Vec<String>,
}

impl CreateAgentDialog {
    /// Reads the Agents listing for this opening.
    pub(super) fn load_names(&mut self) {
        self.taken.clear();
        let request = self.request;
        spawn_async_task(async move {
            let listing = match Service::for_current() {
                Ok(service) => service.execute(Command::Agents).await,
                Err(code) => Err(code),
            };
            // A failed read only loses the numbering: the name stays editable
            // and Create still reports a refusal from the server.
            let names = listing.map(|listing| agent_names(&listing)).unwrap_or_default();
            Cx::post_action(NamesLoaded { request, names });
        });
    }

    /// The owner's own edit of the Name field: from now on it is never replaced.
    pub(super) fn name_typed(&mut self, cx: &mut Cx, name: String) {
        self.name_edited = true;
        self.state.name = name;
        self.render_create(cx);
    }

    /// Until the owner types a name, the field takes the first free name for
    /// the harness shown.
    pub(super) fn handle_name_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions {
            let Some(loaded) = action.downcast_ref::<NamesLoaded>() else { continue };
            if loaded.request != self.request { continue; }
            self.taken = loaded.names.clone();
            if self.name_edited { continue; }
            let runtime = self.state.runtime().unwrap_or_else(|| super::super::HARNESSES[0].clone());
            self.state.name = self.suggested(&runtime);
            self.view.text_input(cx, ids!(name)).set_text(cx, &self.state.name);
            self.render_create(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hagency::create_agent::parse_models;
    use ::hagency::simple_agent::Runtime;

    /// A dialog where only Codex answered, the field still showing the
    /// opening's "anna-claude" (the GPT-review case).
    fn codex_only_dialog(cx: &mut Cx) -> CreateAgentDialog {
        let mut dialog = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let value = script_eval!(vm, {mod.widgets.CreateAgentDialog {}});
            CreateAgentDialog::script_from_value(vm, value)
        });
        dialog.stem = "anna".into();
        dialog.request = 7;
        dialog.state.name = "anna-claude".into();
        dialog.state.probes = std::array::from_fn(|_| Some(Err("provider_models_unavailable".into())));
        dialog.state.probes[1] = Some(Ok(parse_models(&serde_json::json!({"models": [{"model": "gpt-5", "displayName": "GPT-5"}]}))));
        assert_eq!(dialog.state.runtime(), Some(Runtime::Codex));
        dialog
    }
    fn deliver(dialog: &mut CreateAgentDialog, cx: &mut Cx, names: &[&str]) {
        let loaded = NamesLoaded { request: dialog.request, names: names.iter().map(|n| n.to_string()).collect() };
        let actions = cx.capture_actions(|cx| cx.action(loaded));
        dialog.handle_name_actions(cx, &actions);
    }

    #[test]
    fn an_untyped_name_moves_to_the_free_name_for_the_shown_harness() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut dialog = codex_only_dialog(&mut cx);
        deliver(&mut dialog, &mut cx, &["anna-codex", "anna-claude"]);
        assert_eq!(dialog.state.name, "anna-codex-2", "not left as a duplicate anna-claude");
        assert_eq!(dialog.view.text_input(&mut cx, ids!(name)).text(), "anna-codex-2");
    }

    #[test]
    fn a_typed_name_is_never_replaced_even_when_it_equals_the_suggestion() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut dialog = codex_only_dialog(&mut cx);
        dialog.name_typed(&mut cx, "anna-codex".into());
        deliver(&mut dialog, &mut cx, &["anna-codex"]);
        assert_eq!(dialog.state.name, "anna-codex");
    }
}
