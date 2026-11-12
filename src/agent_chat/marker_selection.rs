use matrix_sdk::ruma::serde::Raw;

struct ApprovalMarkerEnvelope<'a> {
    event_type: std::borrow::Cow<'a, str>,
    state_key: std::borrow::Cow<'a, str>,
    has_single_content: bool,
}

impl<'de> serde::Deserialize<'de> for ApprovalMarkerEnvelope<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = ApprovalMarkerEnvelope<'de>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a Matrix state event envelope")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut event_type = None;
                let mut state_key = None;
                let mut content_count = 0_usize;
                while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
                    match key.as_ref() {
                        "type" => {
                            if event_type.is_some() {
                                return Err(serde::de::Error::duplicate_field("type"));
                            }
                            event_type = Some(map.next_value()?);
                        }
                        "state_key" => {
                            if state_key.is_some() {
                                return Err(serde::de::Error::duplicate_field("state_key"));
                            }
                            state_key = Some(map.next_value()?);
                        }
                        "content" => {
                            let _: &serde_json::value::RawValue = map.next_value()?;
                            content_count = content_count.saturating_add(1);
                        }
                        _ => {
                            let _: &serde_json::value::RawValue = map.next_value()?;
                        }
                    }
                }
                Ok(ApprovalMarkerEnvelope {
                    event_type: event_type
                        .ok_or_else(|| serde::de::Error::missing_field("type"))?,
                    state_key: state_key
                        .ok_or_else(|| serde::de::Error::missing_field("state_key"))?,
                    has_single_content: content_count == 1,
                })
            }
        }

        deserializer.deserialize_map(Visitor)
    }
}

pub(super) fn select_approval_marker_raw_state<'a, T: 'a>(
    events: impl IntoIterator<Item = &'a Raw<T>>,
) -> Result<Option<serde_json::Value>, ()> {
    const V2: &str = "com.agentchat.approval.room.v2";
    let mut candidates = Vec::new();
    for raw in events {
        let raw_json = raw.json().get();
        let envelope =
            serde_json::from_str::<ApprovalMarkerEnvelope<'_>>(raw_json).map_err(|_| ())?;
        if envelope.event_type != V2 || !envelope.state_key.is_empty() {
            continue;
        }
        if raw_json.len() > 65_536 || !envelope.has_single_content {
            return Err(());
        }
        candidates.push(serde_json::from_str(raw_json).map_err(|_| ())?);
    }
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.pop()),
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn current_marker_keeps_original_event_evidence() {
        let original = json!({"type":"com.agentchat.approval.room.v2","state_key":"","event_id":"$marker","sender":"@publisher:test","content":{"version":2}});
        let event = Raw::<serde_json::Value>::new(&original).unwrap();
        assert_eq!(
            select_approval_marker_raw_state([&event]).unwrap(),
            Some(original)
        );
    }
    #[test]
    fn duplicate_current_markers_reject_the_room_without_old_fallback() {
        let v1=Raw::<serde_json::Value>::new(&json!({"type":"com.agentchat.approval.room.v1","state_key":"","content":{"version":1}})).unwrap();
        let v2 = Raw::<serde_json::Value>::new(
            &json!({"type":"com.agentchat.approval.room.v2","state_key":"","content":null}),
        )
        .unwrap();
        assert!(select_approval_marker_raw_state([&v1, &v2, &v2]).is_err());
        // Content validation belongs to ingest_marker. Selection retains the
        // actual current event, never substitutes an older protocol payload.
        assert_eq!(
            select_approval_marker_raw_state([&v1, &v2])
                .unwrap()
                .unwrap()["type"],
            "com.agentchat.approval.room.v2"
        );
    }
    #[test]
    fn duplicate_content_keys_are_rejected_before_value_parsing() {
        let raw = Raw::<serde_json::Value>::from_json_string(
            r#"{"type":"com.agentchat.approval.room.v2","state_key":"","content":{},"content":{}}"#
                .into(),
        )
        .unwrap();
        assert!(select_approval_marker_raw_state([&raw]).is_err());
    }
    #[test]
    fn obsolete_marker_alone_is_not_a_destination() {
        let obsolete = Raw::<serde_json::Value>::new(&json!({"type":"com.agentchat.approval.room.v1","state_key":"","content":{"version":1}})).unwrap();
        assert!(
            select_approval_marker_raw_state([&obsolete])
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn obsolete_branding_cannot_select_a_current_marker() {
        for old in ["com.hafleet", "com.hagency"] {
            let event = Raw::<serde_json::Value>::new(&json!({"type":format!("{old}.approval.room.v2"),"state_key":"","content":{"version":2}})).unwrap();
            assert!(
                select_approval_marker_raw_state([&event])
                    .unwrap()
                    .is_none()
            );
        }
    }
}
