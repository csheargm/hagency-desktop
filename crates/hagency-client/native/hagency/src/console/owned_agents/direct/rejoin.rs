//! Brings the owner back into their Agent's private chat after they left it
//! (server ADR 0003). The server's puppet re-invites the owner, or swaps in a
//! replacement Room and invites them there; the owner then joins here. The
//! server's answer is the Agent's current Room, so the journal follows it.
use super::*;

/// The server's recovery outcomes after which the owner has to join.
const JOIN_OUTCOMES: [&str; 3] = ["invited", "reinvited", "replaced"];

/// Before the usual checks, asks the server to recover the owner's access and
/// joins the Room it names. A server without recovery changes nothing.
pub(super) async fn rejoin(
    req: &Request,
    depot: &Depot,
    expected: &OwnerReply,
    cookie: &str,
    record: &mut Record,
    path: &Path,
) -> Result<(), OwnerError> {
    let reply = match api(req, depot, OwnerOperation::OwnerDirectRecover { agent: record.agent.clone() }).await {
        Ok(reply) => reply,
        // Servers before owner-direct-recovery-v1 refuse the unknown route.
        Err(e) if e.status == 400 && e.code == "unsupported_operation" => {
            recheck(depot).map_err(|_| error(401, "sign_in_required"))?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    fenced(depot, expected, &reply)?;
    let outcome = reply.value["outcome"].as_str().ok_or_else(|| error(502, "invalid_server_response"))?;
    let room = match (outcome, &reply.value["ownerDirectRoomId"]) {
        ("none", Value::Null) => return Ok(()),
        ("joined", Value::String(room)) => room.clone(),
        (o, Value::String(room)) if JOIN_OUTCOMES.contains(&o) => room.clone(),
        _ => return Err(error(502, "invalid_server_response")),
    };
    super::super::matrix(&room, '!')?;
    // The server's current Room wins over the journal, also when a reply was
    // lost after a replacement; `validate` still checks the Room's state.
    if record.room.as_deref() != Some(room.as_str()) {
        record.room = Some(room.clone());
        record.phase = "created".into();
        save(path, record)?;
    }
    if JOIN_OUTCOMES.contains(&outcome) {
        call_matrix(depot, expected, cookie, MatrixOperation::Join(room)).await?;
    }
    Ok(())
}
