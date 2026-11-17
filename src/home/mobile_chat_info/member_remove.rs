//! "Remove from room" on a Members row, behind the row's "⋯" button.
//!
//! - One of the signed-in owner's own Agents: confirmed, then the Agent's room
//!   binding is left through the Agent service, exactly as the agent panel's
//!   "Remove my agent from this room" does. Owners may always do this.
//! - Anyone else (other people's Agents included): only for members this
//!   account may kick under the room's power levels, confirmed in the
//!   moderation modal, which checks the power levels again before it kicks.
//!
//! Either way the result shows as a popup, and a success takes the row out
//! at once ([`MemberLeft`]) rather than waiting for the next sync.
use std::collections::HashSet;
use std::cell::RefCell;
use makepad_widgets::*;
use matrix_sdk::{Room, room::RoomMember, ruma::{OwnedRoomId, OwnedUserId, UserId}};
use crate::moderation::{MemberAbilities, MemberAction, MemberModeration};
use crate::moderation_modal::{ModerationModalAction, ModerationRequest};
use crate::profile::user_profile::UserProfile;
use crate::shared::{confirmation_modal::ConfirmationModalContent, popup_list::{PopupKind, enqueue_popup_notification}};
use crate::utils::RoomNameId;

/// How long the result popup of a removal stays up.
const RESULT_POPUP_SECONDS: f64 = 5.0;

/// A member was taken out of a room by this account; its row goes at once.
#[derive(Clone, Debug)]
pub(crate) struct MemberLeft {
    pub room_id: OwnedRoomId,
    pub user_id: OwnedUserId,
}

/// The members the signed-in account may remove (kick) from `room`.
pub(super) async fn removable(room: &Room, members: &[RoomMember]) -> HashSet<OwnedUserId> {
    let Ok(power_levels) = room.power_levels().await else { return HashSet::new() };
    let message = crate::moderation::get_message_event_type(room);
    members.iter().filter(|member| MemberAbilities::from_power_levels(
        &power_levels, room.own_user_id(), member.user_id(), Some(member.membership()), message.clone(),
    ).permits(MemberAction::Kick)).map(|member| member.user_id().to_owned()).collect()
}

/// What removing `member` does for this account, if it may remove them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Removal {
    /// The owner's own Agent: leave its room binding.
    OwnAgent { agent_id: String },
    /// Anyone this account may kick.
    Kick,
}

/// How `member` can be removed, if at all: an own Agent always by its
/// owner, anyone else only with the power to kick.
pub(super) fn removal(own_agent_id: Option<String>, removable: &HashSet<OwnedUserId>, member: &UserId) -> Option<Removal> {
    match own_agent_id {
        Some(agent_id) => Some(Removal::OwnAgent { agent_id }),
        None => removable.contains(member).then_some(Removal::Kick),
    }
}

/// Shows the row's "⋯" for a removable member, and its Remove while open.
pub(super) fn show(cx: &mut Cx, row: &WidgetRef, can_remove: bool, menu_open: bool) {
    row.widget(cx, ids!(member_more)).set_visible(cx, can_remove);
    row.view(cx, ids!(member_menu)).set_visible(cx, can_remove && menu_open);
}

/// Whether the row's "⋯" was clicked.
pub(super) fn more_clicked(cx: &mut Cx, row: &WidgetRef, actions: &Actions) -> bool {
    use crate::shared::navigation_bar_button::NavigationBarButtonWidgetRefExt;
    row.navigation_bar_button(cx, ids!(member_more)).clicked(actions)
}

/// Asks to confirm the removal, if the row's Remove was clicked.
pub(super) fn clicked(cx: &mut Cx, row: &WidgetRef, actions: &Actions, room: &RoomNameId,
    removal: Option<Removal>, profile: &UserProfile) -> bool {
    let Some(removal) = removal.filter(|_| row.button(cx, ids!(remove_member)).clicked(actions)) else { return false };
    match removal {
        Removal::Kick => cx.action(ModerationModalAction::Open(kick_request(room, profile))),
        Removal::OwnAgent { agent_id } => cx.action(crate::app::ConfirmDeleteAction::Show(RefCell::new(Some(
            confirm_unbind(room, profile, agent_id))))),
    }
    true
}

fn kick_request(room: &RoomNameId, profile: &UserProfile) -> ModerationRequest {
    ModerationRequest::Member(MemberModeration {
        room_id: room.room_id().clone(), room_name: room.to_string(), user_id: profile.user_id.clone(),
        user_name: profile.displayable_name().to_owned(), action: MemberAction::Kick,
    })
}

fn confirm_unbind(room: &RoomNameId, profile: &UserProfile, agent_id: String) -> ConfirmationModalContent {
    let name = profile.displayable_name().to_owned();
    let room_id = room.room_id().clone();
    let user_id = profile.user_id.clone();
    let args = [("agent", name.clone()), ("room", room.to_string())];
    ConfirmationModalContent {
        title_text: crate::i18n::format("Remove {agent} from this room?", &args[..1]).into(),
        body_text: crate::i18n::format("{agent} will leave \"{room}\". You can add it again with Add agent.", &args).into(),
        accept_button_text: Some(crate::i18n::tr("Remove").into()),
        cancel_button_text: None,
        on_accept_clicked: Some(Box::new(move |_cx| {
            crate::sliding_sync::spawn_async_task(async move {
                let result = leave_binding(agent_id, room_id.clone()).await;
                report_unbind(&name, result, MemberLeft { room_id, user_id });
            });
        })),
        on_cancel_clicked: None,
    }
}

/// Leaves the Agent's binding for `room_id`, found in the Agent's status.
async fn leave_binding(agent_id: String, room_id: OwnedRoomId) -> Result<(), String> {
    use crate::hagency::model::{BindingAction, Command};
    let account = crate::sliding_sync::current_user_id().map(|id| id.to_string()).unwrap_or_default();
    let status = crate::host::hagency::local::Call::Status(agent_id.clone()).execute(&account).await?;
    let rooms = crate::hagency::agent_panel::rooms::joined_rooms(&status);
    let binding_id = crate::hagency::agent_panel::rooms::open_room_binding(&rooms, Some(room_id.as_str()))
        .ok_or_else(|| "agent_not_in_room".to_owned())?;
    let service = crate::hagency::backend::Service::for_current()?;
    service.execute(Command::Binding { agent_id, binding_id, action: BindingAction::Leave }).await.map(drop)
}

/// The visible result of an own-Agent removal; a success also drops the row.
fn report_unbind(name: &str, result: Result<(), String>, left: MemberLeft) {
    match result {
        Ok(()) => {
            enqueue_popup_notification(crate::i18n::format("Removed {agent} from this room.", &[("agent", name.to_owned())]),
                PopupKind::Success, Some(RESULT_POPUP_SECONDS));
            Cx::post_action(left);
        }
        Err(error) => enqueue_popup_notification(
            crate::i18n::format("Could not remove {agent}: {error}", &[("agent", name.to_owned()), ("error", error)]),
            PopupKind::Error, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str) -> OwnedUserId { OwnedUserId::try_from(id).unwrap() }

    /// Live finding: the Members list had no way to take a member out.
    #[test]
    fn remove_asks_to_kick_that_member_from_this_room() {
        let room = RoomNameId::new(matrix_sdk::RoomDisplayName::Named("Ops".into()), "!ops:s".try_into().unwrap());
        let profile = UserProfile { user_id: user("@bot:s"), username: Some("Bot".into()), avatar_state: crate::shared::avatar::AvatarState::Known(None) };
        let ModerationRequest::Member(m) = kick_request(&room, &profile) else { panic!("a member request") };
        assert_eq!((m.room_id.as_str(), m.user_id.as_str(), m.user_name.as_str()), ("!ops:s", "@bot:s", "Bot"));
        assert!(matches!(m.action, MemberAction::Kick));
    }

    /// Chris: removing his own Agent must unbind it, even without kick power.
    #[test]
    fn an_own_agent_is_unbound_and_others_need_kick_power() {
        let kickable: HashSet<_> = [user("@bob:s")].into();
        assert_eq!(removal(Some("agt_1".into()), &HashSet::new(), &user("@_hagency_agt_1:s")), Some(Removal::OwnAgent { agent_id: "agt_1".into() }));
        assert_eq!(removal(None, &kickable, &user("@bob:s")), Some(Removal::Kick));
        assert_eq!(removal(None, &kickable, &user("@carol:s")), None, "no power, no Remove");
    }

    /// R5: a failed removal is visible, and only a success drops the row.
    #[test]
    fn an_unbind_shows_its_result_and_only_success_drops_the_row() {
        let left = || MemberLeft { room_id: "!r:s".try_into().unwrap(), user_id: user("@_hagency_agt_1:s") };
        let confirm = confirm_unbind(&RoomNameId::empty("!r:s".try_into().unwrap()), &UserProfile {
            user_id: user("@_hagency_agt_1:s"), username: Some("yue-claude".into()), avatar_state: crate::shared::avatar::AvatarState::Known(None),
        }, "agt_1".into());
        assert!(confirm.title_text.contains("yue-claude") && confirm.on_accept_clicked.is_some());
        report_unbind("yue-claude", Err("agent_not_in_room".into()), left());
        report_unbind("yue-claude", Ok(()), left());
    }

    #[test]
    fn the_member_row_hides_remove_behind_its_more_button() {
        let src = include_str!("../mobile_chat_info.rs");
        assert!(src.contains("member_more := NavigationBarButton {visible: false"), "the row's ⋯ starts hidden");
        assert!(src.contains("member_menu := View {visible: false"), "its Remove menu starts closed");
    }
}
