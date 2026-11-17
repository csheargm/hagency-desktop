//! The owner's approvals inbox: every request waiting for this owner's
//! answer, from any room, thread or server, on top of whatever page is open.
//!
//! Until now a request showed only as a card inside its room (in the room
//! timeline, on the thread root), a notice at the end of that room, a 20 s
//! popup and a dot on the Agents rail. An owner reading another room, or the
//! request's own thread, saw none of it and the request expired unanswered.
//! This card lists each request with its room, Agent and exact command, and
//! the same buttons as the room card; an expired one stays listed as
//! "Expired, not answered" until dismissed.
//!
//! It reads only `pending_approvals` and `expired_approvals`, and every
//! decision goes through `pending_approvals::decide`, like the room cards.
use super::expired_approvals::{self, Expired};
use super::pending_approvals::{self, LocalDecision, PendingApproval};
use crate::i18n::{format, tr};
use makepad_widgets::*;

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
    let waiting = entries.iter().filter(|e| matches!(e, Entry::Waiting(p) if p.local == LocalDecision::None)).count();
    let mut lines = vec![match waiting {
        0 => tr("No approvals waiting").to_owned(),
        1 => tr("1 approval waiting for you").to_owned(),
        n => format("{n} approvals waiting for you", &[("n", n.to_string())]),
    }];
    if entries.len() > SLOTS {
        lines.push(format("{n} more below the first {shown}", &[("n", (entries.len() - SLOTS).to_string()), ("shown", SLOTS.to_string())]));
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

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    mod.widgets.OwnerInboxRow = View {
        width: Fill height: Fit flow: Down spacing: 4 padding: Inset{top: 6 bottom: 6}
        title := Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_bold{}}}
        place := Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text.color: mod.widgets.RINX_MUTED}
        details_scroll := ScrollYView {width: Fill height: Fit{max: FitBound.Abs(120.0)}
            details := Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text.color: mod.widgets.RINX_CODE_FG}
        }
        status := Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text.color: mod.widgets.RINX_MUTED}
        buttons := View {width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 6
            once := Button {text: #(crate::i18n::tr("Approve once")) i18n_text: "Approve once"}
            always := Button {text: #(crate::i18n::tr("Always allow this")) i18n_text: "Always allow this"}
            deny := Button {text: #(crate::i18n::tr("Deny")) i18n_text: "Deny"}
            open_room := Button {text: #(crate::i18n::tr("Open room")) i18n_text: "Open room"}
            dismiss := Button {text: #(crate::i18n::tr("Dismiss")) i18n_text: "Dismiss"}
        }
    }
    mod.widgets.OwnerApprovalsInbox = #(OwnerApprovalsInbox::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill height: Fill flow: Down align: Align{x: 1.0 y: 0.0} padding: Inset{top: 12 right: 16}
        visible: false
        card := RoundedView {
            width: 440 height: Fit flow: Down spacing: 6 padding: 12
            show_bg: true draw_bg +: {color: mod.widgets.RINX_SURFACE border_color: mod.widgets.RINX_WARNING_FG border_size: 2 border_radius: 8}
            View {width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
                header := Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_bold{}}}
                toggle := Button {text: #(crate::i18n::tr("Hide")) i18n_text: "Hide"}
            }
            list := ScrollYView {width: Fill height: Fit{max: FitBound.Abs(520.0)} flow: Down
                row0 := mod.widgets.OwnerInboxRow {}
                row1 := mod.widgets.OwnerInboxRow {}
                row2 := mod.widgets.OwnerInboxRow {}
                row3 := mod.widgets.OwnerInboxRow {}
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct OwnerApprovalsInbox {
    #[deref]
    view: View,
    /// The entries the slots showed at the last draw, for their buttons.
    #[rust]
    entries: Vec<Entry>,
    #[rust]
    collapsed: bool,
    /// Waiting ids seen so far: a new one opens a collapsed inbox again.
    #[rust]
    seen: Vec<String>,
}

fn slot(index: usize) -> &'static [LiveId] {
    [ids!(row0), ids!(row1), ids!(row2), ids!(row3)][index]
}

fn room_name(room: &str) -> String {
    let named = matrix_sdk::ruma::OwnedRoomId::try_from(room).ok()
        .and_then(|id| crate::sliding_sync::get_client()?.get_room(&id))
        .and_then(|r| r.cached_display_name().map(|n| n.to_string()).or_else(|| r.name()));
    named.unwrap_or_else(|| room.to_owned())
}

impl OwnerApprovalsInbox {
    /// Re-read the stores and fill the slots.
    pub fn refresh(&mut self, cx: &mut Cx) {
        self.entries = entries(now_ms());
        let fresh: Vec<String> = self.entries.iter()
            .filter(|e| matches!(e, Entry::Waiting(p) if p.local == LocalDecision::None))
            .map(|e| e.id().to_owned()).filter(|id| !self.seen.contains(id)).collect();
        if !fresh.is_empty() {
            self.collapsed = false;
            self.seen.extend(fresh);
        }
        let visible = has_content(&self.entries);
        self.view.set_visible(cx, visible);
        if !visible {
            return;
        }
        self.view.label(cx, ids!(card.header)).set_text(cx, &header(&self.entries));
        self.view.button(cx, ids!(card.toggle)).set_text(cx, tr(if self.collapsed { "Show" } else { "Hide" }));
        self.view.widget(cx, ids!(card.list)).set_visible(cx, !self.collapsed && !self.entries.is_empty());
        for index in 0..SLOTS {
            let row = self.view.view(cx, slot(index));
            let Some(entry) = self.entries.get(index) else {
                row.set_visible(cx, false);
                continue;
            };
            let shown = shown(entry, &room_name);
            row.set_visible(cx, true);
            for (id, text) in [(ids!(title), &shown.title), (ids!(place), &shown.place), (ids!(details), &shown.details), (ids!(status), &shown.status)] {
                row.label(cx, id).set_text(cx, text);
            }
            row.widget(cx, ids!(status)).set_visible(cx, !shown.status.is_empty());
            row.widget(cx, ids!(details_scroll)).set_visible(cx, !shown.details.is_empty());
            for (id, offered) in [(ids!(once), shown.once), (ids!(always), shown.always), (ids!(deny), shown.deny), (ids!(dismiss), shown.dismiss), (ids!(open_room), true)] {
                row.button(cx, id).set_visible(cx, offered);
            }
        }
    }

    /// Answer the request in slot `index`. Returns false when nothing was sent.
    pub fn decide(&mut self, cx: &mut Cx, index: usize, decision: &str) -> bool {
        let Some(Entry::Waiting(p)) = self.entries.get(index) else { return false };
        if !pending_approvals::decide(&p.row, decision) {
            return false;
        }
        cx.action(pending_approvals::PendingApprovalsChanged);
        self.refresh(cx);
        true
    }

    fn open_room(&self, cx: &mut Cx, index: usize) {
        let Some(room) = self.entries.get(index).and_then(|e| matrix_sdk::ruma::OwnedRoomId::try_from(e.room_id()).ok()) else { return };
        let name = crate::utils::RoomNameId::new(matrix_sdk::RoomDisplayName::Named(room_name(room.as_str())), room);
        cx.action(crate::home::main_desktop_ui::MainDesktopUiAction::ShowChats);
        cx.action(crate::app::AppStateAction::NavigateToRoom { room_to_close: None, destination_room: crate::room::BasicRoomDetails::RoomId(name) });
    }
}

impl Widget for OwnerApprovalsInbox {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        if actions.iter().any(|a| a.downcast_ref::<pending_approvals::PendingApprovalsChanged>().is_some()
            || a.downcast_ref::<pending_approvals::PendingApprovalsUpdate>().is_some()
            || a.downcast_ref::<pending_approvals::PendingApprovalDecided>().is_some())
        {
            self.refresh(cx);
            self.view.redraw(cx);
        }
        if !self.view.visible() {
            return;
        }
        if self.view.button(cx, ids!(card.toggle)).clicked(actions) {
            self.collapsed = !self.collapsed;
            self.refresh(cx);
            self.view.redraw(cx);
        }
        for index in 0..SLOTS.min(self.entries.len()) {
            let row = self.view.view(cx, slot(index));
            for (id, decision) in [(ids!(once), "approve_once"), (ids!(always), super::local_approvals::ALWAYS), (ids!(deny), "deny")] {
                if row.button(cx, id).clicked(actions) {
                    self.decide(cx, index, decision);
                }
            }
            if row.button(cx, ids!(open_room)).clicked(actions) {
                self.open_room(cx, index);
            }
            if row.button(cx, ids!(dismiss)).clicked(actions) {
                expired_approvals::dismiss(self.entries[index].id());
                self.refresh(cx);
                self.view.redraw(cx);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
#[path = "owner_inbox_tests.rs"]
mod tests;
