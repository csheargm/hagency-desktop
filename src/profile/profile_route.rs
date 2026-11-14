//! Where a click on a person or an Agent shows their profile.
//!
//! On a desktop window that docks the right-hand workspace region, a
//! profile opens there as a "Profile" tab, so the chat stays readable beside
//! it. Without that region (phones, embedded panes) the room's own sliding
//! pane keeps showing it, so a click is never discarded.

use std::sync::atomic::{AtomicBool, Ordering};
use makepad_widgets::Cx;
use matrix_sdk::ruma::UserId;
use super::user_profile::UserProfilePaneInfo;

/// How a profile is shown in the right-hand region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileRoute {
    /// A Hagency Agent (reserved puppet ID): a read-only profile.
    Agent,
    /// A human: their profile with the usual actions.
    Person,
}

/// Picks the route for `user`.
pub fn route_for(user: &UserId) -> ProfileRoute {
    if is_agent(user) { ProfileRoute::Agent } else { ProfileRoute::Person }
}

/// Whether `user` is a Hagency Agent, by its reserved puppet ID.
pub fn is_agent(user: &UserId) -> bool {
    crate::hagency::backend::is_agent_invitation(user)
}

/// Asks the app to show `info` in the right-hand region's profile tab.
#[derive(Clone, Debug)]
pub struct ShowProfileInSide(pub UserProfilePaneInfo);

/// Whether the app docks a profile tab in the right-hand region. False until
/// the app says so at startup, so tests and embeds keep the sliding pane.
static SIDE_HOST: AtomicBool = AtomicBool::new(false);

/// Set once by the app at startup.
pub fn set_side_host(available: bool) {
    SIDE_HOST.store(available, Ordering::Release);
}

/// Whether a click may use the right-hand profile tab: only on desktop, and
/// only once the app has said it docks one.
pub fn shows_in_side(is_desktop: bool, side_host: bool) -> bool {
    is_desktop && side_host
}

/// Asks for `info` in the right-hand region. Returns `false` when there is
/// none (mobile, or no side host): the caller then shows its own pane.
pub fn open_in_side(cx: &mut Cx, info: UserProfilePaneInfo) -> bool {
    let desktop = crate::home::home_screen::effective_is_desktop(cx);
    if !shows_in_side(desktop, SIDE_HOST.load(Ordering::Acquire)) {
        return false;
    }
    cx.action(ShowProfileInSide(info));
    true
}

/// The detail line a profile shows under the Matrix ID: the user's server.
/// An Agent's line says so, since its server hosts the Agent, not a person.
pub fn detail_line(user: &UserId, route: ProfileRoute) -> String {
    let server = user.server_name().to_string();
    match route {
        ProfileRoute::Person => crate::i18n::format("Server: {0}", &[("0", server)]),
        ProfileRoute::Agent => crate::i18n::format("Agent on {0}", &[("0", server)]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::user_id;

    #[test]
    fn agents_route_to_a_read_only_profile_and_people_to_a_full_one() {
        assert_eq!(route_for(user_id!("@_hagency_agt_1:s")), ProfileRoute::Agent);
        assert_eq!(route_for(user_id!("@ben2:s")), ProfileRoute::Person);
        assert_eq!(route_for(user_id!("@hagency_agt_1:s")), ProfileRoute::Person, "only the reserved prefix");
    }

    #[test]
    fn a_click_without_a_side_host_keeps_the_callers_pane() {
        assert!(!shows_in_side(true, false), "no dock: the sliding pane must show it");
        assert!(!shows_in_side(false, true), "mobile keeps its own pane");
        assert!(shows_in_side(true, true));
    }

    #[test]
    fn details_name_the_server() {
        assert_eq!(detail_line(user_id!("@ben2:example.org"), ProfileRoute::Person), "Server: example.org");
        assert_eq!(detail_line(user_id!("@_hagency_agt_9:example.org"), ProfileRoute::Agent), "Agent on example.org");
    }
}
