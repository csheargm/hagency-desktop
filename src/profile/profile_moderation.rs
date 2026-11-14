//! The moderation buttons (mute, kick, ban, …) of a profile: which ones the
//! signed-in account may use on the shown user, and the request a click
//! makes. Shared by the room's sliding pane and the right-hand profile tab.

use makepad_widgets::*;
use crate::{
    moderation::{MemberAbilities, MemberAbilitiesFetched, MemberAbilitiesInvalidated, MemberAction, MemberModeration},
    moderation_modal::ModerationRequest,
    sliding_sync::{current_user_id, submit_async_request, MatrixRequest},
};
use super::user_profile::UserProfilePaneInfo;

/// Each moderation button and the action it asks for.
const BUTTONS: [(&[LiveId], MemberAction); 5] = [
    (ids!(mute), MemberAction::Mute), (ids!(unmute), MemberAction::Unmute),
    (ids!(kick), MemberAction::Kick), (ids!(ban), MemberAction::Ban), (ids!(unban), MemberAction::Unban),
];

/// What the signed-in account may do to the shown user, for one session.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProfileModeration {
    abilities: MemberAbilities,
    epoch: u64,
}

impl ProfileModeration {
    /// Forgets the old abilities and asks for `info`'s (none for yourself).
    pub fn request(&mut self, info: Option<&UserProfilePaneInfo>) {
        self.abilities = MemberAbilities::empty();
        self.epoch = crate::account_session::epoch();
        if let Some(info) = info && current_user_id().is_some_and(|user| user != info.user_id) {
            submit_async_request(MatrixRequest::GetMemberAbilities { room_id: info.room_id.clone(), user_id: info.user_id.clone() });
        }
    }

    /// Takes fetched abilities (and the fresher member) for `info`, and asks
    /// again when the room's were invalidated. Returns whether to redraw.
    pub fn on_actions(&mut self, info: &mut UserProfilePaneInfo, actions: &Actions) -> bool {
        let epoch = crate::account_session::epoch();
        let invalidated = actions.iter().any(|a| a.downcast_ref::<MemberAbilitiesInvalidated>()
            .is_some_and(|u| u.room_id == info.room_id && u.epoch == epoch));
        let fetched = actions.iter().filter_map(|a| a.downcast_ref::<MemberAbilitiesFetched>())
            .rfind(|f| f.room_id == info.room_id && f.user_id == info.user_id && f.epoch == epoch);
        let mut redraw = false;
        if let Some(fetched) = fetched {
            self.abilities = fetched.abilities;
            self.epoch = fetched.epoch;
            info.room_member = fetched.room_member.clone();
            redraw = true;
        }
        if invalidated { self.request(Some(info)); redraw = true; }
        redraw
    }

    /// The abilities, if they belong to the current session.
    pub fn current(&self) -> MemberAbilities {
        if self.epoch == crate::account_session::epoch() { self.abilities } else { MemberAbilities::empty() }
    }

    /// Shows the moderation `panel` and only the buttons it permits.
    pub fn show(&self, cx: &mut Cx, panel: &ViewRef) {
        let abilities = self.current();
        panel.set_visible(cx, !abilities.is_empty());
        for (button, action) in BUTTONS {
            panel.button(cx, button).set_visible(cx, abilities.permits(action));
        }
    }

    /// The moderation request a permitted button in `panel` made, if any.
    pub fn clicked(&self, cx: &mut Cx, panel: &ViewRef, info: &UserProfilePaneInfo, actions: &Actions) -> Option<ModerationRequest> {
        let abilities = self.current();
        BUTTONS.into_iter().find_map(|(button, action)| {
            (abilities.permits(action) && panel.button(cx, button).clicked(actions)).then(|| ModerationRequest::Member(MemberModeration {
                room_id: info.room_id.clone(), room_name: info.room_name.clone(), user_id: info.user_id.clone(),
                user_name: info.displayable_name().to_owned(), action,
            }))
        })
    }
}
