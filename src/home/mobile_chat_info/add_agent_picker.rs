//! Add agent picker rules for the room information panel: which owned Agents
//! are already in the open room, what the picker shows for them, and how a
//! bind outcome is reported and logged.
//!
//! Membership comes from what the panel already loaded (joined members and the
//! Agent rows' `ownerDirectRoomId`); nothing here makes a network call.

use makepad_widgets::*;
use matrix_sdk::ruma::{RoomId, UserId};
use serde_json::Value;

use super::{MobileChatInfo, RoomAgentsLoaded};

/// Bind refusals meaning "this Agent already holds this room": a binding in
/// another scope (its private chat) or one that cannot be re-bound.
const ALREADY_IN_ROOM_CODES: [&str; 2] = ["room_binding_scope_conflict", "binding_not_bindable"];

/// An Agent is already here when its puppet is a joined member, or this room
/// is the Agent's own private chat with its owner.
pub(super) fn agent_in_room<'a>(agent: &Value, room: &RoomId, mut members: impl Iterator<Item = &'a UserId>) -> bool {
    if agent["ownerDirectRoomId"].as_str() == Some(room.as_str()) {
        return true;
    }
    let Some(puppet) = agent["puppetMxid"].as_str() else { return false };
    members.any(|member| member.as_str() == puppet)
}

pub(super) fn agent_name(agent: &Value) -> &str {
    agent["displayName"].as_str().filter(|name| !name.is_empty()).unwrap_or("Agent")
}

/// The drop-down row for one Agent; Agents already here say so.
pub(super) fn picker_label(agent: &Value, in_room: bool) -> String {
    if in_room {
        crate::i18n::format("{0} (already in this room)", &[("0", agent_name(agent).to_owned())])
    } else {
        agent_name(agent).to_owned()
    }
}

pub(super) fn already_in_room_text(name: &str) -> String {
    crate::i18n::format("{0} is already in this room", &[("0", name.to_owned())])
}

pub(super) fn is_already_in_room(code: &str) -> bool {
    ALREADY_IN_ROOM_CODES.contains(&code)
}

/// "<agent> is already in this room" for a duplicate-bind refusal, else None.
pub(in crate::home) fn already_in_room_error(code: &str, name: &str) -> Option<String> {
    is_already_in_room(code).then(|| already_in_room_text(name))
}

/// The code a bind result is logged under: the command state, or the error.
pub(super) fn bind_outcome_code(result: &Result<Value, String>) -> String {
    match result {
        Ok(value) => value["commandState"].as_str().unwrap_or("missing_command_state").to_owned(),
        Err(code) if code.is_empty() => "empty_error_code".to_owned(),
        Err(code) => code.clone(),
    }
}

/// Every bind completion leaves its outcome code in the log.
pub(in crate::home) fn log_bind_code(agent: &str, room: &RoomId, code: &str) {
    if code == "active" || code == "pending" {
        log!("Add agent: bind of {agent} to {room} returned {code}");
    } else {
        warning!("Add agent: bind of {agent} to {room} failed with {code}");
    }
}

impl MobileChatInfo {
    fn joined_member_ids(&self) -> impl Iterator<Item = &UserId> {
        self.data.iter().flat_map(|data| data.members.iter().map(|p| p.user_id.as_ref()))
    }

    fn selected_agent(&self, cx: &mut Cx) -> Option<&Value> {
        let index = self.view.drop_down(cx, ids!(my_agents)).selected_item().checked_sub(1)?;
        self.agents.get(index)
    }

    /// Whether `agent` is already in the room this panel shows.
    fn agent_here(&self, agent: &Value) -> bool {
        self.room.as_ref().is_some_and(|room| agent_in_room(agent, room.room_id(), self.joined_member_ids()))
    }

    /// The selected Agent is already in this room, so it cannot be added again.
    pub(super) fn selected_agent_in_room(&self, cx: &mut Cx) -> bool {
        self.selected_agent(cx).is_some_and(|agent| self.agent_here(agent))
    }

    /// Drop-down labels for the loaded Agents, marking those already here.
    pub(super) fn agent_picker_labels(&self) -> Vec<String> {
        let rows = self.agents.iter().map(|agent| picker_label(agent, self.agent_here(agent)));
        std::iter::once(crate::i18n::tr("Choose my Agent").to_owned()).chain(rows).collect()
    }

    /// Status after the user picks a row.
    pub(super) fn selected_agent_status(&self, cx: &mut Cx) -> String {
        match self.selected_agent(cx).filter(|agent| self.agent_here(agent)) {
            Some(agent) => already_in_room_text(agent_name(agent)),
            None => crate::i18n::tr("Choose an Agent you own to bind to this Room.").to_owned(),
        }
    }

    /// A bind refused because the Agent already holds this room names it.
    pub(super) fn bind_error_text(&self, cx: &mut Cx, code: Option<&str>) -> Option<String> {
        let code = code?;
        let name = self.selected_agent(cx).map_or("Agent", agent_name).to_owned();
        already_in_room_error(code, &name)
    }

    /// Logs the outcome of a picker bind (not of the Agent list load).
    pub(super) fn log_bind_outcome(&self, cx: &mut Cx, loaded: &RoomAgentsLoaded) {
        if loaded.binding {
            let agent = self.selected_agent(cx).and_then(|a| a["id"].as_str()).unwrap_or("unknown");
            log_bind_code(agent, &loaded.room_id, &bind_outcome_code(&loaded.result));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{home::mobile_chat_info::ChatInfoData, utils::RoomNameId};
    use matrix_sdk::ruma::{room_id, user_id};

    fn ann5() -> Value {
        serde_json::json!({"id":"agent-5","puppetMxid":"@_hagency_agt_ann5:server","displayName":"ann5agent","state":"active","ownerDirectRoomId":"!private:server"})
    }
    fn ben2() -> Value {
        serde_json::json!({"id":"agent-2","puppetMxid":"@_hagency_agt_ben2:server","displayName":"ben2agent","state":"active","ownerDirectRoomId":"!elsewhere:server"})
    }

    #[test]
    fn agent_is_in_room_by_joined_puppet_or_its_own_private_chat() {
        let joined = [user_id!("@ann:server"), user_id!("@_hagency_agt_ann5:server")];
        assert!(agent_in_room(&ann5(), room_id!("!team:server"), joined.into_iter()));
        assert!(agent_in_room(&ann5(), room_id!("!private:server"), std::iter::empty()));
        assert!(!agent_in_room(&ben2(), room_id!("!team:server"), joined.into_iter()));
        assert!(!agent_in_room(&serde_json::json!({"displayName":"x"}), room_id!("!team:server"), joined.into_iter()));
    }

    #[test]
    fn duplicate_bind_codes_name_the_agent_and_every_outcome_has_a_log_code() {
        assert!(is_already_in_room("room_binding_scope_conflict") && is_already_in_room("binding_not_bindable"));
        assert!(!is_already_in_room("agent_retired"));
        assert_eq!(already_in_room_error("binding_not_bindable", "ann5agent").as_deref(), Some("ann5agent is already in this room"));
        assert_eq!(already_in_room_error("agent_retired", "ann5agent"), None);
        assert_eq!(already_in_room_text("ann5agent"), "ann5agent is already in this room");
        assert_eq!(picker_label(&ann5(), true), "ann5agent (already in this room)");
        assert_eq!(picker_label(&ben2(), false), "ben2agent");
        assert_eq!(bind_outcome_code(&Ok(serde_json::json!({"commandState":"active"}))), "active");
        assert_eq!(bind_outcome_code(&Ok(serde_json::json!({}))), "missing_command_state");
        assert_eq!(bind_outcome_code(&Err("room_binding_scope_conflict".into())), "room_binding_scope_conflict");
        assert_eq!(bind_outcome_code(&Err(String::new())), "empty_error_code");
    }

    /// The reported flow: Add agent in ann5agent's own private chat.
    #[test]
    fn private_chat_picker_marks_its_agent_and_a_duplicate_bind_names_it() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut info = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);vm.bx.captured_errors=Some(Vec::new());crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.MobileChatInfo {}});
            MobileChatInfo::script_from_value(vm, value)
        });
        let owner = user_id!("@ann:server").to_owned();
        let room = room_id!("!private:server").to_owned();
        info.owner = Some(owner.clone());info.origin = "https://server/".into();info.epoch = 4;
        info.room = Some(RoomNameId::empty(room.clone()));info.agent_request = 8;
        let mut loaded = RoomAgentsLoaded {owner, origin: "https://server/".into(), epoch: 4, room_id: room.clone(), request: 8, binding: false,
            result: Ok(serde_json::json!({"roomId": room.as_str(), "projectId": "project", "agents": [ann5(), ben2()]}))};
        info.finish_agent_request(&mut cx, &loaded);
        assert_eq!(info.agent_picker_labels(), vec!["Choose my Agent", "ann5agent (already in this room)", "ben2agent"]);
        info.view.drop_down(&mut cx, ids!(my_agents)).set_selected_item(&mut cx, 1);
        assert!(info.selected_agent_in_room(&mut cx));
        assert_eq!(info.selected_agent_status(&mut cx), "ann5agent is already in this room");
        info.view.drop_down(&mut cx, ids!(my_agents)).set_selected_item(&mut cx, 2);
        assert!(!info.selected_agent_in_room(&mut cx), "ben2agent is neither joined nor this room's private Agent");
        // A joined puppet counts too, once the panel's member list is loaded.
        info.data = Some(ChatInfoData {room: RoomNameId::empty(room.clone()), topic: String::new(), mode: None, can_invite: true, banned_members: Default::default(),
            members: vec![crate::profile::user_profile::UserProfile {user_id: user_id!("@_hagency_agt_ben2:server").to_owned(), username: None, avatar_state: crate::shared::avatar::AvatarState::Known(None)}]});
        assert!(info.selected_agent_in_room(&mut cx));
        info.view.drop_down(&mut cx, ids!(my_agents)).set_selected_item(&mut cx, 1);
        loaded.binding = true;loaded.result = Err("room_binding_scope_conflict".into());
        info.agent_busy = true;
        info.finish_agent_request(&mut cx, &loaded);
        assert!(!info.agent_busy && !info.agent_added);
        assert_eq!(info.view.label(&mut cx, ids!(agent_status)).text(), "ann5agent is already in this room");
    }
}
