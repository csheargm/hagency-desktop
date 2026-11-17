//! One native agent panel (Buzz style): Identity / Context / Activity /
//! Settings. It reads and writes only through the existing owner commands:
//! the mini app's local Agent calls (`host::hagency::local::Call`: status,
//! service, remembered approvals) and the Agent-layer model profile and
//! policy (`LocalAction::AgentModel` / `SaveAgentModel` / `SaveAgentPolicy`).
mod about;
mod activity;
mod activity_load;
mod add_room;
pub(crate) mod backup;
pub(crate) mod activity_rows;
mod design;
mod identity;
mod rename;
pub(crate) mod mapping;
pub(crate) mod rooms;
mod settings;
pub use design::script_mod;

use crate::hagency::{backend::Service, create_agent::ModelChoice, model::{Command, LocalAction}};
use crate::host::hagency::local::Call;
use crate::sliding_sync::spawn_async_task;
use makepad_widgets::*;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

/// What the panel asks of the Agents page that hosts it.
#[derive(Clone, Debug)]
pub enum AgentPanelAction {
    /// Open the full (older) Agent detail page for this Agent.
    Advanced { agent_id: String },
    /// The Agent was retired; the list should refresh.
    Retired { agent_id: String },
    /// The owner renamed the Agent; the list shows the new name.
    Renamed { agent_id: String, display_name: String },
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Tab { #[default] Identity, Context, Activity, Settings }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Load { Status, Model, Models, Retired, Renamed, Activity, Backups, Left, Added, OwnerRooms }
#[derive(Debug)]
struct Loaded { request: u64, load: Load, mutation: bool, result: Result<Value, String> }
static REQUEST: AtomicU64 = AtomicU64::new(1);

#[derive(Script, ScriptHook, Widget)]
pub struct AgentPanel {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    /// The Agent record from the Agents list (`id`, `displayName`, `puppetMxid`).
    #[rust] agent: Value,
    #[rust] request: u64,
    #[rust] busy: bool,
    #[rust] tab: Tab,
    /// `Call::Status` reply: here, deviceName, service, chat, rooms, approvals.
    #[rust] status: Value,
    /// `AgentModel` / save reply: policy {revision, policy}, usage, modelProfile.
    #[rust] model: Value,
    /// The runtime's provider `Models` reply and its parsed choices.
    #[rust] models_reply: Value,
    #[rust] models: Vec<ModelChoice>,
    /// Activity tab: `{requests, approvals}` from `activity_load`.
    #[rust] activity: Value,
    /// Backup model: Ollama choices (`ollama:<model>`) and the saved one.
    #[rust] backups: Vec<String>,
    #[rust] backup_saved: Option<String>,
    /// Context tab: the owner's joined group rooms, as `(room id, name)`.
    #[rust] owner_rooms: Vec<(String, String)>,
}
impl Widget for AgentPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        for action in actions {
            if let Some(loaded) = action.downcast_ref::<Loaded>() && loaded.request == self.request {
                self.apply(cx, loaded.load, loaded.mutation, &loaded.result);
            }
        }
        self.handle_tabs(cx, actions);
        if self.busy || self.agent_id().is_empty() { return; }
        self.handle_identity_actions(cx, actions);
        self.handle_rename_actions(cx, actions);
        self.handle_backup_actions(cx, actions);
        self.handle_settings_actions(cx, actions);
        self.handle_room_follow_actions(cx, actions);
        self.handle_room_leave_actions(cx, actions);
        self.handle_add_room_actions(cx, actions);
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl AgentPanel {
    fn agent_id(&self) -> String { self.agent["id"].as_str().unwrap_or("").to_owned() }
    fn here(&self) -> bool { self.status["here"] == true }
    /// Opens the panel for one Agent; every earlier reply is ignored from now on.
    pub fn open(&mut self, cx: &mut Cx, agent: &Value) {
        self.request = REQUEST.fetch_add(1, Ordering::Relaxed);
        self.agent = agent.clone();
        (self.busy, self.status, self.model, self.models_reply, self.activity) = (false, Value::Null, Value::Null, Value::Null, Value::Null);
        self.models.clear();
        (self.backups, self.backup_saved, self.owner_rooms) = (Vec::new(), None, Vec::new());
        self.view.check_box(cx, ids!(panel_retire_confirm)).set_active(cx, false, Animate::No);
        self.fill_room_follow(cx);
        self.fill_rename(cx);
        self.set_tab(cx, Tab::Identity);
        self.note(cx, "Loading…");
        self.call(Load::Status, false, Call::Status(self.agent_id()));
        self.fill_settings_sections(cx);
        self.load_backups();
        self.load_owner_rooms();
        self.render(cx);
    }
    fn render(&mut self, cx: &mut Cx) {
        self.render_identity(cx);
        self.render_context(cx);
        self.render_activity(cx);
        self.render_settings(cx);
        self.view.button(cx, ids!(advanced)).set_enabled(cx, !self.busy);
        self.view.redraw(cx);
    }
    fn note(&self, cx: &mut Cx, text: &str) {
        self.view.label(cx, ids!(status)).set_text(cx, crate::i18n::tr(text));
    }

    /// Runs one of the mini app's local Agent calls for this Agent's account.
    fn call(&mut self, load: Load, mutation: bool, call: Call) {
        let account = crate::sliding_sync::current_user_id().map(|id| id.to_string()).unwrap_or_default();
        self.spawn(load, mutation, async move { call.execute(&account).await });
    }
    /// Runs one owner command through the current account's service.
    fn command(&mut self, load: Load, mutation: bool, command: Command) {
        match Service::for_current() {
            Ok(service) => self.spawn(load, mutation, async move { service.execute(command).await }),
            Err(code) => self.spawn(load, mutation, async move { Err(code) }),
        }
    }
    fn local(&mut self, load: Load, action: LocalAction, input: Option<Value>) {
        let mutation = input.is_some();
        let command = Command::Local { agent_id: self.agent_id(), action, input, binding_id: None, requester: None };
        self.command(load, mutation, command);
    }
    fn spawn(&mut self, load: Load, mutation: bool, job: impl std::future::Future<Output = Result<Value, String>> + Send + 'static) {
        self.busy |= mutation;
        let request = self.request;
        spawn_async_task(async move {
            let result = job.await;
            Cx::post_action(Loaded { request, load, mutation, result });
        });
    }

    fn apply(&mut self, cx: &mut Cx, load: Load, mutation: bool, result: &Result<Value, String>) {
        if mutation { self.busy = false; }
        match (load, result) {
            (_, Err(code)) => {
                let text = if mutation { "Not saved: {code}" } else { "Could not load: {code}" };
                self.view.label(cx, ids!(status)).set_text(cx, &crate::i18n::format(text, &[("code", code.clone())]));
            }
            (Load::Status, Ok(value)) => {
                self.status = value.clone();
                self.fill_settings_sections(cx);
                self.note(cx, if mutation { "Saved." } else { "" });
                if self.here() && self.model.is_null() {
                    self.local(Load::Model, LocalAction::AgentModel, None);
                }
                self.refresh_activity();
            }
            (Load::Model, Ok(value)) => {
                self.model = value.clone();
                self.fill_budget(cx);
                if mutation { self.note(cx, "Saved."); }
                if self.models_reply.is_null() {
                    let runtime = mapping::runtime_of(value["modelProfile"]["credential_ref"].as_str().unwrap_or(""));
                    self.command(Load::Models, false, mapping::models_command(runtime));
                }
            }
            (Load::Models, Ok(value)) => {
                self.models_reply = value.clone();
                self.models = crate::hagency::create_agent::parse_models(value);
            }
            (Load::Activity, Ok(value)) => self.activity = value.clone(),
            (Load::Backups, Ok(value)) => {
                self.apply_backups(value);
                if mutation { self.note(cx, "Saved."); }
            }
            // The Agent left a room: reload its rooms (the status says "Saved.").
            (Load::Left | Load::Added, Ok(_)) => self.call(Load::Status, true, Call::Status(self.agent_id())),
            (Load::OwnerRooms, Ok(value)) => self.apply_owner_rooms(value),
            (Load::Renamed, Ok(value)) => self.apply_rename(cx, value),
            (Load::Retired, Ok(_)) => cx.action(AgentPanelAction::Retired { agent_id: self.agent_id() }),
        }
        self.render(cx);
    }

    fn handle_tabs(&mut self, cx: &mut Cx, actions: &Actions) {
        for (id, tab) in [(ids!(identity_tab), Tab::Identity), (ids!(context_tab), Tab::Context), (ids!(activity_tab), Tab::Activity), (ids!(settings_tab), Tab::Settings)] {
            if self.view.button(cx, id).clicked(actions) { self.set_tab(cx, tab); }
        }
        if self.view.button(cx, ids!(advanced)).clicked(actions) {
            cx.action(AgentPanelAction::Advanced { agent_id: self.agent_id() });
        }
    }
    fn set_tab(&mut self, cx: &mut Cx, tab: Tab) {
        // Rooms the Agent joined after the panel opened (an auto-join) show up in Settings.
        // The Context tab too: a private Agent's later consent may have added a room.
        if matches!(tab, Tab::Settings | Tab::Context) && self.tab != tab && !self.busy && self.status.is_object() {
            self.call(Load::Status, false, Call::Status(self.agent_id()));
        }
        self.tab = tab;
        self.refresh_activity();
        let tabs = [(ids!(identity_tab), ids!(identity_page), Tab::Identity), (ids!(context_tab), ids!(context_page), Tab::Context),
            (ids!(activity_tab), ids!(activity_page), Tab::Activity), (ids!(settings_tab), ids!(settings_page), Tab::Settings)];
        for (button_id, page, which) in tabs {
            self.view.view(cx, page).set_visible(cx, which == tab);
            let mut button = self.view.button(cx, button_id);
            if which == tab {
                script_apply_eval!(cx, button, { draw_bg +: {color: mod.widgets.RINX_ACCENT color_hover: mod.widgets.RINX_ACCENT color_down: mod.widgets.RINX_ACCENT color_focus: mod.widgets.RINX_ACCENT} draw_text +: {color: mod.widgets.RINX_ON_ACCENT color_hover: mod.widgets.RINX_ON_ACCENT color_down: mod.widgets.RINX_ON_ACCENT color_focus: mod.widgets.RINX_ON_ACCENT} });
            } else {
                script_apply_eval!(cx, button, { draw_bg +: {color: mod.widgets.RINX_SURFACE color_hover: mod.widgets.RINX_HOVER color_down: mod.widgets.RINX_PRESSED color_focus: mod.widgets.RINX_SELECTED} draw_text +: {color: mod.widgets.RINX_INK color_hover: mod.widgets.RINX_INK color_down: mod.widgets.RINX_INK color_focus: mod.widgets.RINX_INK} });
            }
        }
        self.view.redraw(cx);
    }
}

impl AgentPanelRef {
    pub fn open(&self, cx: &mut Cx, agent: &Value) {
        if let Some(mut inner) = self.borrow_mut() { inner.open(cx, agent); }
    }
}

#[cfg(test)]
mod tests;
