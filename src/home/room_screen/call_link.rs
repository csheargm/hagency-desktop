//! The room header's call button: builds the Element Call link for a room.
//!
//! The server's well-known `org.matrix.msc4143.rtc_foci` is not read yet
//! (the desktop has no well-known client today), so the base is env-configured.
//!
//! The Element Call app is configured per process with `HAGENCY_CALL_URL`
//! (the demo script sets it to the app for the window's homeserver). Without
//! it, or with a value that is not a safe base URL, the button stays hidden:
//! HTTPS is required, except plain HTTP on a loopback host for local demos,
//! because the opened link carries the room ID and server name.

use makepad_widgets::{Actions, Cx};
use matrix_sdk::ruma::RoomId;

use crate::shared::navigation_bar_button::NavigationBarButtonRef;

/// Environment variable naming the Element Call app's base URL.
pub const CALL_URL_ENV: &str = "HAGENCY_CALL_URL";

/// The configured Element Call base URL, if it is a usable http(s) URL.
pub fn configured_call_base() -> Option<String> {
    call_base_from(std::env::var(CALL_URL_ENV).ok().as_deref())
}

/// Validates a raw `HAGENCY_CALL_URL` value; `None` hides the call button.
/// Accepts `https://` anywhere and `http://` only on loopback hosts, and
/// refuses embedded credentials, queries and fragments.
pub fn call_base_from(raw: Option<&str>) -> Option<String> {
    let parsed = url::Url::parse(raw?.trim()).ok()?;
    let scheme_ok = match parsed.scheme() {
        "https" => true,
        "http" => is_loopback_host(&parsed),
        _ => false,
    };
    let clean = parsed.username().is_empty() && parsed.password().is_none()
        && parsed.query().is_none() && parsed.fragment().is_none();
    (scheme_ok && clean && parsed.host().is_some()).then(|| parsed.as_str().trim_end_matches('/').to_owned())
}

fn is_loopback_host(parsed: &url::Url) -> bool {
    match parsed.host() {
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Element Call's room link: `<base>/room/#?roomId=…&viaServers=…`, the
/// same shape Element Call builds for its own share links.
pub fn element_call_room_url(base: &str, room_id: &RoomId) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("roomId", room_id.as_str());
    if let Some(server) = room_id.server_name() {
        query.append_pair("viaServers", server.as_str());
    }
    format!("{}/room/#?{}", base.trim_end_matches('/'), query.finish())
}

/// The link the call button opens for this room, or `None` to hide it.
pub fn room_call_url(room_id: Option<&RoomId>) -> Option<String> {
    Some(element_call_room_url(&configured_call_base()?, room_id?))
}

/// Whether the call button is shown: only with a room and a usable base URL.
pub fn call_button_visible(room_id: Option<&RoomId>) -> bool {
    room_call_url(room_id).is_some()
}

/// Shows or hides the header's call `button` for `room_id`.
pub fn sync_call_button(cx: &mut Cx, button: &NavigationBarButtonRef, room_id: Option<&RoomId>) {
    button.set_visible(cx, call_button_visible(room_id));
}

/// Opens the room's Element Call link when the call button was clicked;
/// returns whether it was.
pub fn handle_call_button(button: &NavigationBarButtonRef, actions: &Actions, room_id: Option<&RoomId>) -> bool {
    if !button.clicked(actions) {
        return false;
    }
    if let Some(url) = room_call_url(room_id) { crate::utils::open_url(&url); }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_blank_or_non_http_base_hides_the_button() {
        assert_eq!(call_base_from(None), None);
        assert_eq!(call_base_from(Some("  ")), None);
        assert_eq!(call_base_from(Some("not a url")), None);
        assert_eq!(call_base_from(Some("file:///tmp/call")), None);
    }

    #[test]
    fn http_base_is_kept_without_trailing_slash() {
        assert_eq!(call_base_from(Some(" http://127.0.0.1:8093/ ")).as_deref(), Some("http://127.0.0.1:8093"));
    }

    #[test]
    fn plain_http_is_refused_off_loopback() {
        assert_eq!(call_base_from(Some("http://call.example.org")), None);
        assert_eq!(call_base_from(Some("http://10.0.0.5:8093")), None);
        assert_eq!(call_base_from(Some("http://localhost:8093/")).as_deref(), Some("http://localhost:8093"));
        assert_eq!(call_base_from(Some("http://[::1]:8093")).as_deref(), Some("http://[::1]:8093"));
    }

    #[test]
    fn https_is_accepted_but_credentials_query_and_fragment_are_not() {
        assert_eq!(call_base_from(Some("https://call.example.org/app/")).as_deref(), Some("https://call.example.org/app"));
        assert_eq!(call_base_from(Some("https://user:pw@call.example.org")), None);
        assert_eq!(call_base_from(Some("https://call.example.org/?x=1")), None);
        assert_eq!(call_base_from(Some("https://call.example.org/#frag")), None);
    }

    #[test]
    fn room_url_encodes_room_id_and_via_server() {
        let room = RoomId::parse("!abc:localhost:8452").unwrap();
        assert_eq!(
            element_call_room_url("http://127.0.0.1:8094/", &room),
            "http://127.0.0.1:8094/room/#?roomId=%21abc%3Alocalhost%3A8452&viaServers=localhost%3A8452"
        );
    }

    #[test]
    fn no_room_means_no_link() {
        assert_eq!(room_call_url(None), None);
        assert!(!call_button_visible(None));
    }

    #[test]
    fn button_is_shown_only_with_a_room_and_a_configured_base() {
        // Only this test touches the variable, so the set/remove pair cannot race.
        let room = RoomId::parse("!abc:example.org").unwrap();
        unsafe { std::env::remove_var(CALL_URL_ENV) };
        assert!(!call_button_visible(Some(&room)));
        unsafe { std::env::set_var(CALL_URL_ENV, "https://call.example.org") };
        assert!(call_button_visible(Some(&room)));
        assert_eq!(room_call_url(Some(&room)).as_deref(),
            Some("https://call.example.org/room/#?roomId=%21abc%3Aexample.org&viaServers=example.org"));
        unsafe { std::env::remove_var(CALL_URL_ENV) };
    }
}
