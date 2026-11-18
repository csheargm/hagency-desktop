//! Posting the daily digest (ADR 0028), once per watched repository per
//! local day: only from the owner's own window (this account's Agent, run on
//! this device), through the tracker's room-notice path (`m.notice`, the
//! tracker's marker, a transaction id fixed by repository and day), then the
//! day is recorded in this device's ledger. Post first, then claim: a check
//! that dies in between repeats a duplicate the homeserver drops, and the
//! recorded day stops every later check and restart. A day without moves is
//! recorded without a post, so it stays silent.
use super::digest::{self, Window};
use super::tracker_live::{MARKER, line_txn_id};
use crate::hagency::backend::Service;
use crate::hagency::model::{Command, LocalAction};
use ::hagency::issue_watch::issue_track::Track;
use matrix_sdk::ruma::OwnedRoomId;
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use serde_json::{Map, Value, json};

pub(crate) trait DigestIo {
    /// Whether this window may send for the Agent (see [`own_window`]).
    async fn own_window(&self) -> Result<bool, String>;
    /// The Agent's owner-API record (tracks, sent days) read now.
    async fn fresh(&self) -> Result<Value, String>;
    /// The Activity view's requests that this account sent (its own hand-offs).
    async fn own_requests(&self) -> Result<Value, String>;
    /// One room notice, deduplicated by repository and day.
    async fn post(&self, room: &str, repo: &str, day: &str, text: String) -> Result<(), String>;
    /// Record `day`'s digest for `repo` as sent.
    async fn claim(&self, repo: &str, day: &str) -> Result<(), String>;
    fn paused(&self) -> bool;
}

/// The owner's own window: a signed-in account whose own Agent row this is
/// (a row without `ownerMxid` is this account's own, as on the Agent page),
/// active and assigned to this live device (`kill_switch::here`), so a second
/// window or device of the same owner never posts its own copy.
pub(crate) fn own_window(agent: &Value, me: Option<&str>, devices: &Value) -> bool {
    let Some(me) = me else { return false };
    let own = agent["ownerMxid"].as_str().is_none_or(|owner| owner == me);
    let id = agent["id"].as_str().unwrap_or_default();
    own && !id.is_empty() && crate::hagency::kill_switch::here(&json!({"agents": [agent]}), devices).iter().any(|a| a == id)
}

/// Send every due digest of one Agent's watches; the failures, for its status line.
pub(crate) async fn send_due(io: &impl DigestIo, saved: &Value, window: Option<Window>) -> Vec<String> {
    let Some(window) = window else { return Vec::new() };
    let fail = |error: String| vec![format!("{}: {error}", crate::i18n::tr("Daily digest"))];
    // Stop All holds the digest too; it goes out once the Agent is resumed that day.
    if due(saved, &window).is_empty() || io.paused() {
        return Vec::new();
    }
    match io.own_window().await {
        Ok(true) => {}
        Ok(false) => return Vec::new(),
        Err(error) => return fail(error),
    }
    // Read again after the cutoff: the check's snapshot may predate a move
    // this same check recorded before the send hour (GPT review).
    let fresh = match io.fresh().await {
        Ok(fresh) => fresh,
        Err(error) => return fail(error),
    };
    let tracks: Vec<Track> = if fresh["track"].is_null() { Vec::new() } else {
        match serde_json::from_value(fresh["track"].clone()) {
            Ok(tracks) => tracks,
            Err(error) => return vec![format!("Unreadable issue tracks: {error}")],
        }
    };
    let requests = match io.own_requests().await {
        Ok(requests) => requests,
        Err(error) => return fail(error),
    };
    let mut errors = Vec::new();
    for (repo, room) in due(&fresh, &window) {
        if let Err(error) = send_one(io, (&repo, &room), &tracks, &requests, &window).await {
            errors.push(format!("{repo}: {error}"));
        }
    }
    errors
}

/// The watched repositories whose digest for `window`'s day is not recorded yet.
fn due(saved: &Value, window: &Window) -> Vec<(String, String)> {
    digest::report_rooms(saved).into_iter().filter(|(repo, _)| !digest::sent(saved, repo, &window.day)).collect()
}

/// One repository: post its digest (none on a quiet day), then record the day.
async fn send_one(io: &impl DigestIo, (repo, room): (&str, &str), tracks: &[Track], requests: &Value, window: &Window) -> Result<(), String> {
    // Checked again right before each post or record: a pause during the reads holds the day (GPT review).
    if io.paused() {
        return Ok(());
    }
    let unanswered = digest::unanswered_issues(requests, repo, window);
    if let Some(day) = digest::summarize(repo, tracks, &unanswered, window) {
        // Recorded only after the post went through: a failed post is tried again next check.
        io.post(room, repo, &window.day, digest::text(repo, &day)).await?;
    }
    io.claim(repo, &window.day).await
}

/// The live I/O of one Agent's digest, within account session `epoch`.
pub(crate) struct LiveDigest<'a> {
    pub service: &'a Service,
    pub agent: &'a Value,
    pub epoch: u64,
}

impl LiveDigest<'_> {
    fn agent_id(&self) -> &str {
        self.agent["id"].as_str().unwrap_or_default()
    }
}

impl DigestIo for LiveDigest<'_> {
    async fn own_window(&self) -> Result<bool, String> {
        let me = crate::sliding_sync::current_user_id().map(|id| id.to_string());
        let devices = self.service.execute(Command::Devices).await?;
        Ok(own_window(self.agent, me.as_deref(), &devices))
    }
    async fn fresh(&self) -> Result<Value, String> {
        super::live::saved(self.service, self.agent_id(), None).await
    }
    async fn own_requests(&self) -> Result<Value, String> {
        let ask = Command::Local { agent_id: self.agent_id().into(), action: LocalAction::RecentActivity, input: None, binding_id: None, requester: None };
        let activity = self.service.execute(ask).await?;
        let me = crate::sliding_sync::current_user_id().ok_or("Signed out")?;
        Ok(super::run_end::requested_by(&activity["requests"], me.as_str()))
    }
    async fn post(&self, room: &str, repo: &str, day: &str, text: String) -> Result<(), String> {
        super::still_current(self.epoch)?;
        let room = OwnedRoomId::try_from(room).map_err(|_| format!("Invalid room {room}"))?;
        let mut extra = Map::new();
        extra.insert(MARKER.into(), json!({"repo": repo, "step": "digest", "day": day}));
        // Issue number 0: no issue has it, so the key never meets a tracker line's.
        let txn = line_txn_id(self.agent_id(), repo, 0, &format!("digest-{day}"));
        crate::hagency::room_send::send_routed_with(room, RoomMessageEventContent::notice_plain(text), extra, txn).await
    }
    async fn claim(&self, repo: &str, day: &str) -> Result<(), String> {
        super::still_current(self.epoch)?;
        super::live::saved(self.service, self.agent_id(), Some(json!({"digestSent": {"repo": repo, "day": day}}))).await.map(|_| ())
    }
    fn paused(&self) -> bool {
        crate::hagency::kill_switch::paused_here(self.agent_id())
    }
}
