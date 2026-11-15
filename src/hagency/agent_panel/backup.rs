//! Identity tab: Backup model. Off unless chosen. When the main model fails a
//! turn for a usage limit, overload or outage before any tool ran, the same
//! request is answered once by this Ollama model, text-only (no tools).
//! Stored by `hagency::console::backup_model`, read at each request.
use super::*;
use hagency::console::backup_model;
use serde_json::json;

pub(super) const BACKUP_HINT: &str = "If the main model hits a usage limit or is down, the backup answers that request once, as text only (no tools). Ollama cloud models are faster.";

impl AgentPanel {
    /// Loads the saved backup and the Ollama models this Mac lists.
    pub(super) fn load_backups(&mut self) {
        let agent = self.agent_id();
        self.spawn(Load::Backups, false, async move { Ok(backup_choices(&agent).await) });
    }
    pub(super) fn apply_backups(&mut self, value: &Value) {
        self.backup_saved = value["saved"].as_str().map(str::to_owned);
        self.backups = value["models"].as_array().into_iter().flatten().filter_map(|m| m.as_str().map(str::to_owned)).collect();
    }
    pub(super) fn render_backup(&mut self, cx: &mut Cx) {
        let (labels, index) = backup_labels(&self.backups, self.backup_saved.as_deref());
        self.view.drop_down(cx, ids!(backup)).set_labels(cx, labels);
        self.view.drop_down(cx, ids!(backup)).set_selected_item(cx, index);
        self.view.widget(cx, ids!(backup)).set_disabled(cx, self.busy || !self.here());
        self.view.label(cx, ids!(backup_hint)).set_text(cx, crate::i18n::tr(BACKUP_HINT));
    }
    pub(super) fn handle_backup_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let Some(index) = self.view.drop_down(cx, ids!(backup)).selected(actions) else { return };
        let chosen = index.checked_sub(1).and_then(|i| self.backups.get(i)).cloned();
        let agent = self.agent_id();
        self.note(cx, "Saving…");
        self.spawn(Load::Backups, true, async move {
            backup_model::save(&agent, chosen.as_deref()).map_err(|e| e.to_string())?;
            Ok(backup_choices(&agent).await)
        });
    }
}

async fn backup_choices(agent: &str) -> Value {
    let models: Vec<String> = backup_model::ollama_models().await.iter().map(|m| m.stored()).collect();
    json!({"saved": backup_model::load(agent), "models": models})
}
/// "None" then each model; a saved backup Ollama no longer lists stays shown.
pub(super) fn backup_labels(models: &[String], saved: Option<&str>) -> (Vec<String>, usize) {
    let mut values: Vec<String> = models.to_vec();
    if let Some(saved) = saved && !values.iter().any(|m| m == saved) {
        values.push(saved.to_owned());
    }
    let index = saved.and_then(|s| values.iter().position(|m| m == s)).map_or(0, |i| i + 1);
    let mut labels = vec![crate::i18n::tr("None").to_owned()];
    labels.extend(values.iter().map(|m| backup_model::label(m)));
    (labels, index)
}

#[cfg(test)]
mod tests {
    use super::backup_labels;
    #[test]
    fn labels_list_none_then_models_and_keep_the_saved_one() {
        let models = vec!["ollama:gpt-oss:120b-cloud".to_owned(), "ollama:qwen3:8b".to_owned()];
        let (labels, index) = backup_labels(&models, None);
        assert_eq!(labels[1..], ["Ollama: gpt-oss:120b-cloud (cloud)".to_owned(), "Ollama: qwen3:8b (local)".to_owned()]);
        assert_eq!(index, 0, "off unless chosen");
        assert_eq!(backup_labels(&models, Some("ollama:qwen3:8b")).1, 2);
        let (labels, index) = backup_labels(&[], Some("ollama:gone:1b"));
        assert_eq!((labels.len(), index), (2, 1));
    }
}
