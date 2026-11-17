//! Create agent: running the "What should it work on?" step (`work_step`).
//! Create checks the repositories on GitHub first and stops on a refusal;
//! once the Agent exists, the typed token is kept as its GitHub access and the
//! work list is saved through the owner API, exactly as the Agent panel saves it.
use super::super::work_step::{LiveGithub, Wanted, WorkDraft, check};
use super::CreateAgentDialog;
use crate::hagency::backend::Service;
use crate::shared::popup_list::{PopupKind, enqueue_popup_notification};
use crate::sliding_sync::spawn_async_task;
use ::hagency::github_access::{Token, TokenStore};
use makepad_widgets::*;
use serde_json::{Value, json};

/// How long a "work list not saved" notice stays up, in seconds.
const SAVE_FAILED_SECS: f64 = 12.0;

/// What Create carries from the step into the creation task.
#[derive(Clone, Debug, Default)]
pub(super) struct WorkPlan {
    pub draft: WorkDraft,
    pub wanted: Vec<Wanted>,
    pub token: Option<String>,
    /// The chosen activity room; `None` posts into the Agent's private chat.
    pub room: Option<String>,
}

/// The repositories passed (`Ok`) or were refused (`Err`), for one opening.
#[derive(Debug)]
pub(super) struct Checked {
    pub request: u64,
    pub result: Result<Vec<String>, String>,
}

impl CreateAgentDialog {
    /// The step's fields, or why Create must stop. An empty step plans nothing.
    pub(super) fn work_plan(&mut self, cx: &mut Cx) -> Result<WorkPlan, String> {
        let text = |id: &[LiveId]| self.view.text_input(cx, id).text();
        let draft = WorkDraft { repos: text(ids!(work_repos)), issues_per_day: text(ids!(work_issues)), open_prs: text(ids!(work_prs)), budget: text(ids!(work_budget)) };
        let wanted = draft.wanted()?;
        draft.budget()?;
        draft.entries(&wanted, "!check:local", 0)?;
        let token = Some(text(ids!(work_token))).filter(|t| !t.trim().is_empty());
        if let Some(token) = &token {
            Token::parse(token).map_err(|e| crate::i18n::tr(e).to_owned())?;
        }
        let room = self.view.drop_down(cx, ids!(work_room)).selected_item().checked_sub(1).and_then(|i| self.state.rooms.get_mut(i)).map(|choice| {
            choice.ticked = true; // the Agent must be in the room it posts to
            choice.room_id.clone()
        });
        Ok(WorkPlan { draft, wanted, token, room })
    }

    /// Checks the planned repositories; Create continues on `Checked`.
    pub(super) fn start_check(&mut self, cx: &mut Cx, plan: &WorkPlan) {
        self.view.label(cx, ids!(status)).set_text(cx, crate::i18n::tr("Checking the repositories on GitHub…"));
        let (request, wanted, typed) = (self.request, plan.wanted.clone(), plan.token.clone());
        spawn_async_task(async move {
            let result = match crate::hagency::issue_watch::choose_token(Ok(typed.and_then(|t| Token::parse(&t).ok())), |n| std::env::var(n).ok()) {
                Ok(token) => match Token::parse(&token) {
                    Ok(token) => check(&LiveGithub { client: ::hagency::issue_watch::new_client(), token }, &wanted).await,
                    Err(reason) => Err(crate::i18n::tr(reason).to_owned()),
                },
                Err(_) => Err(crate::i18n::tr("Add a GitHub token so the repositories can be checked, or leave the repositories empty.").to_owned()),
            };
            Cx::post_action(Checked { request, result });
        });
    }

    pub(super) fn handle_work_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions {
            if let Some(checked) = action.downcast_ref::<Checked>() && checked.request == self.request {
                match &checked.result {
                    Ok(lines) => {
                        self.view.label(cx, ids!(work_result)).set_text(cx, &lines.join("\n"));
                        self.create_now(cx);
                    }
                    Err(reason) => {
                        self.busy = false;
                        self.view.label(cx, ids!(work_result)).set_text(cx, reason);
                        self.view.label(cx, ids!(status)).set_text(cx, crate::i18n::tr("Fix the repositories, or leave them empty, then try again."));
                        self.render_create(cx);
                    }
                }
            }
        }
        if self.work_rooms_shown != self.state.rooms.len() {
            self.work_rooms_shown = self.state.rooms.len();
            let labels = std::iter::once(crate::i18n::tr("Its private chat with you").to_owned()).chain(self.state.rooms.iter().map(|r| r.name.clone())).collect();
            self.view.drop_down(cx, ids!(work_room)).set_labels(cx, labels);
        }
    }

    /// After creation: the token, then the work list. The Agent exists either
    /// way; a failed save is told to the owner, who can finish it in the panel.
    pub(super) fn save_work(&self, created: &Value) {
        let Some(plan) = self.work.clone().filter(|p| !p.wanted.is_empty()) else { return };
        let agent = created["creation"]["agent"]["id"].as_str().unwrap_or_default().to_owned();
        let room = plan.room.clone().or_else(|| created["quickSetup"]["ownerDirectRoomId"].as_str().map(str::to_owned)).unwrap_or_default();
        let Ok(service) = Service::for_current() else { return report(crate::i18n::tr("Signed out").to_owned()) };
        spawn_async_task(async move {
            if let Err(error) = save(&service, &agent, &room, &plan).await {
                report(error);
            }
        });
    }
}

fn report(error: String) {
    let message = crate::i18n::format("The agent was created, but its repositories were not saved: {0}. Add them under Repositories in the Agent panel.", &[("0", error)]);
    enqueue_popup_notification(message, PopupKind::Error, Some(SAVE_FAILED_SECS));
}

async fn save(service: &Service, agent: &str, room: &str, plan: &WorkPlan) -> Result<(), String> {
    if agent.is_empty() || room.is_empty() {
        return Err("no agent id or room in the reply".into());
    }
    // Only a typed token is saved; an environment token is resolved again at
    // run time by the same rule (`choose_token` / `demo_env::resolve`).
    if let Some(token) = plan.token.as_deref().map(Token::parse).transpose().map_err(str::to_owned)? {
        let id = agent.to_owned();
        tokio::task::spawn_blocking(move || TokenStore::current().save(&id, &token)).await.map_err(|e| e.to_string())??;
    }
    let work = plan.draft.entries(&plan.wanted, room, chrono::Utc::now().timestamp())?;
    crate::hagency::issue_watch::live::saved(service, agent, Some(json!({"work": work}))).await.map(|_| ())
}
