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
    /// While Create is busy the edit is undone: the clicked name is kept.
    pub(super) fn name_typed(&mut self, cx: &mut Cx, name: String) {
        if self.busy {
            self.view.text_input(cx, ids!(name)).set_text(cx, &self.state.name);
            return;
        }
        self.name_edited = true;
        self.state.name = name;
        self.render_create(cx);
    }

    /// Until the owner types a name, the field takes the first free name for
    /// the harness shown. Once Create is pressed the name is left alone: the
    /// spec is rebuilt after the repository check and must keep what was clicked.
    pub(super) fn handle_name_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions {
            let Some(loaded) = action.downcast_ref::<NamesLoaded>() else { continue };
            if loaded.request != self.request || self.busy { continue; }
            self.taken = loaded.names.clone();
            self.names_loaded = true;
            self.follow_suggestion(cx);
            self.render_create(cx);
        }
    }

    /// A harness's model probe answered. When that changes the harness shown
    /// (the listing may have answered first, with the Claude fallback), an
    /// untyped name follows the shown harness (GPT review, Nov 19). While
    /// Create is busy the answer waits: the shown harness stays the clicked one.
    pub(super) fn probe_answered(&mut self, cx: &mut Cx, index: usize, models: Result<Vec<super::super::ModelChoice>, String>) {
        if self.busy {
            return self.late_probes.push((index, models));
        }
        let before = self.state.runtime();
        self.state.probes[index] = Some(models);
        self.refresh_stem(cx);
        self.render(cx);
        if self.state.runtime() != before {
            self.follow_suggestion(cx);
            self.render_create(cx);
        }
    }

    /// An untyped name becomes the first free name for the harness shown
    /// (lookup, harness change, display name); a typed one is left alone,
    /// and so is any name once Create is pressed. Nothing is suggested
    /// before the listing answers: the suggestion could be taken.
    pub(super) fn follow_suggestion(&mut self, cx: &mut Cx) {
        if self.name_edited || self.busy || !self.names_loaded { return; }
        let runtime = self.state.runtime().unwrap_or_else(|| super::super::HARNESSES[0].clone());
        self.state.name = self.suggested(&runtime);
        self.view.text_input(cx, ids!(name)).set_text(cx, &self.state.name);
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
        dialog.names_loaded = true;
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

    /// The same rule on a harness change: Codex → Claude.
    #[test]
    fn a_harness_change_moves_only_an_untyped_name() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut dialog = codex_only_dialog(&mut cx);
        dialog.state.probes[0] = Some(Ok(parse_models(&serde_json::json!({"models": [{"model": "sonnet"}]}))));
        dialog.taken = vec!["anna-claude".into()];
        dialog.choose_harness(&mut cx, 0);
        assert_eq!(dialog.state.runtime(), Some(Runtime::Claude));
        assert_eq!(dialog.state.name, "anna-claude-2");
        dialog.name_typed(&mut cx, "anna-codex".into());
        dialog.choose_harness(&mut cx, 1);
        assert_eq!(dialog.state.name, "anna-codex", "typed: kept even though it equals a suggestion");
    }

    /// And on a display-name refresh (tests have no signed-in user: stem "my").
    #[test]
    fn a_display_name_refresh_moves_only_an_untyped_name() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut dialog = codex_only_dialog(&mut cx);
        dialog.taken = vec!["my-codex".into()];
        dialog.refresh_stem(&mut cx);
        assert_eq!(dialog.state.name, "my-codex-2", "anna-claude was untyped: follows the shown harness");
        let mut typed = codex_only_dialog(&mut cx);
        typed.name_typed(&mut cx, "anna-codex".into());
        typed.refresh_stem(&mut cx);
        assert_eq!(typed.state.name, "anna-codex");
    }

    #[test]
    fn a_late_lookup_after_create_does_not_change_the_submitted_name() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut dialog = codex_only_dialog(&mut cx);
        dialog.state.name = "anna-codex".into();
        dialog.busy = true; // Create pressed: `create` sets it before the repository check.
        deliver(&mut dialog, &mut cx, &["anna-codex"]);
        let spec = dialog.state.spec(crate::hagency::quick_setup::default_spec).expect("ready");
        assert_eq!(spec.name, "anna-codex");
    }

    /// GPT review, Nov 19: the Name field stayed editable during the
    /// repository check and `create_now` rebuilt the spec with the edit.
    #[test]
    fn an_edit_while_create_is_busy_is_undone_and_the_clicked_name_submitted() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut dialog = codex_only_dialog(&mut cx);
        dialog.name_typed(&mut cx, "anna-codex".into());
        dialog.busy = true;
        dialog.render_create(&mut cx);
        assert!(dialog.view.text_input(&mut cx, ids!(name)).is_read_only(), "the field is read-only while busy");
        dialog.name_typed(&mut cx, "edited".into());
        assert_eq!(dialog.view.text_input(&mut cx, ids!(name)).text(), "anna-codex");
        let spec = dialog.state.spec(crate::hagency::quick_setup::default_spec).expect("ready");
        assert_eq!(spec.name, "anna-codex");
        dialog.busy = false;
        dialog.render_create(&mut cx);
        assert!(!dialog.view.text_input(&mut cx, ids!(name)).is_read_only());
    }

    /// Polish round 3, Nov 19: Create pressed with Codex shown; during the
    /// repository check Claude's probe answered, shifting index 0 to Claude,
    /// and `create_now` rebuilt a Claude spec. The clicked spec is submitted.
    #[test]
    fn a_probe_while_create_is_busy_never_changes_the_submitted_spec() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut dialog = codex_only_dialog(&mut cx);
        dialog.state.probes[0] = None;
        dialog.state.name = "anna-codex".into();
        dialog.frozen = Some(dialog.state.spec(crate::hagency::quick_setup::default_spec).expect("ready"));
        dialog.busy = true; // As `create` leaves it before the repository check.
        dialog.probe_answered(&mut cx, 0, Ok(parse_models(&serde_json::json!({"models": [{"model": "sonnet"}]}))));
        assert_eq!(dialog.state.runtime(), Some(Runtime::Codex), "the shown harness stays the clicked one");
        let spec = dialog.submitted_spec().expect("frozen");
        assert_eq!((spec.name.as_str(), spec.runtime.clone(), spec.model.as_deref()), ("anna-codex", Runtime::Codex, Some("gpt-5")));
        dialog.settle(&mut cx);
        assert!(dialog.state.probes[0].is_some(), "the late probe applies once Create is done");
        assert!(dialog.frozen.is_none() && dialog.late_probes.is_empty());
    }

    /// GPT review, Nov 19: "anna-claude" showed until the listing said it was taken.
    #[test]
    fn no_name_is_suggested_until_the_listing_answers() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut dialog = codex_only_dialog(&mut cx);
        dialog.names_loaded = false;
        dialog.state.name.clear();
        dialog.refresh_stem(&mut cx);
        dialog.choose_harness(&mut cx, 0);
        assert_eq!(dialog.state.name, "", "nothing suggested before the listing");
        assert_eq!(dialog.view.text_input(&mut cx, ids!(name)).text(), "");
        assert!(dialog.state.spec(crate::hagency::quick_setup::default_spec).is_err(), "Create waits for a name");
        deliver(&mut dialog, &mut cx, &[]); // A failed read delivers no names: still fills.
        assert!(dialog.state.name.ends_with("-codex"), "{}", dialog.state.name);
        let mut taken = codex_only_dialog(&mut cx);
        taken.names_loaded = false;
        deliver(&mut taken, &mut cx, &["anna-codex"]);
        assert_eq!(taken.state.name, "anna-codex-2");
    }

    /// GPT review, Nov 19: the listing answered before the probes, so the
    /// Claude fallback was suggested; a Codex-only probe then enabled Create
    /// with "anna-claude".
    #[test]
    fn a_probe_after_the_listing_moves_an_untyped_name_to_the_shown_harness() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        for (taken, expected) in [(vec![], "-codex"), (vec!["my-codex"], "-codex-2")] {
            let mut dialog = codex_only_dialog(&mut cx);
            dialog.state.probes = std::array::from_fn(|_| None);
            dialog.state.name.clear();
            dialog.names_loaded = false;
            deliver(&mut dialog, &mut cx, &taken);
            assert!(dialog.state.name.ends_with("-claude"), "the fallback before any probe: {}", dialog.state.name);
            dialog.probe_answered(&mut cx, 0, Err("provider_models_unavailable".into()));
            dialog.probe_answered(&mut cx, 1, Ok(parse_models(&serde_json::json!({"models": [{"model": "gpt-5"}]}))));
            assert_eq!(dialog.state.runtime(), Some(Runtime::Codex));
            assert!(dialog.state.name.ends_with(expected) && !dialog.state.name.contains("claude"), "{}", dialog.state.name);
            assert_eq!(dialog.view.text_input(&mut cx, ids!(name)).text(), dialog.state.name);
        }
        let mut typed = codex_only_dialog(&mut cx);
        typed.state.probes = std::array::from_fn(|_| None);
        typed.name_typed(&mut cx, "mine".into());
        typed.probe_answered(&mut cx, 1, Ok(parse_models(&serde_json::json!({"models": [{"model": "gpt-5"}]}))));
        assert_eq!(typed.state.name, "mine", "a typed name is kept");
    }
}
