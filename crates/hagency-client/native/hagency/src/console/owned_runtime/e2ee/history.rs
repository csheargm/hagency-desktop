use super::*;
use matrix_sdk::{deserialized_responses::{DeviceLinkProblem, TimelineEvent}, room::MessagesOptions};
use serde_json::{Value, json};

pub(in crate::console) struct HistoryBatch {
    pub events: Vec<Value>,
    pub next: Option<String>,
}

impl AgentDevice {
    pub(in crate::console) async fn history_request(&self, room: &str, event: &str) -> Result<Option<Value>, RuntimeError> {
        let room_id = OwnedRoomId::try_from(room).map_err(|_| RuntimeError::Request)?;
        let event_id = OwnedEventId::try_from(event).map_err(|_| RuntimeError::Request)?;
        let room = self.client.get_room(&room_id).ok_or(RuntimeError::Transport)?;
        let event = room.event(&event_id, None).await.map_err(|_| RuntimeError::Transport)?;
        if event.kind.parse_event_id().as_ref() != Some(&event_id) { return Err(RuntimeError::Transport); }
        Ok(history_event(&event))
    }
    /// The SDK enforces this Agent's membership/history visibility and decrypts
    /// with its own keys. The caller pins the room and anchor from admitted work.
    pub(in crate::console) async fn history_start(
        &self,
        room: &str,
        anchor: &str,
    ) -> Result<Option<String>, RuntimeError> {
        let room_id = OwnedRoomId::try_from(room).map_err(|_| RuntimeError::Request)?;
        let event_id = OwnedEventId::try_from(anchor).map_err(|_| RuntimeError::Request)?;
        let room = self
            .client
            .get_room(&room_id)
            .ok_or(RuntimeError::Transport)?;
        let context = room
            .event_with_context(&event_id, false, 0u32.into(), None)
            .await
            .map_err(|_| RuntimeError::Transport)?;
        if context
            .event
            .as_ref()
            .and_then(|e| e.kind.parse_event_id())
            .as_ref()
            != Some(&event_id)
        {
            return Err(RuntimeError::Transport);
        }
        Ok(context.prev_batch_token)
    }

    pub(in crate::console) async fn history_page(
        &self,
        room: &str,
        from: &str,
    ) -> Result<HistoryBatch, RuntimeError> {
        let room_id = OwnedRoomId::try_from(room).map_err(|_| RuntimeError::Request)?;
        let room = self
            .client
            .get_room(&room_id)
            .ok_or(RuntimeError::Transport)?;
        let mut options = MessagesOptions::backward().from(from);
        options.limit = 50u32.into();
        let page = room
            .messages(options)
            .await
            .map_err(|_| RuntimeError::Transport)?;
        if page.end.as_deref() == Some(from) {
            return Err(RuntimeError::Transport);
        }
        Ok(HistoryBatch {
            events: page.chunk.iter().filter_map(history_event).collect(),
            next: page.end,
        })
    }
}

pub(super) fn history_event(event: &TimelineEvent) -> Option<Value> {
    let raw: Value = serde_json::from_str(event.kind.raw().json().get()).ok()?;
    let unavailable_reason = unavailable_reason(event, &raw);
    let mut record = history_record(&raw, unavailable_reason.is_some())?;
    if let Some(reason) = unavailable_reason {
        record["unavailableReason"] = reason.into();
    }
    Some(record)
}

/// Why an event's content cannot be trusted or read, shared by history and
/// room memory so both apply one authenticity rule.
pub(super) fn unavailable_reason(event: &TimelineEvent, raw: &Value) -> Option<&'static str> {
    match &event.kind {
        TimelineEventKind::UnableToDecrypt { .. } => Some("unable_to_decrypt"),
        TimelineEventKind::Decrypted(d) => {
            if d.encryption_info.sender.as_str() != raw["sender"].as_str().unwrap_or_default() {
                Some("sender_mismatch")
            } else {
                verification_issue(&d.encryption_info.verification_state)
            }
        }
        TimelineEventKind::PlainText { .. } => None,
    }
}

fn verification_issue(state: &VerificationState) -> Option<&'static str> {
    match state {
        VerificationState::Unverified(level) => match level {
            VerificationLevel::None(DeviceLinkProblem::MissingDevice) => Some("sender_device_missing"),
            VerificationLevel::None(DeviceLinkProblem::InsecureSource) => Some("insecure_key_source"),
            VerificationLevel::MismatchedSender => Some("sender_mismatch"),
            VerificationLevel::VerificationViolation => Some("identity_verification_changed"),
            VerificationLevel::UnverifiedIdentity | VerificationLevel::UnsignedDevice => None,
        },
        VerificationState::Verified => None,
    }
}

/// Spec limits for the identifiers a record may carry; `msgtype` values are
/// short namespaced names. Anything else is room-member-controlled JSON.
const MAX_MATRIX_ID_BYTES: usize = 255;
const MAX_MSGTYPE_BYTES: usize = 64;

fn matrix_id(value: &Value, prefix: char) -> Option<&str> {
    value
        .as_str()
        .filter(|s| {
            s.len() > 1 && s.len() <= MAX_MATRIX_ID_BYTES && !s.chars().any(char::is_control)
                && match prefix {
                    '$' => OwnedEventId::try_from(*s).is_ok(),
                    '@' => OwnedUserId::try_from(*s).is_ok(),
                    _ => false,
                }
        })
}

/// Every field is validated and bounded here, so one crafted event (an
/// oversized `msgtype`, a non-string `body`, a huge relation id) can neither
/// exceed a history page nor stall pagination for everything older than it.
/// Bodies stay unbounded strings: the reader splits them into fragments.
pub(in crate::console) fn history_record(raw: &Value, unavailable: bool) -> Option<Value> {
    let id = matrix_id(&raw["event_id"], '$')?;
    let sender = matrix_id(&raw["sender"], '@')?;
    let mut record = json!({"eventId":id,"sender":sender});
    if let Some(ts) = raw["origin_server_ts"].as_u64() {
        record["timestamp"] = ts.into();
    }
    if unavailable {
        record["status"] = "unavailable_encrypted_message".into();
        return Some(record);
    }
    if raw["unsigned"].get("redacted_because").is_some() {
        record["status"] = "redacted".into();
        return Some(record);
    }
    if raw["type"] == "m.room.redaction" {
        // Room versions before 11 carry `redacts` at the top level.
        let target = matrix_id(&raw["content"]["redacts"], '$')
            .or_else(|| matrix_id(&raw["redacts"], '$'))?;
        record["status"] = "redaction".into();
        record["target"] = target.into();
        return Some(record);
    }
    if raw["type"] != "m.room.message" {
        return None;
    }
    let content = &raw["content"];
    if let Some(kind) = content["msgtype"].as_str().filter(|s| s.len() <= MAX_MSGTYPE_BYTES) {
        record["messageType"] = kind.into();
    }
    let relation = &content["m.relates_to"];
    if relation["rel_type"] == "m.replace" {
        // An edit without a well-formed target or replacement text is not an
        // edit of anything; it is dropped rather than blanking the original.
        let target = matrix_id(&relation["event_id"], '$')?;
        let body = content["m.new_content"]["body"].as_str()?;
        record["status"] = "edit".into();
        record["target"] = target.into();
        record["body"] = body.into();
        return Some(record);
    }
    if let Some(body) = content["body"].as_str() {
        record["body"] = body.into();
    } else {
        record["status"] = "unavailable_invalid_message".into();
        record["unavailableReason"] = "body_is_not_text".into();
    }
    if let Some(metadata) = super::documents::metadata(content)
    {
        // Only a display descriptor reaches the model. URL, key, IV and hash
        // remain in the authenticated event on this Agent's own device.
        record["attachment"] = metadata;
    }
    if let Some(root) = matrix_id(&relation["event_id"], '$') {
        record["relatedEventId"] = root.into();
    }
    Some(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: &str, content: Value) -> Value {
        json!({"type":"m.room.message","event_id":id,"sender":"@member:example.test","origin_server_ts":1700000000000u64,"content":content})
    }

    #[test]
    fn history_file_records_expose_only_metadata_and_never_unavailable_attachment_keys() {
        let raw = event("$file", json!({"msgtype":"m.file","body":"设计.pdf","file":{"url":"mxc://example.test/secret","key":"private-key","iv":"private-iv"},"info":{"mimetype":"application/pdf","size":100}}));
        let record = history_record(&raw, false).unwrap();
        assert_eq!(record["attachment"]["filename"], "设计.pdf");
        assert_eq!(record["attachment"]["status"], "not_read");
        assert!(!record.to_string().contains("private"));
        assert!(!record.to_string().contains("mxc://"));
        assert!(history_record(&raw, true).unwrap().get("attachment").is_none());
        let mut redacted = raw.clone();
        redacted["unsigned"] = json!({"redacted_because":{}});
        assert!(history_record(&redacted, false).unwrap().get("attachment").is_none());
    }

    #[test]
    fn history_record_distinguishes_unknown_devices_from_unverified_identities() {
        assert_eq!(verification_issue(&VerificationState::Verified), None);
        for level in [VerificationLevel::UnverifiedIdentity, VerificationLevel::UnsignedDevice] {
            assert_eq!(verification_issue(&VerificationState::Unverified(level)), None);
        }
        for (level, reason) in [
            (VerificationLevel::None(DeviceLinkProblem::MissingDevice), "sender_device_missing"),
            (VerificationLevel::None(DeviceLinkProblem::InsecureSource), "insecure_key_source"),
            (VerificationLevel::MismatchedSender, "sender_mismatch"),
            (VerificationLevel::VerificationViolation, "identity_verification_changed"),
        ] {
            assert_eq!(verification_issue(&VerificationState::Unverified(level)), Some(reason));
        }
        for sender in ["@", "@alice", "@alice:"] {
            let mut raw = event("$message", json!({"body":"hello"}));
            raw["sender"] = sender.into();
            assert!(history_record(&raw, false).is_none(), "{sender}");
        }
    }

    #[test]
    fn history_record_drops_oversized_and_mistyped_fields_but_keeps_long_bodies() {
        let record = history_record(
            &event("$big", json!({"msgtype":"x".repeat(20 * 1024),"body":{"not":"text"},"m.relates_to":{"rel_type":"m.thread","event_id":format!("${}", "r".repeat(400))}})),
            false,
        )
        .unwrap();
        assert!(record.get("messageType").is_none());
        assert!(record.get("body").is_none());
        assert_eq!(record["status"], "unavailable_invalid_message");
        assert!(record.get("relatedEventId").is_none());
        assert!(record.to_string().len() < 256);
        let long = "群".repeat(50_000);
        let record = history_record(&event("$long", json!({"msgtype":"m.text","body":long,"m.relates_to":{"rel_type":"m.thread","event_id":"$root"}})), false).unwrap();
        assert_eq!(record["body"].as_str().unwrap().len(), long.len());
        assert_eq!((record["messageType"].as_str(), record["relatedEventId"].as_str()), (Some("m.text"), Some("$root")));
        assert_eq!(record["timestamp"], 1700000000000u64);
    }

    #[test]
    fn history_record_requires_well_formed_edits_and_redaction_targets() {
        assert!(history_record(&event("$edit", json!({"msgtype":"m.text","body":"* x","m.new_content":{"body":{"no":"text"}},"m.relates_to":{"rel_type":"m.replace","event_id":"$orig"}})), false).is_none());
        let edit = history_record(&event("$edit", json!({"msgtype":"m.text","body":"* fixed","m.new_content":{"msgtype":"m.text","body":"fixed"},"m.relates_to":{"rel_type":"m.replace","event_id":"$orig"}})), false).unwrap();
        assert_eq!((edit["status"].as_str(), edit["target"].as_str(), edit["body"].as_str()), (Some("edit"), Some("$orig"), Some("fixed")));
        let mut redaction = event("$red", json!({"redacts":"$gone","reason":"spam"}));
        redaction["type"] = "m.room.redaction".into();
        let record = history_record(&redaction, false).unwrap();
        assert_eq!((record["status"].as_str(), record["target"].as_str()), (Some("redaction"), Some("$gone")));
        redaction["content"] = json!({"redacts":"not-an-event"});
        assert!(history_record(&redaction, false).is_none());
        let mut bad_sender = event("$e", json!({"msgtype":"m.text","body":"hi"}));
        bad_sender["sender"] = "mallory".into();
        assert!(history_record(&bad_sender, false).is_none());
        let utd = history_record(&json!({"type":"m.room.encrypted","event_id":"$utd","sender":"@member:example.test","content":{"msgtype":"x".repeat(9000)}}), true).unwrap();
        assert_eq!(utd["status"], "unavailable_encrypted_message");
        assert!(utd.to_string().len() < 256);
    }
}
