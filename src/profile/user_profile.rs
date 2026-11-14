//! Widgets and types related to displaying info about a user profile.
use crate::profile::user_verification_cache::{get_or_fetch_user_verification_state, UserVerificationState, UserVerificationStatesUpdated};

use crate::moderation_modal::ModerationModalAction;
use std::ops::{Deref, DerefMut};
use makepad_widgets::*;
use matrix_sdk::{room::RoomMember, ruma::{OwnedRoomId, OwnedUserId}};
use crate::{
    block_user_modal::BlockUserModalAction, shared::avatar::{AvatarState, AvatarWidgetExt}, sliding_sync::{MatrixRequest, is_user_blocked, submit_async_request}, utils
};
use super::{profile_moderation::ProfileModeration, profile_view_fill};
use crate::shared::navigation_bar_button::NavigationBarButtonWidgetExt;


/// Information retrieved about a user: their displayable name, ID, and known avatar state.
#[derive(Clone, Debug)]
pub struct UserProfile {
    pub user_id: OwnedUserId,
    /// The user's default display name, if set.
    /// Note that a user may have per-room display names,
    /// so this should be considered a fallback.
    pub username: Option<String>,
    pub avatar_state: AvatarState,
}
impl UserProfile {
    /// Returns the user's displayable name, using the user ID as a fallback.
    pub fn displayable_name(&self) -> &str {
        if let Some(un) = self.username.as_ref() {
            if !un.is_empty() {
                return un.as_str();
            }
        }
        self.user_id.as_str()
    }

    /// Returns the first "letter" (Unicode grapheme) of the user's name or user ID,
    /// skipping any leading "@" characters.
    #[allow(unused)]
    pub fn first_letter(&self) -> &str {
        self.username.as_deref()
            .and_then(|un| utils::user_name_first_letter(un))
            .or_else(|| utils::user_name_first_letter(self.user_id.as_str()))
            .unwrap_or_default()
    }
}


/// Basic info needed to populate the contents of an avatar widget.
#[derive(Clone, Debug)]
pub struct UserProfileAndRoomId {
    pub user_profile: UserProfile,
    pub room_id: OwnedRoomId,
}
impl Deref for UserProfileAndRoomId {
    type Target = UserProfile;
    fn deref(&self) -> &Self::Target {
        &self.user_profile
    }
}
impl DerefMut for UserProfileAndRoomId {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.user_profile
    }
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*



    mod.widgets.ICON_DOUBLE_CHAT = crate_resource("self://resources/icons/double_chat.svg")

    let ProfileAction = RobrixNeutralIconButton {
        width: Fill height: 36 spacing: 6 margin: 0
        align: Align{x: 0 y: 0.5}
        padding: Inset{left: 8 right: 8 top: 0 bottom: 0}
        icon_walk: Walk{width: 16 height: 16}
    }

    let ModerationButton = RobrixNegativeIconButton {
        width: Fill height: 34 margin: 0 padding: 8
        icon_walk: Walk{width: 16 height: 16}
    }
    let ModerationActions = View {
        width: Fill height: Fit flow: Down spacing: 8 padding: 10 visible: false
        Label {text: #(crate::i18n::tr("Moderation")) i18n_text: "Moderation"}
        mute := ModerationButton {text: #(crate::i18n::tr("Mute User")) i18n_text: "Mute User" draw_icon.svg: ICON_MUTE}
        unmute := ModerationButton {text: #(crate::i18n::tr("Unmute User")) i18n_text: "Unmute User" draw_icon.svg: ICON_CHECKMARK}
        kick := ModerationButton {text: #(crate::i18n::tr("Kick User")) i18n_text: "Kick User" draw_icon.svg: ICON_LOGOUT}
        ban := ModerationButton {text: #(crate::i18n::tr("Ban User")) i18n_text: "Ban User" draw_icon.svg: ICON_FORBIDDEN}
        unban := ModerationButton {text: #(crate::i18n::tr("Unban User")) i18n_text: "Unban User" draw_icon.svg: ICON_CHECKMARK}
    }
    mod.widgets.UserProfileView = ScrollXYView {
        width: Fill,
        height: Fill,
        align: Align{x: 0.5, y: 0},
        padding: Inset{left: 15, right: 15, top: 15, bottom: 50}
        spacing: 20,
        flow: Down,
        cursor: MouseCursor.Default,

        personal_info := View {
            width: Fill, height: Fit
            align: Align{x: 0.5, y: 0.0}
            padding: Inset{left: 10, right: 10}
            spacing: 3
            flow: Down

            avatar_box := View {
                width: 150, height: 150, margin: 10,
                flow: Overlay, align: Align{x: 1.0, y: 1.0}
            avatar := Avatar {
                width: Fill,
                height: Fill,
                text_view +: {
                    text +: {
                        draw_text +: {
                            text_style: theme.font_regular { font_size: (40.0 * mod.widgets.RINX_TEXT_SCALE) }
                        }
                    }
                }
            }
                verified_badge := mod.widgets.IconYes { width: 36, height: 36, visible: false }
                identity_reset_badge := mod.widgets.IconNo { width: 36, height: 36, visible: false }
            }

            user_name := Label {
                width: Fill, height: Fit
                align: Align{x: 0.5}
                flow: Flow.Right{wrap: true}
                draw_text +: {
                    color: mod.widgets.RINX_INK,
                    text_style: USERNAME_TEXT_STYLE { font_size: (12 * mod.widgets.RINX_TEXT_SCALE) },
                }
                text: #(crate::i18n::tr("User Name"))
            }

            user_id := Label {
                width: Fill, height: Fit
                align: Align{x: 0.5}
                flow: Flow.Right{wrap: true}
                draw_text +: {
                    color: (MESSAGE_TEXT_COLOR),
                    text_style: MESSAGE_TEXT_STYLE { font_size: (11 * mod.widgets.RINX_TEXT_SCALE) },
                }
                text: #(crate::i18n::tr("User ID"))
            }
            // The server, or that this is an Agent (right-hand profile tab only).
            profile_details := Label {
                visible: false, width: Fill, height: Fit
                align: Align{x: 0.5}
                flow: Flow.Right{wrap: true}
                draw_text +: {color: (MESSAGE_TEXT_COLOR) text_style: MESSAGE_TEXT_STYLE { font_size: (10 * mod.widgets.RINX_TEXT_SCALE) }}
                text: ""
            }
        }

        LineH { padding: 15 }
        
        membership := View {
            width: Fill,
            height: Fit,
            flow: Down,
            spacing: 3,
            align: Align{x: 0.0, y: 0.0}
            padding: Inset{left: 10, right: 10}

            membership_title_label := Label {
                width: Fill, height: Fit
                flow: Flow.Right{wrap: true}
                draw_text +: {
                    text_style: USERNAME_TEXT_STYLE { font_size: (11.5 * mod.widgets.RINX_TEXT_SCALE) },
                    color: mod.widgets.RINX_INK
                }
                text: #(crate::i18n::tr("Membership in this room")) i18n_text: "Membership in this room"
            }

            membership_status_label := Label {
                margin: Inset{ left: 7 }
                width: Fill, height: Fit
                flow: Flow.Right{wrap: true}
                draw_text +: {
                    color: (MESSAGE_TEXT_COLOR),
                    text_style: MESSAGE_TEXT_STYLE { font_size: (11 * mod.widgets.RINX_TEXT_SCALE) },
                }
                text: #(crate::i18n::tr("Unknown"))
            }

            role_info_label := Label {
                margin: Inset{ left: 7 }
                width: Fill, height: Fit
                flow: Flow.Right{wrap: true}
                draw_text +: {
                    color: (MESSAGE_TEXT_COLOR),
                    text_style: MESSAGE_TEXT_STYLE { font_size: (11 * mod.widgets.RINX_TEXT_SCALE) },
                }
                text: #(crate::i18n::tr("Unknown"))
            }
        }

        LineH { padding: 15 }

        action_buttons := View {
            width: Fill, height: Fit
            flow: Down,
            spacing: 10,
            padding: Inset{left: 10., right: 10, bottom: 10}

            Label {
                width: Fill, height: Fit
                flow: Flow.Right{wrap: true}
                draw_text +: {
                    text_style: USERNAME_TEXT_STYLE { font_size: (11.5 * mod.widgets.RINX_TEXT_SCALE) },
                    color: mod.widgets.RINX_INK
                }
                text: #(crate::i18n::tr("Actions")) i18n_text: "Actions"
            }

            View {width: Fill height: 36 flow: Right spacing: 8
                direct_message_button := RobrixIconButton {
                    width: Fill height: 36 spacing: 6 margin: 0
                    align: Align{x: 0 y: 0.5}
                    padding: Inset{left: 8 right: 8 top: 0 bottom: 0}
                    draw_icon.svg: mod.widgets.ICON_DOUBLE_CHAT
                    icon_walk: Walk{width: 16 height: 16}
                    text: #(crate::i18n::tr("Messages")) i18n_text: "Messages"
                }
                profile_moments := ProfileAction {text: #(crate::i18n::tr("Moments")) i18n_text: "Moments" draw_icon.svg: ICON_GLOBE}
            }
            View {width: Fill height: 36 flow: Right spacing: 8
                copy_link_to_user_button := ProfileAction {
                    draw_icon.svg: ICON_COPY
                    text: #(crate::i18n::tr("Copy Link")) i18n_text: "Copy Link"
                }
                jump_to_read_receipt_button := ProfileAction {
                    draw_icon.svg: ICON_JUMP
                    text: #(crate::i18n::tr("Read Receipt")) i18n_text: "Read Receipt"
                }
            }

            withdraw_verification_button := RobrixNegativeIconButton {
                padding: 10, margin: 0,
                draw_icon.svg: VERIFICATION_NO
                icon_walk: Walk{width: 16, height: 16}
                text: #(crate::i18n::tr("Withdraw Verification")) i18n_text: "Withdraw Verification"
            }

            verify_this_user_button := RobrixPositiveIconButton {
                padding: 10, margin: 0,
                draw_icon.svg: VERIFICATION_YES
                icon_walk: Walk{width: 16, height: 16}
                text: #(crate::i18n::tr("Verify this User")) i18n_text: "Verify this User"
            }

            block_user_button := RobrixNegativeIconButton {
                padding: Inset{top: 10, bottom: 10, left: 12, right: 15}
                margin: 0,
                draw_icon.svg: (ICON_FORBIDDEN)
                icon_walk: Walk{width: 16, height: 16, margin: Inset{left: -2, right: -0.5} }
                text: #(crate::i18n::tr("Block User")) i18n_text: "Block User"
            }
        }

        desktop_moderation := ModerationActions {}

        // A view that allows the user to verify a new DID and associate it
        // with a particular Matrix User ID.
        tsp_verify_user := TspVerifyUser { }
    }


    mod.widgets.UserProfileSlidingPane = #(UserProfileSlidingPane::register_widget(vm)) {
        visible: false,
        flow: Overlay,
        width: Fill,
        height: Fill,
        align: Align{x: 1.0, y: 0}

        bg_view := SolidView {
            width: Fill
            height: Fill
            visible: false,
            show_bg: true
            draw_bg.color: #000000BB
        }

        main_content := SolidView {
            width: 300,
            height: Fill
            flow: Overlay,
            align: Align{x: 0}

            show_bg: true,
            draw_bg.color: (COLOR_PRIMARY)

            user_profile_view := mod.widgets.UserProfileView { }

            mobile_profile := SolidView {
                visible: false width: Fill height: Fill flow: Down draw_bg.color: mod.widgets.RINX_PAGE
                ScrollYView {
                    width: Fill height: Fill flow: Down
                    mp_card := DetailContactCard {}
                    DetailGap {}
                    DetailSection {
                        mp_moments := DetailRow {title.text: #(crate::i18n::tr("Moments")) title.i18n_text: "Moments"}
                        DetailDivider {}
                        mp_copy := DetailRow {title.text: #(crate::i18n::tr("Copy Profile Link")) title.i18n_text: "Copy Profile Link"}
                        DetailDivider {}
                        mp_withdraw_verification := DetailRow {title.text: #(crate::i18n::tr("Withdraw Verification")) title.i18n_text: "Withdraw Verification"}
                        mp_verify := DetailRow {title.text: #(crate::i18n::tr("Verify this User")) title.i18n_text: "Verify this User"}
                        DetailDivider {}
                        mp_receipt := DetailRow {title.text: #(crate::i18n::tr("Last Read Message")) title.i18n_text: "Last Read Message"}
                    }
                    DetailGap {}
                    DetailSection {
                        mp_room := DetailRow {title.text: #(crate::i18n::tr("Chat")) title.i18n_text: "Chat" chevron.visible: false}
                        DetailDivider {}
                        mp_membership := DetailRow {title.text: #(crate::i18n::tr("Membership")) title.i18n_text: "Membership" chevron.visible: false}
                        DetailDivider {}
                        mp_role := DetailRow {title.text: #(crate::i18n::tr("Role")) title.i18n_text: "Role" chevron.visible: false}
                    }
                    DetailGap {}
                    DetailSection {mp_message := DetailAction {title.text: #(crate::i18n::tr("Messages")) title.i18n_text: "Messages"}}
                    DetailGap {}
                    mobile_moderation := ModerationActions {}
                    mp_block_section := DetailSection {mp_block := DetailAction {title +: {text: #(crate::i18n::tr("Block")) i18n_text: "Block" draw_text.color: mod.widgets.RINX_DANGER_FG}}}
                }
            }

            // The "X" close button on the top left
            close_button := RobrixNeutralIconButton {
                width: Fit,
                height: Fit,
                spacing: 0,
                margin: 7,
                padding: 15,
                draw_icon.svg: (ICON_CLOSE)
                icon_walk: Walk{width: 14, height: 14}
            }
        }

        slide: 1.0,

        animator: Animator {
            panel: {
                default: @hide
                show: AnimatorState{
                    redraw: true,
                    from: {all: Forward {duration: 0.5}}
                    ease: Ease.ExpDecay {d1: 0.80, d2: 0.97}
                    apply: {
                        slide: 0.0
                    }
                }
                hide: AnimatorState{
                    redraw: true,
                    from: {all: Forward {duration: 0.5}}
                    ease: Ease.ExpDecay {d1: 0.80, d2: 0.97}
                    apply: {
                        slide: 1.0
                    }
                }
            }
        }
    }
}


#[derive(Clone, Default, Debug)]
pub enum ShowUserProfileAction {
    ShowUserProfile(UserProfileAndRoomId),
    #[default]
    None,
}

/// Actions emitted by the user profile sliding pane, handled by its parent RoomScreen.
#[derive(Clone, Default, Debug)]
pub enum UserProfilePaneAction {
    /// A request to scroll the timeline to this user's latest read receipt.
    JumpToReadReceipt(OwnedUserId),
    #[default]
    None,
}

/// Information needed to populate/display the user profile sliding pane.
#[derive(Clone, Debug)]
pub struct UserProfilePaneInfo {
    pub profile_and_room_id: UserProfileAndRoomId,
    pub room_name: String,
    pub room_member: Option<RoomMember>,
}
impl Deref for UserProfilePaneInfo {
    type Target = UserProfileAndRoomId;
    fn deref(&self) -> &Self::Target {
        &self.profile_and_room_id
    }
}
impl DerefMut for UserProfilePaneInfo {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.profile_and_room_id
    }
}
#[derive(Script, Widget, Animator)]
pub struct UserProfileSlidingPane {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[apply_default] animator: Animator,
    #[live] slide: f32,

    #[rust] info: Option<UserProfilePaneInfo>,
    #[rust] is_animating_out: bool,
    /// The panel width and `slide` value last put on `main_content` and
    /// `bg_view`; `None` when they have not been placed since this pane was
    /// last applied from script.
    #[rust] placed: Option<(f64, f32)>,
    #[rust] moderation: ProfileModeration,
}

impl ScriptHook for UserProfileSlidingPane {
    fn on_after_apply(&mut self, _vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        // Applying the pane itself (creation, a theme or language reload) puts
        // the template's width, margin and backdrop back, so they go on again.
        // The animator's own `slide` steps change the cached value by themselves.
        if !apply.is_animate() {
            self.placed = None;
        }
    }
}

impl Widget for UserProfileSlidingPane {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);

        if !self.visible { return; }

        let animator_action = self.animator_handle_event(cx, event);
        if animator_action.must_redraw() {
            self.redraw(cx);
        }

        // If we're animating the pane closed and the animation track has finished
        // (the Animator removes ended tracks on the same frame it returns the last
        // Animating action), hide everything and mark the pane as invisible.
        if self.is_animating_out && !self.animator.is_track_animating(id!(panel)) {
            self.visible = false;
            self.is_animating_out = false;
            // give up key focus if we still had it
            if cx.has_key_focus(self.view.area()) {
                cx.revert_key_focus();
            }
            self.view(cx, ids!(bg_view)).set_visible(cx, false);
            self.redraw(cx);
            return;
        }
        // While the pane is sliding out, don't handle any events/hits,
        // as those hits are likely intended for a different widget.
        if self.is_animating_out { return; }

        let area = self.view.area();

        // Close the pane if:
        // 1. The close button is clicked,
        // 2. The back navigational gesture/action occurs (e.g., Back on Android),
        // 3. The escape key is pressed if this pane has key focus,
        // 4. The back mouse button is clicked within this view,
        // 5. The user clicks/touches outside the main_content view area.
        let close_pane = {
            matches!(
                event,
                Event::Actions(actions) if self.button(cx, ids!(close_button)).clicked(actions)
            )
            || event.back_pressed()
            || match event.hits_with_capture_overload(cx, area, true) {
                Hit::KeyUp(key) => key.key_code == KeyCode::Escape,
                Hit::FingerDown(_fde) => {
                    cx.set_key_focus(area);
                    false
                }
                Hit::FingerUp(fue) if fue.is_over => {
                    fue.mouse_button().is_some_and(|b| b.is_back())
                    || !self.view(cx, ids!(main_content)).area().rect(cx).contains(fue.abs)
                }
                _ => false,
            }
        };
        if close_pane {
            self.is_animating_out = true;
            self.animator_play(cx, ids!(panel.hide));
            self.redraw(cx);
            return;
        }

        if let Event::Actions(actions) = event
            && actions.iter().any(|a| a.downcast_ref::<UserVerificationStatesUpdated>().is_some())
        { self.redraw(cx); }

        if let Event::Actions(actions) = event && let Some(info) = self.info.as_mut()
            && self.moderation.on_actions(info, actions)
        { self.redraw(cx); }
        // A UI Signal indicates that this user profile's info may have been updated by a background task.
        if let Event::Signal = event {
            let redraw_this_pane = self.info.as_mut().is_some_and(|info| profile_view_fill::refresh_info(cx, info));
            if redraw_this_pane {
                self.redraw(cx);
            }
        }

        let Some(info) = self.info.as_ref() else { return };

        if let Event::Actions(actions) = event {
            if self.button(cx, ids!(profile_moments)).clicked(actions) || self.view.navigation_bar_button(cx, ids!(mp_moments)).clicked(actions) {
                cx.action(crate::moments::ui::MomentsAction::Open {author: Some(info.user_id.clone())});
            }
            if self.button(cx, ids!(direct_message_button)).clicked(actions)
                || self.view.navigation_bar_button(cx, ids!(mp_message)).clicked(actions) {
                submit_async_request(MatrixRequest::OpenOrCreateDirectMessage {
                    user_profile: info.user_profile.clone(),
                    // Don't just create a new DM room; we want to first get confirmation from the user.
                    allow_create: false,
                });
                if !crate::home::home_screen::effective_is_desktop(cx) {
                    self.is_animating_out = true;
                    cx.revert_key_focus();
                    self.animator_play(cx, ids!(panel.hide));
                    self.redraw(cx);
                    return;
                }
            }

            if self.button(cx, ids!(withdraw_verification_button)).clicked(actions)
                || self.view.navigation_bar_button(cx, ids!(mp_withdraw_verification)).clicked(actions)
            {
                submit_async_request(MatrixRequest::AcknowledgeIdentityReset { user_id: info.user_id.clone() });
            }
            if self.button(cx, ids!(verify_this_user_button)).clicked(actions)
                || self.view.navigation_bar_button(cx, ids!(mp_verify)).clicked(actions)
            {
                if get_or_fetch_user_verification_state(cx, &info.user_id) != Some(UserVerificationState::Verified) {
                    submit_async_request(MatrixRequest::RequestUserVerification { user_id: info.user_id.clone() });
                }
            }

            if self.button(cx, ids!(copy_link_to_user_button)).clicked(actions)
                || self.view.navigation_bar_button(cx, ids!(mp_copy)).clicked(actions) {
                profile_view_fill::copy_user_link(cx, info);
            }

            // Handle the jump to read receipt button being clicked, which is mostly handled by the room screen.
            if !self.is_animating_out && (self.button(cx, ids!(jump_to_read_receipt_button)).clicked(actions)
                || self.view.navigation_bar_button(cx, ids!(mp_receipt)).clicked(actions)) {
                cx.widget_action(
                    self.widget_uid(),
                    UserProfilePaneAction::JumpToReadReceipt(info.user_id.clone()),
                );
                // Close the sliding pane so it doesn't cover the event we're jumping to.
                self.is_animating_out = true;
                cx.revert_key_focus();
                self.animator_play(cx, ids!(panel.hide));
                self.redraw(cx);
                return;
            }

            if !self.is_animating_out {
                let request = [ids!(desktop_moderation), ids!(mobile_moderation)].into_iter()
                    .find_map(|panel| self.moderation.clicked(cx, &self.view.view(cx, panel), info, actions));
                if let Some(request) = request {
                    cx.action(ModerationModalAction::Open(request));
                    self.is_animating_out = true;
                    cx.revert_key_focus();
                    self.animator_play(cx, ids!(panel.hide));
                    self.redraw(cx);
                    return;
                }
            }
            if !self.is_animating_out && (self.button(cx, ids!(block_user_button)).clicked(actions)
                || self.view.navigation_bar_button(cx, ids!(mp_block)).clicked(actions)) {
                let request = profile_view_fill::block_request(info);
                // Hide the pane immediately to make room for the confirmation modal
                self.is_animating_out = true;
                cx.revert_key_focus();
                self.animator_play(cx, ids!(panel.hide));
                cx.action(BlockUserModalAction::Open(request));
                self.redraw(cx);
            }
        }
    }


    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(info) = self.info.as_ref() else {
            self.visible = false;
            return self.view.draw_walk(cx, scope, walk);
        };

        // Use the `slide` value to position main_content.
        // slide=0.0 means fully shown; slide=1.0 means fully hidden (off-screen right).
        let mobile = !crate::home::home_screen::effective_is_desktop(cx);
        let panel_width = if mobile { cx.turtle().size().x } else { 300.0 };
        let right_margin = -(f64::from(self.slide) * panel_width);
        // Applying a property asks for a redraw, and this runs on every draw:
        // placing the pane each time kept an open pane drawing for ever.
        let place = self.placed != Some((panel_width, self.slide));
        self.placed = Some((panel_width, self.slide));
        if place {
            let mut main_content = self.view(cx, ids!(main_content));
            script_apply_eval!(cx, main_content, {
                width: #(panel_width)
                margin.right: #(right_margin)
            });
        }
        self.view(cx, ids!(mobile_profile)).set_visible(cx, mobile);
        self.view(cx, ids!(user_profile_view)).set_visible(cx, !mobile);
        self.button(cx, ids!(close_button)).set_visible(cx, !mobile);
        // Also derive the bg_view overlay alpha from `slide`.
        // The animator can only interpolate struct fields, not child view properties,
        // so we compute the bg color here from the smoothly-animated `slide` value.
        let bg_alpha = (1.0 - self.slide) * 0.733; // 0.733 ≈ 0xBB/0xFF
        if place {
            let bg_color = vec4(0.0, 0.0, 0.0, bg_alpha);
            let mut bg_view = self.view(cx, ids!(bg_view));
            script_apply_eval!(cx, bg_view, {
                draw_bg +: { color: #(bg_color) }
            });
        }

        let profile_view = self.view.widget(cx, ids!(user_profile_view));
        profile_view_fill::fill_profile_view(cx, &profile_view, info);
        let (membership, role) = profile_view_fill::standing(cx, info);
        self.label(cx, ids!(mp_card.name)).set_text(cx, info.displayable_name());
        self.label(cx, ids!(mp_card.user_id)).set_text(cx, &crate::i18n::format("Matrix ID: {0}", &[("0", (info.user_id).to_string())]));
        self.label(cx, ids!(mp_room.value)).set_text(cx, &info.room_name);
        self.label(cx, ids!(mp_membership.value)).set_text(cx, membership.trim_start_matches(crate::i18n::tr("Status: ")));
        self.label(cx, ids!(mp_role.value)).set_text(cx, role.trim_start_matches(crate::i18n::tr("Role: ")));
        let mobile_avatar = self.avatar(cx, ids!(mp_card.avatar));
        info.avatar_state.image()
            .and_then(|image| mobile_avatar.show_image(cx, None, |cx, img| utils::load_avatar_image(&img, cx, image)).ok())
            .unwrap_or_else(|| mobile_avatar.show_user_text(cx, &info.user_id, info.displayable_name()));

        let is_pane_showing_current_account = profile_view_fill::is_current_account(info);
        let verification = get_or_fetch_user_verification_state(cx, &info.user_id);
        let verified = verification == Some(UserVerificationState::Verified);
        let reset = verification == Some(UserVerificationState::VerificationViolation);
        self.view.widget(cx, ids!(mp_withdraw_verification)).set_visible(cx, reset);
        self.view.widget(cx, ids!(mp_verify)).set_visible(cx, !is_pane_showing_current_account);
        self.label(cx, ids!(mp_verify.title)).set_text(cx, crate::i18n::tr(if verified {"Verified User"} else if reset {"Identity reset — verify again"} else {"Verify this User"}));

        for panel in [ids!(desktop_moderation), ids!(mobile_moderation)] {
            let view = self.view.view(cx, panel);
            self.moderation.show(cx, &view);
        }
        let is_blocked = is_user_blocked(&info.user_id);
        self.view.widget(cx, ids!(mp_message)).set_visible(cx, true);
        self.label(cx, ids!(mp_message.title)).set_text(cx, profile_view_fill::message_label(is_pane_showing_current_account));
        self.view.widget(cx, ids!(mp_block_section)).set_visible(cx, !is_pane_showing_current_account);
        self.label(cx, ids!(mp_block.title)).set_text(cx, if is_blocked { crate::i18n::tr("Unblock") } else { crate::i18n::tr("Block") });

        self.view.draw_walk(cx, scope, walk)
    }
}


impl UserProfileSlidingPane {
    /// Returns `true` if this pane is currently being shown.
    pub fn is_currently_shown(&self, _cx: &mut Cx) -> bool {
        self.visible
    }

    /// Sets the info to be displayed in this user profile sliding pane.
    ///
    /// If the `room_member` field is `None`, this function will attempt to
    /// retrieve the user's room membership info from the local cache,
    /// and then submit a request to asynchronously fetch it from the server
    /// if it's not found in the cache.
    pub fn set_info(&mut self, cx: &mut Cx, mut info: UserProfilePaneInfo) {
        if info.room_member.is_none() {
            profile_view_fill::fill_member_from_cache(cx, &mut info);
        }
        info.avatar_state.update_from_cache(cx);

        // If TSP is enabled, populate the TSP verification info for this user.
        #[cfg(feature = "tsp")] {
            use crate::tsp::verify_user::TspVerifyUserWidgetExt;
            self.view.tsp_verify_user(cx, ids!(tsp_verify_user))
                .show(cx, info.user_id.clone());
        }

        self.info = Some(info);
        self.request_member_abilities();
    }

    fn request_member_abilities(&mut self) {
        self.moderation.request(self.info.as_ref());
    }

    pub fn show(&mut self, cx: &mut Cx) {
        self.visible = true;
        self.is_animating_out = false;
        cx.set_key_focus(self.view.area());
        self.animator_play(cx, ids!(panel.show));
        self.view(cx, ids!(bg_view)).set_visible(cx, true);

        self.view.button(cx, ids!(close_button)).reset_hover(cx);
        self.view.button(cx, ids!(direct_message_button)).reset_hover(cx);
        self.view.button(cx, ids!(verify_this_user_button)).reset_hover(cx);
        self.view.button(cx, ids!(copy_link_to_user_button)).reset_hover(cx);
        self.view.button(cx, ids!(jump_to_read_receipt_button)).reset_hover(cx);
        self.view.button(cx, ids!(block_user_button)).reset_hover(cx);
        self.redraw(cx);
    }
}

impl UserProfileSlidingPaneRef {
    /// Close the mobile detail page without popping its underlying chat.
    pub fn dismiss(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.is_animating_out = true;
            cx.revert_key_focus();
            inner.animator_play(cx, ids!(panel.hide));
            inner.redraw(cx);
        }
    }
    /// See [`UserProfileSlidingPane::is_currently_shown()`]
    pub fn is_currently_shown(&self, cx: &mut Cx) -> bool {
        let Some(inner) = self.borrow() else { return false };
        inner.is_currently_shown(cx)
    }

    /// See [`UserProfileSlidingPane::set_info()`]
    pub fn set_info(&self, cx: &mut Cx, info: UserProfilePaneInfo) {
        let Some(mut inner) = self.borrow_mut() else { return };
        inner.set_info(cx, info);
    }

    /// See [`UserProfileSlidingPane::show()`]
    pub fn show(&self, cx: &mut Cx) {
        let Some(mut inner) = self.borrow_mut() else { return };
        inner.show(cx);
    }

    /// Hides the pane immediately and clears its state without animating it out.
    pub fn reset(&self, cx: &mut Cx) {
        let Some(mut inner) = self.borrow_mut() else { return };
        inner.visible = false;
        inner.animator_cut(cx, ids!(panel.hide));
        inner.is_animating_out = false;
        inner.info = None;
        inner.view(cx, ids!(bg_view)).set_visible(cx, false);
        if cx.has_key_focus(inner.view.area()) {
            cx.revert_key_focus();
        }
        inner.redraw(cx);
    }
}
