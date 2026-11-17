//! Create agent: the optional "What should it work on?" step (ADR 0017).
//! Repositories, one per line (`owner/name [developer|reviewer|maintainer]`,
//! Developer by default), checked against the owner's GitHub login: each must
//! exist and be readable, and Maintainer needs merge rights there. Limits
//! (issues per day, open pull requests, daily token budget) and an optional
//! activity room. Once the Agent exists, the result is saved as the same
//! owner-controlled work list the Agent panel's "Repositories and roles" card
//! edits (`issue_watch::work_lines`). Leaving the step empty skips it.
use ::hagency::issue_watch::{CAN_MERGE, OwnerRules, Role};
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    let Text = Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}}
    let Hint = Text {draw_text +: {color: mod.widgets.RINX_MUTED text_style.font_size: (10.5 * mod.widgets.RINX_TEXT_SCALE)}}
    let Input = RobrixTextInput {width: Fill height: 40 margin: 0 padding: Inset{left: 12 right: 12 top: 8 bottom: 8}
        draw_text.text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}
    mod.widgets.CreateAgentWorkStep = View {
        width: Fill height: Fit flow: Down spacing: 6
        Text {text: #(crate::i18n::tr("What should it work on? (optional)")) i18n_text: "What should it work on? (optional)" draw_text.text_style: theme.font_bold{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}
        work_repos := Input {height: Fit empty_text: "acme/app; acme/docs reviewer"}
        Hint {text: #(crate::i18n::tr(super::work_step::REPOS_HINT)) i18n_text: "One repository per line or separated by ;  owner/name, then developer (default), reviewer or maintainer. Maintainer needs merge rights there. Leave empty for a plain chat agent."}
        work_token := Input {is_password: true empty_text: #(crate::i18n::tr("GitHub token (optional if GH_TOKEN is set)")) i18n_empty_text: "GitHub token (optional if GH_TOKEN is set)"}
        View {width: Fill height: Fit flow: Right spacing: 8
            work_issues := Input {empty_text: #(crate::i18n::tr("Issues per day: 5")) i18n_empty_text: "Issues per day: 5"}
            work_prs := Input {empty_text: #(crate::i18n::tr("Open PRs: 3")) i18n_empty_text: "Open PRs: 3"}
            work_budget := Input {empty_text: #(crate::i18n::tr("Tokens per day")) i18n_empty_text: "Tokens per day"}
        }
        Text {text: #(crate::i18n::tr("Post its activity in")) i18n_text: "Post its activity in"}
        work_room := RobrixSettingsDropDown {width: Fill height: 40 margin: 0 popup_menu_position: PopupMenuPosition.BelowInput
            draw_text.text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}
            labels: [#(crate::i18n::tr("Its private chat with you"))]}
        work_result := Hint {}
    }
}

pub const REPOS_HINT: &str = "One repository per line or separated by ;  owner/name, then developer (default), reviewer or maintainer. Maintainer needs merge rights there. Leave empty for a plain chat agent.";
/// Open pull requests at once when the owner sets nothing.
pub const DEFAULT_OPEN_PRS: u32 = 3;

/// What the owner typed in the step.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct WorkDraft {
    pub repos: String,
    pub issues_per_day: String,
    pub open_prs: String,
    pub budget: String,
}

/// One repository line before it is checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Wanted {
    pub repo: String,
    pub role: Role,
}

/// A limit field: empty means the default, otherwise 1..=`max`.
fn limit(text: &str, max: u64, what: &str) -> Result<Option<u64>, String> {
    let text = text.trim().replace([',', '_'], "");
    if text.is_empty() {
        return Ok(None);
    }
    match text.parse::<u64>() {
        Ok(n) if (1..=max).contains(&n) => Ok(Some(n)),
        _ => Err(crate::i18n::format("{what} must be a whole number from 1 to {max}", &[("what", crate::i18n::tr(what).to_owned()), ("max", max.to_string())])),
    }
}

impl WorkDraft {
    /// The repositories asked for, or why the text is refused. Empty: skipped.
    pub fn wanted(&self) -> Result<Vec<Wanted>, String> {
        let mut out: Vec<Wanted> = Vec::new();
        for line in self.repos.split(['\n', ';']).map(str::trim).filter(|l| !l.is_empty()) {
            let mut words = line.split_whitespace();
            let repo = words.next().unwrap_or_default().to_owned();
            let role = match words.next() {
                None | Some("developer") => Role::Developer,
                Some("reviewer") => Role::Reviewer,
                Some("maintainer") => Role::Maintainer,
                Some(other) => return Err(crate::i18n::format("{repo}: the role must be developer, reviewer or maintainer, not {other}", &[("repo", repo.clone()), ("other", other.to_owned())])),
            };
            if words.next().is_some() || !::hagency::issue_watch::valid_repo(&repo) {
                return Err(crate::i18n::format("{line}: write owner/name and at most one role", &[("line", line.to_owned())]));
            }
            if out.iter().any(|w| w.repo.eq_ignore_ascii_case(&repo)) {
                return Err(crate::i18n::format("{repo} is listed twice", &[("repo", repo)]));
            }
            out.push(Wanted { repo, role });
        }
        Ok(out)
    }
    /// The daily token budget to create the Agent with, when set.
    pub fn budget(&self) -> Result<Option<u64>, String> {
        limit(&self.budget, u64::from(u32::MAX), "Tokens per day")
    }
    /// The work list for a checked set of repositories.
    pub fn entries(&self, wanted: &[Wanted], room_id: &str, now: i64) -> Result<Vec<OwnerRules>, String> {
        let max = u64::from(::hagency::issue_watch::MAX_WORK_LIMIT);
        let issues = limit(&self.issues_per_day, max, "Issues per day")?.map(|n| n as u32);
        let prs = limit(&self.open_prs, max, "Open PRs")?.map_or(DEFAULT_OPEN_PRS, |n| n as u32);
        wanted.iter().map(|w| {
            let developer = w.role == Role::Developer;
            OwnerRules {
                repo: w.repo.clone(), role: w.role, room_id: room_id.to_owned(), since: now,
                issues_per_day: issues.filter(|_| developer), open_prs: developer.then_some(prs),
                ..Default::default()
            }.validate().map_err(|e| format!("{}: {e}", w.repo))
        }).collect()
    }
}

/// What GitHub says about one repository for the owner's login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Access {
    /// Readable, with the login's permission (`admin`, `write`, `read`, ...).
    Readable(String),
    /// Missing, private to someone else, or the token cannot see it.
    NotFound,
}

/// The owner's GitHub as the step needs it; the live one is `LiveGithub`.
pub(crate) trait Github {
    async fn login(&self) -> Result<String, String>;
    /// The login's own access to `repo`, read with that login's token.
    async fn access(&self, repo: &str) -> Result<Access, String>;
}

/// Checks every repository; `Err` lists each refusal, one per line.
pub(crate) async fn check(github: &impl Github, wanted: &[Wanted]) -> Result<Vec<String>, String> {
    let login = github.login().await?;
    let (mut good, mut bad) = (Vec::new(), Vec::new());
    for w in wanted {
        match (github.access(&w.repo).await?, w.role) {
            (Access::NotFound, _) => bad.push(crate::i18n::format("{repo}: not found, or {login} cannot read it", &[("repo", w.repo.clone()), ("login", login.clone())])),
            (Access::Readable(p), Role::Maintainer) if !CAN_MERGE.contains(&p.as_str()) => bad.push(crate::i18n::format("{repo}: Maintainer needs merge rights; {login} has {permission}. Choose developer or reviewer.", &[("repo", w.repo.clone()), ("login", login.clone()), ("permission", p)])),
            (Access::Readable(p), _) => good.push(crate::i18n::format("{repo}: readable by {login} ({permission})", &[("repo", w.repo.clone()), ("login", login.clone()), ("permission", p)])),
        }
    }
    if bad.is_empty() { Ok(good) } else { Err(bad.join("\n")) }
}

/// The owner's GitHub through the GitHub access check's reads.
pub(crate) struct LiveGithub {
    pub client: ::hagency::issue_watch::Client,
    pub token: ::hagency::github_access::Token,
}
impl Github for LiveGithub {
    async fn login(&self) -> Result<String, String> {
        ::hagency::issue_watch::fetch_login(&self.client, ::hagency::issue_watch::GITHUB_API, self.token.expose()).await
            .map_err(|e| ::hagency::github_access::redact(&e.to_string(), self.token.expose()))
    }
    async fn access(&self, repo: &str) -> Result<Access, String> {
        let permission = ::hagency::github_access::check::permission(&self.client, ::hagency::issue_watch::GITHUB_API, &self.token, repo).await?;
        Ok(permission.map_or(Access::NotFound, Access::Readable))
    }
}

#[cfg(test)]
#[path = "work_step_tests.rs"]
mod tests;
