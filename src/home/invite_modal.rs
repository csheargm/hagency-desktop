//! A modal dialog for inviting a user to a room.

use makepad_widgets::*;
use ruma::OwnedUserId;
use std::collections::HashMap;
use crate::shared::navigation_bar_button::NavigationBarButtonWidgetRefExt;

use crate::home::room_screen::InviteResultAction;
use crate::sliding_sync::{MatrixRequest, get_client, spawn_async_task, submit_async_request};
use crate::utils::RoomNameId;


script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*


    mod.widgets.InviteModal = set_type_default() do #(InviteModal::register_widget(vm)) {
        ..mod.widgets.SmallModal

        title := ModalTitle {}

        user_id_input := RobrixTextInput {
            width: Fill
            height: 44
            draw_text +: {
                text_style: REGULAR_TEXT {font_size: (11 * mod.widgets.RINX_TEXT_SCALE)},
                color: mod.widgets.RINX_INK
            }
            empty_text: #(crate::i18n::tr("Name or @user:example.org")) i18n_empty_text: "Name or @user:example.org",
            autocapitalize: None,
            autocorrect: Disabled,
        }

        people_section := View {
            width: Fill height: Fit flow: Down spacing: 8 margin: Inset{top: 12}
            Label {width: Fill height: Fit
                text: #(crate::i18n::tr("People you know")) i18n_text: "People you know"
                draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}
            }
            people_list := PortalList {
                width: Fill height: 176 flow: Down auto_tail: false
                Empty := Label {width: Fill height: Fit flow: Flow.Right{wrap: true} padding: 10
                    text: #(crate::i18n::tr("Search by name or enter a Matrix ID.")) i18n_text: "Search by name or enter a Matrix ID."
                    draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}
                }
                Person := NavigationBarButton {
                    width: Fill height: Fit{min: FitBound.Abs(56.0)} flow: Down spacing: 3
                    padding: Inset{left: 10 right: 14 top: 8 bottom: 8}
                    align: Align{x: 0.0 y: 0.5}
                    draw_bg +: {color_hover: mod.widgets.RINX_HOVER color_active: mod.widgets.RINX_SELECTED border_radius: 8}
                    person_name := Label {width: Fill height: Fit padding: 0 max_lines: 1 text_overflow: Ellipsis
                        draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular {font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}
                    }
                    person_id := Label {width: Fill height: Fit padding: 0 max_lines: 1 text_overflow: Ellipsis
                        draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: (9 * mod.widgets.RINX_TEXT_SCALE)}}
                    }
                }
            }
        }

        buttons_view := ModalButtonsRow {
            cancel_button := RobrixNeutralIconButton {
                height: 44
                width: Fit{min: FitBound.Abs(120.0)},
                align: Align{x: 0.5, y: 0.5}
                padding: 12,
                draw_icon.svg: (ICON_FORBIDDEN)
                icon_walk: Walk{width: 16, height: 16, margin: Inset{left: -2, right: -1} }
                text: #(crate::i18n::tr("Cancel")) i18n_text: "Cancel"
            }

            confirm_button := RobrixPositiveIconButton {
                height: 44
                width: Fit{min: FitBound.Abs(120.0)}
                align: Align{x: 0.5, y: 0.5}
                padding: 12,
                draw_icon.svg: (ICON_ADD_USER)
                icon_walk: Walk{width: 16, height: 16, margin: Inset{left: -2, right: -1} }
                text: #(crate::i18n::tr("Invite")) i18n_text: "Invite"
            }

            okay_button := RobrixIconButton {
                height: 44
                visible: false
                width: Fit{min: FitBound.Abs(120.0)}
                align: Align{x: 0.5, y: 0.5}
                padding: 12,
                draw_icon.svg: (ICON_CHECKMARK)
                icon_walk: Walk{width: 16, height: 16, margin: Inset{left: -2, right: -1} }
                text: #(crate::i18n::tr("Okay")) i18n_text: "Okay"
            }
        }

        status_label_view := View {
            visible: false
            width: Fill,
            height: Fit,
            align: Align{x: 0.5, y: 0.0}

            status_label := Label {
                width: Fill,
                height: Fit,
                flow: Flow.Right{wrap: true},
                align: Align{x: 0.5, y: 0.0}
                margin: Inset{top: 10}
                draw_text +: {
                    text_style: REGULAR_TEXT {font_size: (11 * mod.widgets.RINX_TEXT_SCALE)},
                    color: mod.widgets.RINX_INK
                }
                text: ""
            }
        }
    }
}

/// Actions emitted by other widgets to show or hide the `InviteModal`.
#[derive(Clone, Debug)]
pub enum InviteModalAction {
    /// Open the modal to invite a user to the given room or space.
    Open(RoomNameId),
    /// Close the modal.
    Close,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum InviteModalState {
    /// Waiting for the user to enter a user ID.
    #[default]
    WaitingForUserInput,
    /// Waiting for the invite to be sent.
    WaitingForInvite(OwnedUserId),
    /// The invite was sent successfully.
    InviteSuccess,
    /// An error occurred while sending the invite.
    InviteError,
}


/// People the invite dialog can offer, by Matrix ID and display name.
type Person = (OwnedUserId, Option<String>);

#[cfg(test)]
mod permission_tests {
    use super::*;

    #[test]
    fn matrix_invite_power_does_not_replace_agent_authorization_or_session_scope() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut modal = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.InviteModal {}});
            InviteModal::script_from_value(vm, value)
        });
        modal.epoch = crate::account_session::epoch();
        let person = ruma::user_id!("@person:test");
        let agent = ruma::user_id!("@_hagency_agt_example:test");
        assert!(!modal.may_invite_user(person));
        assert!(modal.may_invite_user(agent), "Agent invitations still go through the Hagency service");
        modal.can_invite = true;
        assert!(modal.may_invite_user(person));
        modal.can_invite = false;
        assert!(!modal.may_invite_user(person));
        modal.epoch = modal.epoch.wrapping_add(1);
        assert!(!modal.may_invite_user(agent));
        assert!(!modal.may_invite_user(person));
        assert!(!crate::sliding_sync::UserPowerLevels::assumed_until_fetched().can_invite());
    }
}

/// Results of the dialog's background lookups, matched to the request that
/// asked for them so a late answer never overwrites a newer one.
#[derive(Debug)]
enum InvitePeopleAction {
    /// Everyone who shares a joined room with the user.
    Known { request: u64, people: Vec<Person> },
    /// The homeserver directory's answer for a typed name.
    Searched { request: u64, query: String, people: Vec<Person> },
}

#[derive(Script, ScriptHook, Widget)]
pub struct InviteModal {
    #[deref] view: View,
    #[rust] state: InviteModalState,
    #[rust] room_name_id: Option<RoomNameId>,
    #[rust] offered: Vec<Person>,
    /// Pin clicks to the identity actually drawn, even if a lookup replaces the list.
    #[rust] rendered_people: HashMap<WidgetUid, OwnedUserId>,
    /// Registered agents and people who share a room with the user.
    #[rust] known: Vec<Person>,
    #[rust] request: u64,
    #[rust] epoch: u64,
    #[rust] can_invite: bool,
    #[cfg(feature = "onboarding-test-support")]
    #[rust] review_mode: bool,
}

impl Widget for InviteModal {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        self.widget_match_event(cx, event, scope);
        if matches!(event, Event::Actions(_)) {
            self.update_invite_button(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.rendered_people.clear();
        let selected = self.view.text_input(cx, ids!(user_id_input)).text();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.borrow_mut::<PortalList>() {
                list.set_item_range(cx, 0, self.offered.len().max(1));
                while let Some(index) = list.next_visible_item(cx) {
                    if self.offered.is_empty() {
                        list.item(cx, index, id!(Empty)).draw_all(cx, scope);
                        continue;
                    }
                    let Some((id, name)) = self.offered.get(index) else {continue;};
                    let row = list.item(cx, index, id!(Person));
                    row.label(cx, ids!(person_name)).set_text(cx, name.as_deref().filter(|n| !n.is_empty()).unwrap_or(id.localpart()));
                    row.label(cx, ids!(person_id)).set_text(cx, id.as_str());
                    row.as_navigation_bar_button().set_selected(cx, selected == id.as_str());
                    self.rendered_people.insert(row.widget_uid(), id.clone());
                    row.draw_all(cx, scope);
                }
            }
        }
        DrawStep::done()
    }
}

impl WidgetMatchEvent for InviteModal {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions, _scope: &mut Scope) {
        if self.epoch != crate::account_session::epoch() { return; }
        for action in actions {
            if let Some(update) = action.downcast_ref::<crate::sliding_sync::UserPowerLevelsUpdated>()
                && self.room_name_id.as_ref().is_some_and(|room| update.is_current_for(room.room_id()))
            {
                self.can_invite = update.power_levels.can_invite();
            }
            match action.downcast_ref() {
                Some(InvitePeopleAction::Known { request, people }) if *request == self.request => {
                    for person in people {
                        if !self.known.iter().any(|(id, _)| id == &person.0) {
                            self.known.push(person.clone());
                        }
                    }
                    self.known.sort_by_key(|(id, name)| (name.as_deref().unwrap_or(id.as_str()).to_lowercase(), id.clone()));
                    let query = self.view.text_input(cx, ids!(user_id_input)).text();
                    let matches = self.local_matches(query.trim());
                    self.offer(cx, &matches);
                }
                Some(InvitePeopleAction::Searched { request, query, people }) if *request == self.request
                    && self.view.text_input(cx, ids!(user_id_input)).text().trim() == query => {
                    let mut matches = self.local_matches(query);
                    for person in people {
                        if !matches.iter().any(|(id, _)| id == &person.0) {
                            matches.push(person.clone());
                        }
                    }
                    self.resolved(cx, query, matches);
                }
                _ => {}
            }
        }
        if !matches!(self.state, InviteModalState::WaitingForInvite(_)) {
            let input = self.view.text_input(cx, ids!(user_id_input));
            if let Some(query) = input.changed(actions) {
                // A directory reply for the old text is discarded. Its
                // progress message must not remain after the person edits.
                self.view.view(cx, ids!(status_label_view)).set_visible(cx, false);
                let matches = self.local_matches(query.trim());
                self.offer(cx, &matches);
            }
            let list = self.view.portal_list(cx, ids!(people_list));
            for (_, row) in list.items_with_actions(actions) {
                if !list.was_scrolling() && row.as_navigation_bar_button().clicked(actions)
                    && let Some(user) = self.rendered_people.get(&row.widget_uid()) {
                    input.set_text(cx, user.as_str());
                    input.set_key_focus(cx);
                    self.view.redraw(cx);
                    break;
                }
            }
        }
        let cancel_button = self.view.button(cx, ids!(cancel_button));

        // Handle canceling/closing the modal.
        let cancel_clicked = cancel_button.clicked(actions);
        if cancel_clicked ||
            actions.iter().any(|a| matches!(a.downcast_ref(), Some(ModalAction::Dismissed)))
        {
            // If the modal was dismissed by clicking outside of it, we MUST NOT emit
            // a `InviteModalAction::Close` action, as that would cause
            // an infinite action feedback loop.
            if cancel_clicked {
                cx.action(InviteModalAction::Close);
            }
            return;
        }

        // Handle the okay button (shown after invite success).
        let okay_button = self.view.button(cx, ids!(okay_button));
        if okay_button.clicked(actions) {
            cx.action(InviteModalAction::Close);
            return;
        }

        let confirm_button = self.view.button(cx, ids!(confirm_button));
        let user_id_input = self.view.text_input(cx, ids!(user_id_input));

        // Handle return key or invite button click.
        if let Some(user_id_str) = confirm_button.clicked(actions)
            .then(|| user_id_input.text())
            .or_else(|| user_id_input.returned(actions).map(|(t, _)| t))
        {
            if matches!(self.state, InviteModalState::WaitingForInvite(_)) { return; }
            // Validate the user ID
            if user_id_str.trim().is_empty() {
                self.status(cx, crate::i18n::tr("Enter a name or Matrix ID."), true);
                return;
            }

            // A full Matrix ID is invited as typed; anything else is a name.
            let parsed = if user_id_str.trim_start().starts_with('@') {
                ruma::UserId::parse(user_id_str.trim())
            } else {
                Err(ruma::IdParseError::MissingLeadingSigil)
            };
            if parsed.is_err() && !user_id_str.trim_start().starts_with('@') {
                self.search(cx, user_id_str.trim().to_owned());
                return;
            }
            match parsed {
                Ok(user_id) => {
                    if !self.may_invite_user(&user_id) {
                        self.status(cx, crate::i18n::tr("You do not have permission to invite people to this room."), true);
                        return;
                    }
                    if let Some(room_name_id) = &self.room_name_id {
                        let request = MatrixRequest::InviteUser {
                            room_id: room_name_id.room_id().clone(),
                            user_id: user_id.clone(),
                        };
                        #[cfg(feature = "onboarding-test-support")]
                        let offline_review = self.review_mode;
                        #[cfg(not(feature = "onboarding-test-support"))]
                        let offline_review = false;
                        if !offline_review { submit_async_request(request); }
                        let agent = crate::hagency::backend::is_agent_invitation(&user_id);
                        self.state = InviteModalState::WaitingForInvite(user_id);
                        self.status(cx, crate::i18n::tr(if agent {"Adding Agent to this room…"} else {"Sending invite..."}), false);
                        confirm_button.set_enabled(cx, false);
                        user_id_input.set_is_read_only(cx, true);
                    }
                }
                Err(_) => {
                    self.status(cx, crate::i18n::tr("Invalid User ID. Expected format: @user:server.xyz"), true);
                    user_id_input.set_key_focus(cx);
                }
            }
            self.view.redraw(cx);
        }

        // Handle the result of a previously-sent invite.
        for action in actions {
            if let Some(result) = action.downcast_ref::<InviteResultAction>() {
                self.finish_invite(cx, result);
            }
        }
    }
}

impl InviteModal {
    fn finish_invite(&mut self, cx: &mut Cx, result: &InviteResultAction) {
        let (room, user, epoch) = match result {
            InviteResultAction::Agent {room_id,user_id,epoch,..} | InviteResultAction::Waiting {room_id,user_id,epoch} => (room_id,user_id,*epoch),
            InviteResultAction::Sent {room_id,user_id} | InviteResultAction::Failed {room_id,user_id,..} => (room_id,user_id,self.epoch),
        };
        if self.epoch != crate::account_session::epoch() || epoch != self.epoch
            || self.room_name_id.as_ref().is_none_or(|r| r.room_id() != room)
            || !matches!(&self.state, InviteModalState::WaitingForInvite(id) if id == user) { return; }
        let (text, error, success, pending) = match result {
            InviteResultAction::Waiting {..} => {
                self.status(cx, crate::i18n::tr("Still waiting for the server. You can close this panel; the original request will continue."), false);
                return;
            }
            InviteResultAction::Agent {result:Ok(true),..} => (crate::i18n::tr("Agent joined this room. In group chats, @ the Agent to ask it to reply.").to_owned(),false,true,false),
            InviteResultAction::Agent {result:Ok(false),..} => (crate::i18n::tr("The request is saved. The Agent is still joining. You can close this panel or check its status.").to_owned(),false,false,true),
            InviteResultAction::Agent {result:Err(code),..} => (crate::i18n::tr(super::mobile_chat_info::agent_binding_error(code)).to_owned(),true,false,false),
            InviteResultAction::Sent {..} => (crate::i18n::tr("Invitation sent. Waiting for them to join.").to_owned(),false,true,false),
            InviteResultAction::Failed {error,..} => (crate::i18n::format("Failed to send invite.\n\nError: {error}", &[("error", error.to_string())]),true,false,false),
        };
        self.status(cx, &text, error);
        if success {
            let mut label=self.view.label(cx,ids!(status_label_view.status_label));
            script_apply_eval!(cx,label,{draw_text +: {color: mod.widgets.COLOR_FG_ACCEPT_GREEN}});
        }
        self.state=if success {InviteModalState::InviteSuccess} else {InviteModalState::InviteError};
        let confirm=self.view.button(cx,ids!(confirm_button));
        confirm.set_visible(cx,!success);
        confirm.set_enabled(cx,true);
        confirm.set_text(cx,crate::i18n::tr(if pending {"Check status"} else {"Invite"}));
        self.view.button(cx,ids!(cancel_button)).set_visible(cx,!success);
        self.view.button(cx,ids!(okay_button)).set_visible(cx,success);
        self.view.text_input(cx,ids!(user_id_input)).set_is_read_only(cx,false);
        self.view.redraw(cx);
    }
    /// Keep results inside a bounded, scrollable part of the dialog. An overlay
    /// menu cannot fit a large directory or long federated IDs on compact screens.
    fn offer(&mut self, cx: &mut Cx, people: &[Person]) {
        if self.offered == people {return;}
        self.offered = people.to_vec();
        self.rendered_people.clear();
        self.view.portal_list(cx, ids!(people_list)).set_first_id_and_scroll(0, 0.0);
        self.view.redraw(cx);
    }

    /// Known people whose name or ID contains `query`, ignoring case.
    fn local_matches(&self, query: &str) -> Vec<Person> {
        let needle = query.to_lowercase();
        self.known
            .iter()
            .filter(|(id, name)| {
                id.as_str().to_lowercase().contains(&needle)
                    || name.as_deref().is_some_and(|n| n.to_lowercase().contains(&needle))
            })
            .cloned()
            .collect()
    }

    /// A typed name: ask the homeserver directory too, then resolve.
    fn search(&mut self, cx: &mut Cx, query: String) {
        self.request += 1;
        let request = self.request;
        self.status(cx, &crate::i18n::format("Searching for {0}…", &[("0", query.clone())]), false);
        let Some(client) = get_client() else {
            let matches = self.local_matches(&query);
            self.resolved(cx, &query, matches);
            return;
        };
        spawn_async_task(async move {
            let people = match client.search_users(&query, 20).await {
                Ok(found) => found
                    .results
                    .into_iter()
                    .filter(|u| Some(u.user_id.as_ref()) != client.user_id())
                    .map(|u| (u.user_id, u.display_name))
                    .collect(),
                Err(_) => Vec::new(),
            };
            Cx::post_action(InvitePeopleAction::Searched { request, query, people });
            SignalToUI::set_ui_signal();
        });
    }

    /// One match fills the field, ready to invite; several are offered in the
    /// list; none asks for the full ID.
    fn resolved(&mut self, cx: &mut Cx, query: &str, matches: Vec<Person>) {
        match matches.as_slice() {
            [(id, _)] => {
                self.view.text_input(cx, ids!(user_id_input)).set_text(cx, id.as_str());
                self.status(cx, &crate::i18n::format("Found {0}. Press Invite to send.", &[("0", id.to_string())]), false);
            }
            [] => self.status(
                cx,
                &crate::i18n::format("No one found for \"{0}\". Enter their full ID, like @name:server.", &[("0", query.to_owned())]),
                true,
            ),
            _ => {
                self.offer(cx, &matches);
                self.status(cx, &crate::i18n::format("{0} people match. Pick one above.", &[("0", matches.len().to_string())]), false);
            }
        }
    }

    fn status(&mut self, cx: &mut Cx, text: &str, error: bool) {
        let mut label = self.view.label(cx, ids!(status_label_view.status_label));
        let text = text.to_owned();
        if error {
            script_apply_eval!(cx, label, { text: #(text), draw_text +: { color: mod.widgets.COLOR_FG_DANGER_RED } });
        } else {
            script_apply_eval!(cx, label, { text: #(text), draw_text +: { color: mod.widgets.COLOR_ACTIVE_PRIMARY_DARKER } });
        }
        self.view.view(cx, ids!(status_label_view)).set_visible(cx, true);
        self.view.redraw(cx);
    }

    /// Everyone who shares a joined room with the user, except the user and
    /// whoever is already in the target room.
    fn load_known(&mut self, room: ruma::OwnedRoomId) {
        let Some(client) = get_client() else { return };
        let request = self.request;
        spawn_async_task(async move {
            let me = client.user_id().map(ToOwned::to_owned);
            let present: Vec<OwnedUserId> = match client.get_room(&room) {
                Some(target) => target
                    .members_no_sync(matrix_sdk::RoomMemberships::JOIN | matrix_sdk::RoomMemberships::INVITE)
                    .await
                    .map(|members| members.iter().map(|m| m.user_id().to_owned()).collect())
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            let mut people: Vec<Person> = Vec::new();
            for joined in client.joined_rooms() {
                if crate::moments::is_moments(&joined) {
                    continue;
                }
                let Ok(members) = joined.members_no_sync(matrix_sdk::RoomMemberships::JOIN).await else {
                    continue;
                };
                for member in members {
                    let id = member.user_id().to_owned();
                    if Some(&id) == me.as_ref() || present.contains(&id) || people.iter().any(|(p, _)| p == &id) {
                        continue;
                    }
                    people.push((id, member.display_name().map(str::to_owned)));
                    if people.len() >= 500 {
                        break;
                    }
                }
            }
            Cx::post_action(InvitePeopleAction::Known { request, people });
            SignalToUI::set_ui_signal();
        });
    }

    pub fn show(&mut self, cx: &mut Cx, room_name_id: RoomNameId) {
        self.view.label(cx, ids!(title)).set_text(
            cx,
            &crate::i18n::format("Invite to {0}", &[("0", room_name_id.to_string())]),
        );
        let settings = crate::agent_access::current();
        self.request += 1;
        self.epoch = crate::account_session::epoch();
        self.can_invite = false;
        submit_async_request(MatrixRequest::GetRoomPowerLevels {room_id: room_name_id.room_id().clone()});
        self.known = settings
            .agent_registry
            .agents()
            .map(|(id, entry)| (id.clone(), entry.display_name.clone()))
            .collect();
        let known = self.known.clone();
        self.offer(cx, &known);
        self.load_known(room_name_id.room_id().clone());
        self.state = InviteModalState::WaitingForUserInput;
        self.room_name_id = Some(room_name_id);

        // Reset the UI state
        let confirm_button = self.view.button(cx, ids!(confirm_button));
        let cancel_button = self.view.button(cx, ids!(cancel_button));
        let okay_button = self.view.button(cx, ids!(okay_button));
        let user_id_input = self.view.text_input(cx, ids!(user_id_input));
        confirm_button.set_visible(cx, true);
        confirm_button.set_enabled(cx, true);
        confirm_button.set_text(cx, crate::i18n::tr("Invite"));
        confirm_button.reset_hover(cx);
        cancel_button.set_visible(cx, true);
        cancel_button.set_enabled(cx, true);
        cancel_button.reset_hover(cx);
        okay_button.set_visible(cx, false);
        okay_button.reset_hover(cx);
        user_id_input.set_is_read_only(cx, false);
        user_id_input.set_text(cx, "");
        self.view.view(cx, ids!(status_label_view)).set_visible(cx, false);
        self.view.label(cx, ids!(status_label_view.status_label)).set_text(cx, "");
        self.view.redraw(cx);
        user_id_input.set_key_focus(cx);
        self.update_invite_button(cx);
    }

    fn may_invite_user(&self, user_id: &ruma::UserId) -> bool {
        self.epoch == crate::account_session::epoch()
            && (self.can_invite || crate::hagency::backend::is_agent_invitation(user_id))
    }

    fn update_invite_button(&mut self, cx: &mut Cx) {
        let text = self.view.text_input(cx, ids!(user_id_input)).text();
        let allowed = ruma::UserId::parse(text.trim()).ok()
            .map_or(self.can_invite, |id| self.may_invite_user(&id));
        self.view.button(cx, ids!(confirm_button)).set_enabled(cx,
            allowed && self.epoch == crate::account_session::epoch()
                && !matches!(self.state, InviteModalState::WaitingForInvite(_) | InviteModalState::InviteSuccess));
    }
}

impl InviteModalRef {
    /// Offline native interaction tests use the production controls and result
    /// handler. This seam cannot submit commands or run with a signed-in client.
    #[cfg(feature = "onboarding-test-support")]
    pub fn review_result(&self, cx: &mut Cx, scenario: &str) {
        assert!(get_client().is_none(), "Invitation review requires an offline profile");
        let Some(mut inner)=self.borrow_mut() else {return;};
        inner.review_mode=true;
        // This offline fixture represents a room whose current user can invite people.
        inner.can_invite=true;
        inner.update_invite_button(cx);
        if scenario=="ready" {
            inner.known=vec![("@_hagency_agt_cabbage:example.invalid".try_into().unwrap(),Some("小白菜".into())),("@friend:example.invalid".try_into().unwrap(),Some("Alex".into()))];
            let known=inner.known.clone();inner.offer(cx,&known);
            return;
        }
        if scenario=="many-people" {
            inner.known=(0..80).map(|i| (format!("@person_{i:02}:a-very-long-federated-homeserver.example.invalid").try_into().unwrap(), Some(format!("Friend {i:02} · 同名测试")))).collect();
            let known=inner.known.clone();inner.offer(cx,&known);
            return;
        }
        let InviteModalState::WaitingForInvite(user)=&inner.state else {panic!("Submit through the real Invite button first");};
        let user_id=user.clone();
        let room_id=inner.room_name_id.as_ref().unwrap().room_id().clone();
        let epoch=inner.epoch;
        let result=match scenario {
            "slow" => InviteResultAction::Waiting {room_id,user_id,epoch},
            "human" => InviteResultAction::Sent {room_id,user_id},
            "stale" => InviteResultAction::Agent {room_id,user_id,epoch:epoch.wrapping_add(1),result:Ok(true)},
            "other-room" => InviteResultAction::Agent {room_id:"!other:example.invalid".try_into().unwrap(),user_id,epoch,result:Ok(true)},
            _ => InviteResultAction::Agent {room_id,user_id,epoch,result:match scenario {"success"=>Ok(true),"pending"=>Ok(false),code=>Err(code.into())}},
        };
        inner.finish_invite(cx,&result);
    }
    pub fn show(&self, cx: &mut Cx, room_name_id: RoomNameId) {
        let Some(mut inner) = self.borrow_mut() else { return };
        inner.show(cx, room_name_id);
    }
}
