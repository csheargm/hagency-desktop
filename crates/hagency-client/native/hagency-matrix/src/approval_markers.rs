//! Current private-room discovery. A marker never authorizes a tool decision.
use crate::{
    ApprovalCollector, CancellationToken, Error, HostRoom, approval_batch::Room,
    approval_delivery::jobs, collector::Inner, enrollment::checkpoint,
};
use hagency_core::{
    approvals::ApprovalRoomAuthority,
    replies::{matrix_event, matrix_room, RoomPrivacy},
};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    time::Duration,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::time::Instant;

pub(crate) const MARKER_TYPE: &str = "com.agentchat.approval.room.v2";

impl ApprovalCollector {
    /// Publish only after the original private-room observation has admitted
    /// current bindings. Startup and newly admitted factory rooms use this same
    /// writer; direct card delivery also ensures the marker before its card.
    pub async fn prepare_private_approval_rooms(
        &self,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        let permit = self.delivery_permit(false)?;
        let engagements = self.engagements.snapshot()?;
        let inner = self.inner.clone();
        let cancel = cancel.clone();
        let pending_write = Arc::new(AtomicBool::new(false));
        let retained = pending_write.clone();
        let job = self
            .jobs
            .start_classified(false, false, permit, pending_write, async move {
                let deadline = Instant::now() + Duration::from_secs(45);
                let work = async {
                    let rooms = inner.approval_rooms(&engagements).await?;
                    inner.refresh_approval_rooms(&rooms, &cancel).await?;
                    inner
                        .publish_approval_room_markers(&rooms, &cancel, deadline, &retained)
                        .await?;
                    Ok(jobs::Value::Unit)
                };
                tokio::select! { biased;
                    _ = cancel.cancelled() => Err(Error::Cancelled),
                    _ = tokio::time::sleep_until(deadline) => Err(Error::Timeout),
                    result = work => result,
                }
            })?;
        match job.wait().await? {
            jobs::Value::Unit => Ok(()),
            _ => Err(Error::Storage),
        }
    }
}

impl Inner {
    pub(crate) async fn publish_approval_room_markers(
        &self,
        rooms: &[Room],
        cancel: &CancellationToken,
        deadline: Instant,
        pending_write: &AtomicBool,
    ) -> Result<(), Error> {
        let mut seen = BTreeSet::new();
        for room in rooms {
            if seen.insert(room.authority.room_id.clone()) {
                self.publish_approval_room_marker(room, cancel, deadline, pending_write)
                    .await?;
            }
        }
        Ok(())
    }

    async fn marker_content(&self, room: &Room, generation: u64) -> Result<Value, Error> {
        self.domain
            .private_approval_room_marker(room.authority.clone(), room.device.clone(), generation)
            .await
            .map_err(Error::from)
    }

    async fn publish_approval_room_marker(
        &self,
        room: &Room,
        cancel: &CancellationToken,
        deadline: Instant,
        pending_write: &AtomicBool,
    ) -> Result<(), Error> {
        checkpoint(cancel, deadline)?;
        self.whoami(cancel).await?;
        let authority = &room.authority;
        let state = self
            .http
            .request(
                &[
                    "_matrix",
                    "client",
                    "v3",
                    "rooms",
                    &authority.room_id,
                    "state",
                ],
                None,
                cancel,
            )
            .await?
            .success()?;
        let target = HostRoom {
            room_id: authority.room_id.clone(),
            generation: room.generation,
            privacy: RoomPrivacy::Direct {
                human_mxid: authority.owner_mxid.clone(),
            },
        };
        let (observation, _) = self.room(&target, state.clone())?;
        if !observation.encrypted
            || !observation.invite_only
            || observation.joined
                != BTreeSet::from([authority.owner_mxid.clone(), authority.bot_mxid.clone()])
        {
            return Err(Error::Recipients);
        }
        let prior = state
            .as_array()
            .ok_or(Error::Wire)?
            .iter()
            .find(|event| event["type"] == MARKER_TYPE && event["state_key"] == "");
        let generation = if let Some(prior) = prior {
            let previous = validate_event(prior, authority, None)?;
            let expected = self.marker_content(room, previous).await?;
            if prior["content"] == expected {
                return Ok(());
            }
            previous
                .checked_add(1)
                .filter(|g| *g <= hagency_core::JSON_SAFE_MAX)
                .ok_or(Error::Capacity)?
        } else {
            1
        };
        let content = self.marker_content(room, generation).await?;
        checkpoint(cancel, deadline)?;
        // No caller-supplied room, publisher, scope or content crosses this
        // write. The owner/device/current binding is re-read after network IO.
        // A lost write/readback remains owned and blocks retries. A refusal
        // before this point has made no mutation and can be retried normally.
        pending_write.store(true, Ordering::Release);
        let response = self
            .http
            .put(
                &[
                    "_matrix",
                    "client",
                    "v3",
                    "rooms",
                    &authority.room_id,
                    "state",
                    MARKER_TYPE,
                    "",
                ],
                serde_json::to_string(&content).map_err(|_| Error::Wire)?,
                cancel,
            )
            .await?
            .success()?;
        let event_id = response
            .get("event_id")
            .and_then(Value::as_str)
            .ok_or(Error::Wire)?;
        matrix_event(event_id).map_err(|_| Error::Wire)?;
        checkpoint(cancel, deadline)?;
        let event = self
            .http
            .request(
                &[
                    "_matrix",
                    "client",
                    "v3",
                    "rooms",
                    &authority.room_id,
                    "event",
                    event_id,
                ],
                None,
                cancel,
            )
            .await?
            .success()?;
        validate_event(&event, authority, Some(event_id))?;
        if event["content"] != content || self.marker_content(room, generation).await? != content {
            return Err(Error::Generation);
        }
        pending_write.store(false, Ordering::Release);
        checkpoint(cancel, deadline)
    }
}

fn validate_event(
    event: &Value,
    authority: &ApprovalRoomAuthority,
    expected_id: Option<&str>,
) -> Result<u64, Error> {
    let id = event
        .get("event_id")
        .and_then(Value::as_str)
        .ok_or(Error::Wire)?;
    matrix_event(id).map_err(|_| Error::Wire)?;
    if expected_id.is_some_and(|expected| expected != id)
        || event["type"] != MARKER_TYPE
        || event["state_key"] != ""
        || event["sender"] != authority.bot_mxid
        || event
            .get("room_id")
            .is_some_and(|id| id.as_str() != Some(authority.room_id.as_str()))
    {
        return Err(Error::Identity);
    }
    let content = event
        .get("content")
        .and_then(Value::as_object)
        .ok_or(Error::Wire)?;
    let generation = content
        .get("binding_generation")
        .and_then(Value::as_u64)
        .ok_or(Error::Wire)?;
    if content.len() != 5
        || content.get("version").and_then(Value::as_u64) != Some(2)
        || content.get("owner_mxid").and_then(Value::as_str) != Some(authority.owner_mxid.as_str())
        || content.get("publisher_mxid").and_then(Value::as_str)
            != Some(authority.bot_mxid.as_str())
        || generation == 0
        || generation > hagency_core::JSON_SAFE_MAX
    {
        return Err(Error::Identity);
    }
    let associations = content
        .get("project_room_associations")
        .and_then(Value::as_array)
        .ok_or(Error::Wire)?;
    if associations.len() > 256 {
        return Err(Error::Capacity);
    }
    let mut seen = BTreeSet::new();
    for association in associations {
        let item = association
            .as_object()
            .filter(|o| o.len() == 3)
            .ok_or(Error::Wire)?;
        let agent = item
            .get("agent")
            .and_then(Value::as_str)
            .ok_or(Error::Wire)?;
        let project = item
            .get("project_room_id")
            .and_then(Value::as_str)
            .ok_or(Error::Wire)?;
        hagency_core::project::AgentName::try_from(agent.to_owned()).map_err(|_| Error::Wire)?;
        matrix_room(project, &authority.server_name).map_err(|_| Error::Wire)?;
        if item.get("active").and_then(Value::as_bool).is_none() || !seen.insert((agent, project)) {
            return Err(Error::Wire);
        }
    }
    Ok(generation)
}
