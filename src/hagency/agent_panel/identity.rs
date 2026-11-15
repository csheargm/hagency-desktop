//! Identity tab: header (avatar, name, Message, where it runs), Model and
//! Thinking (saved at once), Who can ask (A4) and the
//! credit card (A5).
use super::mapping::{self, WhoCanAsk, WHO_CAN_ASK};
use super::*;
use crate::shared::{avatar::AvatarWidgetExt, progress_bar::ProgressBarWidgetRefExt};
use serde_json::json;

impl AgentPanel {
    pub(super) fn render_identity(&mut self, cx: &mut Cx) {
        self.render_header(cx);
        self.render_model(cx);
        self.render_who(cx);
        self.render_credit(cx);
    }
    fn render_header(&mut self, cx: &mut Cx) {
        let name = self.agent["displayName"].as_str().unwrap_or("");
        self.view.label(cx, ids!(name)).set_text(cx, name);
        let avatar = self.view.avatar(cx, ids!(avatar));
        match matrix_sdk::ruma::OwnedUserId::try_from(self.agent["puppetMxid"].as_str().unwrap_or("")) {
            Ok(user_id) => avatar.show_user_text(cx, &user_id, name),
            Err(_) => avatar.show_text(cx, None, None, name),
        }
        let profile = &self.model["modelProfile"];
        let harness = profile["credential_ref"].as_str().map(|c| mapping::harness_label(mapping::runtime_of(c)));
        let line = if self.status.is_null() { String::new() } else {
            mapping::runs_on(self.here(), self.status["deviceName"].as_str().unwrap_or(""), harness)
        };
        self.view.label(cx, ids!(runs_on)).set_text(cx, &line);
        self.view.button(cx, ids!(message)).set_enabled(cx, self.chat_room().is_some());
    }
    fn chat_room(&self) -> Option<matrix_sdk::ruma::OwnedRoomId> {
        matrix_sdk::ruma::OwnedRoomId::try_from(self.status["chat"]["roomId"].as_str()?).ok()
    }
    /// The saved model's index among the provider's choices.
    fn model_index(&self) -> Option<usize> {
        let saved = self.model["modelProfile"]["model"].as_str()?;
        self.models.iter().position(|m| m.model == saved)
    }
    fn render_model(&mut self, cx: &mut Cx) {
        let editable = self.here() && !self.busy && !self.models.is_empty() && self.model["modelProfile"].is_object();
        let index = self.model_index();
        let mut labels: Vec<String> = self.models.iter().map(|m| m.label.clone()).collect();
        if labels.is_empty() || index.is_none() {
            let saved = self.model["modelProfile"]["model"].as_str().filter(|m| !m.is_empty());
            labels.insert(0, saved.map(str::to_owned).unwrap_or_else(|| crate::i18n::tr(if self.here() { "Checking model choices…" } else { "Set on the device that runs it" }).into()));
        }
        self.view.drop_down(cx, ids!(model)).set_labels(cx, labels);
        self.view.drop_down(cx, ids!(model)).set_selected_item(cx, index.unwrap_or(0));
        let efforts = index.map(|i| self.models[i].efforts.clone()).unwrap_or_default();
        let saved_effort = self.model["modelProfile"]["reasoning_effort"].as_str().unwrap_or("");
        let thinking: Vec<String> = if efforts.is_empty() {
            vec![if saved_effort.is_empty() { crate::i18n::tr("Default").to_owned() } else { saved_effort.to_owned() }]
        } else { efforts.clone() };
        self.view.drop_down(cx, ids!(thinking)).set_labels(cx, thinking);
        self.view.drop_down(cx, ids!(thinking)).set_selected_item(cx, efforts.iter().position(|e| e == saved_effort).unwrap_or(0));
        self.view.widget(cx, ids!(model)).set_disabled(cx, !editable);
        self.view.widget(cx, ids!(thinking)).set_disabled(cx, !editable || efforts.is_empty());
        let hint = if self.status.is_null() || self.here() { "Changes save right away and apply to the next request." } else { "Model and limits are set on the device that runs this agent. Open Advanced settings to change them." };
        self.view.label(cx, ids!(model_hint)).set_text(cx, crate::i18n::tr(hint));
    }
    fn render_who(&mut self, cx: &mut Cx) {
        let requests = self.model["policy"]["policy"]["requests"].as_str().unwrap_or("");
        let choice = WhoCanAsk::from_requests(requests);
        let mut labels: Vec<String> = WHO_CAN_ASK.iter().map(|c| crate::i18n::tr(c.label()).to_owned()).collect();
        if choice.is_none() && !requests.is_empty() {
            labels.push(crate::i18n::tr("Ask me before each request (set in Advanced settings)").into());
        }
        let index = choice.and_then(|c| WHO_CAN_ASK.iter().position(|w| *w == c)).unwrap_or(if requests.is_empty() { 1 } else { WHO_CAN_ASK.len() });
        self.view.drop_down(cx, ids!(who)).set_labels(cx, labels);
        self.view.drop_down(cx, ids!(who)).set_selected_item(cx, index);
        self.view.widget(cx, ids!(who)).set_disabled(cx, !self.here() || self.busy || self.model["policy"].is_null());
        let hint = match choice {
            Some(WhoCanAsk::OnlyMe) => "Only you can ask it, in any room. Others' requests are declined.",
            _ => "People in rooms you add it to can ask it by mentioning it. You can always ask it.",
        };
        self.view.label(cx, ids!(who_hint)).set_text(cx, crate::i18n::tr(hint));
    }
    fn render_credit(&mut self, cx: &mut Cx) {
        let loaded = self.model["policy"]["policy"].is_object();
        let credit = mapping::credit(&self.model["policy"]["policy"], &self.model["usage"]);
        let text = if loaded { credit.text } else { String::new() };
        self.view.label(cx, ids!(credit_text)).set_text(cx, &text);
        self.view.label(cx, ids!(credit_detail)).set_text(cx, if loaded { &credit.detail } else { "" });
        let bar = self.view.child_by_path(ids!(credit_bar));
        bar.set_visible(cx, loaded && credit.left.is_some());
        bar.as_progress_bar().set_progress(cx, credit.left.unwrap_or(0.0));
    }

    pub(super) fn handle_identity_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.view.button(cx, ids!(message)).clicked(actions) && let Some(room_id) = self.chat_room() {
            crate::hagency::ui::open_agent_chat(cx, room_id);
        }
        if let Some(index) = self.view.drop_down(cx, ids!(model)).selected(actions) {
            self.save_model(cx, index, None);
        }
        if let Some(index) = self.view.drop_down(cx, ids!(thinking)).selected(actions) && let Some(model) = self.model_index() {
            let effort = self.models[model].efforts.get(index).cloned();
            self.save_model(cx, model, effort);
        }
        if let Some(index) = self.view.drop_down(cx, ids!(who)).selected(actions) {
            self.save_who(cx, index);
        }
    }
    /// Saves model `index` with `effort`, or the saved Thinking when the model offers it.
    fn save_model(&mut self, cx: &mut Cx, index: usize, effort: Option<String>) {
        let profile = self.model["modelProfile"].clone();
        let Some(choice) = self.models.get(index).cloned() else { return self.render(cx) };
        let wanted = effort.unwrap_or_else(|| profile["reasoning_effort"].as_str().unwrap_or("").to_owned());
        let effort = crate::hagency::create_agent::effort_for(&choice, &wanted);
        let row = self.models_reply["models"].as_array().and_then(|rows| rows.iter().find(|r| r["model"] == json!(choice.model)).cloned());
        let credential = [row.as_ref().and_then(|r| r["credentialRef"].as_str()), self.models_reply["credentialRef"].as_str(), profile["credential_ref"].as_str()]
            .into_iter().flatten().find(|c| !c.is_empty()).unwrap_or("").to_owned();
        if credential.is_empty() {
            self.note(cx, "Connect this Mac's login in Advanced settings first.");
            return self.render(cx);
        }
        match mapping::model_input(&profile, &choice, &credential, &effort) {
            Some(input) => {
                self.note(cx, "Saving…");
                self.local(Load::Model, LocalAction::SaveAgentModel, Some(input));
            }
            None => self.note(cx, "Load this agent's model before changing it."),
        }
        self.render(cx);
    }
    fn save_who(&mut self, cx: &mut Cx, index: usize) {
        let Some(requests) = WHO_CAN_ASK.get(index).and_then(|c| c.requests()) else {
            self.note(cx, "Anyone who can reach it is for hosted Agents. Desktop agents answer people in rooms you add them to.");
            return self.render(cx);
        };
        match mapping::policy_change(&self.model, "requests", json!(requests)) {
            Some(change) => {
                self.note(cx, "Saving…");
                self.local(Load::Model, LocalAction::SaveAgentPolicy, Some(change));
            }
            None => self.note(cx, "Load this agent's policy before changing it."),
        }
        self.render(cx);
    }
}
