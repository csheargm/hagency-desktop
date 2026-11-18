//! The Watches list in the room information pane: one row per watch that
//! reports to this room (Agent, repository, active or paused, owner).
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
            text: #(crate::i18n::tr("GitHub repositories your Agents watch, with reports going to this room."))
            i18n_text: "GitHub repositories your Agents watch, with reports going to this room."
        }
        watches_notice := WatchText {visible: false margin: Inset{left: 16 right: 16 bottom: 8}}
        review_setting := mod.widgets.RoomReviewSetting {}
        watches_list := PortalList {
            width: Fill height: Fill flow: Down
            Watch := View {
                width: Fill height: Fit flow: Down spacing: 4 padding: Inset{left: 16 right: 16 top: 8 bottom: 8}
                title := Label {
                    width: Fill height: Fit
                    draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_bold{font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}
                }
                detail := WatchText {}
            }
            Filler := View {width: Fill height: 24}
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct RoomWatchesPanel {
    #[deref]
    pub view: View,
    #[rust]
    room: Option<OwnedRoomId>,
}

/// The row's two lines: "yue-claude · acme/app" and "Active · owner @a:b".
pub fn row_text(watch: &::hagency::issue_watch::RoomWatch) -> (String, String) {
    let status = crate::i18n::tr(if watch.active { "Active" } else { "Paused" });
    let detail = crate::i18n::format("{status} · owner {owner}", &[("status", status.to_owned()), ("owner", watch.owner.clone())]);
    (format!("{} · {}", watch.agent, watch.repo), detail)
}

/// The notice above the list: loading, a failure, or "none".
pub fn notice(loaded: Option<&Result<Vec<::hagency::issue_watch::RoomWatch>, String>>) -> Option<String> {
    match loaded {
        None => Some(crate::i18n::tr("Loading watches…").to_owned()),
        Some(Err(error)) => Some(crate::i18n::format("Watches are unavailable: {error}", &[("error", error.clone())])),
        Some(Ok(rows)) if rows.is_empty() => Some(crate::i18n::tr("No Agent watches a repository for this room.").to_owned()),
        Some(Ok(_)) => None,
    }
}

impl Widget for RoomWatchesPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        if let Some(room) = self.room.as_ref()
            && actions.iter().any(|a| a.downcast_ref::<super::WatchesChanged>().is_some_and(|c| &c.room_id == room))
        {
            self.view.redraw(cx);
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let loaded = self.room.as_deref().and_then(super::current);
        let rows = loaded.as_ref().and_then(|l| l.as_ref().ok()).cloned().unwrap_or_default();
        let text = notice(loaded.as_ref());
        let label = self.view.label(cx, ids!(watches_notice));
        label.set_visible(cx, text.is_some());
        label.set_text(cx, text.as_deref().unwrap_or_default());
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = item.borrow_mut::<PortalList>() else { continue };
            list.set_item_range(cx, 0, rows.len());
            while let Some(index) = list.next_visible_item(cx) {
                let Some(watch) = rows.get(index) else {
                    list.item(cx, index, id!(Filler)).draw_all(cx, scope);
                    continue;
                };
                let (title, detail) = row_text(watch);
                let row = list.item(cx, index, id!(Watch));
                row.label(cx, ids!(title)).set_text(cx, &title);
                row.label(cx, ids!(detail)).set_text(cx, &detail);
                row.draw_all(cx, scope);
            }
        }
        DrawStep::done()
    }
}

impl RoomWatchesPanelRef {
    /// Shows this room's watches, reloading them.
    pub fn show(&self, cx: &mut Cx, room: &matrix_sdk::ruma::RoomId) {
        let Some(mut panel) = self.borrow_mut() else { return };
        panel.room = Some(room.to_owned());
        super::refresh(room);
        use super::review_setting::RoomReviewSettingWidgetExt;
        panel.view.room_review_setting(cx, ids!(review_setting)).show(cx, room);
        panel.view.redraw(cx);
    }
}
