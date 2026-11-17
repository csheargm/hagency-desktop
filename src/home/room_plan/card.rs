//! The planner card: the plan's repository and title, its issues one per line
//! (editable before filing) and "File these issues". Filing runs on the Agent
//! owner's device (`file`); anywhere else it answers why it cannot.
use super::{PlanCard, edited, titles};
use crate::sliding_sync::spawn_async_task;
use makepad_widgets::*;
use matrix_sdk::ruma::{OwnedRoomId, RoomId};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.PlanProposalCard = #(PlanProposalCard::register_widget(vm)) {
        ..mod.widgets.RoundedView
        visible: false width: Fill height: Fit flow: Down spacing: 6
        margin: Inset{top: 8 right: 10} padding: 12
        draw_bg +: {color: (mod.widgets.RBX_BG_SURFACE) border_radius: 4}
        plan_title := Label {width: Fill height: Fit flow: Flow.Right{wrap: true}
            draw_text +: {color: (mod.widgets.RBX_FG_PRIMARY) text_style: theme.font_bold{font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}}
        Label {width: Fill height: Fit draw_text.color: (mod.widgets.RBX_FG_SECONDARY)
            text: #(crate::i18n::tr("Issues, in order (edit the titles before filing):")) i18n_text: "Issues, in order (edit the titles before filing):"}
        plan_issues := TextInput {width: Fill height: Fit is_multiline: true submit_on_enter: false}
        plan_status := Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text.color: (mod.widgets.RBX_FG_SECONDARY)}
        plan_file := RobrixPositiveIconButton {
            width: Fit height: 32 padding: Inset{left: 12 right: 12 top: 0 bottom: 0} icon_walk: Walk{width: 0 height: 0}
            text: #(crate::i18n::tr("File these issues")) i18n_text: "File these issues"
        }
    }
}

/// What filing did, per plan id, for this run of the app.
static OUTCOMES: LazyLock<Mutex<HashMap<String, Outcome>>> = LazyLock::new(Default::default);

#[derive(Clone, Debug, PartialEq)]
enum Outcome {
    Filing,
    Filed(Vec<u64>),
    Failed(String),
}

#[derive(Debug)]
struct Filed { id: String }

fn outcome(id: &str) -> Option<Outcome> {
    OUTCOMES.lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned()
}
fn set_outcome(id: &str, value: Outcome) {
    OUTCOMES.lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_owned(), value);
}

#[derive(Script, ScriptHook, Widget)]
pub struct PlanProposalCard {
    #[deref]
    pub view: View,
    #[rust]
    context: Option<(OwnedRoomId, PlanCard)>,
}

impl PlanProposalCard {
    fn sync(&mut self, cx: &mut Cx) {
        let Some((_, card)) = &self.context else {
            self.view.set_visible(cx, false);
            return;
        };
        self.view.set_visible(cx, true);
        let heading = crate::i18n::format("Plan for {repo}: {title}", &[("repo", card.plan.repo.clone()), ("title", card.plan.title.clone())]);
        self.view.label(cx, ids!(plan_title)).set_text(cx, &heading);
        let state = outcome(&card.id);
        let status = match &state {
            None => String::new(),
            Some(Outcome::Filing) => crate::i18n::tr("Filing on GitHub…").to_owned(),
            Some(Outcome::Filed(numbers)) => crate::i18n::format("Filed: {issues}. You will be told here when they are merged.", &[("issues", numbers.iter().map(|n| format!("#{n}")).collect::<Vec<_>>().join(", "))]),
            Some(Outcome::Failed(error)) => error.clone(),
        };
        self.view.label(cx, ids!(plan_status)).set_text(cx, &status);
        let open = matches!(state, None | Some(Outcome::Failed(_)));
        self.view.button(cx, ids!(plan_file)).set_visible(cx, open);
        self.view.text_input(cx, ids!(plan_issues)).set_disabled(cx, !open);
        self.view.redraw(cx);
    }
    fn file(&mut self, cx: &mut Cx) {
        let Some((room, card)) = self.context.clone() else { return };
        let plan = match edited(&card.plan, &self.view.text_input(cx, ids!(plan_issues)).text()) {
            Ok(plan) => plan,
            Err(reason) => { set_outcome(&card.id, Outcome::Failed(reason)); return self.sync(cx); }
        };
        set_outcome(&card.id, Outcome::Filing);
        self.sync(cx);
        spawn_async_task(async move {
            let result = super::file::file(room, card.agent.clone(), card.id.clone(), plan).await;
            set_outcome(&card.id, match result { Ok(numbers) => Outcome::Filed(numbers), Err(error) => Outcome::Failed(error) });
            Cx::post_action(Filed { id: card.id });
        });
    }
}

impl Widget for PlanProposalCard {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        let Some((_, card)) = self.context.clone() else { return };
        if actions.iter().any(|a| a.downcast_ref::<Filed>().is_some_and(|f| f.id == card.id)) {
            return self.sync(cx);
        }
        if self.view.button(cx, ids!(plan_file)).clicked(actions) {
            self.file(cx);
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl PlanProposalCardRef {
    /// Shows the card for `card`, or hides it (`None`: not a plan message).
    pub fn show(&self, cx: &mut Cx, room: &RoomId, card: Option<PlanCard>) {
        let Some(mut inner) = self.borrow_mut() else { return };
        let changed = inner.context.as_ref().map(|(_, c)| &c.id) != card.as_ref().map(|c| &c.id);
        if changed && let Some(card) = &card {
            inner.view.text_input(cx, ids!(plan_issues)).set_text(cx, &titles(&card.plan));
        }
        inner.context = card.map(|c| (room.to_owned(), c));
        inner.sync(cx);
    }
}
