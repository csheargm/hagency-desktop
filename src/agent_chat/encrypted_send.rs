//! Do not encrypt the first Agent request to an empty recipient device list.
//! Joining a room and publishing native device keys are separate operations.
use matrix_sdk::{
    Client, Room, RoomMemberships,
    ruma::{OwnedUserId, UserId, events::room::message::RoomMessageEventContent},
};
use std::{collections::BTreeSet, time::Duration};

fn recipients(members: Vec<OwnedUserId>, mentions: &BTreeSet<OwnedUserId>) -> Vec<OwnedUserId> {
    let people: Vec<_> = members
        .into_iter()
        .filter(|u| u.localpart() != "_hagency_service")
        .collect();
    let direct = people.len() == 2;
    people
        .into_iter()
        .filter(|u| u.localpart().starts_with("_hagency_agt_") && (direct || mentions.contains(u)))
        .collect()
}

/// Uses the SDK's signature-checked device store and normal send policy. This
/// never marks a device trusted, shares old room keys, or falls back to plaintext.
pub(crate) async fn ensure_ready(
    room: &Room,
    message: &RoomMessageEventContent,
) -> Result<(), &'static str> {
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        if !room
            .latest_encryption_state()
            .await
            .map_err(|_| ())?
            .is_encrypted()
        {
            return Ok(());
        }
        let members = room.members(RoomMemberships::JOIN).await.map_err(|_| ())?;
        let mentions = message
            .mentions
            .as_ref()
            .map(|m| m.user_ids.clone())
            .unwrap_or_default();
        let users = recipients(
            members
                .into_iter()
                .map(|m| m.user_id().to_owned())
                .collect(),
            &mentions,
        );
        let client = room.client();
        for user in users {
            wait_for_device(&client, &user).await.map_err(|_| ())?;
        }
        Ok::<_, ()>(())
    })
    .await;
    match result {
        Ok(Ok(())) => Ok(()),
        _ => Err(
            "This Agent's device keys are not available; it may not have started on its computer yet. Your message was not sent. Check that its service is running, then try again.",
        ),
    }
}

async fn wait_for_device(client: &Client, user: &UserId) -> Result<(), matrix_sdk::Error> {
    loop {
        let devices = client
            .encryption()
            .get_user_devices(user)
            .await?;
        if devices
            .devices()
            .any(|d| !d.is_deleted() && !d.is_blacklisted() && d.curve25519_key().is_some())
        {
            return Ok(());
        }
        // Query through the SDK so device signatures are validated and its
        // send queue sees the same keys; a bot need not have cross-signing.
        client
            .encryption()
            .request_user_identity(user)
            .await?;
        let devices = client
            .encryption()
            .get_user_devices(user)
            .await?;
        if devices
            .devices()
            .any(|d| !d.is_deleted() && !d.is_blacklisted() && d.curve25519_key().is_some())
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn user(s: &str) -> OwnedUserId {
        s.try_into().unwrap()
    }
    #[test]
    fn encrypted_agent_first_send_waits_only_for_the_addressed_agent() {
        let owner = user("@owner:test");
        let agent = user("@_hagency_agt_one:test");
        let service = user("@_hagency_service:test");
        let other = user("@member:test");
        assert_eq!(
            recipients(
                vec![owner.clone(), service.clone(), agent.clone()],
                &BTreeSet::new()
            ),
            vec![agent.clone()]
        );
        assert!(
            recipients(
                vec![owner.clone(), service.clone(), agent.clone(), other.clone()],
                &BTreeSet::new()
            )
            .is_empty()
        );
        assert_eq!(
            recipients(
                vec![owner.clone(), service, agent.clone(), other.clone()],
                &BTreeSet::from([agent.clone()])
            ),
            vec![agent]
        );
        assert!(recipients(vec![owner, other], &BTreeSet::new()).is_empty());
    }
}

#[cfg(test)]
#[path = "encrypted_send_http_tests.rs"]
mod http_tests;
