//! The "GitHub access" settings block of one Agent: paste a token, Save,
//! Remove, and Check what it can reach. The token is kept on this device
//! only (`::hagency::github_access`) and is never shown again once saved.
pub mod text;
#[cfg(test)]
mod tests;

use ::hagency::github_access::check::{CheckError, CheckReport, check};
use ::hagency::github_access::{Token, TokenStore};
use ::hagency::issue_watch::GITHUB_API;
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    let Text = Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}}
    let Muted = Text {draw_text +: {color: mod.widgets.RINX_MUTED}}
    let Button = RobrixIconButton {height: 40 icon_walk: Walk{width: 0 height: 0} spacing: 0 padding: Inset{left: 14 right: 14 top: 0 bottom: 0}}
    mod.widgets.GithubAccessSection = #(GithubAccessSection::register_widget(vm)) {
        width: Fill height: Fit flow: Down spacing: 8
        Text {text: #(crate::i18n::tr("GitHub access")) i18n_text: "GitHub access" draw_text.text_style: theme.font_bold{font_size: (13 * mod.widgets.RINX_TEXT_SCALE)}}
        Muted {text: #(crate::i18n::tr("Create a fine-grained personal access token: GitHub → Settings → Developer settings → Personal access tokens → Fine-grained tokens → Generate new token. Resource owner: your account. Repository access: Only select repositories → your forks (e.g. csheargm/hagency-desktop, csheargm/hagency-server). Permissions: Contents Read and write, Pull requests Read and write, Issues Read and write, Metadata Read-only. Expiration: 30–90 days.")) i18n_text: "Create a fine-grained personal access token: GitHub → Settings → Developer settings → Personal access tokens → Fine-grained tokens → Generate new token. Resource owner: your account. Repository access: Only select repositories → your forks (e.g. csheargm/hagency-desktop, csheargm/hagency-server). Permissions: Contents Read and write, Pull requests Read and write, Issues Read and write, Metadata Read-only. Expiration: 30–90 days."}
        open_new_token := Button {text: #(crate::i18n::tr("Open GitHub: new fine-grained token")) i18n_text: "Open GitHub: new fine-grained token"}
        Muted {text: #(crate::i18n::tr("To open a pull request against an upstream repository, the token needs push access to your fork; the pull request is then created on the upstream repository from your fork's branch. Classic tokens are not recommended.")) i18n_text: "To open a pull request against an upstream repository, the token needs push access to your fork; the pull request is then created on the upstream repository from your fork's branch. Classic tokens are not recommended."}
        Muted {text: #(crate::i18n::tr("Kept on this device only (Keychain) and never sent to the Hagency server. This Agent's Claude or Codex runtime gets it as GH_TOKEN, and its issue watching uses it.")) i18n_text: "Kept on this device only (Keychain) and never sent to the Hagency server. This Agent's Claude or Codex runtime gets it as GH_TOKEN, and its issue watching uses it."}
        token_state := Text {}
        token_input := RobrixTextInput {width: Fill height: 40 margin: 0 is_password: true padding: Inset{left: 12 right: 12 top: 8 bottom: 8} empty_text: "github_pat_…"}
        View {width: Fill height: Fit flow: Right spacing: 8
            save_token := Button {text: #(crate::i18n::tr("Save token")) i18n_text: "Save token"}
            remove_token := Button {text: #(crate::i18n::tr("Remove token")) i18n_text: "Remove token"}
            check_token := Button {text: #(crate::i18n::tr("Check")) i18n_text: "Check"}
        }
        token_note := Text {}
    }
}

/// What a background step finished with; stale ones are dropped.
#[derive(Debug)]
enum Done {
    /// Whether the Agent has a token now (after a read, save or remove).
    Stored(Result<bool, String>, &'static str),
    Checked(Result<CheckReport, CheckError>),
}
#[derive(Debug)]
struct Reply {
    request: u64,
    agent_id: String,
    done: Done,
}

#[derive(Script, ScriptHook, Widget)]
pub struct GithubAccessSection {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    agent_id: String,
    /// `None` until the store was read.
    #[rust]
    has_token: Option<bool>,
    #[rust]
    request: u64,
    #[rust]
    pending: bool,
    #[rust]
    note: String,
}

impl Widget for GithubAccessSection {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        for action in actions {
            if let Some(reply) = action.downcast_ref::<Reply>() {
                self.apply(cx, reply);
            }
        }
        if self.view.button(cx, ids!(open_new_token)).clicked(actions) {
            crate::utils::open_url(text::NEW_TOKEN_URL);
        }
        if self.view.button(cx, ids!(save_token)).clicked(actions) {
            self.save(cx);
        }
        if self.view.button(cx, ids!(remove_token)).clicked(actions) {
            let agent = self.agent_id.clone();
            self.start(cx, "Removing…", move || Done::Stored(TokenStore::current().remove(&agent).map(|_| false), "Token removed."));
        }
        if self.view.button(cx, ids!(check_token)).clicked(actions) {
            self.check(cx);
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl GithubAccessSection {
    /// Shows `agent_id`'s state; reads the store again only for a new Agent.
    pub fn set_context(&mut self, cx: &mut Cx, agent_id: &str) {
        if self.agent_id != agent_id {
            self.agent_id = agent_id.to_owned();
            self.has_token = None;
            self.note.clear();
            self.view.text_input(cx, ids!(token_input)).set_text(cx, "");
            if !agent_id.is_empty() {
                let agent = self.agent_id.clone();
                self.start(cx, "", move || Done::Stored(TokenStore::current().load(&agent).map(|t| t.is_some()), ""));
            }
        }
        self.render(cx);
    }
    /// Runs `work` off the UI thread (the Keychain may block or prompt).
    fn start(&mut self, cx: &mut Cx, note: &str, work: impl FnOnce() -> Done + Send + 'static) {
        let (request, agent_id) = self.begin(cx, note);
        crate::sliding_sync::spawn_async_task(async move {
            let done = tokio::task::spawn_blocking(work)
                .await
                .unwrap_or_else(|e| Done::Stored(Err(e.to_string()), ""));
            Cx::post_action(Reply { request, agent_id, done });
        });
    }
    /// A new request: older replies are dropped from now on.
    fn begin(&mut self, cx: &mut Cx, note: &str) -> (u64, String) {
        self.request += 1;
        self.pending = true;
        self.note = crate::i18n::tr(note).to_owned();
        self.render(cx);
        (self.request, self.agent_id.clone())
    }
    fn save(&mut self, cx: &mut Cx) {
        let pasted = self.view.text_input(cx, ids!(token_input)).text();
        let token = match Token::parse(&pasted) {
            Ok(token) => token,
            Err(reason) => {
                self.note = crate::i18n::tr(reason).to_owned();
                return self.render(cx);
            }
        };
        // The field is cleared at once: the token is never shown again.
        self.view.text_input(cx, ids!(token_input)).set_text(cx, "");
        let agent = self.agent_id.clone();
        self.start(cx, "Saving…", move || Done::Stored(TokenStore::current().save(&agent, &token).map(|_| true), "Token saved."));
    }
    fn check(&mut self, cx: &mut Cx) {
        let service = crate::hagency::backend::Service::for_current().ok();
        let agent = self.agent_id.clone();
        let (request, agent_id) = self.begin(cx, "Checking…");
        crate::sliding_sync::spawn_async_task(async move {
            let done = checked(service, agent).await;
            Cx::post_action(Reply { request, agent_id, done });
        });
    }
    fn apply(&mut self, cx: &mut Cx, reply: &Reply) {
        if reply.request != self.request || reply.agent_id != self.agent_id {
            return;
        }
        self.pending = false;
        self.note = match &reply.done {
            Done::Stored(Ok(has), note) => {
                self.has_token = Some(*has);
                crate::i18n::tr(note).to_owned()
            }
            Done::Stored(Err(error), _) => crate::i18n::format("Could not use the token store: {error}", &[("error", error.clone())]),
            Done::Checked(Ok(report)) => text::report_lines(report),
            Done::Checked(Err(error)) => text::error_text(error),
        };
        self.render(cx);
    }
    fn render(&mut self, cx: &mut Cx) {
        let state = match self.has_token {
            Some(true) => "A token is saved for this Agent (hidden).",
            Some(false) => "No token saved for this Agent.",
            None => "",
        };
        self.view.label(cx, ids!(token_state)).set_text(cx, crate::i18n::tr(state));
        let ready = !self.agent_id.is_empty() && !self.pending;
        self.view.button(cx, ids!(save_token)).set_enabled(cx, ready);
        self.view.button(cx, ids!(remove_token)).set_enabled(cx, ready && self.has_token == Some(true));
        self.view.button(cx, ids!(check_token)).set_enabled(cx, ready && self.has_token == Some(true));
        self.view.label(cx, ids!(token_note)).set_text(cx, &self.note);
    }
}

impl GithubAccessSectionRef {
    /// See [`GithubAccessSection::set_context`].
    pub fn set_context(&self, cx: &mut Cx, agent_id: &str) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_context(cx, agent_id);
        }
    }
}

/// "Check": the Agent's token against GitHub, with its watched repository.
async fn checked(service: Option<crate::hagency::backend::Service>, agent: String) -> Done {
    let repos = watched_repos(service, &agent).await;
    let loaded = tokio::task::spawn_blocking(move || TokenStore::current().load(&agent)).await;
    match loaded.unwrap_or_else(|e| Err(e.to_string())) {
        Ok(Some(token)) => Done::Checked(check(&::hagency::issue_watch::Client::new(), GITHUB_API, &token, &repos).await),
        Ok(None) => Done::Stored(Ok(false), "No token saved for this Agent yet."),
        Err(error) => Done::Stored(Err(error), ""),
    }
}
/// The repository the Agent's issue watching names. Only the device that
/// runs the Agent holds that setting; elsewhere (or signed out) there is no
/// repository to check and the result says so instead of failing the check.
async fn watched_repos(service: Option<crate::hagency::backend::Service>, agent: &str) -> Vec<String> {
    let Some(service) = service else { return Vec::new() };
    match crate::hagency::issue_watch::live::saved(&service, agent, None).await {
        Ok(saved) => saved["issueWatch"]["repo"].as_str().filter(|r| !r.is_empty()).map(|r| vec![r.to_owned()]).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;
    /// Feeds one result as if the current request had finished with it.
    pub(in crate::hagency::github_access) fn finish(section: &mut GithubAccessSection, cx: &mut Cx, done: Done) {
        let reply = Reply { request: section.request, agent_id: section.agent_id.clone(), done };
        section.apply(cx, &reply);
    }
    pub(in crate::hagency::github_access) fn stored(has: bool, note: &'static str) -> Done {
        Done::Stored(Ok(has), note)
    }
    pub(in crate::hagency::github_access) fn checked(result: Result<CheckReport, CheckError>) -> Done {
        Done::Checked(result)
    }
}
