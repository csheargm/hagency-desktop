//! A person's or an Agent's profile, hosted as a tab in the right-hand
//! workspace region. It reuses the desktop `UserProfileView` the room's
//! sliding pane shows, with the same fill and actions, plus the user's
//! server. An Agent's profile is read-only apart from Message and Copy Link.

use makepad_widgets::*;
use crate::{
    block_user_modal::BlockUserModalAction,
    moderation_modal::ModerationModalAction,
    profile::user_verification_cache::{get_or_fetch_user_verification_state, UserVerificationState, UserVerificationStatesUpdated},
    sliding_sync::{submit_async_request, MatrixRequest},
};
use super::{
    profile_moderation::ProfileModeration,
    profile_route::{self, ProfileRoute},
    profile_view_fill,
    user_profile::UserProfilePaneInfo,
};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.ProfileSidePanel = #(ProfileSidePanel::register_widget(vm)) {
        width: Fill, height: Fill, flow: Down
        show_bg: true, draw_bg.color: (COLOR_PRIMARY)
        user_profile_view := mod.widgets.UserProfileView { }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct ProfileSidePanel {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] info: Option<UserProfilePaneInfo>,
    #[rust] route: Option<ProfileRoute>,
    #[rust] moderation: ProfileModeration,
}

impl Widget for ProfileSidePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::Signal = event
            && self.info.as_mut().is_some_and(|info| profile_view_fill::refresh_info(cx, info))
        { self.view.redraw(cx); }
        let Event::Actions(actions) = event else { return };
        if actions.iter().any(|a| a.downcast_ref::<UserVerificationStatesUpdated>().is_some()) {
            self.view.redraw(cx);
        }
        if let Some(info) = self.info.as_mut() && self.moderation.on_actions(info, actions) {
            self.view.redraw(cx);
        }
        self.handle_profile_actions(cx, actions);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(info) = self.info.as_ref() {
            let panel = self.view.widget(cx, ids!(user_profile_view));
            profile_view_fill::fill_profile_view(cx, &panel, info);
            if self.route == Some(ProfileRoute::Agent) {
                for id in [ids!(block_user_button), ids!(verify_this_user_button), ids!(withdraw_verification_button), ids!(profile_moments)] {
                    panel.widget(cx, id).set_visible(cx, false);
                }
            } else {
                let moderation = panel.view(cx, ids!(desktop_moderation));
                self.moderation.show(cx, &moderation);
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

impl ProfileSidePanel {
    /// Shows `info`: an Agent read-only, a person with their actions.
    pub fn show(&mut self, cx: &mut Cx, mut info: UserProfilePaneInfo) {
        if info.room_member.is_none() {
            profile_view_fill::fill_member_from_cache(cx, &mut info);
        }
        info.avatar_state.update_from_cache(cx);
        let route = profile_route::route_for(&info.user_id);
        let details = self.view.label(cx, ids!(user_profile_view.profile_details));
        details.set_text(cx, &profile_route::detail_line(&info.user_id, route));
        details.set_visible(cx, true);
        // Jumping to a read receipt needs the room's timeline, which is not here.
        self.view.button(cx, ids!(jump_to_read_receipt_button)).set_visible(cx, false);
        let agent = route == ProfileRoute::Agent;
        self.view.button(cx, ids!(profile_moments)).set_visible(cx, !agent);
        self.view.view(cx, ids!(desktop_moderation)).set_visible(cx, false);
        self.moderation.request((!agent).then_some(&info));
        self.route = Some(route);
        self.info = Some(info);
        self.view.redraw(cx);
    }

    /// The room pane's actions, except Read Receipt; an Agent keeps only
    /// Message and Copy Link.
    fn handle_profile_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let Some(info) = self.info.clone() else { return };
        if self.view.button(cx, ids!(direct_message_button)).clicked(actions) {
            submit_async_request(MatrixRequest::OpenOrCreateDirectMessage {
                user_profile: info.user_profile.clone(),
                // Ask for confirmation before creating a new DM room.
                allow_create: false,
            });
        }
        if self.view.button(cx, ids!(copy_link_to_user_button)).clicked(actions) {
            profile_view_fill::copy_user_link(cx, &info);
        }
        if self.route != Some(ProfileRoute::Person) { return; }
        if self.view.button(cx, ids!(profile_moments)).clicked(actions) {
            cx.action(crate::moments::ui::MomentsAction::Open { author: Some(info.user_id.clone()) });
        }
        if self.view.button(cx, ids!(withdraw_verification_button)).clicked(actions) {
            submit_async_request(MatrixRequest::AcknowledgeIdentityReset { user_id: info.user_id.clone() });
        }
        if self.view.button(cx, ids!(verify_this_user_button)).clicked(actions)
            && get_or_fetch_user_verification_state(cx, &info.user_id) != Some(UserVerificationState::Verified)
        {
            submit_async_request(MatrixRequest::RequestUserVerification { user_id: info.user_id.clone() });
        }
        if self.view.button(cx, ids!(block_user_button)).clicked(actions) {
            cx.action(BlockUserModalAction::Open(profile_view_fill::block_request(&info)));
        }
        let moderation = self.view.view(cx, ids!(desktop_moderation));
        if let Some(request) = self.moderation.clicked(cx, &moderation, &info, actions) {
            cx.action(ModerationModalAction::Open(request));
        }
    }
}

impl ProfileSidePanelRef {
    /// See [`ProfileSidePanel::show()`].
    pub fn show(&self, cx: &mut Cx, info: UserProfilePaneInfo) {
        if let Some(mut inner) = self.borrow_mut() { inner.show(cx, info); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{profile::user_profile::{UserProfile, UserProfileAndRoomId}, shared::avatar::AvatarState};
    use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId};

    fn panel(cx: &mut Cx) -> ProfileSidePanel {
        cx.with_vm(|vm| {
            crate::theme::tests::install(vm, Default::default());
            crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let value = script_eval!(vm, {mod.widgets.ProfileSidePanel {}});
            let panel = ProfileSidePanel::script_from_value(vm, value);
            assert!(vm.take_errors().is_empty(), "the side panel's DSL evaluates without errors");
            panel
        })
    }

    fn info(user: &str) -> UserProfilePaneInfo {
        UserProfilePaneInfo {
            profile_and_room_id: UserProfileAndRoomId {
                user_profile: UserProfile { user_id: OwnedUserId::try_from(user).unwrap(), username: Some("Ann".into()), avatar_state: AvatarState::Unknown },
                room_id: OwnedRoomId::try_from("!room:s").unwrap(),
            },
            room_name: "Room".into(),
            room_member: None,
        }
    }

    #[test]
    fn a_person_shows_their_server_and_actions_without_the_read_receipt() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = panel(&mut cx);
        panel.show(&mut cx, info("@ann:example.org"));
        assert_eq!(panel.route, Some(ProfileRoute::Person));
        assert_eq!(panel.view.label(&mut cx, ids!(user_profile_view.profile_details)).text(), "Server: example.org");
        assert!(!panel.view.button(&mut cx, ids!(jump_to_read_receipt_button)).visible(), "no timeline here");
        assert!(panel.view.button(&mut cx, ids!(profile_moments)).visible());
    }

    #[test]
    fn an_agent_is_read_only() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = panel(&mut cx);
        panel.show(&mut cx, info("@_hagency_agt_1:example.org"));
        assert_eq!(panel.route, Some(ProfileRoute::Agent));
        assert_eq!(panel.view.label(&mut cx, ids!(user_profile_view.profile_details)).text(), "Agent on example.org");
        assert!(!panel.view.button(&mut cx, ids!(profile_moments)).visible());
        assert!(!panel.view.view(&mut cx, ids!(desktop_moderation)).visible());
    }
}
