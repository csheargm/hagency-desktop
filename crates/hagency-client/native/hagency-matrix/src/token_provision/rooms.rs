//! Physical agent rooms on the original ordinary-account provision claim.
use super::{ProvisionScope, ProvisionedTokenAccount, SavedResponse};
use crate::{
    CancellationToken, Error, HostRoom, collector::Inner, enrollment::checkpoint, http::Http,
};
use hagency_core::{authority::ProjectRequest, canonical, project, replies::RoomPrivacy};
use hagency_store::EffectOutcome;
use reqwest::header::HeaderValue;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::{
    sync::Semaphore,
    time::{Instant, timeout_at},
};
mod custody;
use custody::Custody;

pub(super) struct Operation {
    scope: ProvisionScope,
    /// A restart re-attaching rooms this custody already completed: replay the
    /// stored responses and read current state; never create, invite or join.
    reattach: bool,
    request: ProjectRequest,
    binding: String,
    root: PathBuf,
    key: [u8; 32],
    agent: Arc<Inner>,
    agent_write: Http,
    representative: Http,
    representative_write: Http,
}
impl Operation {
    pub fn new(
        account: &ProvisionedTokenAccount,
        scope: ProvisionScope,
        token: &str,
    ) -> Result<Self, Error> {
        if !(16..=4096).contains(&token.len()) || !token.bytes().all(|b| (33..=126).contains(&b)) {
            return Err(Error::Config);
        }
        let request: ProjectRequest = serde_json::from_value(
            scope
                .effect
                .payload
                .get("request")
                .ok_or(Error::Config)?
                .clone(),
        )
        .map_err(|_| Error::Config)?;
        request
            .validate(&scope.registration)
            .map_err(|_| Error::Config)?;
        if request.engagement_id().map_err(|_| Error::Config)? != scope.effect.engagement_id {
            return Err(Error::Config);
        }
        let binding = canonical::transport_digest(&json!({"kind":"agent-rooms-v1","account":account.context,
            "request":request.digest().map_err(|_|Error::Config)?,"representative":project::hash(token.as_bytes()),
            "agent_credential":project::hash(account.token.as_bytes()),"key":project::hash(&account.key)})).map_err(|_|Error::Config)?;
        let config = account.host_config(
            1,
            account.key,
            vec![HostRoom {
                room_id: request.target_room_id.clone(),
                generation: 1,
                privacy: RoomPrivacy::Group {},
            }],
        )?;
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| Error::Config)?;
        authorization.set_sensitive(true);
        let mut small = config.limits.clone();
        small.bytes = small.bytes.min(16384);
        let agent_write = Http::for_host(
            &config.endpoint,
            Some(&config.authorization),
            &small,
            &config.roots,
        )?;
        let representative = Http::for_host(
            &config.endpoint,
            Some(&authorization),
            &config.limits,
            &config.roots,
        )?;
        let representative_write = Http::for_host(
            &config.endpoint,
            Some(&authorization),
            &small,
            &config.roots,
        )?;
        let root = account
            .root
            .parent()
            .ok_or(Error::Config)?
            .join(format!("agent-rooms-{}", scope.effect.id));
        Ok(Self {
            scope,
            reattach: account.reattach,
            request,
            binding,
            root,
            key: account.key,
            agent: Inner::new(
                config,
                account.scope.as_ref().ok_or(Error::Config)?.domain.clone(),
            )?,
            agent_write,
            representative,
            representative_write,
        })
    }
    async fn writer(&self, cancel: &CancellationToken, deadline: Instant) -> Result<(), Error> {
        checkpoint(cancel, deadline)?;
        self.validate().await?;
        if let Some(guard) = &self.agent.config.as_guard {
            guard.check(cancel).await?;
            self.validate().await?;
        }
        checkpoint(cancel, deadline)
    }
    async fn validate(&self) -> Result<(), Error> {
        let (effect, registration) = (self.scope.effect.clone(), self.scope.registration.clone());
        if self.reattach {
            self.scope
                .domain
                .validate_active_provision_account(effect, registration)
                .await?;
        } else {
            self.scope
                .domain
                .validate_provision_account(effect, registration)
                .await?;
        }
        Ok(())
    }
    async fn agent_current(
        &self,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<(), Error> {
        self.writer(cancel, deadline).await?;
        self.agent.whoami(cancel).await?;
        self.writer(cancel, deadline).await
    }
    async fn project(&self, cancel: &CancellationToken, deadline: Instant) -> Result<Value, Error> {
        self.writer(cancel, deadline).await?;
        let who = self
            .representative
            .request(
                &["_matrix", "client", "v3", "account", "whoami"],
                None,
                cancel,
            )
            .await?
            .success()?;
        if who.get("user_id").and_then(Value::as_str)
            != Some(self.scope.registration.representative_mxid.as_str())
            || who
                .get("device_id")
                .and_then(Value::as_str)
                .is_none_or(|s| s.is_empty() || s.len() > 255 || s.chars().any(char::is_control))
            || who
                .get("is_guest")
                .is_some_and(|v| v != &Value::Bool(false))
        {
            return Err(Error::Identity);
        }
        let value = self
            .representative
            .request(
                &[
                    "_matrix",
                    "client",
                    "v3",
                    "rooms",
                    &self.request.target_room_id,
                    "state",
                ],
                None,
                cancel,
            )
            .await?
            .success()?;
        let target = &self.agent.config.rooms[0];
        let (room, facts) = self.agent.room(target, value.clone())?;
        let binding = facts.binding.as_ref().ok_or(Error::Recipients)?;
        let owner = &self.request.owner_mxid;
        let rep = &self.scope.registration.representative_mxid;
        if !room.joined.contains(owner)
            || !room.joined.contains(rep)
            || facts
                .powers
                .get(owner)
                .copied()
                .unwrap_or(facts.default_power)
                < facts.invite_power
            || facts
                .powers
                .get(rep)
                .copied()
                .unwrap_or(facts.default_power)
                < facts.invite_power
            || binding["v"] != 1
            || binding["purpose"] != "project"
            || binding["authVersion"] != 1
            || binding["fleetId"] != self.request.fleet_id
            || binding["projectId"] != self.request.target_project_id
            || binding["ownerMxid"] != *owner
        {
            return Err(Error::Recipients);
        }
        self.writer(cancel, deadline).await?;
        Ok(value)
    }
    fn member(&self, value: &Value, membership: &str) -> bool {
        value.as_array().is_some_and(|events| {
            events.iter().any(|event| {
                event["type"] == "m.room.member"
                    && event["state_key"] == self.agent.config.identity.transport.sender_mxid
                    && event["content"]["membership"] == membership
            })
        })
    }
    fn dm_id(&self, response: &SavedResponse) -> Result<String, Error> {
        let value = success(response)?;
        let room = value
            .get("room_id")
            .and_then(Value::as_str)
            .ok_or(Error::Wire)?;
        hagency_core::replies::matrix_room(room, &self.scope.registration.server_name)
            .map_err(|_| Error::Wire)?;
        if [
            self.request.target_room_id.as_str(),
            self.request.owner_dm_room_id.as_str(),
            self.scope.registration.reception_room_id.as_str(),
        ]
        .contains(&room)
        {
            return Err(Error::Conflict);
        }
        Ok(room.into())
    }
    fn rooms(&self, dm: &str) -> Vec<HostRoom> {
        vec![
            self.agent.config.rooms[0].clone(),
            HostRoom {
                room_id: dm.into(),
                generation: 1,
                privacy: RoomPrivacy::Direct {
                    human_mxid: self.request.owner_mxid.clone(),
                },
            },
        ]
    }
    async fn joined_dm(
        &self,
        dm: &str,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<bool, Error> {
        self.agent_current(cancel, deadline).await?;
        let value = self
            .agent
            .http
            .request(
                &["_matrix", "client", "v3", "rooms", dm, "state"],
                None,
                cancel,
            )
            .await?
            .success()?;
        let target = self.rooms(dm).pop().ok_or(Error::Config)?;
        let (room, _) = self.agent.room(&target, value.clone())?;
        let sender = &self.agent.config.identity.transport.sender_mxid;
        let owner = &self.request.owner_mxid;
        let events = value.as_array().ok_or(Error::Wire)?;
        let created = events.iter().any(|e| {
            e["type"] == "m.room.create"
                && e["state_key"] == ""
                && e["sender"] == *sender
                && e["content"]["m.federate"] == false
                && e["content"]
                    .get("creator")
                    .is_none_or(|v| v.as_str() == Some(sender.as_str()))
        });
        let invited_history = events.iter().any(|e| {
            e["type"] == "m.room.history_visibility"
                && e["state_key"] == ""
                && e["content"]["history_visibility"] == "invited"
        });
        if !created
            || !invited_history
            || !room.invite_only
            || !room.encrypted
            || !room.joined.contains(sender)
            || events.iter().any(|e| {
                e["type"] == "m.room.member"
                    && e["state_key"] != *sender
                    && e["state_key"] != *owner
            })
        {
            return Err(Error::Recipients);
        }
        // The retained three-way verdict (`bridge-matrix.js:9271-9292`), named
        // here so this read's outcome is the RETAINED vocabulary rather than a
        // bare boolean: `Present` is the owner joined, `Absent` the owner
        // provably not in the room. A room whose shape is wrong is refused below
        // — that is the retained Unreadable case, expressed as an error because
        // "I could not ask" must never be reported as an absence (`:9264`).
        let members: Vec<String> = room.joined.iter().cloned().collect();
        let verdict = crate::identity_polish::owner_membership_verdict(Some(&members), owner);
        let joined =
            verdict == crate::identity_polish::OwnerVerdict::Present && room.joined.len() == 2;
        if !joined
            && !events.iter().any(|e| {
                e["type"] == "m.room.member"
                    && e["state_key"] == *owner
                    && e["content"]["membership"] == "invite"
            })
        {
            return Err(Error::Recipients);
        }
        self.writer(cancel, deadline).await?;
        Ok(joined)
    }
    /// On a REFUSED invite, read the room's power levels and — only when the read
    /// ESTABLISHED that our representative cannot invite — name the cause and the
    /// project's remedy (`backend-v2.js:14466-14482`). Read on the failure path
    /// only and never matched on the error string, the two things the retained
    /// comment insists on (`:14476`): "A guess here would be worse than the bare
    /// error: it would name a cause we did not establish."
    async fn refuse_invite<'a>(
        &self,
        response: &'a SavedResponse,
        cancel: &CancellationToken,
    ) -> Result<&'a Value, Error> {
        // Classify only a credential/power refusal, and only then read the room.
        if matches!(response.status, 401 | 403) {
            let levels = self.read_power_levels(cancel).await;
            let power = crate::identity_polish::representative_invite_power(
                levels.as_ref(),
                self.scope.registration.representative_mxid.as_str(),
            );
            // `power.known && power.can == false`, exactly TS's guard: a diagnosis
            // that fired on every 403 would send the project to change a setting
            // that is already right (`:14591-14593`).
            if power.known && !power.can {
                eprintln!(
                    "{}",
                    crate::identity_polish::invite_power_remedy(
                        power.mine,
                        power.required,
                        &self.request.target_room_id,
                        self.agent.config.identity.transport.sender_mxid.as_str(),
                    )
                );
            }
        }
        // The error mapping is `success`'s, reused rather than restated, so a
        // redirect stays a redirect and the classification cannot change which
        // error the caller sees.
        success(response)
    }
    /// The power-levels read the classification needs, taken with the
    /// representative's credential. `None` is the UNREADABLE case — never an
    /// absence, and never a reason to fail the invitation being checked.
    async fn read_power_levels(&self, cancel: &CancellationToken) -> Option<Value> {
        let response = self
            .representative
            .request(
                &[
                    "_matrix",
                    "client",
                    "v3",
                    "rooms",
                    &self.request.target_room_id,
                    "state",
                ],
                None,
                cancel,
            )
            .await
            .ok()?;
        if response.status != 200 {
            return None;
        }
        let events = response.value?;
        events
            .as_array()?
            .iter()
            .find(|event| {
                event["type"] == "m.room.power_levels" && event["state_key"].as_str() == Some("")
            })
            .map(|event| event["content"].clone())
    }
    async fn post(
        &self,
        http: &Http,
        path: &[&str],
        body: Value,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<SavedResponse, Error> {
        if self.reattach {
            // The one place a create, invite or join leaves this operation.
            return Err(Error::Storage);
        }
        self.writer(cancel, deadline).await?;
        let response = http
            .post(
                path,
                serde_json::to_string(&body).map_err(|_| Error::Config)?,
                cancel,
            )
            .await
            .map_err(|_| Error::OutcomeUnknown)?;
        Ok(SavedResponse {
            status: response.status,
            value: response.value,
        })
    }
    async fn run(
        &self,
        job: &Job,
        cancel: &CancellationToken,
        deadline: Instant,
        resuming: bool,
        phase: Phase,
    ) -> Result<Vec<HostRoom>, Error> {
        let root = self.root.clone();
        let binding = self.binding.clone();
        let key = self.key;
        let custody = Arc::new(
            tokio::task::spawn_blocking(move || Custody::open(root, binding, key))
                .await
                .map_err(|_| Error::OutcomeUnknown)??,
        );
        let inspect = custody.clone();
        let records = tokio::task::spawn_blocking(move || inspect.values())
            .await
            .map_err(|_| Error::OutcomeUnknown)??;
        match phase {
            Phase::Rooms => {
                self.agent_rooms(job, &custody, &records, cancel, deadline, resuming)
                    .await
            }
            Phase::Owner => {
                self.owner(job, &custody, &records, cancel, deadline, resuming)
                    .await
            }
        }
    }
    /// The stored create/invite/join responses of a finished rooms step.
    fn stored_dm(&self, records: &[Option<Value>]) -> Result<String, Error> {
        for index in [0, 2, 4] {
            if records[index].as_ref().is_none_or(|v| !v.is_null()) {
                return Err(Error::Storage);
            }
        }
        let created: SavedResponse =
            serde_json::from_value(records[1].clone().ok_or(Error::Storage)?)
                .map_err(|_| Error::Storage)?;
        let invited: SavedResponse =
            serde_json::from_value(records[3].clone().ok_or(Error::Storage)?)
                .map_err(|_| Error::Storage)?;
        let joined: SavedResponse =
            serde_json::from_value(records[5].clone().ok_or(Error::Storage)?)
                .map_err(|_| Error::Storage)?;
        let dm = self.dm_id(&created)?;
        if success(&invited)?.as_object().is_none_or(|v| !v.is_empty())
            || success(&joined)?.get("room_id").and_then(Value::as_str)
                != Some(self.request.target_room_id.as_str())
            || records[AGENT_ROOMS].as_ref()
                != Some(&json!({"dm":dm,"project":self.request.target_room_id}))
        {
            return Err(Error::Storage);
        }
        Ok(dm)
    }
    /// ADR-184 step 1-2: the agent-only DM and the project join. The owner is
    /// not invited here; enrollment runs next, then `owner`.
    async fn agent_rooms(
        &self,
        job: &Job,
        custody: &Arc<Custody>,
        records: &[Option<Value>],
        cancel: &CancellationToken,
        deadline: Instant,
        resuming: bool,
    ) -> Result<Vec<HostRoom>, Error> {
        let custody = custody.clone();
        // An owner invite with no `complete` is either a wait for the owner or
        // a completed custody whose last record was lost; on disk they look
        // the same. Only the job that observed the wait may replay it.
        if records[OWNER_INVITE_POSSIBLE].is_some()
            && records[COMPLETE].is_none()
            && !resuming
        {
            return Err(Error::OutcomeUnknown);
        }
        if records[COMPLETE].is_some()
            && (records[OWNER_INVITE_POSSIBLE]
                .as_ref()
                .is_none_or(|v| !v.is_null())
                || records[OWNER_INVITE_RESPONSE].is_none()
                || records[AGENT_ROOMS] != records[COMPLETE])
        {
            return Err(Error::Storage);
        }
        let dm = if records[..6].iter().all(Option::is_some) && records[AGENT_ROOMS].is_some() {
            self.stored_dm(records)?
        } else {
            if records.iter().any(Option::is_some) {
                return Err(Error::OutcomeUnknown);
            }
            if self.reattach {
                // No rooms were ever completed here: that is a provision.
                return Err(Error::Storage);
            }
            write(&custody, "dm-possible", Value::Null).await?;
            self.agent_current(cancel, deadline).await?;
            // The agent's definition display name (bridge-matrix.js:5938-5955):
            // set once at the identity moment of this provision — a custom
            // name never present on a fresh account still wins inside the
            // reconcile itself (matrix-agent-profile.js:22).
            crate::identity_polish::reconcile_display_name(
                &self.agent.http,
                &self.agent.config.identity.transport.sender_mxid,
                self.request.agent_definition.name.as_str(),
                self.request.agent_definition.name.as_str(),
                cancel,
            )
            .await?;
            self.project(cancel, deadline).await?;
            let response = self.post(&self.agent_write,&["_matrix","client","v3","createRoom"],json!({
                "preset":"private_chat","is_direct":true,"invite":[],
                "name":self.request.agent_definition.name,"creation_content":{"m.federate":false},
                "initial_state":[{"type":"m.room.encryption","state_key":"","content":{"algorithm":"m.megolm.v1.aes-sha2"}},
                    {"type":"m.room.history_visibility","state_key":"","content":{"history_visibility":"invited"}},
                    {"type":"m.room.power_levels","state_key":"","content":crate::identity_polish::approval_room_power_levels(
                        &self.agent.config.identity.transport.sender_mxid)?}]
            }),cancel,deadline).await?;
            write(
                &custody,
                "dm-response",
                serde_json::to_value(&response).map_err(|_| Error::Storage)?,
            )
            .await?;
            let dm = self.dm_id(&response)?;
            *job.dm.lock().map_err(|_| Error::OutcomeUnknown)? = Some(dm.clone());
            write(&custody, "invite-possible", Value::Null).await?;
            self.project(cancel, deadline).await?;
            let response = self
                .post(
                    &self.representative_write,
                    &[
                        "_matrix",
                        "client",
                        "v3",
                        "rooms",
                        &self.request.target_room_id,
                        "invite",
                    ],
                    json!({"user_id":self.agent.config.identity.transport.sender_mxid}),
                    cancel,
                    deadline,
                )
                .await?;
            write(
                &custody,
                "invite-response",
                serde_json::to_value(&response).map_err(|_| Error::Storage)?,
            )
            .await?;
            // The refusal path (:14466-14482): classify, name the project remedy
            // when the read ESTABLISHED too little power, then refuse exactly as
            // `success()` would have.
            self.refuse_invite(&response, cancel).await?;
            if success(&response)?
                .as_object()
                .is_none_or(|v| !v.is_empty())
            {
                return Err(Error::Wire);
            }
            write(&custody, "join-possible", Value::Null).await?;
            let invited = self.project(cancel, deadline).await?;
            if !self.member(&invited, "invite") {
                return Err(Error::Recipients);
            }
            self.agent_current(cancel, deadline).await?;
            let response = self
                .post(
                    &self.agent_write,
                    &[
                        "_matrix",
                        "client",
                        "v3",
                        "join",
                        &self.request.target_room_id,
                    ],
                    json!({}),
                    cancel,
                    deadline,
                )
                .await?;
            write(
                &custody,
                "join-response",
                serde_json::to_value(&response).map_err(|_| Error::Storage)?,
            )
            .await?;
            if success(&response)?.get("room_id").and_then(Value::as_str)
                != Some(self.request.target_room_id.as_str())
            {
                return Err(Error::Identity);
            }
            dm
        };
        *job.dm.lock().map_err(|_| Error::OutcomeUnknown)? = Some(dm.clone());
        let project = self.project(cancel, deadline).await?;
        if !self.member(&project, "join") {
            return Err(Error::Recipients);
        }
        if records[AGENT_ROOMS].is_none() {
            write(
                &custody,
                "agent-rooms",
                json!({"dm":dm,"project":self.request.target_room_id}),
            )
            .await?;
        }
        self.writer(cancel, deadline).await?;
        Ok(self.rooms(&dm))
    }
    /// ADR-184 step 4: invite the owner once the agent is enrolled, then wait
    /// for the owner's join (no deadline; ADR-147 as amended 2026-09-22).
    async fn owner(
        &self,
        job: &Job,
        custody: &Arc<Custody>,
        records: &[Option<Value>],
        cancel: &CancellationToken,
        deadline: Instant,
        resuming: bool,
    ) -> Result<Vec<HostRoom>, Error> {
        let custody = custody.clone();
        if records[AGENT_ROOMS].is_none() {
            return Err(Error::Storage);
        }
        let dm = self.stored_dm(records)?;
        *job.dm.lock().map_err(|_| Error::OutcomeUnknown)? = Some(dm.clone());
        if records[COMPLETE].is_some() {
            if records[OWNER_INVITE_POSSIBLE]
                .as_ref()
                .is_none_or(|v| !v.is_null())
                || records[OWNER_INVITE_RESPONSE].is_none()
                || records[COMPLETE].as_ref()
                    != Some(&json!({"dm":dm,"project":self.request.target_room_id}))
            {
                return Err(Error::Storage);
            }
            self.writer(cancel, deadline).await?;
            return Ok(self.rooms(&dm));
        }
        let resumed = match (
            records[OWNER_INVITE_POSSIBLE].is_some(),
            records[OWNER_INVITE_RESPONSE].is_some(),
        ) {
            (false, false) => {
                if self.reattach {
                    return Err(Error::Storage);
                }
                write(&custody, "owner-invite-possible", Value::Null).await?;
                self.agent_current(cancel, deadline).await?;
                let response = self
                    .post(
                        &self.agent_write,
                        &["_matrix", "client", "v3", "rooms", &dm, "invite"],
                        json!({"user_id": self.request.owner_mxid}),
                        cancel,
                        deadline,
                    )
                    .await?;
                write(
                    &custody,
                    "owner-invite-response",
                    serde_json::to_value(&response).map_err(|_| Error::Storage)?,
                )
                .await?;
                if success(&response)?
                    .as_object()
                    .is_none_or(|v| !v.is_empty())
                {
                    return Err(Error::Wire);
                }
                false
            }
            // The invite may have crossed the wire and its response was lost:
            // inspect, never repeat. `joined_dm` refuses unless the owner is
            // invited or joined, which is exactly the accepted-invite state.
            (true, false) => {
                self.joined_dm(&dm, cancel, deadline).await?;
                resuming
            }
            (true, true) => {
                let response: SavedResponse = serde_json::from_value(
                    records[OWNER_INVITE_RESPONSE]
                        .clone()
                        .ok_or(Error::Storage)?,
                )
                .map_err(|_| Error::Storage)?;
                if success(&response)?
                    .as_object()
                    .is_none_or(|v| !v.is_empty())
                {
                    return Err(Error::Storage);
                }
                // Only the job that observed the wait resumes it; a restart
                // mid-wait stays with the operator, as before ADR-184.
                if !resuming {
                    return Err(Error::OutcomeUnknown);
                }
                true
            }
            (false, true) => return Err(Error::Storage),
        };
        // The owner's join has no deadline. A first attempt polls to its own
        // budget, since owners usually join within seconds; a resumed attempt
        // looks once and hands the wait back to the next coordinator turn.
        // Neither running out is evidence of anything: the attempt ends as
        // awaiting the owner, and the effect stays Started.
        let poll_until = if resumed {
            std::cmp::min(deadline, Instant::now() + std::time::Duration::from_secs(2))
        } else {
            deadline
        };
        loop {
            if self.joined_dm(&dm, cancel, deadline).await? {
                break;
            }
            if Instant::now() + std::time::Duration::from_secs(3) >= poll_until {
                // The owner-absent warning (bridge-matrix.js:9267-9297): the
                // request reached its DM, but nobody who can decide will see
                // it until the owner joins — invited and never joined.
                //
                // THE VERDICT IS ALREADY THE PROBE. `joined_dm` returning
                // `Ok(false)` IS the retained membership read: it succeeded, the
                // room's shape is right (created by us, invite-only, encrypted,
                // history `invited`) and the owner is only `invite` — an
                // ESTABLISHED absence, not a guess. A read that did not establish
                // it is `Err(Recipients)` above and never reaches this warning,
                // which is the retained "an unreadable membership says nothing"
                // rule (:9264-9265). No second request is issued.
                //
                // Warned once on the first attempt's wait handoff; resumed turns
                // re-check quietly (TS posts per delivery and native delivers
                // once), and like the retained check it never takes the delivery
                // it is checking down with it (:9293-9296).
                if !resumed {
                    eprintln!(
                        "{}",
                        crate::identity_polish::owner_absent_warning(
                            Some(self.request.agent_definition.name.as_str()),
                            &dm,
                            &self.request.owner_mxid,
                        )
                    );
                }
                return Err(Error::AwaitingOwner);
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            checkpoint(cancel, deadline)?;
        }
        self.writer(cancel, deadline).await?;
        write(
            &custody,
            "complete",
            json!({"dm":dm,"project":self.request.target_room_id}),
        )
        .await?;
        self.writer(cancel, deadline).await?;
        Ok(self.rooms(&dm))
    }
}
/// Which half of the rooms step runs (ADR-184).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Phase {
    /// The agent-only DM and the project join, before enrollment.
    Rooms,
    /// The owner's DM invite and join, after enrollment.
    Owner,
}
const COMPLETE: usize = 6;
const AGENT_ROOMS: usize = 7;
const OWNER_INVITE_POSSIBLE: usize = 8;
const OWNER_INVITE_RESPONSE: usize = 9;
#[derive(Default)]
pub(super) struct Jobs(Mutex<Option<Arc<Job>>>);
struct Job {
    operation: Operation,
    busy: Arc<Semaphore>,
    result: Mutex<Option<Result<Vec<HostRoom>, Error>>>,
    dm: Mutex<Option<String>>,
    /// This job itself observed the owner wait (ADR-184). Kept apart from
    /// `result`: a resumed turn replays the rooms phase before the owner phase,
    /// and that replay's result must not erase the observation.
    awaiting: std::sync::atomic::AtomicBool,
}
impl Jobs {
    pub fn rooms(&self) -> Result<Vec<HostRoom>, Error> {
        let job = self
            .0
            .lock()
            .map_err(|_| Error::OutcomeUnknown)?
            .clone()
            .ok_or(Error::Config)?;
        job.result
            .lock()
            .map_err(|_| Error::OutcomeUnknown)?
            .as_ref()
            .cloned()
            .unwrap_or(Err(Error::Busy))
    }
    pub fn check_rooms(&self, rooms: &[HostRoom]) -> Result<(), Error> {
        if self.0.lock().map_err(|_| Error::OutcomeUnknown)?.is_none() {
            return Ok(());
        }
        let actual = self.rooms()?;
        if canonical::transport_digest(&serde_json::to_value(actual).map_err(|_| Error::Config)?)
            .map_err(|_| Error::Config)?
            != canonical::transport_digest(&serde_json::to_value(rooms).map_err(|_| Error::Config)?)
                .map_err(|_| Error::Config)?
        {
            return Err(Error::Conflict);
        }
        Ok(())
    }
    pub fn dm(&self) -> Result<Option<String>, Error> {
        let job = self.0.lock().map_err(|_| Error::OutcomeUnknown)?.clone();
        match job {
            Some(job) => Ok(job.dm.lock().map_err(|_| Error::OutcomeUnknown)?.clone()),
            None => Ok(None),
        }
    }
    pub async fn run(
        &self,
        operation: Operation,
        phase: Phase,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut resuming = false;
        let job = {
            let mut guard = self.0.lock().map_err(|_| Error::OutcomeUnknown)?;
            if let Some(job) = guard.as_ref() {
                if job.operation.binding != operation.binding {
                    return Err(Error::Conflict);
                }
                match job
                    .result
                    .lock()
                    .map_err(|_| Error::OutcomeUnknown)?
                    .as_ref()
                {
                    Some(Ok(_)) => {}
                    // Waiting for the owner is resumed, never replayed as a failure.
                    Some(Err(Error::AwaitingOwner)) => resuming = true,
                    Some(Err(error)) => return Err(error.clone()),
                    None => return Err(Error::Busy),
                }
                job.clone()
            } else {
                let job = Arc::new(Job {
                    operation,
                    busy: Arc::new(Semaphore::new(1)),
                    result: Mutex::new(None),
                    dm: Mutex::new(None),
                    awaiting: std::sync::atomic::AtomicBool::new(false),
                });
                *guard = Some(job.clone());
                job
            }
        };
        let permit = job
            .busy
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let deadline = Instant::now() + job.operation.agent.config.limits.sdk;
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let result = timeout_at(
                deadline,
                job.operation.run(
                    &job,
                    &cancel,
                    deadline,
                    resuming || job.awaiting.load(std::sync::atomic::Ordering::Acquire),
                    phase,
                ),
            )
            .await
            .unwrap_or(Err(Error::Timeout));
            if let Err(error) = &result
                && *error != Error::AwaitingOwner
            {
                eprintln!(
                    "provision room step failed: effect={} error={error:?}",
                    job.operation.scope.effect.id
                );
            }
            let result = if result.as_ref().is_err_and(|e| *e != Error::AwaitingOwner)
                && job
                    .operation
                    .scope
                    .domain
                    .observe_effect(
                        job.operation.scope.effect.id.clone(),
                        job.operation.scope.effect.fence,
                        EffectOutcome::Unknown,
                    )
                    .await
                    .is_err()
            {
                Err(Error::OutcomeUnknown)
            } else {
                result
            };
            if phase == Phase::Owner {
                job.awaiting.store(
                    result.as_ref().is_err_and(|e| *e == Error::AwaitingOwner),
                    std::sync::atomic::Ordering::Release,
                );
            }
            let response = result.as_ref().map(|_| ()).map_err(Clone::clone);
            *job.result.lock().map_err(|_| Error::OutcomeUnknown)? = Some(result);
            response
        })
        .await
        .map_err(|_| Error::OutcomeUnknown)?
    }
}
fn success(response: &SavedResponse) -> Result<&Value, Error> {
    if response.status != 200 {
        return Err(if matches!(response.status, 401 | 403) {
            Error::Unauthorized
        } else {
            Error::Remote(response.status)
        });
    }
    response.value.as_ref().ok_or(Error::Wire)
}
async fn write(custody: &Arc<Custody>, stage: &'static str, value: Value) -> Result<(), Error> {
    let custody = custody.clone();
    tokio::task::spawn_blocking(move || custody.write(stage, value))
        .await
        .map_err(|_| Error::OutcomeUnknown)?
}
