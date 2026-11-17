//! What the owner approvals inbox shows, apart from its widget: its lines,
//! each line's text and buttons, and the header.
use super::expired_approvals::{self, Expired};
use super::pending_approvals::{self, LocalDecision, PendingApproval};
use crate::i18n::{format, tr};

/// Requests shown at once; the header counts the rest.
pub const SLOTS: usize = 4;

/// One line of the inbox.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    Waiting(PendingApproval),
    Expired(Expired),
}
impl Entry {
    pub fn id(&self) -> &str {
        match self {
            Entry::Waiting(p) => p.id(),
            Entry::Expired(e) => e.id(),
        }
    }
    fn row(&self) -> &serde_json::Value {
        match self {
            Entry::Waiting(p) => &p.row,
            Entry::Expired(e) => &e.row,
        }
    }
    pub fn room_id(&self) -> &str {
        self.row()["approval"]["roomId"].as_str().unwrap_or_default()
    }
}

/// What one slot shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shown {
    pub title: String,
    /// Room and Agent.
    pub place: String,
    /// The command, file or address, in full.
    pub details: String,
    pub status: String,
    pub once: bool,
    pub always: bool,
    pub deny: bool,
    pub dismiss: bool,
}

/// The inbox's lines: waiting requests first (oldest first, the next to
/// expire on top), then expired ones (newest first).
pub fn entries(now: u64) -> Vec<Entry> {
    let mut waiting = pending_approvals::waiting();
    waiting.sort_by_key(|p| p.expires_at_ms().unwrap_or(u64::MAX));
    waiting.into_iter().map(Entry::Waiting)
        .chain(expired_approvals::listed(now).into_iter().map(Entry::Expired))
        .collect()
}

/// The slot text of `entry`. `room_name` names a room this client knows.
pub fn shown(entry: &Entry, room_name: &dyn Fn(&str) -> String) -> Shown {
    let row = entry.row();
    let (asked, details) = pending_approvals::describe(row);
    let agent = row["approval"]["agentName"].as_str().unwrap_or("Agent");
    let place = format("In {room} · {agent}", &[("room", room_name(entry.room_id())), ("agent", agent.to_owned())]);
    let mut shown = Shown { title: asked.clone(), place, details, ..Shown::default() };
    match entry {
        Entry::Expired(_) => {
            shown.title = format("Expired, not answered: {title}", &[("title", asked)]);
            shown.status = tr("The Agent was told nobody answered. Ask it again when you are here.").to_owned();
            shown.dismiss = true;
        }
        Entry::Waiting(p) => match &p.local {
            LocalDecision::Sending(d) => shown.status = format("Sending: {decision}…", &[("decision", tr(pending_approvals::decision_label(d)).to_owned())]),
            LocalDecision::Sent(d) => shown.status = tr(pending_approvals::sent_status(d)).to_owned(),
            LocalDecision::None | LocalDecision::Failed(..) => {
                let can = p.can_decide();
                if let LocalDecision::Failed(..) = p.local {
                    shown.status = tr("Could not send the decision. Try again.").to_owned();
                } else if !can {
                    shown.status = format("Waiting for resource owner {owner}", &[("owner", p.owner_mxid().to_owned())]);
                }
                shown.once = can;
                shown.always = can && p.always_allowed();
                shown.deny = can;
            }
        },
    }
    shown
}

/// The header: how many wait, and why some may be missing.
pub fn header(entries: &[Entry]) -> String {
    // Counted as the buttons are offered: answerable now, or after a failed send.
    let waiting = entries.iter()
        .filter(|e| matches!(e, Entry::Waiting(p) if p.can_decide() && matches!(p.local, LocalDecision::None | LocalDecision::Failed(..))))
        .count();
    let mut lines = vec![match waiting {
        0 => tr("No approvals waiting").to_owned(),
        1 => tr("1 approval waiting for you").to_owned(),
        n => format("{n} approvals waiting for you", &[("n", n.to_string())]),
    }];
    if entries.len() > SLOTS {
        lines.push(format("{n} more after these; answer or dismiss one to see the next.", &[("n", (entries.len() - SLOTS).to_string())]));
    }
    let (error, unreadable) = pending_approvals::read_problems();
    if let Some(code) = error {
        lines.push(format("Could not check for approvals ({code}); retrying.", &[("code", code)]));
    }
    for (agent, code) in unreadable {
        lines.push(format("Could not read {agent}'s requests ({code}); some may be missing.", &[("agent", agent), ("code", code)]));
    }
    lines.join("\n")
}

/// Whether the inbox has anything to show.
pub fn has_content(entries: &[Entry]) -> bool {
    let (error, unreadable) = pending_approvals::read_problems();
    !entries.is_empty() || error.is_some() || !unreadable.is_empty()
}

