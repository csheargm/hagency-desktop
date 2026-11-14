//! Fills the desktop profile view (`UserProfileView`) from a profile, and the
//! profile actions both of its hosts share: the room's sliding pane and the
//! right-hand region's profile tab.

use std::borrow::Cow;
use makepad_widgets::*;
use crate::{
    avatar_cache,
    block_user_modal::BlockUserRequest,
    shared::{avatar::{AvatarState, AvatarWidgetRefExt}, popup_list::{enqueue_popup_notification, PopupKind}},
    sliding_sync::{current_user_id, is_user_blocked},
    utils,
};
use super::{
    member_summary::{self, MemberState},
    user_profile_cache,
    user_profile::UserProfilePaneInfo,
    user_verification_cache::{get_or_fetch_user_verification_state, UserVerificationState},
};

/// How long the "copied" notice stays, in seconds.
const COPIED_NOTICE_SECS: f64 = 3.0;

/// Whether `info` shows the signed-in account itself.
pub fn is_current_account(info: &UserProfilePaneInfo) -> bool {
    info.room_member.as_ref()
        .map(|rm| rm.is_account_user())
        .unwrap_or_else(|| current_user_id().is_some_and(|uid| uid == info.user_id))
}

/// The Message action's label: your own profile opens File Transfer instead.
pub fn message_label(own: bool) -> &'static str {
    crate::i18n::tr(if own { "File Transfer" } else { "Messages" })
}

/// The "Membership in …" title for the room `info` was opened from.
pub fn membership_title(info: &UserProfilePaneInfo) -> String {
    if info.room_name.is_empty() {
        crate::i18n::format("Membership in Room {0}", &[("0", info.room_id.to_string())])
    } else {
        crate::i18n::format("Membership in {0}", &[("0", info.room_name.clone())])
    }
}

/// The (membership, role) lines for `info`, from its member or the cache.
pub fn standing(cx: &mut Cx, info: &UserProfilePaneInfo) -> (Cow<'static, str>, Cow<'static, str>) {
    let state = match info.room_member.clone() {
        Some(member) => MemberState::Member(member),
        None => member_summary::lookup(cx, &info.user_id, &info.room_id, false),
    };
    member_summary::standing_lines(&state)
}

/// Sets the name, Matrix ID, avatar, membership, verification and buttons of
/// the desktop profile `view`.
pub fn fill_profile_view(cx: &mut Cx, view: &WidgetRef, info: &UserProfilePaneInfo) {
    view.label(cx, ids!(user_name)).set_text(cx, info.displayable_name());
    view.label(cx, ids!(user_id)).set_text(cx, info.user_id.as_str());
    let avatar_ref = view.avatar(cx, ids!(avatar));
    info.avatar_state
        .image()
        .and_then(|image| avatar_ref.show_image(cx, None, |cx, img| utils::load_avatar_image(&img, cx, image)).ok())
        .unwrap_or_else(|| avatar_ref.show_user_text(cx, &info.user_id, info.displayable_name()));
    view.label(cx, ids!(membership_title_label)).set_text(cx, &membership_title(info));
    let (membership, role) = standing(cx, info);
    view.label(cx, ids!(membership_status_label)).set_text(cx, &membership);
    view.label(cx, ids!(role_info_label)).set_text(cx, &role);

    let own = is_current_account(info);
    let message = view.button(cx, ids!(direct_message_button));
    message.set_visible(cx, true);
    message.set_text(cx, message_label(own));

    let verification = get_or_fetch_user_verification_state(cx, &info.user_id);
    let verified = verification == Some(UserVerificationState::Verified);
    let reset = verification == Some(UserVerificationState::VerificationViolation);
    view.view(cx, ids!(verified_badge)).set_visible(cx, verified);
    view.view(cx, ids!(identity_reset_badge)).set_visible(cx, reset);
    view.button(cx, ids!(withdraw_verification_button)).set_visible(cx, reset);
    view.button(cx, ids!(verify_this_user_button)).set_visible(cx, !own && !verified);

    // The Matrix SDK's RoomMember does not track blocking, so it is kept separately.
    let block = view.button(cx, ids!(block_user_button));
    block.set_visible(cx, !own);
    block.set_text(cx, crate::i18n::tr(if is_user_blocked(&info.user_id) { "Unblock User" } else { "Block User" }));
}

/// Copies the user's matrix.to link and says so.
pub fn copy_user_link(cx: &mut Cx, info: &UserProfilePaneInfo) {
    cx.copy_to_clipboard(&info.user_id.matrix_to_uri().to_string());
    enqueue_popup_notification(crate::i18n::tr("Copied User ID to the clipboard."), PopupKind::Success, Some(COPIED_NOTICE_SECS));
}

/// The Block (or Unblock) request for the user `info` shows.
pub fn block_request(info: &UserProfilePaneInfo) -> BlockUserRequest {
    BlockUserRequest {
        user_id: info.user_id.clone(),
        display_name: info.username.clone(),
        block: !is_user_blocked(&info.user_id),
        reject_invite_to: None,
    }
}

/// Fills `info`'s room member (and the avatar and name it brings) from the
/// cache, asking the server for it when it is missing.
pub fn fill_member_from_cache(cx: &mut Cx, info: &mut UserProfilePaneInfo) {
    let room_id = info.room_id.clone();
    let Some((new_profile, Some(room_member))) = user_profile_cache::with_user_profile(
        cx, info.user_id.clone(), Some(&room_id), true,
        |profile, rooms| (profile.clone(), rooms.get(&room_id).and_then(|entry| entry.loaded()).cloned()),
    ) else { return };
    // Prefer the room member's avatar, which is specific to this room.
    if let Some(uri) = room_member.avatar_url() {
        info.avatar_state = AvatarState::Known(Some(uri.to_owned()));
    } else if let s @ (AvatarState::Known(Some(_)) | AvatarState::Loaded(_)) = new_profile.avatar_state {
        info.avatar_state = s;
    }
    if info.username.is_none() {
        info.username = room_member.display_name().map(|dn| dn.to_owned()).or(new_profile.username);
    }
    info.room_member = Some(room_member);
}

/// Re-reads `info` from the caches after a background update (a UI Signal).
/// Returns whether anything was found, so the caller redraws.
pub fn refresh_info(cx: &mut Cx, info: &mut UserProfilePaneInfo) -> bool {
    user_profile_cache::process_user_profile_updates(cx);
    avatar_cache::process_avatar_updates(cx);
    let room_id = info.room_id.clone();
    let Some((new_profile, room_member)) = user_profile_cache::with_user_profile(
        cx, info.user_id.clone(), Some(&room_id), false,
        |profile, rooms| (profile.clone(), rooms.get(&room_id).and_then(|entry| entry.loaded()).cloned()),
    ) else { return false };
    let prev_avatar_state = info.avatar_state.clone();
    info.user_profile = new_profile;
    info.room_member = room_member;
    // The room member's avatar is the most up-to-date one for this room.
    if let Some(avatar_uri) = info.room_member.as_ref().and_then(|rm| rm.avatar_url().map(|u| u.to_owned())) {
        info.avatar_state = AvatarState::Known(Some(avatar_uri));
    }
    info.avatar_state.update_from_cache(cx);
    // Keep a fully loaded avatar rather than fall back to a not-yet-loaded one.
    if let (prev @ AvatarState::Loaded(_), current) = (prev_avatar_state, &mut info.avatar_state) {
        if !matches!(current, AvatarState::Loaded(_)) { *current = prev; }
    }
    true
}
