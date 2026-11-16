//! Create agent: "Backup model", the same choice and storage as the Agent
//! panel's Identity tab (`agent_panel::backup`): None by default, then the
//! Ollama models this Mac lists. Saved for the new Agent once it exists.
use super::*;
use crate::hagency::agent_panel::backup::{BACKUP_HINT, backup_labels};
use crate::shared::popup_list::{PopupKind, enqueue_popup_notification};
use ::hagency::console::backup_model;

/// How long the "backup not saved" notice stays up, in seconds.
const SAVE_FAILED_SECS: f64 = 12.0;

/// The Ollama listing for one opening of the dialog.
#[derive(Debug)]
struct Backups { request: u64, models: Vec<String> }

impl CreateAgentDialog {
    pub(super) fn load_backups(&mut self, cx: &mut Cx) {
        self.render_backup(cx);
        let request = self.request;
        spawn_async_task(async move {
            let models = backup_model::ollama_models().await.iter().map(|m| m.stored()).collect();
            Cx::post_action(Backups { request, models });
        });
    }
    pub(super) fn handle_backup_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions {
            if let Some(listed) = action.downcast_ref::<Backups>() && listed.request == self.request {
                self.state.backups = listed.models.clone();
                self.render_backup(cx);
            }
        }
        if let Some(index) = self.view.drop_down(cx, ids!(backup)).selected(actions) {
            self.state.backup = index;
        }
    }
    fn render_backup(&mut self, cx: &mut Cx) {
        let (labels, _) = backup_labels(&self.state.backups, None);
        self.state.backup = self.state.backup.min(labels.len() - 1);
        self.view.drop_down(cx, ids!(backup)).set_labels(cx, labels);
        self.view.drop_down(cx, ids!(backup)).set_selected_item(cx, self.state.backup);
        self.view.label(cx, ids!(backup_hint)).set_text(cx, crate::i18n::tr(BACKUP_HINT));
    }
    /// The Agent exists either way; a failed save is told to the owner, who can set it in the panel.
    pub(super) fn save_backup(&self, created: &Value) {
        if let Err(error) = save_for_created(created, self.state.backup_choice().as_deref(), backup_model::save) {
            let message = crate::i18n::format("The agent was created, but its backup model was not saved: {0}", &[("0", error)]);
            enqueue_popup_notification(message, PopupKind::Error, Some(SAVE_FAILED_SECS));
        }
    }
}

/// Saves `backup` for the Agent in a Create reply (`creation.agent.id`); nothing to do without one.
pub(super) fn save_for_created(
    created: &Value,
    backup: Option<&str>,
    save: impl FnOnce(&str, Option<&str>) -> std::io::Result<()>,
) -> Result<(), String> {
    let Some(backup) = backup else { return Ok(()) };
    let agent = created["creation"]["agent"]["id"].as_str().filter(|id| !id.is_empty()).ok_or("no agent id in the reply")?;
    save(agent, Some(backup)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::save_for_created;
    use serde_json::json;

    #[test]
    fn the_chosen_backup_is_saved_for_the_new_agent_id() {
        let created = json!({"creation": {"agent": {"id": "agt_1"}}});
        let mut saved = None;
        save_for_created(&created, Some("ollama:qwen3:8b|local"), |id, b| { saved = Some((id.to_owned(), b.map(str::to_owned))); Ok(()) }).unwrap();
        assert_eq!(saved, Some(("agt_1".to_owned(), Some("ollama:qwen3:8b|local".to_owned()))));
    }
    #[test]
    fn none_saves_nothing_and_a_missing_id_or_write_error_is_reported() {
        let created = json!({"creation": {"agent": {"id": "agt_1"}}});
        save_for_created(&created, None, |_, _| panic!("None must not write")).unwrap();
        assert!(save_for_created(&json!({}), Some("ollama:x"), |_, _| Ok(())).is_err());
        assert!(save_for_created(&created, Some("ollama:x"), |_, _| Err(std::io::Error::other("disk full"))).is_err());
    }
}
