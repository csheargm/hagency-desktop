//! Read-only recovery after an uncertain owner-direct createRoom.
use super::*;
use std::collections::BTreeSet;

pub(super) async fn joined_room(
    depot: &Depot,
    expected: &OwnerReply,
    cookie: &str,
    record: &Record,
) -> Result<Option<String>, OwnerError> {
    let joined = call_matrix(depot, expected, cookie, MatrixOperation::JoinedRooms).await?;
    let rooms = joined.value["joined_rooms"]
        .as_array()
        .filter(|rooms| rooms.len() <= 256)
        .ok_or_else(|| error(502, "invalid_matrix_response"))?;
    let rooms = rooms
        .iter()
        .map(|room| {
            room.as_str()
                .map(str::to_owned)
                .ok_or_else(|| error(502, "invalid_matrix_response"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    scan(rooms, record, |room| async move {
        call_matrix(depot, expected, cookie, MatrixOperation::State(room))
            .await
            .map(|reply| reply.value)
    })
    .await
}

async fn scan<F, Fut>(
    rooms: Vec<String>,
    record: &Record,
    mut state: F,
) -> Result<Option<String>, OwnerError>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<Value, OwnerError>>,
{
    if rooms.len() > 256 {
        return Err(error(502, "invalid_matrix_response"));
    }
    let mut seen = BTreeSet::new();
    for room in &rooms {
        super::super::matrix(room, '!')?;
        if !seen.insert(room) {
            return Err(error(502, "invalid_matrix_response"));
        }
    }
    let mut found = None;
    for room in rooms {
        // A failed state read leaves uniqueness unknown: do not adopt an
        // earlier match or skip an inaccessible possible duplicate.
        if candidate(&state(room.clone()).await?, record)? {
            select(&mut found, &room)?;
        }
    }
    Ok(found)
}

fn select(found: &mut Option<String>, room: &str) -> Result<(), OwnerError> {
    if found.is_some() {
        return Err(error(409, "owner_direct_recovery_ambiguous"));
    }
    *found = Some(room.into());
    Ok(())
}

fn candidate(state: &Value, record: &Record) -> Result<bool, OwnerError> {
    let events = state
        .as_array()
        .ok_or_else(|| error(502, "invalid_matrix_response"))?;
    // Never use a display name as identity. A matching immutable creation
    // marker is required before considering an unrelated joined Room.
    if !events.iter().any(|event| {
        event["type"] == MARKER
            && event["content"]["agentId"] == record.agent
            && event["content"]["ownerMxid"] == record.owner
    }) {
        return Ok(false);
    }
    validate(state, &record.owner, &record.agent, &record.puppet)?;
    let server = record
        .owner
        .split_once(':')
        .ok_or_else(|| error(409, "owner_direct_room_identity_mismatch"))?
        .1;
    let alias = format!("#hagency-agent-{}:{server}", record.agent);
    let mut canonical = events
        .iter()
        .filter(|event| event["type"] == "m.room.canonical_alias" && event["state_key"] == "");
    let exact_alias = canonical
        .next()
        .is_some_and(|event| event["sender"] == record.owner && event["content"]["alias"] == alias)
        && canonical.next().is_none();
    let third_party = events.iter().any(|event| {
        event["type"] == "m.room.member"
            && matches!(
                event["content"]["membership"].as_str(),
                Some("join" | "invite")
            )
            && event["state_key"] != record.owner
            && event["state_key"] != record.puppet
    });
    if !exact_alias || third_party {
        return Err(error(409, "owner_direct_room_identity_mismatch"));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> Record {
        Record {
            version: 1,
            agent: "a".into(),
            origin: "https://test/".into(),
            issuer: "https://test/auth/".into(),
            subject: "sub".into(),
            owner: "@owner:test".into(),
            puppet: "@puppet:test".into(),
            room: None,
            phase: "creating".into(),
            last_error: Some("owner_direct_creation_outcome_unknown".into()),
        }
    }
    fn state() -> Value {
        let mut state = super::super::tests::state();
        state.as_array_mut().unwrap().push(json!({"type":"m.room.canonical_alias","state_key":"","sender":"@owner:test","content":{"alias":"#hagency-agent-a:test"}}));
        state
    }
    #[test]
    fn partial_creation_recovers_exact_original_room_without_directory_alias() {
        assert!(candidate(&state(), &record()).unwrap());
        let mut unrelated = state();
        unrelated[1]["content"]["agentId"] = "other".into();
        assert!(!candidate(&unrelated, &record()).unwrap());
        let mut found = None;
        select(&mut found, "!original:test").unwrap();
        assert_eq!(found.as_deref(), Some("!original:test"));
        assert_eq!(
            select(&mut found, "!duplicate:test").unwrap_err().code,
            "owner_direct_recovery_ambiguous"
        );
    }
    #[test]
    fn recovery_rejects_changed_identity_privacy_alias_and_unexpected_members() {
        for (index, key, value) in [
            (0, "sender", json!("@foreign:test")),
            (1, "sender", json!("@foreign:test")),
            (2, "content", json!({"history_visibility":"world_readable"})),
            (3, "content", json!({"guest_access":"can_join"})),
            (4, "content", json!({"join_rule":"public"})),
            (7, "content", json!({"alias":"#other:test"})),
        ] {
            let mut bad = state();
            bad[index][key] = value;
            assert!(candidate(&bad, &record()).is_err(), "{index}/{key}");
        }
        for membership in ["join", "invite"] {
            let mut bad = state();
            bad.as_array_mut().unwrap().push(json!({"type":"m.room.member","state_key":"@third:test","content":{"membership":membership}}));
            assert!(candidate(&bad, &record()).is_err());
        }
        let mut duplicate = state();
        let alias = duplicate[7].clone();
        duplicate.as_array_mut().unwrap().push(alias);
        assert!(candidate(&duplicate, &record()).is_err());
        assert!(candidate(&json!({}), &record()).is_err());
    }
    #[tokio::test]
    async fn recovery_requires_a_complete_unique_scan_and_never_creates_a_room() {
        let rooms = vec!["!original:test".to_owned(), "!unrelated:test".to_owned()];
        let mut calls = Vec::new();
        let found = scan(rooms.clone(), &record(), |room| {
            calls.push(room.clone());
            let mut value = state();
            if room == "!unrelated:test" {
                value[1]["content"]["agentId"] = "other".into();
            }
            async move { Ok(value) }
        })
        .await
        .unwrap();
        assert_eq!(found.as_deref(), Some("!original:test"));
        assert_eq!(calls, rooms);
        let failure = scan(rooms.clone(), &record(), |room| async move {
            if room == "!original:test" {
                Ok(state())
            } else {
                Err(error(503, "matrix_unavailable"))
            }
        })
        .await
        .unwrap_err();
        assert_eq!(failure.code, "matrix_unavailable");
        let ambiguous = scan(rooms, &record(), |_| async { Ok(state()) })
            .await
            .unwrap_err();
        assert_eq!(ambiguous.code, "owner_direct_recovery_ambiguous");
        assert_eq!(
            scan(vec![], &record(), |_| async { Ok(state()) })
                .await
                .unwrap(),
            None
        );
    }
    #[tokio::test]
    async fn recovery_rejects_unbounded_duplicate_or_invalid_joined_lists_before_reading_state() {
        for rooms in [
            vec!["!same:test".into(); 257],
            vec!["!same:test".into(); 2],
            vec!["not-a-room".into()],
        ] {
            assert!(
                scan(rooms, &record(), |_| async {
                    panic!("invalid room list must not issue requests")
                })
                .await
                .is_err()
            );
        }
    }
}
