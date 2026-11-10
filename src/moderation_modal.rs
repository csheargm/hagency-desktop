//! A modal for confirming a moderation action, with an optional reason for it.

use makepad_widgets::*;
use matrix_sdk_ui::timeline::TimelineEventItemId;

use crate::{
    moderation::{MemberAction, MemberModeration, ReportTarget},
    sliding_sync::{MatrixRequest, TimelineKind, submit_async_request},
};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*


    mod.widgets.ModerationModal = set_type_default() do #(ModerationModal::register_widget(vm)) {
        ..mod.widgets.SmallModal

        icon_delete: (ICON_TRASH)
        icon_report: (ICON_FLAG)
        icon_kick: (ICON_LOGOUT)
        icon_ban: (ICON_FORBIDDEN)
        icon_mute: (ICON_MUTE)

        title := ModalTitle {}

        body := ModalBody {}

        reason_view := View {
            width: Fill, height: Fit
            margin: Inset{top: 15}

            reason_input := RobrixTextInput {
                height: Fit { max: FitBound.Rel{base: Base.Full, factor: 0.5} }
                is_multiline: true
                submit_on_enter: true
                empty_text: #(crate::i18n::tr("Reason (optional)")) i18n_empty_text: "Reason (optional)"
                draw_text +: {
                    text_style: REGULAR_TEXT {font_size: 11},
                    color: mod.widgets.RINX_INK
                }
            }
        }

        buttons_view := ModalButtonsRow {
            cancel_button := RobrixNeutralIconButton {
                width: Fit{min: FitBound.Abs(120.0)},
                align: Align{x: 0.5, y: 0.5}
                padding: 15,
                draw_icon.svg: (ICON_CLOSE)
                icon_walk: Walk{width: 16, height: 16, margin: Inset{left: -2, right: -1} }
                text: #(crate::i18n::tr("Cancel")) i18n_text: "Cancel"
            }

            // Its icon is set to match the request that it confirms.
            negative_button := RobrixNegativeIconButton {
                width: Fit{min: FitBound.Abs(120)},
                align: Align{x: 0.5, y: 0.5}
                padding: 15,
                icon_walk: Walk{width: 16, height: 16, margin: Inset{left: -2, right: -1} }
                text: ""
            }

            positive_button := RobrixPositiveIconButton {
                visible: false,
                width: Fit{min: FitBound.Abs(120)},
                align: Align{x: 0.5, y: 0.5}
                padding: 15,
                draw_icon.svg: (ICON_CHECKMARK)
                icon_walk: Walk{width: 16, height: 16, margin: Inset{left: -2, right: -1} }
                text: ""
            }
        }
    }
}

/// A moderation action that the user must confirm.
#[derive(Clone, Debug)]
pub enum ModerationRequest {
    /// Delete a message.
    #[doc(alias("redact"))]
    DeleteMessage {
        timeline_kind: TimelineKind,
        timeline_event_id: TimelineEventItemId,
        /// The name of the message's sender, or `None` if it's the user's own message.
        sender_name: Option<String>,
    },
    /// Report something to the homeserver's admins.
    Report(ReportTarget),
    /// Moderate a member of a room.
    Member(MemberModeration),
}

/// Actions handled by the parent widget of the [`ModerationModal`].
#[derive(Clone, Debug)]
pub enum ModerationModalAction {
    /// The modal should be opened by its parent widget to confirm the given request.
    Open(ModerationRequest),
    /// The modal requested its parent widget to close it.
    Close,
}

#[derive(Script, ScriptHook, Widget)]
pub struct ModerationModal {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,

    #[live]
    icon_delete: Option<ScriptHandleRef>,
    #[live]
    icon_report: Option<ScriptHandleRef>,
    #[live]
    icon_kick: Option<ScriptHandleRef>,
    #[live]
    icon_ban: Option<ScriptHandleRef>,
    #[live]
    icon_mute: Option<ScriptHandleRef>,

    #[rust]
    request: Option<ModerationRequest>,
    #[rust]
    epoch: u64,
}

impl Widget for ModerationModal {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.request.is_some() && self.epoch != crate::account_session::epoch() {
            self.request = None;
            cx.action(ModerationModalAction::Close);
            return;
        }
        self.view.handle_event(cx, event, scope);
        self.widget_match_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl WidgetMatchEvent for ModerationModal {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions, _scope: &mut Scope) {
        let reason_input = self.view.text_input(cx, ids!(reason_input));

        let cancel_clicked = self.view.button(cx, ids!(cancel_button)).clicked(actions);
        let is_dismissed = actions
            .iter()
            .any(|a| matches!(a.downcast_ref(), Some(ModalAction::Dismissed)));
        if cancel_clicked || is_dismissed {
            self.request = None;
            // The text input won't get its focus-lost event once we're hidden, so we hide its keyboard ourselves.
            if is_dismissed && reason_input.key_focus(cx) {
                cx.hide_text_ime();
            }
            // Dismissing by clicking outside already closes the modal, and emitting
            // `Close` for it would cause an infinite action loop.
            if cancel_clicked {
                cx.action(ModerationModalAction::Close);
            }
            return;
        }

        let confirmed = self.view.button(cx, ids!(negative_button)).clicked(actions)
            || self.view.button(cx, ids!(positive_button)).clicked(actions)
            || reason_input.returned(actions).is_some();
        if !confirmed {
            return;
        }
        let Some(request) = self.request.take() else {
            return;
        };

        let reason = match reason_input.text().trim() {
            "" => None,
            reason => Some(reason.to_owned()),
        };
        submit_async_request(match request {
            ModerationRequest::DeleteMessage {
                timeline_kind,
                timeline_event_id,
                ..
            } => MatrixRequest::RedactMessage {
                timeline_kind,
                timeline_event_id,
                reason,
            },
            ModerationRequest::Report(target) => MatrixRequest::Report { target, reason },
            ModerationRequest::Member(moderation) => {
                MatrixRequest::ModerateMember { moderation, reason }
            }
        });
        cx.action(ModerationModalAction::Close);
    }
}

impl ModerationModal {
    fn set_info(&mut self, cx: &mut Cx, request: ModerationRequest) {
        let (title, body, confirm_text, negative_icon) = match &request {
            ModerationRequest::DeleteMessage { sender_name, .. } => (
                "Delete this message?",
                match sender_name {
                    Some(name) => crate::i18n::format("Are you sure you want to delete this message from {name}? This cannot be undone.", &[("name", name.to_string())]),
                    None => crate::i18n::tr("Are you sure you want to delete this message? This cannot be undone.").to_owned(),
                },
                "Delete",
                &self.icon_delete,
            ),
            ModerationRequest::Report(ReportTarget::Message { .. }) => (
                "Report this message?",
                crate::i18n::tr("Are you sure you want to report this message to your homeserver's admins?\n\nIf this room is encrypted, they won't be able to read the message.").to_owned(),
                "Report",
                &self.icon_report,
            ),
            ModerationRequest::Report(ReportTarget::Room(room_name_id)) => (
                "Report this room?",
                crate::i18n::format("Are you sure you want to report \"{room_name_id}\" to your homeserver's admins?", &[("room_name_id", room_name_id.to_string())]),
                "Report",
                &self.icon_report,
            ),
            ModerationRequest::Report(ReportTarget::User { user_name, .. }) => (
                "Report this user?",
                crate::i18n::format("Are you sure you want to report {user_name} to your homeserver's admins?", &[("user_name", user_name.to_string())]),
                "Report",
                &self.icon_report,
            ),
            ModerationRequest::Member(MemberModeration { room_name, user_name, action, .. }) => match action {
                MemberAction::Kick => (
                    "Kick this user?",
                    crate::i18n::format("Are you sure you want to kick {user_name} from \"{room_name}\"?\n\nThey'll be able to rejoin if the room allows it.", &[("user_name", user_name.to_string()), ("room_name", room_name.to_string())]),
                    "Kick",
                    &self.icon_kick,
                ),
                MemberAction::Ban => (
                    "Ban this user?",
                    crate::i18n::format("Are you sure you want to ban {user_name} from \"{room_name}\"?\n\nThey won't be able to rejoin unless they're unbanned.", &[("user_name", user_name.to_string()), ("room_name", room_name.to_string())]),
                    "Ban",
                    &self.icon_ban,
                ),
                MemberAction::Unban => (
                    "Unban this user?",
                    crate::i18n::format("Are you sure you want to unban {user_name} from \"{room_name}\"?\n\nThey'll be able to rejoin if the room allows it.", &[("user_name", user_name.to_string()), ("room_name", room_name.to_string())]),
                    "Unban",
                    &None,
                ),
                MemberAction::Mute => (
                    "Mute this user?",
                    crate::i18n::format("Are you sure you want to mute {user_name} in \"{room_name}\"?\n\nThey won't be able to send messages until they're unmuted.", &[("user_name", user_name.to_string()), ("room_name", room_name.to_string())]),
                    "Mute",
                    &self.icon_mute,
                ),
                MemberAction::Unmute => (
                    "Unmute this user?",
                    crate::i18n::format("Are you sure you want to unmute {user_name} in \"{room_name}\"?\n\nThey'll be able to send messages again.", &[("user_name", user_name.to_string()), ("room_name", room_name.to_string())]),
                    "Unmute",
                    &None,
                ),
            },
        };
        let is_positive = negative_icon.is_none();
        // A change to someone's power level has nowhere to put a reason.
        let has_reason = !matches!(
            &request,
            ModerationRequest::Member(MemberModeration {
                action: MemberAction::Mute | MemberAction::Unmute,
                ..
            })
        );

        self.view
            .label(cx, ids!(title))
            .set_text(cx, crate::i18n::tr(title));
        self.view.label(cx, ids!(body)).set_text(cx, &body);

        self.view
            .view(cx, ids!(reason_view))
            .set_visible(cx, has_reason);
        self.view
            .text_input(cx, ids!(reason_input))
            .set_text(cx, "");

        let negative_button = self.view.button(cx, ids!(negative_button));
        negative_button.set_visible(cx, !is_positive);
        negative_button.set_text(cx, crate::i18n::tr(confirm_text));
        negative_button.reset_hover(cx);
        if let Some(mut button) = negative_button.borrow_mut() {
            button.draw_icon.svg = negative_icon.clone();
        }

        let positive_button = self.view.button(cx, ids!(positive_button));
        positive_button.set_visible(cx, is_positive);
        positive_button.set_text(cx, crate::i18n::tr(confirm_text));
        positive_button.reset_hover(cx);

        self.view.button(cx, ids!(cancel_button)).reset_hover(cx);
        self.epoch = crate::account_session::epoch();
        self.request = Some(request);
        self.redraw(cx);
    }
}

impl ModerationModalRef {
    /// See [`ModerationModal::set_info()`].
    pub fn set_info(&self, cx: &mut Cx, request: ModerationRequest) {
        let Some(mut inner) = self.borrow_mut() else {
            return;
        };
        inner.set_info(cx, request);
    }
}
