//! Mobile navigation surfaces. Profiles and contacts come from the active Matrix
//! account; the presentation never substitutes fixture data for server results.

use makepad_widgets::*;
use matrix_sdk::ruma::{api::client::profile::{AvatarUrl, DisplayName}, OwnedUserId, UserId};

use crate::{
    home::navigation_tab_bar::{get_own_profile, NavigationBarAction, SelectedTab},
    app::SelectedRoom,
    home::rooms_list::RoomsListAction,
    logout::logout_confirm_modal::LogoutAction,
    profile::user_profile::UserProfile,
    shared::{avatar::{AvatarState, AvatarWidgetRefExt}, navigation_bar_button::{NavigationBarButtonWidgetExt, NavigationBarButtonWidgetRefExt}},
    sliding_sync::{current_user_id, get_client, spawn_async_task, submit_async_request, MatrixRequest},
    utils,
};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.MOBILE_BG = mod.widgets.RINX_PAGE
    mod.widgets.MOBILE_GREEN = mod.widgets.RINX_ACCENT
    mod.widgets.MOBILE_INK = mod.widgets.RINX_INK
    mod.widgets.MOBILE_MUTED = mod.widgets.RINX_MUTED

    mod.widgets.MobileAvatar = Avatar {
        width: 48 height: 48
        text_view +: {draw_bg +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, 5.0)
                sdf.fill(self.color)
                return sdf.result
            }
        }
        }
        img_view +: {img +: {draw_bg +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, 5.0)
                sdf.fill(self.get_color())
                return sdf.result
            }
        }}}
    }

    mod.widgets.MobileTitle = SolidView {
        width: Fill height: 48
        flow: Overlay
        draw_bg.color: mod.widgets.MOBILE_BG
        title := Label {
            width: Fill height: Fill
            align: Align{x: 0.5 y: 0.5}
            draw_text +: {color: mod.widgets.MOBILE_INK text_style: theme.font_bold {font_size: (12.5 * mod.widgets.RINX_TEXT_SCALE)}}
        }
        controls := View {
            width: Fill height: Fill
            flow: Right align: Align{y: 0.5}
            left := RobrixNeutralIconButton {
                visible: false width: 48 height: 48 padding: 14
                align: Align{x: 0.5 y: 0.5} spacing: 0
                draw_bg +: {color: #x00000000 color_hover: #x00000000 color_down: #x00000000}
                draw_icon +: {svg: ICON_CHEVRON_LEFT color: mod.widgets.MOBILE_INK}
                icon_walk: Walk{width: 8 height: 14}
            }
            View {width: Fill height: 1}
            right := RobrixNeutralIconButton {
                visible: false width: 48 height: 48 padding: 14
                draw_bg +: {color: #x00000000 color_hover: #x00000000 color_down: #x00000000}
                draw_icon +: {svg: ICON_ADD color: mod.widgets.MOBILE_INK}
                icon_walk: Walk{width: 20 height: 20}
            }
        }
    }

    mod.widgets.MobileRow = NavigationBarButton {
        width: Fill height: 56
        flow: Right spacing: 16 padding: Inset{left: 20 right: 16}
        align: Align{y: 0.5}
        draw_bg +: {color_hover: mod.widgets.RINX_HOVER color_active: mod.widgets.RINX_HOVER border_radius: 0}
        icon := Icon {
            icon_walk: Walk{width: 24 height: 24}
            draw_icon +: {color: mod.widgets.RINX_ACCENT svg: ICON_PEOPLE}
        }
        title := Label {
            width: Fill height: Fit max_lines: 1 text_overflow: Ellipsis
            draw_text +: {color: mod.widgets.MOBILE_INK text_style: theme.font_regular {font_size: (12.5 * mod.widgets.RINX_TEXT_SCALE)}}
        }
        chevron := Icon {
            icon_walk: Walk{width: 8 height: 13}
            draw_icon +: {color: mod.widgets.RINX_DISABLED svg: ICON_CHEVRON_RIGHT}
        }
    }
    mod.widgets.MobileSection = SolidView {
        width: Fill height: Fit flow: Down
        draw_bg.color: mod.widgets.RINX_SURFACE
    }
    mod.widgets.MobileDivider = SolidView {
        width: Fill height: 0.5 margin: Inset{left: 60}
        draw_bg.color: mod.widgets.RINX_BORDER
    }

    mod.widgets.MobileHub = #(MobileHub::register_widget(vm)) {
        ..mod.widgets.SolidView
        width: Fill height: Fill flow: Down show_bg: true draw_bg.color: mod.widgets.MOBILE_BG
        title_bar := mod.widgets.MobileTitle {}
        pages := PageFlip {
            width: Fill height: Fill lazy_init: true active_page: @contacts
            contacts := View {
                width: Fill height: Fill flow: Down
                search_area := SolidView {
                    width: Fill height: 48 padding: Inset{left: 10 right: 10 bottom: 10}
                    draw_bg.color: mod.widgets.MOBILE_BG
                    search := RobrixTextInput {
                        width: Fill height: 36 padding: Inset{left: 12 right: 12 top: 8 bottom: 8}
                        empty_text: #(crate::i18n::tr("Search")) i18n_empty_text: "Search" autocapitalize: None
                        draw_text +: {color: mod.widgets.MOBILE_INK text_style: theme.font_regular {font_size: (11.5 * mod.widgets.RINX_TEXT_SCALE)}}
                        draw_bg +: {
                            color: mod.widgets.RINX_FIELD color_hover: mod.widgets.RINX_FIELD color_focus: mod.widgets.RINX_FIELD
                            color_empty: mod.widgets.RINX_FIELD border_color: mod.widgets.RINX_BORDER border_color_focus: mod.widgets.RINX_ACCENT border_size: 1 border_radius: 6
                        }
                    }
                }
                shortcuts := mod.widgets.MobileSection {
                    new_friends := mod.widgets.MobileRow {
                        title.text: #(crate::i18n::tr("New Friends")) title.i18n_text: "New Friends"
                        icon +: {draw_icon +: {svg: ICON_ADD_USER color: mod.widgets.RINX_WARNING_FG}}
                    }
                }
                status := Label {
                    width: Fill height: Fit padding: 16
                    draw_text +: {color: mod.widgets.MOBILE_MUTED text_style: theme.font_regular {font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}
                    text: #(crate::i18n::tr("Loading contacts…")) i18n_text: "Loading contacts…"
                }
                list := PortalList {
                    width: Fill height: Fill
                    Filler := SolidView {width: Fill height: 100 draw_bg.color: mod.widgets.RINX_SURFACE}
                    Contact := View {
                        width: Fill height: Fit flow: Down
                        section := SolidView {
                            width: Fill height: 28 padding: Inset{left: 16} align: Align{y: 0.5}
                            draw_bg.color: mod.widgets.MOBILE_BG
                            letter := Label {draw_text +: {color: mod.widgets.MOBILE_MUTED text_style: theme.font_regular {font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}}
                        }
                        row := NavigationBarButton {
                            width: Fill height: 60 flow: Right spacing: 14 padding: Inset{left: 16 right: 16}
                            align: Align{y: 0.5}
                            draw_bg +: {
                                color_hover: mod.widgets.RINX_BORDER border_radius: 0
                                get_color: fn() -> vec4 {return mod.widgets.RINX_ON_ACCENT.mix(self.color_hover, self.hover)}
                            }
                            avatar := mod.widgets.MobileAvatar {width: 40 height: 40}
                            View {width: Fill height: Fit flow: Down spacing: 3
                                name := Label {
                                    width: Fill padding: 0 max_lines: 1 text_overflow: Ellipsis
                                    draw_text +: {color: mod.widgets.MOBILE_INK text_style: theme.font_regular {font_size: (12.5 * mod.widgets.RINX_TEXT_SCALE)}}
                                }
                                user_id := Label {
                                    width: Fill padding: 0 max_lines: 1 text_overflow: Ellipsis
                                    draw_text +: {color: mod.widgets.MOBILE_MUTED text_style: theme.font_regular {font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
                                }
                            }
                        }
                        mod.widgets.MobileDivider {margin: Inset{left: 70}}
                    }
                }
            }
            discover := ScrollYView {
                width: Fill height: Fill flow: Down spacing: 8
                mod.widgets.MobileSection {
                    discover_mini_apps := mod.widgets.MobileRow {title.text: "Mini apps" icon.draw_icon.svg: ICON_SQUARES}
                    mod.widgets.MobileDivider {}
                    discover_article := mod.widgets.MobileRow {title.text: #(crate::i18n::tr("Article editor")) title.i18n_text: "Article editor" icon.draw_icon.svg: ICON_FILE}
                    mod.widgets.MobileDivider {}
                    discover_moments := mod.widgets.MobileRow {title.text: #(crate::i18n::tr("Moments")) title.i18n_text: "Moments" icon.draw_icon.svg: ICON_GLOBE}
                    mod.widgets.MobileDivider {}
                    // Mobile has no rail: File Transfer sits with the rail's other tools here.
                    discover_file_transfer := mod.widgets.MobileRow {title.text: #(crate::i18n::tr("File Transfer")) title.i18n_text: "File Transfer" icon.draw_icon.svg: ICON_FILE}
                    mod.widgets.MobileDivider {}
                    explore := mod.widgets.MobileRow {title.text: #(crate::i18n::tr("Explore Groups & Spaces")) title.i18n_text: "Explore Groups & Spaces" icon.draw_icon.svg: ICON_GLOBE}
                }
            }
            joined_groups := View {
                width: Fill height: Fill flow: Down
                group_status := Label {
                    width: Fill height: Fit padding: 16
                    draw_text +: {color: mod.widgets.MOBILE_MUTED text_style: theme.font_regular {font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}
                }
                group_list := PortalList {
                    width: Fill height: Fill
                    Filler := View {width: Fill height: 100}
                    Group := mod.widgets.MobileSection {
                        group_row := NavigationBarButton {
                            width: Fill height: 72 flow: Right spacing: 14 padding: Inset{left: 16 right: 16}
                            align: Align{y: 0.5}
                            draw_bg +: {color_hover: mod.widgets.RINX_BORDER border_radius: 0}
                            avatar := mod.widgets.MobileAvatar {width: 48 height: 48}
                            name := Label {
                                width: Fill max_lines: 1 text_overflow: Ellipsis
                                draw_text +: {color: mod.widgets.MOBILE_INK text_style: theme.font_regular {font_size: (12.5 * mod.widgets.RINX_TEXT_SCALE)}}
                            }
                        }
                        mod.widgets.MobileDivider {margin: Inset{left: 78}}
                    }
                }
            }
            account := ScrollYView {
                width: Fill height: Fill flow: Down spacing: 8
                mod.widgets.MobileSection {
                    own_profile := NavigationBarButton {
                        width: Fill height: 128 flow: Right spacing: 20 padding: Inset{left: 24 right: 20}
                        align: Align{y: 0.5}
                        avatar := mod.widgets.MobileAvatar {width: 64 height: 64}
                        View {
                            width: Fill height: Fit flow: Down spacing: 12
                            name := Label {
                                width: Fill max_lines: 1 text_overflow: Ellipsis
                                draw_text +: {color: mod.widgets.MOBILE_INK text_style: theme.font_bold {font_size: (17 * mod.widgets.RINX_TEXT_SCALE)}}
                            }
                            user_id := Label {
                                width: Fill max_lines: 1 text_overflow: Ellipsis
                                draw_text +: {color: mod.widgets.MOBILE_MUTED text_style: theme.font_regular {font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}
                            }
                        }
                        Icon {icon_walk: Walk{width: 8 height: 13} draw_icon +: {color: mod.widgets.RINX_DISABLED svg: ICON_CHEVRON_RIGHT}}
                    }
                }
                mod.widgets.MobileSection {
                    my_posts := mod.widgets.MobileRow {title.text: #(crate::i18n::tr("My Posts")) title.i18n_text: "My Posts" icon.draw_icon.svg: ICON_GLOBE}
                    mod.widgets.MobileDivider {}
                    settings := mod.widgets.MobileRow {title.text: #(crate::i18n::tr("Settings")) title.i18n_text: "Settings" icon.draw_icon.svg: ICON_SETTINGS}
                }
            }
            profile := ScrollYView {
                width: Fill height: Fill flow: Down
                contact_card := DetailContactCard {}
                DetailGap {}
                DetailSection {
                    contact_moments := DetailRow {title.text: #(crate::i18n::tr("Moments")) title.i18n_text: "Moments"}
                    contact_copy := DetailRow {title.text: #(crate::i18n::tr("Copy Profile Link")) title.i18n_text: "Copy Profile Link"}
                }
                DetailGap {}
                DetailSection {message := DetailAction {title.text: #(crate::i18n::tr("Messages")) title.i18n_text: "Messages"}}
                DetailGap {}
                DetailSection {contact_block := DetailAction {title +: {text: #(crate::i18n::tr("Block")) i18n_text: "Block" draw_text.color: mod.widgets.RINX_DANGER_FG}}}
            }
        }
    }
}

#[derive(Debug)]
struct ContactsLoaded {
    owner: OwnedUserId,
    request: u64,
    result: Result<Vec<UserProfile>, String>,
}

#[derive(Clone, Debug)]
struct MobileGroup {
    name: utils::RoomNameId,
    avatar: AvatarState,
}

#[derive(Debug)]
struct GroupsLoaded {
    owner: OwnedUserId,
    request: u64,
    groups: Vec<MobileGroup>,
}

#[derive(Debug)]
pub enum MobileNavigationAction {
    DetailVisibility(bool),
    NewChatRequested,
}

#[derive(Script, ScriptHook, Widget)]
pub struct MobileHub {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    /// 0: Contacts, 1: Discover, 2: Me.
    #[live] kind: u32,
    #[rust] owner: Option<OwnedUserId>,
    #[rust] contacts: Vec<UserProfile>,
    #[rust] visible_contacts: Vec<usize>,
    #[rust] selected_profile: Option<UserProfile>,
    #[rust] adding_friend: bool,
    #[rust] groups_open: bool,
    #[rust] groups: Vec<MobileGroup>,
    #[rust] group_request: u64,
    #[rust] query: String,
    #[rust] request: u64,
    #[rust] search_timer: Timer,
    #[rust] status: String,
}

/// A username is relative to the signed-in account's homeserver; display-name
/// queries still go to the directory, so partial matches never need a full ID.
fn contact_query_id(owner: &UserId, query: &str) -> Option<OwnedUserId> {
    let query = query.trim();
    if let Ok(id) = UserId::parse(query) { return Some(id); }
    let local = query.strip_prefix('@').unwrap_or(query);
    if local.is_empty() || local.len() > 255 || local.contains(':') || local.chars().any(char::is_whitespace) {
        return None;
    }
    UserId::parse(format!("@{local}:{}", owner.server_name())).ok()
}

async fn search_contact_profiles(client: &matrix_sdk::Client, owner: &UserId, query: &str) -> Result<Vec<UserProfile>, String> {
    let query = query.trim();
    let mut profiles = Vec::new();
    if UserId::parse(query).is_err() {
        let results = client.search_users(query, 100).await.map_err(|e| e.to_string())?;
        profiles.extend(results.results.into_iter().map(|u| UserProfile {
            user_id: u.user_id, username: u.display_name, avatar_state: AvatarState::Known(u.avatar_url),
        }));
    }
    // Exact local usernames also find real accounts hidden from the directory.
    if let Some(id) = contact_query_id(owner, query)
        && id != owner && !profiles.iter().any(|p| p.user_id == id) {
        match client.account().fetch_user_profile_of(&id).await {
            Ok(p) => profiles.push(UserProfile {
                user_id: id,
                username: p.get_static::<DisplayName>().ok().flatten(),
                avatar_state: AvatarState::Known(p.get_static::<AvatarUrl>().ok().flatten()),
            }),
            Err(e) if matches!(e.client_api_error_kind(), Some(matrix_sdk::ruma::api::error::ErrorKind::NotFound)) => {},
            Err(e) if profiles.is_empty() => return Err(e.to_string()),
            Err(_) => {},
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    profiles.retain(|p| p.user_id != owner && seen.insert(p.user_id.clone()));
    Ok(profiles)
}

impl MobileHub {
    fn begin_new_chat(&mut self, cx: &mut Cx) {
        if self.kind != 0 { return; }
        self.groups_open = false;
        self.selected_profile = None;
        if !self.adding_friend {
            self.request = Self::next_request();
            self.adding_friend = true;
            self.query.clear();
            self.contacts.clear();
            self.status = crate::i18n::tr("Type a username or display name to find people.").into();
            self.view.text_input(cx, ids!(search)).set_text(cx, "");
        }
        self.view.text_input(cx, ids!(search)).set_key_focus(cx);
        cx.action(MobileNavigationAction::DetailVisibility(true));
        self.view.redraw(cx);
    }

    fn clear_account_state(&mut self, cx: &mut Cx) {
        cx.stop_timer(self.search_timer);
        self.search_timer = Timer::empty();
        self.owner = None;
        self.contacts.clear();
        self.visible_contacts.clear();
        self.selected_profile = None;
        self.adding_friend = false;
        self.groups_open = false;
        self.groups.clear();
        self.group_request = 0;
        self.query.clear();
        self.request = 0;
        self.status.clear();
        self.view.text_input(cx, ids!(search)).set_text(cx, "");
        cx.action(MobileNavigationAction::DetailVisibility(false));
    }

    fn next_request() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    fn refresh_groups(&mut self, cx: &mut Cx) {
        let Some(client) = get_client() else { return };
        let Some(owner) = client.user_id().map(ToOwned::to_owned) else { return };
        self.request = 0;
        self.group_request = Self::next_request();
        let request = self.group_request;
        self.status = crate::i18n::tr("Loading group chats…").into();
        self.view.redraw(cx);
        spawn_async_task(async move {
            let mut groups = Vec::new();
            for room in client.joined_rooms() {
                if crate::moments::is_moments(&room) {continue;}
                if !room.is_space() && !room.is_direct().await.unwrap_or(false) {
                    groups.push(MobileGroup {
                        name: utils::RoomNameId::from_room(&room).await,
                        avatar: AvatarState::Known(room.avatar_url()),
                    });
                }
            }
            groups.sort_by_key(|group| (group.name.display().to_lowercase(), group.name.room_id().clone()));
            Cx::post_action(GroupsLoaded {owner, request, groups});
        });
    }

    fn refresh_contacts(&mut self, cx: &mut Cx, search_directory: bool) {
        let Some(client) = get_client() else { return };
        let Some(owner) = client.user_id().map(ToOwned::to_owned) else { return };
        self.owner = Some(owner.clone());
        // A globally unique generation also distinguishes separate widget instances
        // recreated by AdaptiveView and prevents late requests overwriting new ones.
        self.request = Self::next_request();
        let request = self.request;
        let query = self.query.clone();
        self.status = crate::i18n::tr("Loading contacts…").into();
        self.view.redraw(cx);
        spawn_async_task(async move {
            let result = async {
                let mut profiles = Vec::new();
                if search_directory && !query.trim().is_empty() {
                    profiles = tokio::time::timeout(std::time::Duration::from_secs(12),
                        search_contact_profiles(&client, &owner, &query)).await
                        .map_err(|_| "Search timed out. Please try again.".to_owned())??;
                } else {
                    let mut ids = std::collections::BTreeSet::new();
                    for room in client.joined_rooms() {
                if crate::moments::is_moments(&room) {continue;}
                        for target in room.direct_targets() {
                            if let Ok(id) = UserId::parse(target.as_str()) {
                                if id != owner { ids.insert(id); }
                            }
                        }
                    }
                    for id in ids {
                        let p = client.account().fetch_user_profile_of(&id).await;
                        profiles.push(UserProfile {
                            username: p.as_ref().ok().and_then(|p| p.get_static::<DisplayName>().ok().flatten()),
                            avatar_state: AvatarState::Known(p.ok().and_then(|p| p.get_static::<AvatarUrl>().ok().flatten())),
                            user_id: id,
                        });
                    }
                }
                profiles.sort_by_key(|p| (p.displayable_name().to_lowercase(), p.user_id.clone()));
                Ok(profiles)
            }.await;
            Cx::post_action(ContactsLoaded {owner, request, result});
        });
    }

    fn back(&mut self, cx: &mut Cx) {
        cx.stop_timer(self.search_timer);
        self.search_timer = Timer::empty();
        if self.selected_profile.take().is_none() {
            self.adding_friend = false;
            self.groups_open = false;
            self.group_request = 0;
            self.query.clear();
            self.view.text_input(cx, ids!(search)).set_text(cx, "");
            self.refresh_contacts(cx, false);
        }
        cx.action(MobileNavigationAction::DetailVisibility(self.adding_friend));
        self.view.redraw(cx);
    }

    fn populate_profile(cx: &mut Cx, widget: &WidgetRef, profile: &UserProfile) {
        widget.label(cx, ids!(name)).set_text(cx, profile.displayable_name());
        widget.label(cx, ids!(user_id)).set_text(cx, profile.user_id.as_str());
        let avatar = widget.avatar(cx, ids!(avatar));
        let mut avatar_state = profile.avatar_state.clone();
        if let Some(image) = avatar_state.update_from_cache(cx) {
            if avatar.show_image(cx, None, |cx, img| utils::load_avatar_image(&img, cx, image)).is_ok() { return; }
        }
        avatar.show_user_text(cx, &profile.user_id, profile.displayable_name());
    }
}

impl Widget for MobileHub {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.search_timer.is_event(event).is_some() {
            self.search_timer = Timer::empty();
            if self.adding_friend && self.selected_profile.is_none() && !self.query.trim().is_empty()
                && self.owner.as_ref() == current_user_id().as_ref() {
                self.refresh_contacts(cx, true);
            }
        }
        if matches!(event, Event::Signal) {
            crate::profile::user_profile_cache::process_user_profile_updates(cx);
            crate::avatar_cache::process_avatar_updates(cx);
            self.view.redraw(cx);
        }
        if self.kind == 0 && (self.selected_profile.is_some() || self.adding_friend || self.groups_open)
            && scope.data.get::<crate::app::AppState>().is_some_and(|app| app.selected_tab == SelectedTab::Contacts)
            && event.back_pressed() {
            self.back(cx);
            return;
        }
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            for action in actions {
                if let Some(loaded) = action.downcast_ref::<GroupsLoaded>() {
                    if Some(&loaded.owner) == current_user_id().as_ref() && loaded.request == self.group_request {
                        self.groups = loaded.groups.clone();
                        self.status = if self.groups.is_empty() { crate::i18n::tr("No group chats yet.") } else { "" }.into();
                        self.view.redraw(cx);
                    }
                }
                if let Some(loaded) = action.downcast_ref::<ContactsLoaded>() {
                    if Some(&loaded.owner) == current_user_id().as_ref() && loaded.request == self.request {
                        match &loaded.result {
                            Ok(contacts) => {
                                self.contacts = contacts.clone();
                                self.status = if contacts.is_empty() {
                                    if self.query.is_empty() { crate::i18n::tr("No contacts yet. Tap + to find a friend.") } else { crate::i18n::tr("No people found.") }
                                } else { "" }.into();
                            }
                            Err(error) => self.status = crate::i18n::format("Could not load contacts: {error}", &[("error", (error).to_string())]),
                        }
                        self.view.redraw(cx);
                    }
                }
                if let Some(NavigationBarAction::TabSelected(SelectedTab::Contacts)) = action.downcast_ref() {
                    if self.kind == 0 {
                        if self.groups_open { self.refresh_groups(cx); }
                        else if !self.adding_friend { self.refresh_contacts(cx, false); }
                    }
                }
                if matches!(action.downcast_ref(), Some(MobileNavigationAction::NewChatRequested)) {
                    self.begin_new_chat(cx);
                }
                if let Some(LogoutAction::ClearAppState {..}) = action.downcast_ref() {
                    self.clear_account_state(cx);
                }
            }
            let search = self.view.text_input(cx, ids!(search));
            if let Some(query) = search.changed(actions) {
                self.query = query;
                if self.adding_friend {
                    // Invalidate in-flight results immediately, before the debounce fires.
                    self.request = Self::next_request();
                    cx.stop_timer(self.search_timer);
                    self.search_timer = Timer::empty();
                    self.contacts.clear();
                    self.visible_contacts.clear();
                    self.status = crate::i18n::tr(if self.query.trim().is_empty() {
                        "Type a username or display name to find people."
                    } else { "Searching server accounts…" }).into();
                    if !self.query.trim().is_empty() { self.search_timer = cx.start_timeout(0.35); }
                }
                self.view.redraw(cx);
            }
            if search.returned(actions).is_some() && !self.query.trim().is_empty() {
                cx.stop_timer(self.search_timer);
                self.search_timer = Timer::empty();
                self.refresh_contacts(cx, true);
            }
            if self.view.button(cx, ids!(title_bar.controls.right)).clicked(actions)
                || self.view.navigation_bar_button(cx, ids!(new_friends)).clicked(actions)
            {
                self.begin_new_chat(cx);
                if !self.query.trim().is_empty() { self.refresh_contacts(cx, true); }
            }
            let list = self.view.portal_list(cx, ids!(list));
            for (index, widget) in list.items_with_actions(actions) {
                if !list.was_scrolling() && widget.navigation_bar_button(cx, ids!(row)).clicked(actions) {
                    self.selected_profile = self.visible_contacts.get(index).and_then(|i| self.contacts.get(*i)).cloned();
                    cx.action(MobileNavigationAction::DetailVisibility(true));
                    self.view.redraw(cx);
                }
            }
            if self.view.button(cx, ids!(title_bar.controls.left)).clicked(actions) {
                self.back(cx);
            }
            if self.view.navigation_bar_button(cx,ids!(discover_mini_apps)).clicked(actions){cx.action(crate::octoscript_apps::MiniAppsAction::Open);}
            if self.view.navigation_bar_button(cx, ids!(discover_article)).clicked(actions) { crate::system_apps::open_article(cx); }
            if self.view.navigation_bar_button(cx, ids!(discover_moments)).clicked(actions) {
                cx.action(crate::moments::ui::MomentsAction::Open {author: None});
            }
            if self.view.navigation_bar_button(cx, ids!(discover_file_transfer)).clicked(actions) {
                cx.action(crate::moments::ui::MomentsAction::FileTransfer);
            }
            if self.view.navigation_bar_button(cx, ids!(my_posts)).clicked(actions) {
                cx.action(crate::moments::ui::MomentsAction::Open {author: current_user_id()});
            }
            if self.view.navigation_bar_button(cx, ids!(contact_moments)).clicked(actions) {
                if let Some(profile) = &self.selected_profile {
                    cx.action(crate::moments::ui::MomentsAction::Open {author: Some(profile.user_id.clone())});
                }
            }
            if self.view.navigation_bar_button(cx, ids!(message)).clicked(actions) {
                if let Some(profile) = self.selected_profile.clone() {
                    submit_async_request(MatrixRequest::OpenOrCreateDirectMessage {user_profile: profile, allow_create: false});
                }
            }
            let group_list = self.view.portal_list(cx, ids!(group_list));
            for (index, widget) in group_list.items_with_actions(actions) {
                if !group_list.was_scrolling() && widget.navigation_bar_button(cx, ids!(group_row)).clicked(actions) {
                    if let Some(group) = self.groups.get(index) {
                        cx.widget_action(self.widget_uid(), RoomsListAction::Selected(SelectedRoom::JoinedRoom {room_name_id: group.name.clone()}));
                    }
                }
            }
            if self.view.navigation_bar_button(cx, ids!(explore)).clicked(actions) {
                cx.action(NavigationBarAction::GoToAddRoom);
            }
            if let Some(profile) = &self.selected_profile {
                if self.view.navigation_bar_button(cx, ids!(contact_copy)).clicked(actions) {
                    cx.copy_to_clipboard(&profile.user_id.matrix_to_uri().to_string());
                    crate::shared::popup_list::enqueue_popup_notification(crate::i18n::tr("Profile link copied."), crate::shared::popup_list::PopupKind::Success, Some(2.0));
                }
                if self.view.navigation_bar_button(cx, ids!(contact_block)).clicked(actions) {
                    cx.action(crate::block_user_modal::BlockUserModalAction::Open(crate::block_user_modal::BlockUserRequest {
                        user_id: profile.user_id.clone(), display_name: profile.username.clone(),
                        block: !crate::sliding_sync::is_user_blocked(&profile.user_id), reject_invite_to: None,
                    }));
                }
            }
            if self.view.navigation_bar_button(cx, ids!(settings)).clicked(actions) {
                cx.action(NavigationBarAction::OpenSettings);
            }
            if self.view.navigation_bar_button(cx, ids!(own_profile)).clicked(actions) {
                cx.action(NavigationBarAction::OpenOwnProfile);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if self.kind == 0 && self.owner.as_ref() != current_user_id().as_ref() {
            // Lazy pages may miss the logout action while another tab is
            // active. Clear profile, search and group state before drawing any
            // content for a different signed-in account.
            self.clear_account_state(cx);
            self.refresh_contacts(cx, false);
        }
        if self.kind == 1 {
            let invites = get_client().map(|c|c.invited_rooms().into_iter().filter(crate::moments::is_moments).count()).unwrap_or(0);
            self.view.label(cx, ids!(discover_moments.title)).set_text(cx, &if invites == 0 {crate::i18n::tr("Moments").into()} else {crate::i18n::format("Moments · {invites} invitation{0}", &[("invites", (invites).to_string()), ("0", (crate::i18n::plural_suffix(invites)).to_string())])});
        }
        let profile_open = self.selected_profile.is_some();
        let (page, title) = if profile_open { (id!(profile), crate::i18n::tr("Contact Info")) } else if self.groups_open {
            (id!(joined_groups), crate::i18n::tr("Group Chats"))
        } else if self.adding_friend {
            (id!(contacts), crate::i18n::tr("New Friends"))
        } else {
            match self.kind { 1 => (id!(discover), crate::i18n::tr("Discover")), 2 => (id!(account), crate::i18n::tr("Me")), _ => (id!(contacts), crate::i18n::tr("Contacts")) }
        };
        let active = self.view.child_by_path(ids!(pages)).as_page_flip().set_active_page(cx, page);
        self.view.label(cx, ids!(title_bar.title)).set_text(cx, title);
        self.view.button(cx, ids!(title_bar.controls.left)).set_visible(cx, profile_open || self.adding_friend || self.groups_open);
        self.view.view(cx, ids!(shortcuts)).set_visible(cx, !self.adding_friend);
        self.view.text_input(cx, ids!(search)).set_empty_text(cx, crate::i18n::tr(if self.adding_friend {
            "Search by name or username"
        } else { "Search" }).to_owned());
        self.view.button(cx, ids!(title_bar.controls.right)).set_visible(cx, self.kind == 0 && !profile_open && !self.groups_open && !self.adding_friend);
        if let (Some(widget), Some(profile)) = (active.as_ref(), self.selected_profile.as_ref()) {
            Self::populate_profile(cx, widget, profile);
            widget.label(cx, ids!(contact_card.user_id)).set_text(cx, &crate::i18n::format("Matrix ID: {0}", &[("0", (profile.user_id).to_string())]));
            widget.label(cx, ids!(contact_block.title)).set_text(cx, if crate::sliding_sync::is_user_blocked(&profile.user_id) { crate::i18n::tr("Unblock") } else { crate::i18n::tr("Block") });
        } else if self.kind == 2 {
            if let Some(profile) = get_own_profile(cx) {
                let widget = self.view.widget(cx, ids!(own_profile));
                Self::populate_profile(cx, &widget, &profile);
            }
        }
        self.view.label(cx, ids!(status)).set_text(cx, &self.status);
        self.view.label(cx, ids!(status)).set_visible(cx, !self.status.is_empty());
        self.view.label(cx, ids!(group_status)).set_text(cx, &self.status);
        self.view.label(cx, ids!(group_status)).set_visible(cx, !self.status.is_empty());
        let query = self.query.trim().to_lowercase();
        self.visible_contacts = self.contacts.iter().enumerate().filter(|(_, p)| {
            p.displayable_name().to_lowercase().contains(&query) || p.user_id.as_str().to_lowercase().contains(&query)
        }).map(|(i, _)| i).collect();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.borrow_mut::<PortalList>() {
                if self.groups_open {
                    list.set_item_range(cx, 0, self.groups.len());
                    while let Some(index) = list.next_visible_item(cx) {
                        let Some(group) = self.groups.get_mut(index) else {
                            list.item(cx, index, id!(Filler)).draw_all(cx, scope);
                            continue;
                        };
                        let widget = list.item(cx, index, id!(Group));
                        widget.label(cx, ids!(name)).set_text(cx, &group.name.display());
                        let avatar = widget.avatar(cx, ids!(avatar));
                        let loaded = group.avatar.update_from_cache(cx).is_some_and(|image| {
                            avatar.show_image(cx, None, |cx, img| utils::load_avatar_image(&img, cx, image)).is_ok()
                        });
                        if !loaded { avatar.show_text(cx, None, None, &group.name.display()); }
                        widget.draw_all(cx, scope);
                    }
                    continue;
                }
                list.set_item_range(cx, 0, self.visible_contacts.len());
                while let Some(index) = list.next_visible_item(cx) {
                    let Some(profile) = self.visible_contacts.get(index).and_then(|i| self.contacts.get(*i)) else {
                        list.item(cx, index, id!(Filler)).draw_all(cx, scope);
                        continue;
                    };
                    let widget = list.item(cx, index, id!(Contact));
                    let letter = profile.first_letter().to_uppercase();
                    let previous = index.checked_sub(1).and_then(|i| self.visible_contacts.get(i)).map(|i| self.contacts[*i].first_letter().to_uppercase());
                    widget.view(cx, ids!(section)).set_visible(cx, previous.as_ref() != Some(&letter));
                    widget.label(cx, ids!(letter)).set_text(cx, &letter);
                    Self::populate_profile(cx, &widget, profile);
                    widget.draw_all(cx, scope);
                }
            }
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod new_chat_tests {
    use super::*;
    #[tokio::test]
    async fn friend_search_matches_partial_names_and_exact_usernames() {
        let server = crate::profile::search_test_server::DirectoryServer::new().await;
        let client = server.client().await;
        let owner = ruma::user_id!("@owner:example.test");
        let matches = search_contact_profiles(&client, owner, "ch").await.unwrap();
        assert_eq!(matches.iter().filter(|p| p.user_id.as_str() == "@chris:example.test").count(), 1);
        assert!(matches.iter().any(|p| p.user_id.as_str() == "@chris:remote.test"), "friend search preserves federated matches");
        for query in ["chris", "@chris", "@chris:example.test"] {
            let matches = search_contact_profiles(&client, owner, query).await.unwrap();
            assert_eq!(matches.len(), 1);
            assert_eq!(matches[0].user_id.as_str(), "@chris:example.test");
        }
        let private = search_contact_profiles(&client, owner, "private").await.unwrap();
        assert_eq!(private[0].username.as_deref(), Some("Private account"));
        assert!(search_contact_profiles(&client, owner, "not-registered").await.unwrap().is_empty());
    }
    #[test]
    fn usernames_use_the_current_homeserver_without_requiring_a_full_id() {
        let owner = ruma::user_id!("@owner:river.ominix.io:19443");
        for query in ["chris", "@chris", " chris "] {
            assert_eq!(contact_query_id(owner, query).unwrap().as_str(), "@chris:river.ominix.io:19443");
        }
        assert_eq!(contact_query_id(owner, "@chris:other.test").unwrap().as_str(), "@chris:other.test");
        for query in ["", "Chris Smith", "someone:malformed"] {
            assert!(contact_query_id(owner, query).is_none());
        }
    }
    #[test]
    fn new_chat_opens_existing_contact_search_without_sending() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut hub = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm); crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.MobileHub {kind: 0}});
            MobileHub::script_from_value(vm, value)
        });
        hub.groups_open = true;
        hub.query = "old query".into();
        let request = hub.request;
        let actions = cx.capture_actions(|cx| cx.action(MobileNavigationAction::NewChatRequested));
        hub.handle_event(&mut cx, &Event::Actions(actions), &mut Scope::empty());
        assert!(hub.adding_friend);
        assert!(!hub.groups_open);
        assert!(hub.query.is_empty());
        assert_eq!(hub.view.text_input(&mut cx, ids!(search)).text(), "");
        assert_ne!(hub.request, request, "Opening search invalidates old requests without sending a search or message");
        let request = hub.request;
        let input = hub.view.text_input(&mut cx, ids!(search)).widget_uid();
        let actions = cx.capture_actions(|cx| cx.widget_action(input, TextInputAction::Changed("ch".into())));
        hub.handle_event(&mut cx, &Event::Actions(actions), &mut Scope::empty());
        assert_eq!(hub.query, "ch");
        assert_ne!(hub.request, request, "Typing invalidates any previous directory response immediately");
        assert!(!hub.search_timer.is_empty(), "A short name starts a debounced search without Enter");
        let request = hub.request;
        let actions = cx.capture_actions(|cx| cx.widget_action(input, TextInputAction::Changed(String::new())));
        hub.handle_event(&mut cx, &Event::Actions(actions), &mut Scope::empty());
        assert_ne!(hub.request, request);
        assert!(hub.search_timer.is_empty(), "Clearing the search cancels the pending lookup");
    }
}

/// Mobile has no left rail. Its Discover page carries the rail's tools, File Transfer included.
#[cfg(test)]
mod discover_tests {
    use super::*;
    use makepad_widgets::makepad_draw::cx_draw::CxDraw;

    #[test]
    fn discover_keeps_file_transfer_reachable_on_mobile() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut hub = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let value = script_eval!(vm, {mod.widgets.MobileHub {kind: 1}});
            let hub = MobileHub::script_from_value(vm, value);
            let errors = vm.take_errors();
            assert!(errors.is_empty(), "Discover registration: {errors:?}");
            hub
        });
        let size = dvec2(375.0, 700.0);
        cx.redraw_all();
        let pass = DrawPass::new(&mut cx);
        pass.set_size(&mut cx, size);
        let mut list = DrawList2d::new(&mut cx);
        let event = DrawEvent::default();
        {
            let mut draw = CxDraw::new(&mut cx, &event);
            let mut cx2d = Cx2d::new(&mut draw);
            cx2d.begin_pass(&pass, None);
            list.begin_always(&mut cx2d);
            cx2d.begin_root_turtle(size, Layout::flow_overlay());
            hub.draw_walk_all(&mut cx2d, &mut Scope::empty(), Walk {
                width: Size::Fixed(size.x),
                height: Size::Fixed(size.y),
                ..Walk::default()
            });
            cx2d.end_pass_sized_turtle();
            list.end(&mut cx2d);
            cx2d.end_pass(&pass);
        }
        // Moments, then File Transfer, then Explore: the rail's tools in one list.
        let rows = [ids!(discover_moments), ids!(discover_file_transfer), ids!(explore)]
            .map(|id| hub.view.navigation_bar_button(&mut cx, id).area().rect(&cx));
        assert!(rows[0].pos.y < rows[1].pos.y && rows[1].pos.y < rows[2].pos.y, "{rows:?}");
        assert!(rows[1].size.x >= 44.0 && rows[1].size.y >= 44.0, "a full target: {:?}", rows[1]);
        assert_eq!(hub.view.label(&mut cx, ids!(discover_file_transfer.title)).text(), crate::i18n::tr("File Transfer"));
        let uid = hub.view.navigation_bar_button(&mut cx, ids!(discover_file_transfer)).widget_uid();
        let clicked = cx.capture_actions(|cx| cx.widget_action(uid, crate::shared::navigation_bar_button::NavigationBarButtonAction::Clicked));
        let emitted = cx.capture_actions(|cx| hub.handle_event(cx, &Event::Actions(clicked), &mut Scope::empty()));
        assert_eq!(emitted.iter().filter(|a| matches!(a.downcast_ref(), Some(crate::moments::ui::MomentsAction::FileTransfer))).count(), 1);
    }
}
