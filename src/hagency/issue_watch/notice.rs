//! The watcher's hand-off message: plain text the Agent reads (with the
//! issue URL), a mention of the Agent so dispatch wakes it exactly like a
//! typed request, and an `io.hagency.issue_handoff` field that marks it as
//! the watcher's. The timeline draws a marked message as a compact notice
//! (`view`) with no link preview, so it never looks typed by the owner.
use ::hagency::issue_watch::Issue;
use matrix_sdk::ruma::UserId;
use matrix_sdk::ruma::events::Mentions;
use matrix_sdk::ruma::events::room::message::{FormattedBody, RoomMessageEventContent};
use serde_json::{Map, Value, json};

/// The content field that marks a message as the issue watcher's hand-off.
pub const MARKER: &str = "io.hagency.issue_handoff";

/// One issue as the hand-off message carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub repo: String,
    pub number: i64,
    pub url: String,
    pub title: String,
    pub agent: String,
    /// The watching Agent's auto-merge switch is on: the hand-off also tells it
    /// to carry the pull request on to review (the merge itself stays gated).
    pub merge_policy: bool,
}

/// What the hand-off adds when the watching Agent's auto-merge is on, so the
/// fixer continues past the draft without a person's prompt.
// The data line that carries the issue title opens with
// `::hagency::ISSUE_TITLE_LABEL`. The title is written by whoever opened the
// issue, so it follows the fixed request, quoted, and the Agent's
// instructions (the runtime's AGENT_MERGE_NOTE) name that label as data.
/// The longest title the data line carries, in characters.
const MAX_TITLE_CHARS: usize = 200;

/// The title as one quoted, inert line: no quotes or backticks that could
/// close the quote, no newlines or control characters, capped in length.
pub fn quoted_title(title: &str) -> String {
    let inert: String = title.chars().map(|c| match c {
        '"' | '`' | '“' | '”' => '\'',
        c if c.is_control() => ' ',
        c => c,
    }).collect();
    let words = inert.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut capped: String = words.chars().take(MAX_TITLE_CHARS).collect();
    if words.chars().count() > MAX_TITLE_CHARS {
        capped.push('…');
    }
    format!("\"{capped}\"")
}

pub const MERGE_POLICY_STEP: &str = "This repository has an agent merge policy: write \"Closes #<issue>\" in the pull request description and, when it is ready, mark it ready (gh pr ready <N> --repo <owner/repo>) so a reviewer or maintainer Agent picks it up.";

/// Always part of the fixed request, so the tracker (ADR 0018) can link the
/// pull request and hear a give-up: `{n}` is the issue number.
pub fn track_step(number: i64) -> String {
    format!(" Write \"Closes #{number}\" in its description. If you cannot fix it, comment on the issue with \"{}\" and the reason.", ::hagency::issue_watch::issue_track::CANNOT_FIX_MARKER)
}

impl Notice {
    pub fn new(repo: &str, issue: &Issue, agent_label: &str) -> Self {
        let title = issue.title.split_whitespace().collect::<Vec<_>>().join(" ");
        Self { repo: repo.to_owned(), number: issue.number, url: issue.url.clone(), title, agent: agent_label.trim_start_matches('@').to_owned(), merge_policy: false }
    }

    /// The same notice for an Agent whose auto-merge switch is `on`.
    pub fn with_merge_policy(mut self, on: bool) -> Self { self.merge_policy = on; self }
    fn merge_step(&self) -> String { if self.merge_policy { format!(" {MERGE_POLICY_STEP}") } else { String::new() } }
    /// The data line after the request; empty for an untitled issue.
    fn title_line(&self) -> String { if self.title.is_empty() { String::new() } else { format!("\n{}: {}", ::hagency::ISSUE_TITLE_LABEL, quoted_title(&self.title)) } }

    /// The plain body the Agent reads. English on purpose: it is the Agent's
    /// instruction; people see the localized `view` instead. The fixed request
    /// comes first; the issue's own title only follows as quoted data.
    pub fn body(&self) -> String {
        format!("{}#{} in {} — @{} please fix it on a branch and open a draft pull request: {}{}{}{}", ::hagency::ISSUE_NOTICE_LEAD, self.number, self.repo, self.agent, self.url, track_step(self.number), self.merge_step(), self.title_line())
    }

    /// The message: text (dispatch wakes on text from the owner), the Agent
    /// in `m.mentions`, and the HTML other clients show.
    pub fn content(&self, puppet: &UserId) -> RoomMessageEventContent {
        let esc = |s: &str| htmlize::escape_text(s).into_owned();
        let title = esc(&self.title_line()).replace('\n', "<br>");
        let html = format!(
            "🔔 New GitHub issue <a href=\"{url}\">#{n}</a> in {repo} — <a href=\"{pill}\">@{agent}</a> please fix it on a branch and open a draft pull request{track}{step}{title}",
            track = esc(&track_step(self.number)), step = esc(&self.merge_step()), url = htmlize::escape_attribute(&self.url), n = self.number, repo = esc(&self.repo), pill = puppet.matrix_to_uri(), agent = esc(&self.agent),
        );
        RoomMessageEventContent::text_html(self.body(), html).add_mentions(Mentions::with_user_ids([puppet.to_owned()]))
    }

    /// The extra content fields sent beside the message.
    pub fn marker(&self) -> Map<String, Value> {
        let mut extra = Map::new();
        extra.insert(MARKER.into(), json!({"repo": self.repo, "number": self.number, "url": self.url, "title": self.title, "agent": self.agent, "mergePolicy": self.merge_policy}));
        extra
    }

    /// Reads the marker from a message's content; `None` when absent or malformed.
    pub fn from_content(content: &Value) -> Option<Self> {
        let m = content.get(MARKER)?;
        let text = |k: &str| m.get(k)?.as_str().map(str::to_owned);
        let url = text("url").filter(|u| u.starts_with("https://"))?;
        Some(Self { repo: text("repo")?, number: m.get("number")?.as_i64()?, url, title: text("title").unwrap_or_default(), agent: text("agent").unwrap_or_default(),
            merge_policy: m.get("mergePolicy").and_then(Value::as_bool).unwrap_or(false) })
    }

    /// The compact notice the timeline shows: a "From issue watching" label,
    /// the issue, who was asked, and an "Open on GitHub" link.
    pub fn view(&self) -> (String, FormattedBody) {
        let esc = |s: &str| htmlize::escape_text(s).into_owned();
        let label = crate::i18n::tr("From issue watching");
        let heading = crate::i18n::format("New GitHub issue #{number} in {repo}", &[("number", self.number.to_string()), ("repo", self.repo.clone())]);
        let asked = crate::i18n::format("Asked {agent} to fix it", &[("agent", format!("@{}", self.agent))]);
        let open = crate::i18n::tr("Open on GitHub");
        let title = if self.title.is_empty() { String::new() } else { format!("<br><b>{}</b>", esc(&self.title)) };
        let html = format!("<i>🔔 {}</i><br>{}{title}<br><i>{} · <a href=\"{}\">{}</a></i>", esc(label), esc(&heading), esc(&asked), htmlize::escape_attribute(&self.url), esc(open));
        let plain = format!("🔔 {label} — {heading} {} — {asked}", self.title);
        (plain, FormattedBody::html(html))
    }
}

/// The watcher's notice in a timeline event, when it is one.
pub fn of_event(event: &matrix_sdk_ui::timeline::EventTimelineItem) -> Option<Notice> {
    let raw = event.latest_json()?;
    // Unreadable JSON is simply not a hand-off: the message renders as sent.
    let value: Value = serde_json::from_str(raw.json().get()).ok()?;
    Notice::from_content(&value["content"])
}
