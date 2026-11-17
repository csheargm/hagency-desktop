//! Repair a missing service invitation only after the server has checked the
//! requested binding's owner, Project, Room and creation policy. Never grant
//! power levels, invite arbitrary users, or replace the durable command key.
use super::matrix_creations::{MatrixOperation, same_reply_identity, service_identity};
use super::{OwnerError, OwnerOperation, OwnerReply, ServerLogin};
use serde_json::Value;

impl ServerLogin {
    pub(in crate::console) async fn bind_agent_room(
        &self,
        cookie: &str,
        agent: String,
        project: String,
        room: String,
        command: String,
    ) -> Result<OwnerReply, OwnerError> {
        let operation = || OwnerOperation::Bind {
            agent: agent.clone(),
            project: project.clone(),
            room: room.clone(),
            command: command.clone(),
        };
        match self.owner_api(cookie, operation()).await {
            Err(error) if error.code == "service_invitation_denied" => {}
            result => return result,
        }
        let discovery = self.matrix_api(cookie, MatrixOperation::Discovery).await?;
        let service = service_identity(&discovery.value, &discovery.owner)?;
        let state = self
            .matrix_api(cookie, MatrixOperation::State(room.clone()))
            .await?;
        if !same_reply_identity(&discovery, &state) {
            return Err(OwnerError::new(401, "matrix_account_changed"));
        }
        if needs_invitation(&state.value, &state.owner, &service)? {
            let invited = self
                .matrix_api(
                    cookie,
                    MatrixOperation::InviteService {
                        room: room.clone(),
                        owner: state.owner.clone(),
                        discovery: discovery.value.clone(),
                    },
                )
                .await?;
            if !same_reply_identity(&discovery, &invited) {
                return Err(OwnerError::new(401, "matrix_account_changed"));
            }
        }
        // The server accepts the invitation and reconciles puppet membership.
        // An uncertain invite is recovered by reading membership on the next
        // same-key retry; an accepted binding is recovered by command status.
        self.owner_api(cookie, operation()).await
    }
}

fn needs_invitation(state: &Value, owner: &str, service: &str) -> Result<bool, OwnerError> {
    let events = state
        .as_array()
        .ok_or_else(|| OwnerError::new(502, "invalid_matrix_response"))?;
    let content = |kind: &str, key: &str| {
        events
            .iter()
            .find(|event| event["type"] == kind && event["state_key"] == key)
            .map(|event| &event["content"])
    };
    if content("m.room.member", owner).is_none_or(|c| c["membership"] != "join") {
        return Err(OwnerError::new(403, "room_join_required"));
    }
    let powers = content("m.room.power_levels", "")
        .ok_or_else(|| OwnerError::new(502, "invalid_matrix_response"))?;
    let level = |key: &str, default: i64| -> Result<i64, OwnerError> {
        match powers.get(key) {
            None => Ok(default),
            Some(value) => value
                .as_i64()
                .ok_or_else(|| OwnerError::new(502, "invalid_matrix_response")),
        }
    };
    let invite = level("invite", 0)?;
    let default = level("users_default", 0)?;
    let user_level = |user: &str| -> Result<i64, OwnerError> {
        match powers.get("users").and_then(|users| users.get(user)) {
            None => Ok(default),
            Some(value) => value
                .as_i64()
                .ok_or_else(|| OwnerError::new(502, "invalid_matrix_response")),
        }
    };
    if user_level(service)? < invite {
        return Err(OwnerError::new(
            403,
            "agent_service_invite_permission_required",
        ));
    }
    match content("m.room.member", service).and_then(|c| c["membership"].as_str()) {
        Some("join" | "invite") => Ok(false),
        Some("ban") => Err(OwnerError::new(403, "agent_service_banned")),
        _ if user_level(owner)? < invite => Err(OwnerError::new(403, "matrix_permission_denied")),
        _ => Ok(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use super::super::matrix_creations::tests::fixture_with_status_and_hook;

    /// Private refusal -> explicit owner choice -> same-key retry. The fake
    /// server replays exactly these requests in order, so any other call (a
    /// second Agent, a pool allocation, a Matrix invite, a new key) fails it.
    #[tokio::test]
    async fn private_refusal_then_owner_choice_retries_the_original_bind_only() {
        let bind = "POST /hagency/api/v1/agents/agt_1/bindings";
        let body = json!({"projectId":"prj_1","roomId":"!room:example.test","idempotencyKey":"original"});
        let agent = json!({"id":"agt_1","ownerUserId":"@owner:example.test","puppetMxid":"@_hagency_agt_1:example.test","displayName":"A","state":"active","generation":1,"executionDeviceId":null,"ownerDirectRoomId":"!dm:example.test","visibility":"public"});
        let binding = json!({"id":"bnd_1","agentId":"agt_1","projectId":"prj_1","scopeKind":"project","roomId":"!room:example.test","state":"active","generation":1});
        let (login, task) = fixture_with_status_and_hook(vec![
            (bind, 401, json!({"code":"private_agent_owner_direct_only"}), Some(body.clone())),
            ("PUT /hagency/api/v1/agents/agt_1/visibility", 200, json!({"agent":agent}), Some(json!({"visibility":"public"}))),
            (bind, 200, json!({"creation":{"agent":agent,"binding":binding}}), Some(body)),
        ], |_| {}).await;
        let call = || login.bind_agent_room("cookie", "agt_1".into(), "prj_1".into(), "!room:example.test".into(), "original".into());
        assert_eq!(call().await.err().expect("private refusal").code, "private_agent_owner_direct_only");
        let allowed = login.owner_api("cookie", OwnerOperation::AllowGroupRooms { agent: "agt_1".into() }).await.unwrap();
        assert_eq!(allowed.value["agent"]["id"], "agt_1");
        let retried = call().await.unwrap();
        assert_eq!(retried.value["creation"]["binding"]["state"], "active");
        tokio::time::timeout(std::time::Duration::from_secs(5), task).await.unwrap().unwrap();
    }
    #[test]
    fn service_preparation_respects_membership_and_never_grants_power() {
        let mut state = json!([
            {"type":"m.room.member","state_key":"@owner:test","content":{"membership":"join"}},
            {"type":"m.room.power_levels","state_key":"","content":{"users":{"@owner:test":100},"invite":0}}
        ]);
        assert!(needs_invitation(&state, "@owner:test", "@_hagency_service:test").unwrap());
        state[1]["content"]["invite"] = json!(50);
        assert_eq!(
            needs_invitation(&state, "@owner:test", "@_hagency_service:test")
                .unwrap_err()
                .code,
            "agent_service_invite_permission_required"
        );
        state[1]["content"]["users"]["@_hagency_service:test"] = json!(50);
        assert!(needs_invitation(&state, "@owner:test", "@_hagency_service:test").unwrap());
        state[1]["content"]["users"]["@owner:test"] = json!(0);
        assert_eq!(
            needs_invitation(&state, "@owner:test", "@_hagency_service:test")
                .unwrap_err()
                .code,
            "matrix_permission_denied"
        );
        state.as_array_mut().unwrap().push(json!({"type":"m.room.member","state_key":"@_hagency_service:test","content":{"membership":"ban"}}));
        assert_eq!(
            needs_invitation(&state, "@owner:test", "@_hagency_service:test")
                .unwrap_err()
                .code,
            "agent_service_banned"
        );
        state[0]["content"]["membership"] = json!("leave");
        assert_eq!(
            needs_invitation(&state, "@owner:test", "@_hagency_service:test")
                .unwrap_err()
                .code,
            "room_join_required"
        );
    }
}
