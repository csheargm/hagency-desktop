//! Owner's private Matrix DM. Unknown creation is recovered by its stable alias,
//! or an exact, unique joined Room, never by repeating createRoom.
//! No messages or provider calls occur here.
use super::*;
use crate::console::server_login::matrix_creations::MatrixOperation;
use std::io::Read;
use tokio::sync::Mutex;
static COMMANDS: Mutex<()> = Mutex::const_new(());
const MARKER: &str = "im.hagency.agent.owner_direct";
mod recovery;
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    version: u32,
    agent: String,
    origin: String,
    issuer: String,
    subject: String,
    owner: String,
    puppet: String,
    room: Option<String>,
    phase: String,
    last_error: Option<String>,
}
fn same(a: &OwnerReply, b: &OwnerReply) -> bool {
    a.origin == b.origin && a.issuer == b.issuer && a.subject == b.subject && a.owner == b.owner
}
fn fenced(depot: &Depot, expected: &OwnerReply, actual: &OwnerReply) -> Result<(), OwnerError> {
    recheck(depot).map_err(|_| error(401, "sign_in_required"))?;
    if !same(expected, actual) {
        return Err(error(401, "sign_in_required"));
    }
    Ok(())
}
fn load(path: &Path) -> Result<Option<Record>, OwnerError> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(error(503, "owner_direct_journal_unavailable")),
        Ok(_) => {}
    }
    let file = hagency_store::private::open(path, false)
        .map_err(|_| error(503, "owner_direct_journal_unavailable"))?;
    let mut raw = vec![];
    file.take(32769)
        .read_to_end(&mut raw)
        .map_err(|_| error(503, "owner_direct_journal_unavailable"))?;
    if raw.len() > 32768 {
        return Err(error(503, "owner_direct_journal_unavailable"));
    }
    let record: Record =
        serde_json::from_slice(&raw).map_err(|_| error(503, "owner_direct_journal_unavailable"))?;
    if record.version != 1 {
        return Err(error(503, "owner_direct_journal_unavailable"));
    }
    Ok(Some(record))
}
fn save(path: &Path, record: &Record) -> Result<(), OwnerError> {
    hagency_store::private::replace(
        path,
        &serde_json::to_vec(record).map_err(|_| error(503, "owner_direct_journal_unavailable"))?,
    )
    .map_err(|_| error(503, "owner_direct_journal_unavailable"))
}
fn validate(state: &Value, owner: &str, agent: &str, puppet: &str) -> Result<(), OwnerError> {
    let events = state
        .as_array()
        .ok_or_else(|| error(409, "owner_direct_room_identity_mismatch"))?;
    let one = |kind: &str| -> Option<&Value> {
        let mut items = events
            .iter()
            .filter(|e| e["type"] == kind && e["state_key"] == "");
        let first = items.next()?;
        if items.next().is_some() {
            None
        } else {
            Some(first)
        }
    };
    let create =
        one("m.room.create").ok_or_else(|| error(409, "owner_direct_room_identity_mismatch"))?;
    let marker = one(MARKER).ok_or_else(|| error(409, "owner_direct_room_identity_mismatch"))?;
    let history = one("m.room.history_visibility")
        .ok_or_else(|| error(409, "owner_direct_room_identity_mismatch"))?;
    let guests = one("m.room.guest_access")
        .ok_or_else(|| error(409, "owner_direct_room_identity_mismatch"))?;
    let joins = one("m.room.join_rules")
        .ok_or_else(|| error(409, "owner_direct_room_identity_mismatch"))?;
    if create["sender"] != owner
        || create["content"].get("type").is_some()
        || create["content"].get("creator").is_some_and(|v| v != owner)
        || marker["sender"] != owner
        || marker["content"] != json!({"version":1,"agentId":agent,"ownerMxid":owner})
        || !matches!(
            history["content"]["history_visibility"].as_str(),
            Some("joined" | "invited" | "shared")
        )
        || guests["content"]["guest_access"] != "forbidden"
        || joins["content"]["join_rule"] != "invite"
        // The owner may turn on Megolm: the Agent's own device on its
        // execution host reads and answers; the server routes ciphertext.
        || events.iter().any(|e| {
            e["type"] == "m.room.encryption"
                && (e["state_key"] != "" || e["content"]["algorithm"] != "m.megolm.v1.aes-sha2")
        })
    {
        return Err(error(409, "owner_direct_room_identity_mismatch"));
    }
    let mut owner_joined = false;
    let mut puppet_present = false;
    for event in events.iter().filter(|e| e["type"] == "m.room.member") {
        let member = event["state_key"]
            .as_str()
            .ok_or_else(|| error(409, "owner_direct_room_identity_mismatch"))?;
        let membership = event["content"]["membership"]
            .as_str()
            .ok_or_else(|| error(409, "owner_direct_room_identity_mismatch"))?;
        owner_joined |= member == owner && membership == "join";
        puppet_present |= member == puppet && matches!(membership, "join" | "invite");
    }
    if !owner_joined || !puppet_present {
        return Err(error(409, "owner_direct_room_identity_mismatch"));
    }
    Ok(())
}
fn merge(mut index: Value, puppet: &str, room: &str) -> Result<(Value, bool), OwnerError> {
    let map = index
        .as_object_mut()
        .ok_or_else(|| error(502, "invalid_direct_index"))?;
    let pair = map.entry(puppet.to_owned()).or_insert_with(|| json!([]));
    let rooms = pair
        .as_array_mut()
        .ok_or_else(|| error(502, "invalid_direct_index"))?;
    if rooms.iter().any(|r| !r.is_string()) {
        return Err(error(502, "invalid_direct_index"));
    }
    let changed = !rooms.iter().any(|r| r == room);
    if changed {
        rooms.push(room.into());
    }
    Ok((index, changed))
}
fn pending(record: &Record) -> Value {
    json!({"ownerDirect":{"roomId":record.room,"bindingId":null,"state":"pending","phase":record.phase,"lastError":record.last_error},"ownerDirectRoomId":record.room})
}
pub(super) async fn ensure(
    req: &mut Request,
    depot: &Depot,
    agent: &str,
) -> Result<Value, OwnerError> {
    if req.uri().query().is_some()
        || !body(req, 1)
            .await
            .map_err(|_| error(400, "invalid_arguments"))?
            .is_empty()
    {
        return Err(error(400, "invalid_arguments"));
    }
    let expected = api(req, depot, OwnerOperation::Agents).await?;
    recheck(depot).map_err(|_| error(401, "sign_in_required"))?;
    let agents: Vec<ServerAgent> = serde_json::from_value(expected.value["agents"].clone())
        .map_err(|_| error(502, "invalid_server_response"))?;
    let mut owned = agents
        .into_iter()
        .find(|a| a.id == agent && !matches!(a.state.as_str(), "retiring" | "retired"))
        .ok_or_else(|| error(403, "owner_scope_required"))?;
    let host = console(depot).map_err(|_| error(503, "local_state_unavailable"))?;
    let root = host
        .0
        .server_login
        .state_directory()
        .ok_or_else(|| error(503, "local_state_unavailable"))?;
    let base = ledger_path(
        &root,
        &expected.origin,
        &expected.issuer,
        &expected.subject,
        &expected.owner,
    )
    .map_err(|_| error(503, "local_state_unavailable"))?;
    let dir = base.parent().unwrap().join("owner-direct");
    hagency_store::private::directory(&dir)
        .map_err(|_| error(503, "owner_direct_journal_unavailable"))?;
    let path = dir.join(format!("{agent}.json"));
    let _guard = COMMANDS.lock().await;
    recheck(depot).map_err(|_| error(401, "sign_in_required"))?;
    let mut record = load(&path)?.unwrap_or(Record {
        version: 1,
        agent: agent.into(),
        origin: expected.origin.clone(),
        issuer: expected.issuer.clone(),
        subject: expected.subject.clone(),
        owner: expected.owner.clone(),
        puppet: owned.puppet_mxid.clone(),
        room: None,
        phase: "new".into(),
        last_error: None,
    });
    if record.agent != agent
        || record.origin != expected.origin
        || record.issuer != expected.issuer
        || record.subject != expected.subject
        || record.owner != expected.owner
        || record.puppet != owned.puppet_mxid
    {
        return Err(error(403, "owner_scope_required"));
    }
    if owned.state == "creating" && record.room.is_none() && record.phase == "new" {
        record.phase = "waiting_identity".into();
        save(&path, &record)?;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    while owned.state == "creating" && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        recheck(depot).map_err(|_| error(401, "sign_in_required"))?;
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let fresh =
            match tokio::time::timeout(remaining, api(req, depot, OwnerOperation::Agents)).await {
                Ok(reply) => reply?,
                Err(_) => break,
            };
        fenced(depot, &expected, &fresh)?;
        let agents: Vec<ServerAgent> = serde_json::from_value(fresh.value["agents"].clone())
            .map_err(|_| error(502, "invalid_server_response"))?;
        owned = agents
            .into_iter()
            .find(|a| a.id == agent && !matches!(a.state.as_str(), "retiring" | "retired"))
            .ok_or_else(|| error(403, "owner_scope_required"))?;
    }
    recheck(depot).map_err(|_| error(401, "sign_in_required"))?;
    if !matches!(owned.state.as_str(), "active" | "paused" | "suspended") {
        if record.room.is_none() && matches!(record.phase.as_str(), "new" | "waiting_identity") {
            record.phase = "waiting_identity".into();
        }
        save(&path, &record)?;
        return Ok(pending(&record));
    }
    let result = advance(
        req,
        depot,
        &expected,
        &mut record,
        &path,
        &owned.display_name,
    )
    .await;
    match result {
        Ok(v) => Ok(v),
        Err(e) if e.status == 401 => Err(e),
        Err(e) => {
            recheck(depot).map_err(|_| error(401, "sign_in_required"))?;
            if e.code == "owner_direct_room_identity_mismatch" {
                record.phase = "blocked".into();
                record.last_error = Some(e.code.clone());
                save(&path, &record)?;
                return Err(e);
            }
            record.last_error = Some(
                if e.code == "owner_direct_room_identity_mismatch" {
                    "owner_direct_room_identity_mismatch"
                } else if record.room.is_none()
                    && !matches!(record.phase.as_str(), "new" | "waiting_identity")
                {
                    "owner_direct_creation_outcome_unknown"
                } else {
                    "owner_direct_unavailable"
                }
                .into(),
            );
            save(&path, &record)?;
            Ok(pending(&record))
        }
    }
}
async fn call_matrix(
    depot: &Depot,
    expected: &OwnerReply,
    cookie: &str,
    op: MatrixOperation,
) -> Result<OwnerReply, OwnerError> {
    let reply = console(depot)
        .map_err(|_| error(503, "local_state_unavailable"))?
        .0
        .server_login
        .matrix_api(cookie, op)
        .await?;
    fenced(depot, expected, &reply)?;
    Ok(reply)
}
async fn advance(
    req: &Request,
    depot: &Depot,
    expected: &OwnerReply,
    record: &mut Record,
    path: &Path,
    name: &str,
) -> Result<Value, OwnerError> {
    let cookie = cookie(req).map_err(|_| error(401, "sign_in_required"))?;
    let known = api(
        req,
        depot,
        OwnerOperation::AgentOwnerDirect {
            agent: record.agent.clone(),
        },
    )
    .await?;
    fenced(depot, expected, &known)?;
    if known.value.get("ownerDirectRoomId").is_none()
        || (!known.value["ownerDirectRoomId"].is_null()
            && !known.value["ownerDirectRoomId"].is_string())
    {
        return Err(error(502, "invalid_server_response"));
    }
    if let Some(room) = known.value["ownerDirectRoomId"].as_str() {
        super::matrix(room, '!')?;
        if record
            .room
            .as_deref()
            .is_some_and(|previous| previous != room)
        {
            return Err(error(409, "owner_direct_room_identity_mismatch"));
        }
        record.room = Some(room.into());
        save(path, record)?;
    }
    if record.room.is_none() {
        match call_matrix(
            depot,
            expected,
            cookie,
            MatrixOperation::DirectAlias {
                agent: record.agent.clone(),
                owner: record.owner.clone(),
            },
        )
        .await
        {
            Ok(alias) => {
                let room = alias.value["room_id"]
                    .as_str()
                    .ok_or_else(|| error(502, "invalid_matrix_response"))?;
                super::matrix(room, '!')?;
                record.room = Some(room.into());
                save(path, record)?;
            }
            Err(e)
                if e.status == 404
                    && matches!(record.phase.as_str(), "new" | "waiting_identity") =>
            {
                recheck(depot).map_err(|_| error(401, "sign_in_required"))?;
                record.phase = "creating".into();
                save(path, record)?;
                let creation = call_matrix(
                    depot,
                    expected,
                    cookie,
                    MatrixOperation::DirectCreate {
                        agent: record.agent.clone(),
                        owner: record.owner.clone(),
                        puppet: record.puppet.clone(),
                        name: name.into(),
                    },
                )
                .await;
                let created = match creation {
                    Ok(reply) => reply,
                    Err(e) => {
                        recheck(depot).map_err(|_| error(401, "sign_in_required"))?;
                        if e.status == 403 && e.code == "matrix_permission_denied" {
                            record.phase = "new".into();
                            save(path, record)?;
                        }
                        return Err(e);
                    }
                };
                let room = created.value["room_id"]
                    .as_str()
                    .ok_or_else(|| error(502, "invalid_matrix_response"))?;
                super::matrix(room, '!')?;
                record.room = Some(room.into());
                record.phase = "created".into();
                save(path, record)?;
            }
            Err(e) if e.status == 404 && record.phase == "creating" => {
                // createRoom can persist state before its alias or response.
                // Recover the same Room only after a complete, fenced scan;
                // never repeat an uncertain POST or guess from its name.
                let room = tokio::time::timeout(
                    std::time::Duration::from_secs(45),
                    recovery::joined_room(depot, expected, cookie, record),
                )
                .await
                .map_err(|_| error(503, "owner_direct_recovery_unavailable"))??
                .ok_or(e)?;
                record.room = Some(room);
                record.phase = "created".into();
                save(path, record)?;
            }
            Err(e) => return Err(e),
        }
    }
    let room = record.room.clone().unwrap();
    let state = call_matrix(
        depot,
        expected,
        cookie,
        MatrixOperation::State(room.clone()),
    )
    .await?;
    validate(&state.value, &record.owner, &record.agent, &record.puppet)?;
    let index = match call_matrix(
        depot,
        expected,
        cookie,
        MatrixOperation::DirectIndexGet(record.owner.clone()),
    )
    .await
    {
        Ok(v) => v.value,
        Err(e) if e.status == 404 => {
            recheck(depot).map_err(|_| error(401, "sign_in_required"))?;
            json!({})
        }
        Err(e) => return Err(e),
    };
    let (index, changed) = merge(index, &record.puppet, &room)?;
    if changed {
        call_matrix(
            depot,
            expected,
            cookie,
            MatrixOperation::DirectIndexPut(record.owner.clone(), index),
        )
        .await?;
    }
    record.phase = "indexed".into();
    save(path, record)?;
    let joined = api(
        req,
        depot,
        OwnerOperation::OwnerDirect {
            agent: record.agent.clone(),
            room: room.clone(),
        },
    )
    .await?;
    fenced(depot, expected, &joined)?;
    let mut result = public(&joined.value, "creation")?;
    let binding = result["creation"]["binding"].clone();
    if binding["agentId"] != record.agent
        || binding["roomId"] != room
        || binding["scopeKind"] != "owner_direct"
        || !binding["projectId"].is_null()
        || joined.value["ownerDirectRoomId"] != room
    {
        return Err(error(502, "invalid_server_response"));
    }
    record.phase = if binding["state"] == "active" {
        "active"
    } else {
        "pending"
    }
    .into();
    record.last_error = None;
    save(path, record)?;
    result["ownerDirectRoomId"] = room.clone().into();
    result["ownerDirect"] = json!({"roomId":room,"bindingId":binding["id"],"state":binding["state"],"phase":record.phase,"lastError":null});
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn state() -> Value {
        json!([
            {"type":"m.room.create","state_key":"","sender":"@owner:test","content":{}},
            {"type":MARKER,"state_key":"","sender":"@owner:test","content":{"version":1,"agentId":"a","ownerMxid":"@owner:test"}},
            {"type":"m.room.history_visibility","state_key":"","content":{"history_visibility":"joined"}},
        {"type":"m.room.guest_access","state_key":"","content":{"guest_access":"forbidden"}},
            {"type":"m.room.join_rules","state_key":"","content":{"join_rule":"invite"}},
            {"type":"m.room.member","state_key":"@owner:test","content":{"membership":"join"}},
            {"type":"m.room.member","state_key":"@puppet:test","content":{"membership":"invite"}}
        ])
    }
    #[test]
    fn direct_state_is_private_owner_bound_and_rejects_foreign_or_unknown_encryption() {
        assert!(validate(&state(), "@owner:test", "a", "@puppet:test").is_ok());
        let mut foreign = state();
        foreign[1]["sender"] = "@other:test".into();
        assert!(validate(&foreign, "@owner:test", "a", "@puppet:test").is_err());
        let mut extra = state();
        extra.as_array_mut().unwrap().push(json!({"type":"m.room.member","state_key":"@other:test","content":{"membership":"join"}}));
        assert!(validate(&extra, "@owner:test", "a", "@puppet:test").is_ok());
        let mut unknown = state();
        unknown
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"m.room.encryption","state_key":"","content":{}}));
        assert!(validate(&unknown, "@owner:test", "a", "@puppet:test").is_err());
        let mut megolm = state();
        megolm
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"m.room.encryption","state_key":"",
            "content":{"algorithm":"m.megolm.v1.aes-sha2"}}));
        assert!(validate(&megolm, "@owner:test", "a", "@puppet:test").is_ok());
        for visibility in ["world_readable", "unexpected"] {
            let mut public = state();
            public[2]["content"]["history_visibility"] = visibility.into();
            assert!(validate(&public, "@owner:test", "a", "@puppet:test").is_err());
        }
        let mut guest = state();
        guest[3]["content"]["guest_access"] = "can_join".into();
        assert!(validate(&guest, "@owner:test", "a", "@puppet:test").is_err());
        let mut space = state();
        space[0]["content"]["type"] = "m.space".into();
        assert!(validate(&space, "@owner:test", "a", "@puppet:test").is_err());
    }
    #[test]
    #[cfg(unix)]
    fn direct_journal_dangling_symlink_is_not_a_fresh_creation() {
        let dir = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = dir.path().join("journal.json");
        std::os::unix::fs::symlink(dir.path().join("absent"), &path).unwrap();
        assert!(load(&path).is_err());
        assert!(load(&dir.path().join("fresh.json")).unwrap().is_none());
    }
    #[test]
    fn dm_index_merge_is_idempotent_and_preserves_unrelated_pairs() {
        let previous = json!({"@other:test":["!unrelated:test"],"@puppet:test":["!older:test"]});
        let (merged, changed) = merge(previous.clone(), "@puppet:test", "!direct:test").unwrap();
        assert!(changed);
        assert_eq!(merged["@other:test"], previous["@other:test"]);
        assert_eq!(
            merged["@puppet:test"],
            json!(["!older:test", "!direct:test"])
        );
        let (again, changed) = merge(merged.clone(), "@puppet:test", "!direct:test").unwrap();
        assert!(!changed);
        assert_eq!(again, merged);
        assert!(
            merge(
                json!({"@puppet:test":"invalid"}),
                "@puppet:test",
                "!direct:test"
            )
            .is_err()
        );
    }
}
