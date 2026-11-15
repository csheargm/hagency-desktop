//! Registers a joined Room under the owner's own Agent space so the owner can
//! add their Agents to it, including a Room hosted on another homeserver
//! (ADR 0001 of hagency-server: any joined member registers a Room under their
//! own Project). The link is an `m.space.child` in the owner's own Space, so it
//! needs no power in the Room itself; the binding is then created on the
//! owner's own Hagency server.
use std::future::Future;

use serde_json::Value;

use crate::hagency::{backend::Service, model::Command};

/// The lookup error that means "no Project of this owner lists this Room".
pub const REGISTRATION_REQUIRED: &str = "room_project_registration_required";

/// The owner's Project to register a Room under: the first active one with a
/// Space, in the server's order. `None` when the owner has none yet.
pub fn project_to_register(projects: &Value) -> Option<(String, String)> {
    projects["projects"].as_array()?.iter().find_map(|row| {
        let id = row["id"].as_str().filter(|id| !id.is_empty())?;
        let space = row["spaceId"].as_str().filter(|id| id.starts_with('!'))?;
        (row["active"] == true).then(|| (id.to_owned(), space.to_owned()))
    })
}

/// Looks up the Room's Agent context; when the Room is not yet registered,
/// links it under the owner's first Project Space, adopts it, and looks again.
/// Every other lookup error is returned unchanged. The closures keep the order
/// testable without a Matrix client or a server.
pub async fn context_registering<Lk, LkF, Pr, PrF, Ln, LnF, Ad, AdF>(
    room: &str, lookup: Lk, projects: Pr, link: Ln, adopt: Ad,
) -> Result<Value, String>
where
    Lk: Fn() -> LkF, LkF: Future<Output = Result<Value, String>>,
    Pr: FnOnce() -> PrF, PrF: Future<Output = Result<Value, String>>,
    Ln: FnOnce(String) -> LnF, LnF: Future<Output = Result<(), String>>,
    Ad: FnOnce(Command) -> AdF, AdF: Future<Output = Result<Value, String>>,
{
    match lookup().await {
        Err(error) if error == REGISTRATION_REQUIRED => {}
        other => return other,
    }
    let (project_id, space) = project_to_register(&projects().await?)
        .ok_or_else(|| REGISTRATION_REQUIRED.to_owned())?;
    link(space).await?;
    adopt(Command::AdoptRoom { project_id, room_id: room.to_owned() }).await?;
    lookup().await
}

/// [`context_registering`] against the signed-in Matrix client and owner.
pub async fn agents_for_room(service: &Service, room: &str) -> Result<Value, String> {
    context_registering(
        room,
        || service.agents_for_registered_room(room),
        || service.execute(Command::Projects),
        |space| async move {
            let client = crate::sliding_sync::get_client().ok_or("matrix_sign_in_required")?;
            let parent = matrix_sdk::ruma::OwnedRoomId::try_from(space.as_str()).map_err(|_| "invalid_room_id")?;
            let child = matrix_sdk::ruma::OwnedRoomId::try_from(room).map_err(|_| "invalid_room_id")?;
            // A missing backlink in a Room on another server is only advisory.
            crate::home::space_management::link(&client, &parent, &child, true)
                .await.map(|_| ()).map_err(|e| format!("room_link_failed: {e}"))
        },
        |command| service.execute(command),
    ).await
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use serde_json::json;

    use super::*;

    const ROOM: &str = "!ctbLMBDnHvJKrhmRRF:localhost:8450";

    fn projects() -> Value {
        json!({"projects":[
            {"id":"prj_off","spaceId":"!off:localhost:8452","active":false},
            {"id":"prj_ben","spaceId":"!ben:localhost:8452","active":true}]})
    }

    #[test]
    fn picks_the_first_active_project_with_a_space() {
        assert_eq!(project_to_register(&projects()), Some(("prj_ben".into(), "!ben:localhost:8452".into())));
        assert_eq!(project_to_register(&json!({"projects":[]})), None);
        assert_eq!(project_to_register(&json!({})), None);
    }

    /// User flow: Ben, on beta, opens Add agent in a Room hosted on alpha that
    /// none of his Projects lists. The Room is linked under his own Space,
    /// adopted on his own server, and the picker gets his Agents.
    #[tokio::test]
    async fn a_remote_room_is_registered_under_the_owners_space_then_listed() {
        let calls = RefCell::new(Vec::new());
        let registered = RefCell::new(false);
        let result = context_registering(ROOM,
            || { let done = *registered.borrow(); async move {
                if done { Ok(json!({"roomId":ROOM,"projectId":"prj_ben","agents":[{"id":"agt_ben"}]})) }
                else { Err(REGISTRATION_REQUIRED.to_owned()) } } },
            || async { Ok(projects()) },
            |space| { calls.borrow_mut().push(format!("link {space}")); async { Ok(()) } },
            |command| {
                let Command::AdoptRoom { project_id, room_id } = command else { panic!("only adopt") };
                calls.borrow_mut().push(format!("adopt {project_id} {room_id}"));
                *registered.borrow_mut() = true;
                async { Ok(json!({})) }
            },
        ).await.unwrap();
        assert_eq!(result["projectId"], "prj_ben");
        assert_eq!(result["agents"][0]["id"], "agt_ben");
        assert_eq!(*calls.borrow(), vec![
            "link !ben:localhost:8452".to_owned(), format!("adopt prj_ben {ROOM}")]);
    }

    #[tokio::test]
    async fn no_project_keeps_the_create_an_agent_error_and_touches_nothing() {
        let result = context_registering(ROOM,
            || async { Err(REGISTRATION_REQUIRED.to_owned()) },
            || async { Ok(json!({"projects":[]})) },
            |_| async { panic!("no link without a Project") },
            |_| async { panic!("no adopt without a Project") },
        ).await;
        assert_eq!(result.unwrap_err(), REGISTRATION_REQUIRED);
    }

    #[tokio::test]
    async fn other_lookup_errors_and_link_failures_are_not_hidden() {
        let lookup = context_registering(ROOM,
            || async { Err("room_join_required".to_owned()) },
            || async { panic!("no project read") },
            |_| async { panic!("no link") }, |_| async { panic!("no adopt") },
        ).await;
        assert_eq!(lookup.unwrap_err(), "room_join_required");
        let link = context_registering(ROOM,
            || async { Err(REGISTRATION_REQUIRED.to_owned()) },
            || async { Ok(projects()) },
            |_| async { Err("room_link_failed: forbidden".to_owned()) },
            |_| async { panic!("no adopt after a failed link") },
        ).await;
        assert_eq!(link.unwrap_err(), "room_link_failed: forbidden");
    }
}
