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

    /// An untouched suggestion moves to the first free name; a typed name stays.
    pub(super) fn handle_name_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions {
            let Some(loaded) = action.downcast_ref::<NamesLoaded>() else { continue };
            if loaded.request != self.request { continue; }
            let runtime = self.state.runtime().unwrap_or_else(|| super::super::HARNESSES[0].clone());
            let untouched = self.state.name == self.suggested(&runtime);
            self.taken = loaded.names.clone();
            if untouched {
                self.state.name = self.suggested(&runtime);
                self.view.text_input(cx, ids!(name)).set_text(cx, &self.state.name);
                self.render_create(cx);
            }
        }
    }
}
