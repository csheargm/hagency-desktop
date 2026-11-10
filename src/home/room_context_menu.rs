//! A context menu that appears when the user right-clicks
//! or long-presses on a room in the room list.

use makepad_widgets::*;
use matrix_sdk::{ruma::OwnedRoomId, notification_settings::RoomNotificationMode};
use super::rooms_list::RoomNotificationModeUpdated;
use crate::{home::invite_modal::InviteModalAction, settings::app_preferences::preferred_receipt_type, shared::{context_menu::{ContextMenuClosed, expected_menu_size}, popup_list::{PopupKind, enqueue_popup_notification}}, sliding_sync::{MatrixRequest, submit_async_request}, utils::RoomNameId};

/// Nothing here is conditionally shown, so keep these matching the DSL below.
const NUM_BUTTONS: usize = 13;
const NUM_DIVIDERS: usize = 3;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.RoomContextMenu = set_type_default() do #(RoomContextMenu::register_widget(vm)) {
        ..mod.widgets.SolidView

        visible: false,
        flow: Overlay,
        width: Fill,
        height: Fill,
        cursor: MouseCursor.Default,
        align: Align{x: 0, y: 0}

        show_bg: true
        draw_bg +: {
            color: #0000004d
        }

        main_content := mod.widgets.ContextMenuContent {
            mark_unread_button := mod.widgets.ContextMenuButton {
                draw_icon +: { svg: (ICON_CHECKMARK) }
                text: #(crate::i18n::tr("Mark as Unread")) i18n_text: "Mark as Unread"
            }

            favorite_button := mod.widgets.ContextMenuButton {
                draw_icon +: { svg: (ICON_PIN) }
                text: #(crate::i18n::tr("Favorite")) i18n_text: "Favorite"
            }
            hide_chat_button := mod.widgets.ContextMenuButton {text: #(crate::i18n::tr("Hide Chat")) i18n_text: "Hide Chat" draw_icon.svg: ICON_FORBIDDEN}
            delete_chat_button := mod.widgets.ContextMenuDangerButton {text: #(crate::i18n::tr("Delete Chat")) i18n_text: "Delete Chat" draw_icon.svg: ICON_TRASH}

            priority_button := mod.widgets.ContextMenuButton {
                draw_icon +: { svg: (ICON_TOMBSTONE) }
                text: #(crate::i18n::tr("Set Low Priority")) i18n_text: "Set Low Priority"
            }

            copy_link_button := mod.widgets.ContextMenuButton {
                draw_icon +: { svg: (ICON_LINK) }
                text: #(crate::i18n::tr("Copy Link to Room")) i18n_text: "Copy Link to Room"
            }

            divider1 := mod.widgets.ContextMenuDivider { }

            room_settings_button := mod.widgets.ContextMenuButton {
                draw_icon +: { svg: (ICON_SETTINGS) }
                text: #(crate::i18n::tr("Settings")) i18n_text: "Settings"
            }
            search_history_button := mod.widgets.ContextMenuButton {text: #(crate::i18n::tr("Search Chat History")) i18n_text: "Search Chat History" draw_icon.svg: ICON_SEARCH}
            shared_attachments_button := mod.widgets.ContextMenuButton {text: #(crate::i18n::tr("Shared Attachments")) i18n_text: "Shared Attachments" draw_icon.svg: ICON_COPY}

            notifications_button := mod.widgets.ContextMenuButton {
                // TODO: use a proper bell icon
                draw_icon +: { svg: (ICON_INFO) }
                text: #(crate::i18n::tr("Notifications")) i18n_text: "Notifications"
            }

            invite_button := mod.widgets.ContextMenuButton {
                draw_icon +: { svg: (ICON_ADD_USER) }
                text: #(crate::i18n::tr("Invite")) i18n_text: "Invite"
            }

            divider2 := mod.widgets.ContextMenuDivider { }

            diagnostics_button := mod.widgets.ContextMenuButton {
                draw_icon +: { svg: (ICON_COPY) }
                text: #(crate::i18n::tr("Copy Room Diagnostics")) i18n_text: "Copy Room Diagnostics"
            }

            divider3 := mod.widgets.ContextMenuDivider { }

            leave_button := mod.widgets.ContextMenuDangerButton {
                draw_icon.svg: (ICON_LOGOUT)
                text: #(crate::i18n::tr("Leave Room")) i18n_text: "Leave Room"
            }

            notify_all := mod.widgets.ContextMenuButton {visible: false text: #(crate::i18n::tr("All Messages")) i18n_text: "All Messages"}
            notify_mentions := mod.widgets.ContextMenuButton {visible: false text: #(crate::i18n::tr("Mentions and Keywords")) i18n_text: "Mentions and Keywords"}
            notify_mute := mod.widgets.ContextMenuButton {visible: false text: #(crate::i18n::tr("Mute Notifications")) i18n_text: "Mute Notifications"}
            notify_default := mod.widgets.ContextMenuButton {visible: false text: #(crate::i18n::tr("Use Account Defaults")) i18n_text: "Use Account Defaults"}
            notify_back := mod.widgets.ContextMenuButton {visible: false text: #(crate::i18n::tr("Back")) i18n_text: "Back"}
        }
    }
}

/// Details needed to populate the room context menu.
#[derive(Clone, Debug)]
pub struct RoomContextMenuDetails {
    pub notification_mode: Option<RoomNotificationMode>,
    pub room_name_id: RoomNameId,
    pub is_favorite: bool,
    pub is_low_priority: bool,
    /// Whether the room has any unreads:
    /// messages, mentions, or the marked-unread state.
    pub has_unreads: bool,
}

/// Actions emitted from the RoomContextMenu widget, as they must be handled
/// by other widgets with more information (e.g., the RoomsList).
#[derive(Clone, Default, Debug)]
pub enum RoomContextMenuAction {
    Notifications(OwnedRoomId),
    OpenRoomSettings(OwnedRoomId),
    #[default]
    None,
}

#[derive(Script, ScriptHook, Widget)]
pub struct RoomContextMenu {
    #[deref] view: View,
    #[source] source: ScriptObjectRef,
    #[rust] details: Option<RoomContextMenuDetails>,
    #[rust] focus_after_draw: bool,
}

impl Widget for RoomContextMenu {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if self.details.is_none() {
            self.visible = false;
        };
        let step = self.view.draw_walk(cx, scope, walk);
        if self.visible {
            let main_content_area = self.view(cx, ids!(main_content)).area();
            cx.block_scrolling_except_within(main_content_area);
            if step.is_done() && self.focus_after_draw {
                cx.set_key_focus(self.view.area());
                self.focus_after_draw = false;
            }
        }
        step
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !self.visible { return; }
        self.view.handle_event(cx, event, scope);

        // Close logic similar to NewMessageContextMenu
        let area = self.view.area();
        let close_menu = {
            event.back_pressed()
            || matches!(event, Event::KeyDown(KeyEvent {key_code: KeyCode::Escape, ..}))
            || match event.hits_with_capture_overload(cx, area, true) {
                Hit::FingerUp(fue) if fue.is_over => {
                     !self.view(cx, ids!(main_content)).area().rect(cx).contains(fue.abs)
                }
                _ => false,
            }
        };

        if close_menu {
            self.close(cx);
            return;
        }

        self.widget_match_event(cx, event, scope);
    }
}

impl WidgetMatchEvent for RoomContextMenu {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions, _scope: &mut Scope) {
        for action in actions {
            if let Some(update) = action.downcast_ref::<RoomNotificationModeUpdated>() {
                if let Some(details) = self.details.as_mut() {
                    if details.room_name_id.room_id() == &update.room_id {
                        details.notification_mode = update.mode;
                        let details = details.clone();
                        self.update_buttons(cx, &details);
                    }
                }
            }
        }
        let Some(details) = self.details.as_ref() else { return };
        let mut close_menu = false;

        for (id, mode) in [
            (ids!(notify_all), Some(RoomNotificationMode::AllMessages)),
            (ids!(notify_mentions), Some(RoomNotificationMode::MentionsAndKeywordsOnly)),
            (ids!(notify_mute), Some(RoomNotificationMode::Mute)),
            (ids!(notify_default), None),
        ] {
            if self.button(cx, id).clicked(actions) {
                submit_async_request(MatrixRequest::SetRoomNotificationMode {room_id: details.room_name_id.room_id().clone(), mode});
                self.close(cx);
                return;
            }
        }
        if self.button(cx, ids!(notify_back)).clicked(actions) {
            self.show_notification_choices(cx, false);
            return;
        }
        
        if self.button(cx, ids!(mark_unread_button)).clicked(actions) {
            if details.has_unreads {
                // Send a read receipt for the latest event to clear the unread counts.
                submit_async_request(MatrixRequest::MarkRoomAsRead {
                    room_id: details.room_name_id.room_id().clone(),
                    receipt_type: preferred_receipt_type(),
                });
            } else {
                submit_async_request(MatrixRequest::SetUnreadFlag {
                    room_id: details.room_name_id.room_id().clone(),
                    mark_as_unread: true,
                });
            }
            close_menu = true;
        }
        else if self.button(cx, ids!(favorite_button)).clicked(actions) {
            submit_async_request(MatrixRequest::SetIsFavorite {
                room_id: details.room_name_id.room_id().clone(),
                is_favorite: !details.is_favorite,
            });
            close_menu = true;
        }
        else if self.button(cx, ids!(search_history_button)).clicked(actions)
            || self.button(cx, ids!(shared_attachments_button)).clicked(actions)
        {
            cx.action(super::room_history::RoomHistoryAction::Open {
                room: details.room_name_id.clone(),
                filter: if self.button(cx, ids!(search_history_button)).clicked(actions) {super::room_history::HistoryFilter::Messages} else {super::room_history::HistoryFilter::Media},
            });
            close_menu = true;
        }
        else if self.button(cx, ids!(hide_chat_button)).clicked(actions)
            || self.button(cx, ids!(delete_chat_button)).clicked(actions)
        {
            let room = details.room_name_id.room_id().clone();
            let timestamp = cx.get_global::<super::rooms_list::RoomsListRef>().latest_timestamp(&room);
            if self.button(cx, ids!(delete_chat_button)).clicked(actions) {
                super::chat_actions::confirm_delete(cx, room, timestamp);
            } else {
                super::chat_actions::hide(cx, room, timestamp, false);
            }
            close_menu = true;
        }
        else if self.button(cx, ids!(priority_button)).clicked(actions) {
            submit_async_request(MatrixRequest::SetIsLowPriority {
                room_id: details.room_name_id.room_id().clone(),
                is_low_priority: !details.is_low_priority,
            });
            close_menu = true;
        }
        else if self.button(cx, ids!(copy_link_button)).clicked(actions) {
            submit_async_request(MatrixRequest::GenerateMatrixLink {
                room_id: details.room_name_id.room_id().clone(),
                event_id: None,
                use_matrix_scheme: false,
                join_on_click: false,
            });
            close_menu = true;
        }
         else if self.button(cx, ids!(room_settings_button)).clicked(actions) {
            // TODO: handle/implement this
            enqueue_popup_notification(
                "The room settings page is not yet implemented.",
                PopupKind::Warning,
                Some(5.0),
            );
            close_menu = true;
        }
        else if self.button(cx, ids!(notifications_button)).clicked(actions) {
            self.show_notification_choices(cx, true);
            return;
        }
        else if self.button(cx, ids!(invite_button)).clicked(actions) {
            cx.action(InviteModalAction::Open(details.room_name_id.clone()));
            close_menu = true;
        }
        else if self.button(cx, ids!(diagnostics_button)).clicked(actions) {
            submit_async_request(MatrixRequest::GetRoomDiagnostics {
                room_id: details.room_name_id.room_id().clone(),
            });
            close_menu = true;
        }
        else if self.button(cx, ids!(leave_button)).clicked(actions) {
            use crate::join_leave_room_modal::{JoinLeaveRoomModalAction, JoinLeaveModalKind};
            use crate::room::BasicRoomDetails;
            let room_details = BasicRoomDetails::Name(details.room_name_id.clone());
            cx.action(JoinLeaveRoomModalAction::Open {
                kind: JoinLeaveModalKind::LeaveRoom(room_details),
                show_tip: false,
            });
            close_menu = true;
        }

        if close_menu {
            self.close(cx);
        }
    }
}

impl RoomContextMenu {
    pub fn is_currently_shown(&self, _cx: &mut Cx) -> bool {
        self.visible
    }

    pub fn show(&mut self, cx: &mut Cx, details: RoomContextMenuDetails) -> DVec2 {
        self.show_notification_choices(cx, false);
        self.update_buttons(cx, &details);
        self.details = Some(details);
        self.visible = true;
        self.focus_after_draw = true;
        expected_menu_size(NUM_BUTTONS, NUM_DIVIDERS)
    }

    fn show_notification_choices(&mut self, cx: &mut Cx, shown: bool) {
        self.focus_after_draw = true;
        for id in [ids!(hide_chat_button), ids!(delete_chat_button)] { self.button(cx, id).set_visible(cx, !shown); }
        for id in [ids!(mark_unread_button), ids!(favorite_button), ids!(priority_button), ids!(copy_link_button),
            ids!(room_settings_button), ids!(search_history_button), ids!(shared_attachments_button), ids!(notifications_button), ids!(invite_button), ids!(diagnostics_button), ids!(leave_button),
            ids!(divider1), ids!(divider2), ids!(divider3)] {
            self.view.widget(cx, id).set_visible(cx, !shown);
        }
        for id in [ids!(notify_all), ids!(notify_mentions), ids!(notify_mute), ids!(notify_default), ids!(notify_back)] {
            self.view.button(cx, id).set_visible(cx, shown);
            self.view.button(cx, id).reset_hover(cx);
        }
        self.redraw(cx);
    }

    fn update_buttons(&mut self, cx: &mut Cx, details: &RoomContextMenuDetails) {
        for (id, mode, text) in [
            (ids!(notify_all), Some(RoomNotificationMode::AllMessages), crate::i18n::tr("All Messages")),
            (ids!(notify_mentions), Some(RoomNotificationMode::MentionsAndKeywordsOnly), crate::i18n::tr("Mentions and Keywords")),
            (ids!(notify_mute), Some(RoomNotificationMode::Mute), crate::i18n::tr("Mute Notifications")),
            (ids!(notify_default), None, crate::i18n::tr("Use Account Defaults")),
        ] {
            let label = if details.notification_mode == mode { format!("✓ {text}") } else { text.to_owned() };
            self.button(cx, id).set_text(cx, &label);
        }
        let mark_unread_button = self.button(cx, ids!(mark_unread_button));
        if details.has_unreads {
            mark_unread_button.set_text(cx, crate::i18n::tr("Mark as Read"));
        } else {
            mark_unread_button.set_text(cx, crate::i18n::tr("Mark as Unread"));
        }
        
        let favorite_button = self.button(cx, ids!(favorite_button));
        if details.is_favorite {
            favorite_button.set_text(cx, crate::i18n::tr("Un-favorite"));
        } else {
             favorite_button.set_text(cx, crate::i18n::tr("Favorite"));
        }

        let priority_button = self.button(cx, ids!(priority_button));
        if details.is_low_priority {
            priority_button.set_text(cx, crate::i18n::tr("Un-set Low Priority"));
        } else {
            priority_button.set_text(cx, crate::i18n::tr("Set Low Priority"));
        }
        
        // Reset hover states
        mark_unread_button.reset_hover(cx);
        favorite_button.reset_hover(cx);
        priority_button.reset_hover(cx);
        self.button(cx, ids!(copy_link_button)).reset_hover(cx);
        self.button(cx, ids!(room_settings_button)).reset_hover(cx);
        self.button(cx, ids!(notifications_button)).reset_hover(cx);
        self.button(cx, ids!(invite_button)).reset_hover(cx);
        self.button(cx, ids!(diagnostics_button)).reset_hover(cx);
        self.button(cx, ids!(leave_button)).reset_hover(cx);

        self.redraw(cx);
    }

    fn close(&mut self, cx: &mut Cx) {
        self.visible = false;
        self.details = None;
        self.focus_after_draw = false;
        cx.revert_key_focus();
        cx.unblock_scrolling();
        cx.action(ContextMenuClosed);
        cx.clear_all_hovers();
        self.redraw(cx);
    }
}

impl RoomContextMenuRef {
    pub fn is_currently_shown(&self, cx: &mut Cx) -> bool {
        let Some(inner) = self.borrow() else { return false };
        inner.is_currently_shown(cx)
    }

    pub fn show(&self, cx: &mut Cx, details: RoomContextMenuDetails) -> DVec2 {
        let Some(mut inner) = self.borrow_mut() else { return DVec2::default()};
        inner.show(cx, details)
    }
}
