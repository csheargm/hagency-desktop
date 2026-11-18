//! The Status tab in the room information pane (ADR 0023): open Agent tasks,
//! this account's watches, and for a room with repositories one section per
//! repository (with a filter), plus linking and unlinking repositories for
//! members whose power level allows it.
use makepad_widgets::*;
use matrix_sdk::ruma::OwnedRoomId;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    let WatchText = Label {
        width: Fill height: Fit flow: Flow.Right{wrap: true}
        draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
    }

    mod.widgets.RoomWatchesPanel = #(RoomWatchesPanel::register_widget(vm)) {
        ..mod.widgets.View
        visible: false width: Fill height: Fill flow: Down
        watches_intro := WatchText {
            margin: Inset{left: 16 right: 16 top: 4 bottom: 8}
            text: #(crate::i18n::tr("What this room's Agents are doing, and the status of its GitHub repositories."))
            i18n_text: "What this room's Agents are doing, and the status of its GitHub repositories."
        }
        watches_notice := WatchText {visible: false margin: Inset{left: 16 right: 16 bottom: 8}}
        repo_filter := RobrixSettingsDropDown {visible: false width: Fill height: 36 margin: Inset{left: 16 right: 16 bottom: 8} labels: ["All repositories"]
            popup_menu_position: PopupMenuPosition.BelowInput
        }
        watches_list := PortalList {
            width: Fill height: Fill flow: Down
            Section := View {
                width: Fill height: Fit padding: Inset{left: 16 right: 16 top: 12 bottom: 4}
                heading := Label {
                    width: Fill height: Fit
                    draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_bold{font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
                }
            }
            Watch := View {
                width: Fill height: Fit flow: Down spacing: 4 padding: Inset{left: 16 right: 16 top: 8 bottom: 8}
                title := Label {
                    width: Fill height: Fit flow: Flow.Right{wrap: true}
                    draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_bold{font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}
                }
                detail := WatchText {}
            }
            Filler := View {width: Fill height: 24}
        }
        link_box := View {visible: false width: Fill height: Fit flow: Down spacing: 8 padding: 12
            repo_input := TextInput {
                width: Fill height: Fit
                empty_text: #(crate::i18n::tr("owner/name of a GitHub repository")) i18n_empty_text: "owner/name of a GitHub repository"
            }
            View {width: Fill height: Fit flow: Right spacing: 8
                link_repo := RobrixPositiveIconButton {
                    width: Fit height: 32 padding: Inset{left: 12 right: 12 top: 0 bottom: 0} icon_walk: Walk{width: 0 height: 0}
                    text: #(crate::i18n::tr("Link a repo")) i18n_text: "Link a repo"
                }
                unlink_repo := RobrixNeutralIconButton {
                    width: Fit height: 32 padding: Inset{left: 12 right: 12 top: 0 bottom: 0} icon_walk: Walk{width: 0 height: 0}
                    text: #(crate::i18n::tr("Unlink")) i18n_text: "Unlink"
                }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct RoomWatchesPanel {
    #[deref]
    pub view: View,
    #[rust]
    room: Option<OwnedRoomId>,
    /// The repository filter's current choices (the drop-down keeps no copy to compare).
    #[rust]
    labels: Vec<String>,
}

/// The row's two lines: "yue-claude · acme/app" and "Active · owner @a:b".
pub fn row_text(watch: &::hagency::issue_watch::RoomWatch) -> (String, String) {
    let status = crate::i18n::tr(if watch.active { "Active" } else { "Paused" });
    let detail = crate::i18n::format("{status} · owner {owner}", &[("status", status.to_owned()), ("owner", watch.owner.clone())]);
    (format!("{} · {}", watch.agent, watch.repo), detail)
}

/// The notice above the list: loading, a failure, "none", the status's own
/// failure or notes, and the last link change's failure.
pub fn notice(loaded: Option<&Result<super::Watches, String>>, link_error: Option<&str>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    match loaded {
        None => parts.push(crate::i18n::tr("Loading watches…").to_owned()),
        Some(Err(error)) => parts.push(crate::i18n::format("Watches are unavailable: {error}", &[("error", error.clone())])),
        Some(Ok(w)) => {
            if w.rows.is_empty() && w.status.as_ref().is_ok_and(|s| s.tables.is_empty()) {
                parts.push(crate::i18n::tr("No Agent watches a repository for this room.").to_owned());
            }
            match &w.status {
                Ok(status) => parts.extend(status.notes.iter().cloned()),
                Err(error) => parts.push(crate::i18n::format("Room status is unavailable: {error}", &[("error", error.clone())])),
            }
        }
    }
    parts.extend(link_error.map(str::to_owned));
    (!parts.is_empty()).then(|| parts.join("\n"))
}

impl RoomWatchesPanel {
    fn filter(&self, cx: &mut Cx, loaded: Option<&super::Watches>) -> Option<String> {
        let index = self.view.drop_down(cx, ids!(repo_filter)).selected_item();
        loaded.and_then(|w| super::lines::filter_labels(w).get(index).cloned()).filter(|_| index > 0)
    }
}

impl Widget for RoomWatchesPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        let Some(room) = self.room.clone() else { return };
        for (button, link) in [(ids!(link_repo), true), (ids!(unlink_repo), false)] {
            if self.view.button(cx, button).clicked(actions) {
                let repo = self.view.text_input(cx, ids!(repo_input)).text();
                if !repo.trim().is_empty() {
                    super::set_link(&room, repo.trim().to_owned(), link);
                    self.view.text_input(cx, ids!(repo_input)).set_text(cx, "");
                }
            }
        }
        if self.view.drop_down(cx, ids!(repo_filter)).changed(actions).is_some()
            || actions.iter().any(|a| a.downcast_ref::<super::WatchesChanged>().is_some_and(|c| c.room_id == room))
        {
            self.view.redraw(cx);
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let loaded = self.room.as_deref().and_then(super::current);
        let link_error = self.room.as_deref().and_then(super::link_error);
        let watches = loaded.as_ref().and_then(|l| l.as_ref().ok());
        let filter = self.filter(cx, watches);
        let lines = watches.map(|w| super::lines::lines(w, filter.as_deref())).unwrap_or_default();
        let text = notice(loaded.as_ref(), link_error.as_deref());
        let label = self.view.label(cx, ids!(watches_notice));
        label.set_visible(cx, text.is_some());
        label.set_text(cx, text.as_deref().unwrap_or_default());
        let status = watches.and_then(|w| w.status.as_ref().ok());
        let labels = watches.map(super::lines::filter_labels).unwrap_or_default();
        let dropdown = self.view.drop_down(cx, ids!(repo_filter));
        // The filter appears only for a room with more than one repository.
        dropdown.set_visible(cx, labels.len() > 2);
        if labels.len() > 2 && self.labels != labels {
            dropdown.set_labels(cx, labels.clone());
            self.labels = labels;
        }
        self.view.view(cx, ids!(link_box)).set_visible(cx, status.is_some_and(|s| s.can_link));
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = item.borrow_mut::<PortalList>() else { continue };
            list.set_item_range(cx, 0, lines.len());
            while let Some(index) = list.next_visible_item(cx) {
                match lines.get(index) {
                    Some(super::lines::Line::Section(heading)) => {
                        let row = list.item(cx, index, id!(Section));
                        row.label(cx, ids!(heading)).set_text(cx, heading);
                        row.draw_all(cx, scope);
                    }
                    Some(super::lines::Line::Item(title, detail)) => {
                        let row = list.item(cx, index, id!(Watch));
                        row.label(cx, ids!(title)).set_text(cx, title);
                        row.label(cx, ids!(detail)).set_text(cx, detail);
                        row.draw_all(cx, scope);
                    }
                    None => list.item(cx, index, id!(Filler)).draw_all(cx, scope),
                }
            }
        }
        DrawStep::done()
    }
}

impl RoomWatchesPanelRef {
    /// Shows this room's status, reloading it.
    pub fn show(&self, cx: &mut Cx, room: &matrix_sdk::ruma::RoomId) {
        let Some(mut panel) = self.borrow_mut() else { return };
        panel.room = Some(room.to_owned());
        super::refresh(room);
        panel.view.redraw(cx);
    }
}
