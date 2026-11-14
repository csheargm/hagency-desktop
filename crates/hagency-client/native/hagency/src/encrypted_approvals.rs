//! Private permission details and decisions use Matrix Olm, including when the
//! resource owner is not a room member. The management service pins only salted
//! commitments and retains its existing role, expiry, and single-consumer checks.
use matrix_sdk::{
    Client,
    deserialized_responses::{AlgorithmInfo, EncryptionInfo, VerificationLevel, VerificationState},
    event_handler::EventHandlerHandle,
    ruma::{
        OwnedUserId,
        events::{AnyToDeviceEvent, AnyToDeviceEventContent, ToDeviceEventType},
        serde::Raw,
        to_device::DeviceIdOrAllDevices,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const REQUEST: &str = "im.hagency.shared_approval.request.v1";
const DECISION: &str = "im.hagency.shared_approval.decision.v1";
pub const ALGORITHM: &str = "m.olm.v1.curve25519-aes-sha2";
const MAX_BYTES: usize = 32 * 1024;
type Result<T> = std::result::Result<T, &'static str>;
fn invalid() -> &'static str {
    "invalid_encrypted_approval"
}
fn hash(v: &Value) -> Result<String> {
    hagency_core::canonical::digest(v).map_err(|_| invalid())
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(invalid)
}
fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn nonce() -> Result<String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| invalid())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub fn commitment(request: &Value, nonce: &str) -> Result<String> {
    if !digest(nonce) || serde_json::to_vec(request).map_or(true, |b| b.len() > MAX_BYTES - 2048) {
        return Err(invalid());
    }
    hash(&json!({"nonce":nonce,"request":request}))
}
pub fn metadata(request: &Value, nonce: &str, device: &str) -> Result<Value> {
    if !request["sessionAllowed"].is_boolean() || !request["managerCanDecide"].is_boolean() {
        return Err(invalid());
    }
    Ok(
        json!({"encrypted":{"algorithm":ALGORITHM,"requestDigest":commitment(request,nonce)?,"deviceId":device},
        "sessionAllowed":request["sessionAllowed"],"managerCanDecide":request["managerCanDecide"]}),
    )
}
fn payload(row: &Value) -> Result<&Value> {
    let a = &row["approval"];
    if hash(a)? != string(row, "digest")?
        || a["request"]["encrypted"]["algorithm"] != ALGORITHM
        || !a["expiresAtMs"]
            .as_u64()
            .is_some_and(|t| t > now() && t <= now() + 300_000)
    {
        return Err(invalid());
    }
    for k in ["id", "workId", "agentMxid", "ownerMxid", "managerMxid"] {
        string(a, k)?;
    }
    if requester_decides(a) {
        string(a, "deciderMxid")?;
    }
    string(&a["request"]["encrypted"], "deviceId")?;
    if !digest(string(&a["request"]["encrypted"], "requestDigest")?) {
        return Err(invalid());
    }
    Ok(a)
}
fn validate_details(a: &Value, request: &Value, nonce: &str) -> Result<()> {
    if commitment(request, nonce)? != string(&a["request"]["encrypted"], "requestDigest")?
        || request["sessionAllowed"] != a["request"]["sessionAllowed"]
        || request["managerCanDecide"] != a["request"]["managerCanDecide"]
        || !matches!(
            request["method"].as_str(),
            Some(
                "item/commandExecution/requestApproval"
                    | "item/fileChange/requestApproval"
                    | "item/permissions/requestApproval"
                    | "claude/can_use_tool"
                    | "octos/can_use_tool"
            )
        )
        || !request["params"].is_object()
        || matches!(request["method"].as_str(), Some("claude/can_use_tool" | "octos/can_use_tool"))
            && (a["sessionScope"] != if request["method"] == "octos/can_use_tool" { "current_octos_session" } else { "current_claude_session" }
                || request["sessionAllowed"] != false
                // The sandboxed native tool is delegated exactly when the
                // person who sent the message decides it; never for Octos.
                || request["managerCanDecide"] != requester_decides(a)
                || request["method"] == "octos/can_use_tool" && requester_decides(a)
                || request.as_object().is_none_or(|v| {
                    v.len() != 5
                        || v.keys().any(|k| {
                            !matches!(
                                k.as_str(),
                                "id" | "method" | "params" | "sessionAllowed" | "managerCanDecide"
                            )
                        })
                })
                || request["params"].as_object().is_none_or(|v| {
                    v.len() != 3
                        || v.keys()
                            .any(|k| !matches!(k.as_str(), "sessionId" | "toolName" | "input"))
                })
                || !request["params"]["input"].is_object()
                || [
                    request.get("id"),
                    request["params"].get("sessionId"),
                    request["params"].get("toolName"),
                ]
                .iter()
                .any(|v| {
                    v.and_then(Value::as_str).is_none_or(|s| {
                        s.is_empty() || s.len() > 255 || s.chars().any(char::is_control)
                    })
                }))
    {
        return Err(invalid());
    }
    Ok(())
}
/// The server names the person who sent the message as `deciderMxid` when the
/// owner's daemon gave them the request. Then only they decide; the owner is
/// neither asked nor sent the details.
fn requester_decides(a: &Value) -> bool {
    a.get("deciderMxid").is_some()
}
fn may_decide(a: &Value, sender: &str, decision: &str) -> bool {
    let decider = if requester_decides(a) {
        a["deciderMxid"] == sender && a["request"]["managerCanDecide"] == true
    } else {
        a["ownerMxid"] == sender
            || a["managerMxid"] == sender && a["request"]["managerCanDecide"] == true
    };
    decider
        && matches!(decision, "approve_once" | "approve_session" | "deny")
        && (decision != "approve_session" || a["request"]["sessionAllowed"] == true)
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Message {
    version: u16,
    id: String,
    approval_digest: String,
    expires_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nonce: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decision: Option<String>,
}
#[derive(Clone)]
struct Received {
    sender: String,
    device: String,
    kind: String,
    message: Message,
    received: Instant,
}
fn receive(raw: &Raw<AnyToDeviceEvent>, info: Option<&EncryptionInfo>) -> Option<Received> {
    if raw.json().get().len() > MAX_BYTES {
        return None;
    }
    let info = info?;
    if !matches!(
        info.algorithm_info,
        AlgorithmInfo::OlmV1Curve25519AesSha2 { .. }
    ) || matches!(
        info.verification_state,
        VerificationState::Unverified(
            VerificationLevel::None(_)
                | VerificationLevel::MismatchedSender
                | VerificationLevel::VerificationViolation
        )
    ) {
        return None;
    }
    let value: Value = raw.deserialize_as().ok()?;
    let kind = value["type"].as_str()?;
    if !matches!(kind, REQUEST | DECISION) || value["sender"] != info.sender.as_str() {
        return None;
    }
    let m: Message = serde_json::from_value(value["content"].clone()).ok()?;
    if m.version != 1
        || m.id.len() > 128
        || !m.id.starts_with("sap_")
        || !digest(&m.approval_digest)
        || m.expires_at_ms <= now()
        || m.expires_at_ms > now() + 300_000
        || if kind == REQUEST {
            m.request.is_none() || m.nonce.is_none() || m.decision.is_some()
        } else {
            m.request.is_some()
                || m.nonce.is_some()
                || !matches!(
                    m.decision.as_deref(),
                    Some("approve_once" | "approve_session" | "deny")
                )
        }
    {
        return None;
    }
    Some(Received {
        sender: info.sender.to_string(),
        device: info.sender_device.as_ref()?.to_string(),
        kind: kind.into(),
        message: m,
        received: Instant::now(),
    })
}
/// Per-SDK-client, bounded and volatile. Retiring a session drops its handler and
/// pending details. The executor resends pending requests for late receivers.
pub struct Inbox {
    client: Client,
    handle: EventHandlerHandle,
    entries: Arc<Mutex<Vec<Received>>>,
}
impl Drop for Inbox {
    fn drop(&mut self) {
        self.client.remove_event_handler(self.handle.clone());
    }
}
impl Inbox {
    pub fn attach(client: &Client) -> Self {
        let entries = Arc::new(Mutex::new(Vec::<Received>::new()));
        let pending = entries.clone();
        let handle = client.add_event_handler(
            move |event: Raw<AnyToDeviceEvent>, info: Option<EncryptionInfo>| {
                let pending = pending.clone();
                async move {
                    let Some(value) = receive(&event, info.as_ref()) else {
                        return;
                    };
                    let Ok(mut entries) = pending.lock() else {
                        return;
                    };
                    entries.retain(|e| {
                        e.received.elapsed() < Duration::from_secs(150)
                            && e.message.expires_at_ms > now()
                    });
                    entries.retain(|e| {
                        !(e.kind == value.kind
                            && e.sender == value.sender
                            && e.device == value.device
                            && e.message.id == value.message.id
                            && e.message.approval_digest == value.message.approval_digest
                            && e.message.decision == value.message.decision)
                    });
                    if entries.len() >= 256 {
                        entries.remove(0);
                    }
                    entries.push(value);
                }
            },
        );
        Self {
            client: client.clone(),
            handle,
            entries,
        }
    }
    fn matching(&self, row: &Value, kind: &str) -> Result<Vec<Received>> {
        let a = payload(row)?;
        let entries = self.entries.lock().map_err(|_| invalid())?;
        Ok(entries
            .iter()
            .filter(|e| {
                e.kind == kind
                    && e.message.id == a["id"]
                    && e.message.approval_digest == row["digest"]
                    && e.message.expires_at_ms == a["expiresAtMs"]
                    && e.received.elapsed() < Duration::from_secs(150)
            })
            .cloned()
            .collect())
    }
    pub fn details(&self, row: &Value) -> Result<Option<Value>> {
        let a = payload(row)?;
        for e in self.matching(row, REQUEST)? {
            if e.sender != a["agentMxid"] || e.device != a["request"]["encrypted"]["deviceId"] {
                continue;
            }
            let (Some(request), Some(nonce)) = (e.message.request, e.message.nonce) else {
                continue;
            };
            if validate_details(a, &request, &nonce).is_ok() {
                return Ok(Some(request));
            }
        }
        Ok(None)
    }
    pub fn has_decision(&self, row: &Value) -> Result<bool> {
        let a = payload(row)?;
        Ok(self
            .matching(row, DECISION)?
            .iter()
            .any(|e| may_decide(a, &e.sender, e.message.decision.as_deref().unwrap_or(""))))
    }
    pub fn matches_decision(&self, row: &Value, sender: &str, decision: &str) -> Result<bool> {
        let a = payload(row)?;
        Ok(may_decide(a, sender, decision)
            && self
                .matching(row, DECISION)?
                .iter()
                .any(|e| e.sender == sender && e.message.decision.as_deref() == Some(decision)))
    }
}
async fn send(
    client: &Client,
    kind: &str,
    recipients: BTreeMap<OwnedUserId, Vec<DeviceIdOrAllDevices>>,
    message: Message,
) -> Result<()> {
    // Owners can be outside every shared room: explicitly query their signed
    // device keys instead of relying on room membership's tracked-user cache.
    for user in recipients.keys() {
        client
            .encryption()
            .request_user_identity(user)
            .await
            .map_err(|_| "encrypted_approval_keys_pending")?;
        if client
            .encryption()
            .get_user_devices(user)
            .await
            .map_err(|_| invalid())?
            .devices()
            .next()
            .is_none()
        {
            return Err("encrypted_approval_keys_pending");
        }
    }
    let value = serde_json::to_value(message).map_err(|_| invalid())?;
    let raw: Raw<AnyToDeviceEventContent> =
        Raw::from_json(serde_json::value::to_raw_value(&value).map_err(|_| invalid())?);
    let failed = client
        .send_encrypted_to_device(&ToDeviceEventType::from(kind), recipients, raw)
        .await
        .map_err(|_| "encrypted_approval_delivery_pending")?;
    if !failed.is_empty() {
        return Err("encrypted_approval_delivery_pending");
    }
    Ok(())
}
fn message(row: &Value) -> Result<Message> {
    let a = payload(row)?;
    Ok(Message {
        version: 1,
        id: string(a, "id")?.into(),
        approval_digest: string(row, "digest")?.into(),
        expires_at_ms: a["expiresAtMs"].as_u64().ok_or_else(invalid)?,
        request: None,
        nonce: None,
        decision: None,
    })
}
pub async fn send_request(
    client: &Client,
    row: &Value,
    request: &Value,
    nonce: &str,
) -> Result<()> {
    let a = payload(row)?;
    validate_details(a, request, nonce)?;
    if client.user_id().map(|u| u.as_str()) != a["agentMxid"].as_str()
        || client.device_id().map(|d| d.as_str()) != a["request"]["encrypted"]["deviceId"].as_str()
    {
        return Err(invalid());
    }
    let recipient = |key| -> Result<_> {
        Ok(BTreeMap::from([(
            OwnedUserId::try_from(string(a, key)?).map_err(|_| invalid())?,
            vec![DeviceIdOrAllDevices::AllDevices],
        )]))
    };
    let mut m = message(row)?;
    m.request = Some(request.clone());
    m.nonce = Some(nonce.into());
    if requester_decides(a) {
        return send(client, REQUEST, recipient("deciderMxid")?, m).await;
    }
    let owner = recipient("ownerMxid")?;
    let manager = recipient("managerMxid")?;
    if owner == manager {
        return send(client, REQUEST, owner, m).await;
    }
    // A manager using only the web client may have no Matrix device keys. Send
    // independently so this cannot prevent delivery to the owner's desktop.
    let (owner, manager) = tokio::join!(
        send(client, REQUEST, owner, m.clone()),
        send(client, REQUEST, manager, m),
    );
    if owner.is_ok() || manager.is_ok() {
        Ok(())
    } else {
        Err("encrypted_approval_delivery_pending")
    }
}

pub async fn send_decision(client: &Client, row: &Value, decision: &str) -> Result<()> {
    let a = payload(row)?;
    if !may_decide(a, client.user_id().ok_or_else(invalid)?.as_str(), decision) {
        return Err(invalid());
    }
    let target = OwnedUserId::try_from(string(a, "agentMxid")?).map_err(|_| invalid())?;
    let recipients = BTreeMap::from([(
        target,
        vec![DeviceIdOrAllDevices::DeviceId(
            string(&a["request"]["encrypted"], "deviceId")?.into(),
        )],
    )]);
    let mut m = message(row)?;
    m.decision = Some(decision.into());
    send(client, DECISION, recipients, m).await
}

#[cfg(test)]
#[path = "encrypted_approvals_live_tests.rs"]
mod live_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::deserialized_responses::ProcessedToDeviceEvent;
    use matrix_sdk::ruma::{
        api::client::keys::{claim_keys, get_keys},
        device_id, user_id,
    };
    use matrix_sdk_crypto::{
        CollectStrategy, DecryptionSettings, EncryptionSyncChanges, OlmMachine, TrustRequirement,
        types::requests::AnyOutgoingRequest,
    };

    fn fixture() -> (Value, Value, String) {
        let request = json!({"id":7,"method":"item/commandExecution/requestApproval","params":{"threadId":"t","turnId":"turn","itemId":"item","command":"private-command-with-secret"},"sessionAllowed":true,"managerCanDecide":false});
        let nonce = nonce().unwrap();
        let approval = json!({"id":"sap_test","workId":"work_test","agentMxid":"@agent:test.local","ownerMxid":"@owner:test.local","managerMxid":"@manager:test.local","expiresAtMs":now()+120000,"request":metadata(&request,&nonce,"HAGE2EEAGENT").unwrap()});
        (
            json!({"digest":hash(&approval).unwrap(),"approval":approval}),
            request,
            nonce,
        )
    }
    #[tokio::test]
    async fn shared_claude_encrypted_approval_keeps_owner_and_closed_native_request() {
        let (mut row,_,salt)=fixture();
        let request=json!({"id":"native-request","method":"claude/can_use_tool","params":{"sessionId":"native-session","toolName":"Bash","input":{"command":"private-command"}},"sessionAllowed":false,"managerCanDecide":false});
        row["approval"]["request"]=metadata(&request,&salt,"HAGE2EEAGENT").unwrap();
        row["approval"]["sessionScope"]=json!("current_claude_session");
        row["digest"]=json!(hash(&row["approval"]).unwrap());
        let (agent,owner)=pair().await;
        let mut message=message(&row).unwrap();message.request=Some(request.clone());message.nonce=Some(salt.clone());
        let(raw,info)=roundtrip(&agent,&owner,REQUEST,&message).await;
        let card=inbox().await;
        card.entries.lock().unwrap().push(receive(&raw,Some(&info)).unwrap());
        assert_eq!(card.details(&row).unwrap(),Some(request.clone()));
        assert!(may_decide(&row["approval"],"@owner:test.local","approve_once"));
        assert!(!may_decide(&row["approval"],"@owner:test.local","approve_session"));
        assert!(!may_decide(&row["approval"],"@manager:test.local","approve_once"));
        for(pointer,value) in [("/sessionAllowed",json!(true)),("/managerCanDecide",json!(true)),("/id",json!(7)),("/method",json!("request_user_input")),("/params/input",json!("ls"))] {
            let mut changed=request.clone();*changed.pointer_mut(pointer).unwrap()=value;
            let mut approval=row["approval"].clone();approval["request"]=metadata(&changed,&salt,"HAGE2EEAGENT").unwrap();
            assert!(validate_details(&approval,&changed,&salt).is_err(),"{pointer}");
        }
        let mut changed=request;changed["params"]["threadId"]=json!("codex-thread");
        let mut approval=row["approval"].clone();approval["request"]=metadata(&changed,&salt,"HAGE2EEAGENT").unwrap();
        assert!(validate_details(&approval,&changed,&salt).is_err());
    }
    #[tokio::test]
    async fn shared_octos_encrypted_approval_binds_owner_and_session_scope() {
        let (mut row, _, salt) = fixture();
        let request = json!({"id":"approval-1","method":"octos/can_use_tool",
            "params":{"sessionId":"session-1","toolName":"shell","input":{"body":"private command"}},
            "sessionAllowed":false,"managerCanDecide":false});
        row["approval"]["request"] = metadata(&request, &salt, "HAGE2EEAGENT").unwrap();
        row["approval"]["sessionScope"] = json!("current_octos_session");
        row["digest"] = json!(hash(&row["approval"]).unwrap());
        let (agent, owner) = pair().await;
        let mut msg = message(&row).unwrap();
        msg.request = Some(request.clone());
        msg.nonce = Some(salt.clone());
        let (raw, info) = roundtrip(&agent, &owner, REQUEST, &msg).await;
        let card = inbox().await;
        card.entries.lock().unwrap().push(receive(&raw, Some(&info)).unwrap());
        assert_eq!(card.details(&row).unwrap(), Some(request.clone()));
        assert!(may_decide(&row["approval"], "@owner:test.local", "approve_once"));
        assert!(!may_decide(&row["approval"], "@manager:test.local", "approve_once"));
        assert!(!may_decide(&row["approval"], "@owner:test.local", "approve_session"));
        let mut wrong = row["approval"].clone();
        wrong["sessionScope"] = json!("current_claude_session");
        assert!(validate_details(&wrong, &request, &salt).is_err());
    }
    #[test]
    fn a_request_the_sender_decides_is_answered_only_by_that_sender() {
        let (mut row, _, salt) = fixture();
        let request = json!({"id":"native-request","method":"claude/can_use_tool","params":{"sessionId":"native-session","toolName":"Bash","input":{"command":"private-command"}},"sessionAllowed":false,"managerCanDecide":true});
        row["approval"]["request"] = metadata(&request, &salt, "HAGE2EEAGENT").unwrap();
        row["approval"]["sessionScope"] = json!("current_claude_session");
        row["approval"]["deciderMxid"] = json!("@sender:test.local");
        row["digest"] = json!(hash(&row["approval"]).unwrap());
        let a = payload(&row).unwrap();
        validate_details(a, &request, &salt).unwrap();
        assert!(may_decide(a, "@sender:test.local", "approve_once"));
        assert!(may_decide(a, "@sender:test.local", "deny"));
        assert!(!may_decide(a, "@sender:test.local", "approve_session"));
        for other in ["@owner:test.local", "@manager:test.local", "@stranger:test.local"] {
            assert!(!may_decide(a, other, "approve_once"), "{other}");
            assert!(!may_decide(a, other, "deny"), "{other}");
        }
        // Without a named decider the native tool stays the owner's.
        let mut closed = request.clone();
        closed["managerCanDecide"] = json!(false);
        let mut approval = a.clone();
        approval["request"] = metadata(&closed, &salt, "HAGE2EEAGENT").unwrap();
        assert!(validate_details(&approval, &closed, &salt).is_err());
        assert!(!may_decide(&approval, "@sender:test.local", "approve_once"));
        // A decider row must name someone.
        let mut nameless = row.clone();
        nameless["approval"]["deciderMxid"] = json!("");
        nameless["digest"] = json!(hash(&nameless["approval"]).unwrap());
        assert!(payload(&nameless).is_err());
    }
    async fn inbox() -> Inbox {
        Inbox::attach(
            &Client::builder()
                .homeserver_url("http://localhost:9")
                .build()
                .await
                .unwrap(),
        )
    }
    async fn pair() -> (OlmMachine, OlmMachine) {
        let agent =
            OlmMachine::new(user_id!("@agent:test.local"), device_id!("HAGE2EEAGENT")).await;
        let owner = OlmMachine::new(user_id!("@owner:test.local"), device_id!("OWNER")).await;
        let mut keys = json!({});
        let mut one_time = None;
        for machine in [&agent, &owner] {
            for outgoing in machine.outgoing_requests().await.unwrap() {
                if let AnyOutgoingRequest::KeysUpload(upload) = outgoing.request() {
                    keys[machine.user_id().as_str()][machine.device_id().as_str()] =
                        serde_json::to_value(&upload.device_keys).unwrap();
                    if machine.user_id() == owner.user_id() {
                        one_time = Some(upload.one_time_keys.clone());
                    }
                }
            }
        }
        let mut query = get_keys::v3::Response::new();
        query.device_keys = serde_json::from_value(keys).unwrap();
        for machine in [&agent, &owner] {
            let (id, _) = machine.query_keys_for_users([agent.user_id(), owner.user_id()]);
            machine.mark_request_as_sent(&id, &query).await.unwrap();
        }
        let (id, _) = agent
            .get_missing_sessions(std::iter::once(owner.user_id()))
            .await
            .unwrap()
            .unwrap();
        let otk = one_time.unwrap().into_iter().next().unwrap();
        let response = claim_keys::v3::Response::new(BTreeMap::from([(
            owner.user_id().to_owned(),
            BTreeMap::from([(owner.device_id().to_owned(), BTreeMap::from([otk]))]),
        )]));
        agent.mark_request_as_sent(&id, &response).await.unwrap();
        (agent, owner)
    }
    async fn roundtrip(
        sender: &OlmMachine,
        receiver: &OlmMachine,
        kind: &str,
        content: &Message,
    ) -> (Raw<AnyToDeviceEvent>, EncryptionInfo) {
        let device = sender
            .get_device(receiver.user_id(), receiver.device_id(), None)
            .await
            .unwrap()
            .unwrap();
        let wire = device
            .encrypt_event_raw(
                kind,
                &serde_json::to_value(content).unwrap(),
                CollectStrategy::AllDevices,
            )
            .await
            .unwrap();
        assert!(!wire.json().get().contains("private-command-with-secret"));
        assert!(!wire.json().get().contains("approve_once"));
        let event = serde_json::from_value(
            json!({"sender":sender.user_id(),"type":"m.room.encrypted","content":wire}),
        )
        .unwrap();
        let (events, _) = receiver
            .receive_sync_changes(
                EncryptionSyncChanges {
                    to_device_events: vec![event],
                    changed_devices: &Default::default(),
                    one_time_keys_counts: &Default::default(),
                    unused_fallback_keys: None,
                    next_batch_token: None,
                },
                &DecryptionSettings {
                    sender_device_trust_requirement: TrustRequirement::Untrusted,
                },
            )
            .await
            .unwrap();
        let ProcessedToDeviceEvent::Decrypted {
            raw,
            encryption_info,
        } = events.into_iter().next().unwrap()
        else {
            panic!("Olm event must decrypt")
        };
        (raw, encryption_info)
    }
    #[tokio::test]
    async fn encrypted_approval_olm_roundtrip_without_room_and_authenticated_decision() {
        let (agent, owner) = pair().await;
        let (row, request, nonce) = fixture();
        let mut m = message(&row).unwrap();
        m.request = Some(request.clone());
        m.nonce = Some(nonce);
        let (raw, info) = roundtrip(&agent, &owner, REQUEST, &m).await;
        assert!(
            receive(&raw, None).is_none(),
            "plaintext spoofing cannot fill the card"
        );
        let card = inbox().await;
        let received = receive(&raw, Some(&info)).unwrap();
        card.entries.lock().unwrap().push(received.clone());
        assert_eq!(card.details(&row).unwrap(), Some(request.clone()));
        let mut wrong = received.clone();
        wrong.device = "OTHER".into();
        card.entries.lock().unwrap().clear();
        card.entries.lock().unwrap().push(wrong);
        assert!(card.details(&row).unwrap().is_none());
        let mut wrong = received.clone();
        wrong.message.request.as_mut().unwrap()["params"]["command"] = json!("changed command");
        card.entries.lock().unwrap().clear();
        card.entries.lock().unwrap().push(wrong);
        assert!(card.details(&row).unwrap().is_none());
        let mut changed = row.clone();
        changed["approval"]["workId"] = json!("other_work");
        assert!(card.details(&changed).is_err());
        let mut decision = message(&row).unwrap();
        decision.decision = Some("approve_once".into());
        let (raw, info) = roundtrip(&owner, &agent, DECISION, &decision).await;
        let executor = inbox().await;
        executor
            .entries
            .lock()
            .unwrap()
            .push(receive(&raw, Some(&info)).unwrap());
        assert!(executor.has_decision(&row).unwrap());
        assert!(
            executor
                .matches_decision(&row, "@owner:test.local", "approve_once")
                .unwrap()
        );
        assert!(
            !executor
                .matches_decision(&row, "@manager:test.local", "approve_once")
                .unwrap()
        );
        assert!(
            !executor
                .matches_decision(&row, "@owner:test.local", "approve_session")
                .unwrap()
        );
        let mut expired = row;
        expired["approval"]["expiresAtMs"] = json!(1);
        expired["digest"] = json!(hash(&expired["approval"]).unwrap());
        assert!(executor.has_decision(&expired).is_err());
    }
    #[test]
    fn encrypted_approval_commitments_hide_plaintext_and_pin_policy() {
        let (row, request, salt) = fixture();
        let a = &row["approval"];
        assert!(!row.to_string().contains("private-command"));
        assert!(!row.to_string().contains(&salt));
        assert_ne!(
            commitment(&request, &salt).unwrap(),
            commitment(&request, &nonce().unwrap()).unwrap()
        );
        let mut changed = request;
        changed["sessionAllowed"] = json!(false);
        assert!(validate_details(a, &changed, &salt).is_err());
        assert!(!may_decide(a, "@manager:test.local", "approve_once"));
        let mut delegated = a.clone();
        delegated["request"]["managerCanDecide"] = json!(true);
        delegated["request"]["sessionAllowed"] = json!(false);
        assert!(may_decide(
            &delegated,
            "@manager:test.local",
            "approve_once"
        ));
        assert!(!may_decide(
            &delegated,
            "@manager:test.local",
            "approve_session"
        ));
        assert!(!may_decide(&delegated, "@stranger:test.local", "deny"));
    }
}
