//! The async steps: read the owner's Agents, look at the room, then bind
//! through `Service::invite_agent_to_room` (the Add agent path). Binds run
//! one after another so two binds of one Agent never race in the journal.
use super::{AgentPick, retry, Follow, RoomFacts, active_agents, follow_on_join, prefs, prompt::BringAgentAction, should_auto_join, should_catch_up};
use crate::hagency::{backend::Service, model::Command};
use crate::shared::popup_list::{PopupKind, enqueue_popup_notification};
use crate::utils::RoomNameId;
use makepad_widgets::*;
use matrix_sdk::ruma::{OwnedRoomId, UserId, events::room::member::MembershipState};

/// How long a bind outcome stays on screen.
const RESULT_POPUP_SECONDS: f64 = 8.0;

/// One bind finished; the UI thread shows its outcome.
#[derive(Debug)]
pub(super) struct BindDone {
    pub epoch: u64,
    pub agent: String,
    pub room: String,
    pub result: Result<bool, String>,
}

/// Binds the Agent `puppet` into each `(room id, name)` in order, reporting each outcome.
pub fn bind_into_rooms(agent: String, puppet: String, rooms: Vec<(String, String)>) {
    bind_into_rooms_then(agent, puppet, rooms, |_| {});
}

/// As [`bind_into_rooms`], also handing each outcome to `done` (off the UI thread).
pub fn bind_into_rooms_then(agent: String, puppet: String, rooms: Vec<(String, String)>, done: impl Fn(&Result<bool, String>) + Send + 'static) {
    if rooms.is_empty() { return; }
    let epoch = crate::account_session::epoch();
    crate::account_session::spawn(async move {
        let service = Service::for_current();
        for (room_id, room) in rooms {
            let result = match &service {
                Ok(service) => service.invite_agent_to_room(&room_id, &puppet).await,
                Err(code) => Err(code.clone()),
            };
            done(&result);
            Cx::post_action(BindDone { epoch, agent: agent.clone(), room, result });
        }
    });
}

/// Follows the app's actions: a joined room brings the owner's Agents along,
/// and every bind outcome becomes a popup.
pub fn observe(actions: &Actions) {
    for action in actions {
        if let Some(crate::home::invite_screen::JoinRoomResultAction::Joined { room_id }) = action.downcast_ref() {
            follow_after_join(room_id.clone());
        }
        if let Some(done) = action.downcast_ref::<BindDone>() && done.epoch == crate::account_session::epoch() {
            let kind = match done.result { Ok(true) => PopupKind::Success, Ok(false) => PopupKind::Info, Err(_) => PopupKind::Error };
            enqueue_popup_notification(super::bind_result_text(&done.agent, &done.room, &done.result), kind, Some(RESULT_POPUP_SECONDS));
        }
    }
}

/// After a room the owner created (New room) exists: every Agent whose
/// "Join rooms I join" setting is on joins it.
pub fn owner_created_room(room_id: OwnedRoomId) {
    let epoch = crate::account_session::epoch();
    crate::account_session::spawn(async move {
        match when_loaded(epoch, || prepare(&room_id)).await {
            Ok((agents, room)) => {
                if epoch != crate::account_session::epoch() { return; }
                let saved = prefs::load_current();
                for (agent, facts) in agents {
                    if should_auto_join(saved.joins_rooms(&agent.id), facts) && !saved.is_handled(room_id.as_str(), &agent.id) {
                        follow_bind(room_id.clone(), agent, room.clone());
                    }
                }
            }
            // Optional convenience: the room was created either way and Add
            // agent still works there, so a failed lookup only skips it.
            Err(code) => warning!("Bring agent: no automatic add to new room {room_id}: {code}"),
        }
    });
}

/// Agents new to this account's follow settings (just created, or older than
/// the catch-up) join the group rooms the owner created before they existed,
/// once per Agent. Rooms the owner only joined are left to their prompt.
pub fn catch_up_agents() {
    let epoch = crate::account_session::epoch();
    crate::account_session::spawn(async move {
        if let Err(code) = catch_up(epoch).await {
            // Optional convenience: Add agent still works in each room, and an
            // unclaimed Agent tries again on the next listing.
            warning!("Bring agent: owner's earlier rooms not caught up: {code}");
        }
    });
}

async fn catch_up(epoch: u64) -> Result<(), String> {
    let client = crate::sliding_sync::get_client().ok_or("matrix_sign_in_required")?;
    let me = crate::sliding_sync::current_user_id().ok_or("matrix_sign_in_required")?;
    let agents = active_agents(&Service::for_current()?.execute(Command::Agents).await?);
    let saved = prefs::load_current();
    for agent in agents.into_iter().filter(|a| saved.joins_rooms(&a.id) && saved.needs_catch_up(&a.id)) {
        // Every room is read before anything is recorded: a room still loading
        // fails this pass, and the Agent is caught up on a later listing.
        let mut joins = Vec::new();
        for room in client.joined_rooms() {
            if epoch != crate::account_session::epoch() { return Err("account_changed".into()); }
            let owner_created = room.creators().is_some_and(|c| c.contains(&me));
            if !owner_created { continue; }
            let facts = room_facts(&room, &agent).await?;
            if should_catch_up(true, saved.is_handled(room.room_id().as_str(), &agent.id), true, facts) {
                joins.push((room.room_id().to_owned(), RoomNameId::from_room(&room).await.to_string()));
            }
        }
        // Each pair is recorded only when its bind settles (`follow_bind`), so a
        // failed bind is retried by the next join of that room.
        if !prefs::update_current(|prefs| prefs.claim_catch_up(&agent.id))? { continue; }
        for (room_id, name) in joins { follow_bind(room_id, agent.clone(), name); }
    }
    Ok(())
}

/// Records the pair as handled; false when it already was. A failed save
/// still allows this one: the in-room Add agent path is idempotent, and the
/// warning says the guard may not hold across restarts.
fn claim(room_id: &OwnedRoomId, agent: &AgentPick) -> bool {
    match prefs::update_current(|prefs| prefs.claim(room_id.as_str(), &agent.id)) {
        Ok(fresh) => fresh,
        Err(code) => { warning!("Bring agent: could not record {} in {room_id}: {code}", agent.id); true }
    }
}

/// Runs `look` until the room is loaded, waiting between bounded attempts:
/// `Joined` arrives before sliding sync has the room or its power levels.
async fn when_loaded<T, F, Fut>(epoch: u64, mut look: F) -> Result<T, String>
where F: FnMut() -> Fut, Fut: std::future::Future<Output = Result<T, String>> {
    let mut attempt = 0;
    loop {
        match look().await {
            Err(code) if retry::lookup_retries(&code) => {
                let Some(wait) = retry::delay_after(attempt) else { return Err(code) };
                attempt += 1;
                tokio::time::sleep(wait).await;
                if epoch != crate::account_session::epoch() { return Err("account_changed".into()); }
            }
            other => return other,
        }
    }
}

/// Binds one following Agent, retrying transient errors a bounded number of
/// times. The pair is recorded only on success or a definitive refusal, so a
/// failure leaves it open for the next join or restart.
fn follow_bind(room_id: OwnedRoomId, agent: AgentPick, room: String) {
    let key = prefs::pair(room_id.as_str(), &agent.id);
    let Some(in_flight) = retry::begin(&key) else { return };
    let epoch = crate::account_session::epoch();
    crate::account_session::spawn(async move {
        let result = bind_with_retries(epoch, &room_id, &agent.puppet).await;
        drop(in_flight);
        if retry::bind_settles(&result) { claim(&room_id, &agent); }
        Cx::post_action(BindDone { epoch, agent: agent.name, room, result });
    });
}

async fn bind_with_retries(epoch: u64, room_id: &OwnedRoomId, puppet: &str) -> Result<bool, String> {
    let mut attempt = 0;
    loop {
        let result = match Service::for_current() {
            Ok(service) => service.invite_agent_to_room(room_id.as_str(), puppet).await,
            Err(code) => Err(code),
        };
        if retry::bind_settles(&result) { return result; }
        let Some(wait) = retry::delay_after(attempt) else { return result };
        attempt += 1;
        tokio::time::sleep(wait).await;
        if epoch != crate::account_session::epoch() { return result; }
    }
}

/// After the owner joins a room: each Agent joins, is offered, or is skipped.
fn follow_after_join(room_id: OwnedRoomId) {
    let epoch = crate::account_session::epoch();
    crate::account_session::spawn(async move {
        let steps = match when_loaded(epoch, || plan_follow(&room_id)).await {
            Ok(steps) => steps,
            // Optional convenience: the room's Add agent still works, and the
            // pairs stay open, so the next join tries again.
            Err(code) => { warning!("Bring agent: nothing follows into joined room {room_id}: {code}"); return; }
        };
        if epoch != crate::account_session::epoch() { return; }
        for (agent, room_name, follow) in steps {
            run_step(&room_id, agent, room_name, follow);
        }
    });
}

/// An offer or a "no invite rights" notice settles the pair at once; a join
/// settles it only when its bind does.
fn run_step(room_id: &OwnedRoomId, agent: AgentPick, room_name: String, follow: Follow) {
    match follow {
        Follow::Join => follow_bind(room_id.clone(), agent, room_name),
        Follow::Offer => if claim(room_id, &agent) {
            Cx::post_action(BringAgentAction::Offer { room_id: room_id.clone(), room_name, agent });
        },
        Follow::NoInviteRights => if claim(room_id, &agent) {
            enqueue_popup_notification(super::no_invite_rights_text(&agent.name, &room_name), PopupKind::Warning, Some(RESULT_POPUP_SECONDS));
        },
        Follow::Skip => {}
    }
}

/// Every own active Agent with what to do for it in this room.
async fn plan_follow(room_id: &OwnedRoomId) -> Result<Vec<(AgentPick, String, Follow)>, String> {
    let client = crate::sliding_sync::get_client().ok_or("matrix_sign_in_required")?;
    let room = client.get_room(room_id).ok_or(retry::ROOM_NOT_LOADED)?;
    let service = Service::for_current()?;
    let agents = active_agents(&service.execute(Command::Agents).await?);
    let name = RoomNameId::from_room(&room).await.to_string();
    let saved = prefs::load_current();
    let mut steps = Vec::with_capacity(agents.len());
    for agent in agents {
        let facts = room_facts(&room, &agent).await?;
        let follow = follow_on_join(saved.joins_rooms(&agent.id), saved.is_handled(room_id.as_str(), &agent.id), facts);
        steps.push((agent, name.clone(), follow));
    }
    Ok(steps)
}

/// Every active Agent with what the room is for it, and the room's name.
async fn prepare(room_id: &OwnedRoomId) -> Result<(Vec<(AgentPick, RoomFacts)>, String), String> {
    let client = crate::sliding_sync::get_client().ok_or("matrix_sign_in_required")?;
    let room = client.get_room(room_id).ok_or(retry::ROOM_NOT_LOADED)?;
    let service = Service::for_current()?;
    let mut agents = Vec::new();
    for agent in active_agents(&service.execute(Command::Agents).await?) {
        let facts = room_facts(&room, &agent).await?;
        agents.push((agent, facts));
    }
    Ok((agents, RoomNameId::from_room(&room).await.to_string()))
}

async fn room_facts(room: &matrix_sdk::Room, agent: &AgentPick) -> Result<RoomFacts, String> {
    let private_chat = agent.direct_room.as_deref() == Some(room.room_id().as_str());
    let member = match UserId::parse(agent.puppet.as_str()) {
        Ok(puppet) => room.get_member_no_sync(&puppet).await.ok().flatten()
            .is_some_and(|m| matches!(m.membership(), MembershipState::Join | MembershipState::Invite)),
        Err(_) => false,
    };
    // An unreadable DM flag counts as a DM: never add an Agent into a private chat by mistake.
    let direct = room.is_direct().await.unwrap_or(true);
    Ok(RoomFacts { space: room.is_space(), direct, agent_present: private_chat || member, can_invite: can_invite(room).await? })
}

/// Whether the signed-in owner may invite here. Power levels not readable
/// yet (the room is still loading) are retried, never read as "no".
async fn can_invite(room: &matrix_sdk::Room) -> Result<bool, String> {
    let me = crate::sliding_sync::current_user_id().ok_or("matrix_sign_in_required")?;
    let levels = room.power_levels().await.map_err(|_| retry::POWER_LEVELS_UNREADABLE.to_owned())?;
    Ok(levels.user_can_invite(&me))
}
