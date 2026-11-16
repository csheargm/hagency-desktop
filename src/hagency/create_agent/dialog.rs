//! The Create agent dialog widget. Opening it checks which local harnesses
//! answer with models; Create runs `quick_setup::create_spec` (one-click
//! defaults plus Model and Thinking) and opens the Agent's private chat.
use super::{DialogState, default_name, harness_label, parse_models};
use crate::hagency::{backend::Service, quick_setup};
use crate::sliding_sync::{current_user_id, spawn_async_task};
use ::hagency::native_owner::ProviderAction;
use ::hagency::simple_agent;
use makepad_widgets::*;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    let Text = Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}}
    let Hint = Text {draw_text +: {color: mod.widgets.RINX_MUTED text_style.font_size: (10.5 * mod.widgets.RINX_TEXT_SCALE)}}
    let Field = View {width: Fill height: Fit flow: Down spacing: 4}
    let Input = RobrixTextInput {
        width: Fill height: 40 margin: 0 padding: Inset{left: 12 right: 12 top: 8 bottom: 8}
        draw_text.text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}
    }
    let Select = RobrixSettingsDropDown {
        width: Fill height: 40 margin: 0
        popup_menu_position: PopupMenuPosition.BelowInput
        draw_text.text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}
        popup_menu: RobrixSettingsPopupMenu {width: 420 menu_item: RobrixSettingsPopupMenuItem {height: 36 draw_text.text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}}
    }
    let Tick = CheckBox {visible: false width: Fill text: ""}
    let Action = RobrixNeutralIconButton {height: 40 icon_walk: Walk{width: 0 height: 0} spacing: 0 padding: Inset{left: 14 right: 14 top: 0 bottom: 0}}
    let Primary = RobrixIconButton {height: 40 icon_walk: Walk{width: 0 height: 0} spacing: 0 padding: Inset{left: 14 right: 14 top: 0 bottom: 0}}

    mod.widgets.CreateAgentDialog = #(CreateAgentDialog::register_widget(vm)) {
        ..mod.widgets.SmallModal
        width: Fill{max: 480} spacing: 14
        ModalTitle {text: #(crate::i18n::tr("Create agent")) i18n_text: "Create agent" margin: Inset{bottom: 6}}
        // Fields scroll; the title and the Cancel/Create row stay visible on short windows.
        fields := ScrollYView {width: Fill height: Fit{max: FitBound.Rel{base: Base.Full, factor: 0.7}} flow: Down spacing: 14
        Field {Text {text: #(crate::i18n::tr("Name")) i18n_text: "Name"} name := Input {empty_text: #(crate::i18n::tr("Agent display name")) i18n_empty_text: "Agent display name"}}
        Field {
            Text {text: #(crate::i18n::tr("Harness")) i18n_text: "Harness"}
            harness := Select {labels: [#(crate::i18n::tr("Checking this Mac's logins…"))]}
        }
        View {width: Fill height: Fit flow: Right spacing: 12
            Field {Text {text: #(crate::i18n::tr("Model")) i18n_text: "Model"} model := Select {labels: [#(crate::i18n::tr("Checking model choices…"))]}}
            Field {width: 150 Text {text: #(crate::i18n::tr("Thinking")) i18n_text: "Thinking"} thinking := Select {labels: [#(crate::i18n::tr("Low")) #(crate::i18n::tr("Medium")) #(crate::i18n::tr("High"))]}}
        }
        runs_on := Hint {}
        Field {
            rooms_heading := Text {text: #(crate::i18n::tr("Add to rooms")) i18n_text: "Add to rooms"}
            room_0 := Tick {} room_1 := Tick {} room_2 := Tick {} room_3 := Tick {} room_4 := Tick {} room_5 := Tick {} room_6 := Tick {} room_7 := Tick {}
            rooms_hint := Hint {}
        }
        // Below the rooms: the checklist must stay above the fold (rooms.rs scar test).
        Field {Text {text: #(crate::i18n::tr("Backup model")) i18n_text: "Backup model"} backup := Select {labels: [#(crate::i18n::tr("None"))]}}
        backup_hint := Hint {}
        }
        status := Text {}
        ModalButtonsRow {
            sign_in := Primary {visible: false text: #(crate::i18n::tr("Sign in again")) i18n_text: "Sign in again"}
            cancel := Action {text: #(crate::i18n::tr("Cancel")) i18n_text: "Cancel"}
            create := Primary {text: #(crate::i18n::tr("Create agent")) i18n_text: "Create agent"}
        }
    }
}

/// Opens or closes the dialog; the app shell owns its modal.
#[derive(Clone, Debug)]
pub enum CreateAgentAction {
    Open,
    Close,
}
#[derive(Debug)]
struct Probed { request: u64, index: usize, result: Result<Value, String> }
#[derive(Debug)]
struct Created { request: u64, identity: String, result: Result<Value, String> }
static REQUEST: AtomicU64 = AtomicU64::new(1);
fn next_request() -> u64 { REQUEST.fetch_add(1, Ordering::Relaxed) }

#[path = "rooms.rs"]
mod rooms;
#[path = "backup.rs"]
mod backup;

#[derive(Script, ScriptHook, Widget)]
pub struct CreateAgentDialog {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] state: DialogState,
    #[rust] request: u64,
    #[rust] busy: bool,
    #[rust] timer: Timer,
}
impl Widget for CreateAgentDialog {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if self.timer.is_event(event).is_some() && self.busy {
            let line = simple_agent::progress_text(simple_agent::current_step());
            self.view.label(cx, ids!(status)).set_text(cx, crate::i18n::tr(line));
            self.timer = cx.start_timeout(0.5);
        }
        if let Event::Actions(actions) = event {
            self.handle_actions(cx, actions);
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl CreateAgentDialog {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions {
            if let Some(probed) = action.downcast_ref::<Probed>() && probed.request == self.request {
                let models = probed.result.as_ref().map(parse_models).map_err(Clone::clone);
                self.state.probes[probed.index] = Some(models);
                self.render(cx);
            }
            if let Some(created) = action.downcast_ref::<Created>() && created.request == self.request {
                self.finish(cx, &created.identity, &created.result);
            }
        }
        self.handle_room_actions(cx, actions);
        self.handle_backup_actions(cx, actions);
        if self.view.button(cx, ids!(cancel)).clicked(actions) { cx.action(CreateAgentAction::Close); }
        if self.view.button(cx, ids!(sign_in)).clicked(actions) {
            cx.action(CreateAgentAction::Close);
            cx.action(crate::accounts::AccountAction::Reauthenticate);
        }
        if self.view.button(cx, ids!(create)).clicked(actions) { self.create(cx); }
        if let Some(name) = self.view.text_input(cx, ids!(name)).changed(actions) { self.state.name = name; self.render_create(cx); }
        if let Some(index) = self.view.drop_down(cx, ids!(harness)).selected(actions) { self.choose_harness(cx, index); }
        if let Some(index) = self.view.drop_down(cx, ids!(model)).selected(actions) { self.state.model = index; }
        if let Some(index) = self.view.drop_down(cx, ids!(thinking)).selected(actions) { self.state.thinking = index; }
    }

    fn open(&mut self, cx: &mut Cx) {
        self.request = next_request();
        self.busy = false;
        self.state = DialogState::new(|runtime| default_name(&localpart(), runtime));
        self.view.text_input(cx, ids!(name)).set_text(cx, &self.state.name);
        self.view.drop_down(cx, ids!(thinking)).set_selected_item(cx, self.state.thinking);
        self.view.label(cx, ids!(status)).set_text(cx, "");
        match Service::for_current() {
            Ok(service) => self.probe(service),
            Err(code) => self.state.probes = [Some(Err(code.clone())), Some(Err(code))],
        }
        self.load_rooms(cx);
        self.load_backups(cx);
        self.render(cx);
    }
    /// Read-only: lists each harness's models without connecting a login.
    fn probe(&self, service: Service) {
        for (index, runtime) in DialogState::probe_commands() {
            let (request, service) = (self.request, service.clone());
            let Some(command) = simple_agent::provider_command(&runtime, ProviderAction::Models) else { continue };
            spawn_async_task(async move {
                let result = service.execute(command).await;
                Cx::post_action(Probed { request, index, result });
            });
        }
    }

    fn choose_harness(&mut self, cx: &mut Cx, index: usize) {
        let before = self.state.runtime();
        self.state.harness = index;
        self.state.model = 0;
        if let (Some(before), Some(after)) = (before, self.state.runtime())
            && self.state.name == default_name(&localpart(), &before)
        {
            self.state.name = default_name(&localpart(), &after);
            self.view.text_input(cx, ids!(name)).set_text(cx, &self.state.name);
        }
        self.render(cx);
    }

    fn render(&mut self, cx: &mut Cx) {
        let available = self.state.available();
        self.state.harness = self.state.harness.min(available.len().saturating_sub(1));
        let labels: Vec<String> = if available.is_empty() {
            vec![crate::i18n::tr(if self.state.needs_sign_in() { "Agent sign-in expired" } else if self.state.checking() { "Checking this Mac's logins…" } else { "No Claude Code or Codex login found" }).into()]
        } else {
            available.iter().map(|i| format!("{} · {}", crate::i18n::tr("Local"), harness_label(&super::HARNESSES[*i]))).collect()
        };
        self.view.drop_down(cx, ids!(harness)).set_labels(cx, labels);
        self.view.drop_down(cx, ids!(harness)).set_selected_item(cx, self.state.harness);
        let models: Vec<String> = self.state.models().iter().map(|m| m.label.clone()).collect();
        self.state.model = self.state.model.min(models.len().saturating_sub(1));
        let empty = vec![crate::i18n::tr(if self.state.checking() { "Checking model choices…" } else { "No models" }).into()];
        self.view.drop_down(cx, ids!(model)).set_labels(cx, if models.is_empty() { empty } else { models });
        self.view.drop_down(cx, ids!(model)).set_selected_item(cx, self.state.model);
        self.render_lines(cx);
        self.render_create(cx);
    }
    fn render_lines(&mut self, cx: &mut Cx) {
        let runs_on = match self.state.runtime() {
            Some(runtime) => crate::i18n::tr("Runs on this Mac with your {harness} login; usage counts against that account.").replace("{harness}", harness_label(&runtime)),
            None if self.state.needs_sign_in() => crate::i18n::tr("Your Agent sign-in on this Mac expired. Click Sign in again to reconnect your Agents; your rooms and Agents are kept.").into(),
            None if self.state.checking() => String::new(),
            None => crate::i18n::tr("Sign in on this Mac first: run `claude login` or `codex login` in a terminal, then reopen this dialog.").into(),
        };
        self.view.label(cx, ids!(runs_on)).set_text(cx, &runs_on);
        let sign_in = self.state.available().is_empty() && self.state.needs_sign_in();
        self.view.button(cx, ids!(sign_in)).set_visible(cx, sign_in);
    }
    fn render_create(&mut self, cx: &mut Cx) {
        let ready = !self.busy && self.state.spec(quick_setup::default_spec).is_ok();
        self.view.button(cx, ids!(create)).set_enabled(cx, ready);
        self.view.redraw(cx);
    }

    fn create(&mut self, cx: &mut Cx) {
        if self.busy { return; }
        let spec = match self.state.spec(quick_setup::default_spec) {
            Ok(spec) => spec,
            Err(reason) => { self.view.label(cx, ids!(status)).set_text(cx, crate::i18n::tr(reason)); return; }
        };
        let service = match Service::for_current() {
            Ok(service) => service,
            Err(code) => { self.view.label(cx, ids!(status)).set_text(cx, &code); return; }
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
    fn finish(&mut self, cx: &mut Cx, identity: &str, result: &Result<Value, String>) {
        self.busy = false;
        cx.stop_timer(self.timer);
        match result {
            Ok(value) => {
                crate::hagency::agent_presence::set_has_agent(true);
                self.bind_ticked(value);
                // The new Agent also joins the group rooms its owner made earlier.
                crate::hagency::bring_agent::catch_up_agents();
                self.save_backup(value);
                cx.action(crate::home::project_tree::ProjectTreeChanged { identity: identity.to_owned() });
                if let Some(room_id) = value["quickSetup"]["ownerDirectRoomId"].as_str().and_then(|id| matrix_sdk::ruma::OwnedRoomId::try_from(id).ok()) {
                    cx.action(crate::hagency::ui::OpenAgentChat { room_id });
                }
                cx.action(CreateAgentAction::Close);
            }
            Err(code) => {
                let step = simple_agent::last_failed_step().unwrap_or("setup");
                let message = super::failure_message(step, code);
                self.view.label(cx, ids!(status)).set_text(cx, &message);
                self.render_create(cx);
            }
        }
    }

}

fn localpart() -> String {
    current_user_id().map(|id| id.localpart().to_owned()).unwrap_or_else(|| "my".into())
}

impl CreateAgentDialogRef {
    pub fn action(&self, cx: &mut Cx, modal: ModalRef, action: &CreateAgentAction) {
        let Some(mut inner) = self.borrow_mut() else { return };
        match action {
            CreateAgentAction::Open => {
                inner.open(cx);
                modal.open(cx);
            }
            CreateAgentAction::Close => {
                // A late probe or creation result for this opening is ignored.
                inner.request = next_request();
                inner.busy = false;
                cx.stop_timer(inner.timer);
                modal.close(cx);
            }
        }
    }
}
