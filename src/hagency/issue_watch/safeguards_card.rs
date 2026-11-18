//! The "Safeguards" card under issue watching (ADR 0026): one glance at what
//! the watching Agent may do alone, what needs the owner, and the limits.
//! Lines come from [`super::safeguards`]; this file only reads the saved
//! watch and the repository's guards, then shows them. Empty while the
//! watch is Off or runs on another device; a watch that cannot be read says
//! why. The root stays visible (only `content` hides): a parent View neither
//! draws a hidden child nor routes actions to it, so a hidden root could
//! never pick up a read that finished while the Settings tab was hidden.
use super::safeguards::{self, Enforced, Repo, Watch};
use super::live;
use crate::hagency::backend::Service;
use makepad_widgets::*;
use std::sync::Mutex;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    let Text = Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}}
    let Group = Text {draw_text.text_style: theme.font_bold{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}
    let Muted = Text {draw_text +: {color: mod.widgets.RINX_MUTED}}
    mod.widgets.SafeguardsCard = #(SafeguardsCard::register_widget(vm)) {
        width: Fill height: Fit flow: Down
        content := View {visible: false width: Fill height: Fit flow: Down spacing: 4 margin: Inset{top: 8}
            Text {text: #(crate::i18n::tr("Safeguards")) i18n_text: "Safeguards" draw_text.text_style: theme.font_bold{font_size: (13 * mod.widgets.RINX_TEXT_SCALE)}}
            failed := View {visible: false width: Fill height: Fit reason := Muted {}}
            groups := View {width: Fill height: Fit flow: Down spacing: 4
                Group {text: #(crate::i18n::tr("May do alone")) i18n_text: "May do alone"}
                alone := Muted {}
                Group {text: #(crate::i18n::tr("Needs you")) i18n_text: "Needs you"}
                needs_you := Muted {}
                Group {text: #(crate::i18n::tr("Limits")) i18n_text: "Limits"}
                limits := Muted {}
                protection := Text {margin: Inset{top: 4}}
            }
        }
    }
}

/// What the card shows for one Agent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Shown {
    /// Nothing: the watch is Off, runs elsewhere, or is still being read.
    #[default]
    Hidden,
    Card(Watch, Repo),
    /// An active watch whose saved state could not be read (R5: say so).
    Failed(String),
}

/// One finished read for one Agent; stale ones are dropped.
#[derive(Clone, Debug)]
struct Loaded {
    request: u64,
    agent_id: String,
    shown: Shown,
}

/// The last read, for a card that did not receive the action (nested page).
static LATEST: Mutex<Option<Loaded>> = Mutex::new(None);
/// How long the saved-watch read may take before the card says it failed.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Script, ScriptHook, Widget)]
pub struct SafeguardsCard {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    agent_id: String,
    #[rust]
    auto_merge: bool,
    #[rust]
    request: u64,
    #[rust]
    shown: Shown,
}

impl Widget for SafeguardsCard {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        for action in actions {
            if let Some(loaded) = action.downcast_ref::<Loaded>() {
                self.apply(cx, loaded);
            }
        }
        // A saved (or re-read) watch may have changed the repository or push-freely.
        if !self.agent_id.is_empty() && actions.iter().any(|a| a.downcast_ref::<super::section::IssueWatchReplyReady>().is_some()) {
            self.load();
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.apply_latest(cx);
        self.view.draw_walk(cx, scope, walk)
    }
}

impl SafeguardsCard {
    /// Shows `agent_id`'s card while this device runs it (`editable`); reads
    /// again only when the Agent or its merge switch changed.
    pub fn set_context(&mut self, cx: &mut Cx, agent_id: &str, editable: Option<bool>, auto_merge: bool) {
        if editable != Some(true) {
            self.agent_id.clear();
            self.shown = Shown::Hidden;
            return self.render(cx);
        }
        if self.agent_id != agent_id || self.auto_merge != auto_merge {
            self.agent_id = agent_id.to_owned();
            self.auto_merge = auto_merge;
            self.load();
        }
    }
    fn load(&mut self) {
        self.request += 1;
        let (request, agent_id, auto_merge) = (self.request, self.agent_id.clone(), self.auto_merge);
        let service = Service::for_current();
        crate::sliding_sync::spawn_async_task(async move {
            let shown = match service {
                Ok(service) => read(&service, &agent_id, auto_merge).await,
                Err(error) => Err(error),
            };
            let shown = shown.unwrap_or_else(|error| {
                makepad_widgets::log!("WARN safeguards_card_unreadable agent={agent_id}: {error}");
                Shown::Failed(error)
            });
            finish(Loaded { request, agent_id, shown });
        });
    }
    /// The finished read, if it is this card's current one: the action is
    /// dropped while an ancestor (the Settings tab) is hidden, so every draw
    /// looks here too.
    fn apply_latest(&mut self, cx: &mut Cx) {
        let latest = LATEST.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(loaded) = latest.filter(|l| l.request == self.request && l.agent_id == self.agent_id) {
            self.apply(cx, &loaded);
        }
    }
    fn apply(&mut self, cx: &mut Cx, loaded: &Loaded) {
        if loaded.request != self.request || loaded.agent_id != self.agent_id || self.shown == loaded.shown {
            return;
        }
        self.shown = loaded.shown.clone();
        self.render(cx);
    }
    fn render(&mut self, cx: &mut Cx) {
        let (card, reason) = match &self.shown {
            Shown::Hidden => (None, None),
            Shown::Card(watch, repo) => (Some(safeguards::card(watch, repo, &Enforced::current())), None),
            Shown::Failed(error) => (None, Some(crate::i18n::format("Safeguards could not be read: {error}", &[("error", error.clone())]))),
        };
        self.view.view(cx, ids!(content)).set_visible(cx, card.is_some() || reason.is_some());
        self.view.view(cx, ids!(groups)).set_visible(cx, card.is_some());
        self.view.view(cx, ids!(failed)).set_visible(cx, reason.is_some());
        self.view.label(cx, ids!(reason)).set_text(cx, reason.as_deref().unwrap_or(""));
        let card = card.unwrap_or_default();
        self.view.label(cx, ids!(alone)).set_text(cx, &bullets(&card.alone));
        self.view.label(cx, ids!(needs_you)).set_text(cx, &bullets(&card.needs_you));
        self.view.label(cx, ids!(limits)).set_text(cx, &bullets(&card.limits));
        self.view.label(cx, ids!(protection)).set_text(cx, &card.protection);
        self.view.redraw(cx);
    }
}

/// Keeps `loaded` for the next draw and tells the card it is ready.
fn finish(loaded: Loaded) {
    LATEST.lock().unwrap_or_else(|p| p.into_inner()).replace(loaded.clone());
    Cx::post_action(loaded);
}

/// The saved watch, then the repository's guards with the Agent's token.
/// `Hidden` for an Off watch; `Err` when the saved watch cannot be read.
async fn read(service: &Service, agent_id: &str, auto_merge: bool) -> Result<Shown, String> {
    let saved = tokio::time::timeout(READ_TIMEOUT, live::saved(service, agent_id, None)).await
        .map_err(|_| "no answer from this device's Agent host within 10 s".to_owned())??;
    let Some(watch) = safeguards::watch_of(&saved["issueWatch"], auto_merge) else { return Ok(Shown::Hidden) };
    let repo = match super::github_token(agent_id).await {
        Ok(token) => {
            let client = ::hagency::issue_watch::new_client();
            match ::hagency::issue_watch::safeguards::fetch_repo_guards(&client, ::hagency::issue_watch::GITHUB_API, &watch.repo, &token).await {
                Ok(guards) => Repo::Read { branch: guards.branch, protected: guards.protected, policy: guards.policy },
                Err(error) => Repo::Failed(error.to_string()),
            }
        }
        Err(error) => Repo::Failed(error),
    };
    Ok(Shown::Card(watch, repo))
}

/// One "• line" per entry.
pub(crate) fn bullets(lines: &[String]) -> String {
    lines.iter().map(|l| format!("• {l}")).collect::<Vec<_>>().join("\n")
}

impl SafeguardsCardRef {
    /// See [`SafeguardsCard::set_context`].
    pub fn set_context(&self, cx: &mut Cx, agent_id: &str, editable: Option<bool>, auto_merge: bool) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_context(cx, agent_id, editable, auto_merge);
        }
    }
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;
    /// Shows `shown` as if the current read had finished with it.
    pub fn show(card: &mut SafeguardsCard, cx: &mut Cx, agent_id: &str, shown: Shown) {
        card.agent_id = agent_id.to_owned();
        let loaded = Loaded { request: card.request, agent_id: agent_id.to_owned(), shown };
        card.apply(cx, &loaded);
    }
    /// Finishes the current read as `load` does, with the action never
    /// routed to the card (hidden tab), then runs the draw-time pickup.
    pub fn finish_unrouted(card: &mut SafeguardsCard, cx: &mut Cx, agent_id: &str, shown: Shown) {
        card.agent_id = agent_id.to_owned();
        card.request += 1;
        super::finish(Loaded { request: card.request, agent_id: agent_id.to_owned(), shown });
        card.apply_latest(cx);
    }
}
