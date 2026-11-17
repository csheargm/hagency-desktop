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
use super::expired_approvals;
use super::pending_approvals::{self, LocalDecision};
pub use super::owner_inbox_model::*;
use makepad_widgets::*;

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
                hide := Button {text: #(crate::i18n::tr("Hide")) i18n_text: "Hide"}
                show := Button {visible: false text: #(crate::i18n::tr("Show")) i18n_text: "Show"}
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
        self.view.button(cx, ids!(card.hide)).set_visible(cx, !self.collapsed);
        self.view.button(cx, ids!(card.show)).set_visible(cx, self.collapsed);
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

    /// Answer the request in slot `index` of `shown` (what the slots showed
    /// when the click happened). The request is looked up again by id, and
    /// only the same digest is answered: a slot that now shows another
    /// request never receives this click. Returns false when nothing was sent.
    pub fn decide(&mut self, cx: &mut Cx, shown: &[Entry], index: usize, decision: &str) -> bool {
        let Some(Entry::Waiting(seen)) = shown.get(index) else { return false };
        let Some(current) = pending_approvals::waiting().into_iter().find(|p| p.id() == seen.id() && p.digest() == seen.digest()) else {
            return false;
        };
        if !pending_approvals::decide(&current.row, decision) {
            return false;
        }
        cx.action(pending_approvals::PendingApprovalsChanged);
        self.refresh(cx);
        true
    }

    fn open_room(cx: &mut Cx, shown: &[Entry], index: usize) {
        let Some(room) = shown.get(index).and_then(|e| matrix_sdk::ruma::OwnedRoomId::try_from(e.room_id()).ok()) else { return };
        let name = crate::utils::RoomNameId::new(matrix_sdk::RoomDisplayName::Named(room_name(room.as_str())), room);
        cx.action(crate::home::main_desktop_ui::MainDesktopUiAction::ShowChats);
        cx.action(crate::app::AppStateAction::NavigateToRoom { room_to_close: None, destination_room: crate::room::BasicRoomDetails::RoomId(name) });
    }
}

impl Widget for OwnerApprovalsInbox {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        // Clicks are resolved against what the slots showed, before this
        // batch's own updates refresh them.
        let shown = std::mem::take(&mut self.entries);
        self.entries = shown.clone();
        let mut changed = actions.iter().any(|a| a.downcast_ref::<pending_approvals::PendingApprovalsChanged>().is_some()
            || a.downcast_ref::<pending_approvals::PendingApprovalsUpdate>().is_some()
            || a.downcast_ref::<pending_approvals::PendingApprovalDecided>().is_some());
        if self.view.visible() {
            for (id, collapsed) in [(ids!(card.hide), true), (ids!(card.show), false)] {
                if self.view.button(cx, id).clicked(actions) {
                    self.collapsed = collapsed;
                    changed = true;
                }
            }
            for index in 0..SLOTS.min(shown.len()) {
                let row = self.view.view(cx, slot(index));
                for (id, decision) in [(ids!(once), "approve_once"), (ids!(always), super::local_approvals::ALWAYS), (ids!(deny), "deny")] {
                    if row.button(cx, id).clicked(actions) {
                        changed |= self.decide(cx, &shown, index, decision);
                    }
                }
                if row.button(cx, ids!(open_room)).clicked(actions) {
                    Self::open_room(cx, &shown, index);
                }
                if row.button(cx, ids!(dismiss)).clicked(actions) {
                    expired_approvals::dismiss(shown[index].id());
                    changed = true;
                }
            }
        }
        if changed {
            // Either reading may have noticed an expiry; tell the owner once.
            pending_approvals::announce_expired(cx);
            self.refresh(cx);
            self.view.redraw(cx);
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
