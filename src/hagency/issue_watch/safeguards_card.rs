//! The "Safeguards" card under issue watching (ADR 0026): one glance at what
//! the watching Agent may do alone, what needs the owner, and the limits.
//! Lines come from [`super::safeguards`]; this file only reads the saved
//! watch and the repository's guards, then shows them. Hidden while the
//! watch is Off or runs on another device.
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
        visible: false width: Fill height: Fit flow: Down spacing: 4 margin: Inset{top: 8}
        Text {text: #(crate::i18n::tr("Safeguards")) i18n_text: "Safeguards" draw_text.text_style: theme.font_bold{font_size: (13 * mod.widgets.RINX_TEXT_SCALE)}}
        Group {text: #(crate::i18n::tr("May do alone")) i18n_text: "May do alone"}
        alone := Muted {}
        Group {text: #(crate::i18n::tr("Needs you")) i18n_text: "Needs you"}
        needs_you := Muted {}
        Group {text: #(crate::i18n::tr("Limits")) i18n_text: "Limits"}
        limits := Muted {}
        protection := Text {margin: Inset{top: 4}}
    }
}

/// One finished read for one Agent; stale ones are dropped.
#[derive(Clone, Debug)]
struct Loaded {
    request: u64,
    agent_id: String,
    shown: Option<(Watch, Repo)>,
}

/// The last read, for a card that did not receive the action (nested page).
static LATEST: Mutex<Option<Loaded>> = Mutex::new(None);
/// How long the saved-watch read may take before the card stays hidden.
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
    shown: Option<(Watch, Repo)>,
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
        let latest = LATEST.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(loaded) = latest.filter(|l| l.request == self.request && l.agent_id == self.agent_id) {
            self.apply(cx, &loaded);
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

impl SafeguardsCard {
    /// Shows `agent_id`'s card while this device runs it (`editable`); reads
    /// again only when the Agent or its merge switch changed.
    pub fn set_context(&mut self, cx: &mut Cx, agent_id: &str, editable: Option<bool>, auto_merge: bool) {
        if editable != Some(true) {
            self.agent_id.clear();
            self.shown = None;
            return self.render(cx);
        }
        if self.agent_id != agent_id || self.auto_merge != auto_merge {
            self.agent_id = agent_id.to_owned();
            self.auto_merge = auto_merge;
            self.load();
        }
    }
    fn load(&mut self) {
        let Ok(service) = Service::for_current() else { return };
        self.request += 1;
        let (request, agent_id, auto_merge) = (self.request, self.agent_id.clone(), self.auto_merge);
        crate::sliding_sync::spawn_async_task(async move {
            let shown = read(&service, &agent_id, auto_merge).await;
            let loaded = Loaded { request, agent_id, shown };
            LATEST.lock().unwrap_or_else(|p| p.into_inner()).replace(loaded.clone());
            Cx::post_action(loaded);
        });
    }
    fn apply(&mut self, cx: &mut Cx, loaded: &Loaded) {
        if loaded.request != self.request || loaded.agent_id != self.agent_id || self.shown == loaded.shown {
            return;
        }
        self.shown = loaded.shown.clone();
        self.render(cx);
    }
    fn render(&mut self, cx: &mut Cx) {
        let card = self.shown.as_ref().map(|(watch, repo)| safeguards::card(watch, repo, &Enforced::current()));
        self.view.set_visible(cx, card.is_some());
        let card = card.unwrap_or_default();
        self.view.label(cx, ids!(alone)).set_text(cx, &bullets(&card.alone));
        self.view.label(cx, ids!(needs_you)).set_text(cx, &bullets(&card.needs_you));
        self.view.label(cx, ids!(limits)).set_text(cx, &bullets(&card.limits));
        self.view.label(cx, ids!(protection)).set_text(cx, &card.protection);
        self.view.redraw(cx);
    }
}

/// The saved watch, then the repository's guards with the Agent's token. A
/// watch that cannot be read hides the card (the section above says why).
async fn read(service: &Service, agent_id: &str, auto_merge: bool) -> Option<(Watch, Repo)> {
    let saved = tokio::time::timeout(READ_TIMEOUT, live::saved(service, agent_id, None)).await.ok()?.ok()?;
    let watch = safeguards::watch_of(&saved["issueWatch"], auto_merge)?;
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
    Some((watch, repo))
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
    pub fn show(card: &mut SafeguardsCard, cx: &mut Cx, agent_id: &str, shown: Option<(Watch, Repo)>) {
        card.agent_id = agent_id.to_owned();
        let loaded = Loaded { request: card.request, agent_id: agent_id.to_owned(), shown };
        card.apply(cx, &loaded);
    }
}
