//! The Agents joined to a watch's report room that may review its Agent's
//! pull request (ADR 0025): this owner's other Agents, other owners' Agents
//! on this server's project roster, and every other Agent joined to the room
//! as its Matrix profile publishes its runtime: another server's Agent (it
//! has no roster row here), and this server's too while the roster cannot
//! be read (Nov 18: a timed-out roster read hid bird-codex, and the room
//! said "No Codex reviewer").
use super::reviewer_pick::{self, Candidate, Peers};
use super::work::Worker;
use matrix_sdk::{Room, RoomMemberships, ruma::{OwnedRoomId, OwnedUserId}};
use std::time::Duration;

/// Upper bound of joined Agents whose profiles are read for one pick.
const MAX_PROFILES: usize = 32;
/// How long the pick waits for those profiles.
const PROFILES_TIMEOUT: Duration = Duration::from_secs(8);

/// The other Agents in the room that may review, any owner's, and how to
/// pick one: preferring a different runtime while the room asks for that
/// (ADR 0025). Empty when the room is not known here.
pub(super) async fn peers(worker: &Worker<'_>, room: &OwnedRoomId) -> Peers {
    let me = worker.agent["id"].as_str().unwrap_or_default();
    let Some(joined) = crate::sliding_sync::get_client().and_then(|c| c.get_room(room)) else { return Peers::default() };
    let mut candidates = own_peers(worker, &joined, me).await;
    let mine: Vec<&str> = worker.peers.iter().chain([worker.agent]).filter_map(|p| p["puppetMxid"].as_str()).collect();
    let roster_read = add_roster(worker, &joined, &mine, &mut candidates).await;
    let complete = add_members(&joined, &mine, roster_read, &mut candidates).await;
    let prefer = super::room_review::prefers_different_model(&joined).await.unwrap_or_else(|error| {
        // The last value read for the room, else its default (GPT review).
        makepad_widgets::log!("WARN issue_track_review_setting_unreadable room={room}: {error}");
        super::room_review::last_read(room).unwrap_or_else(|| reviewer_pick::different_model(None))
    });
    let author = crate::profile::agent_facts::runtime_kind(worker.service, me).await;
    Peers { candidates, author, prefer_different: prefer, complete }
}

/// This owner's other Agents joined to the room.
async fn own_peers(worker: &Worker<'_>, joined: &Room, me: &str) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for peer in worker.peers.iter().filter(|p| p["id"].as_str().is_some_and(|id| id != me)) {
        let Ok(puppet) = OwnedUserId::try_from(peer["puppetMxid"].as_str().unwrap_or_default()) else { continue };
        let Some(name) = joined_name(joined, &puppet).await else { continue };
        let runtime = crate::profile::agent_facts::runtime_kind(worker.service, peer["id"].as_str().unwrap_or_default()).await;
        candidates.push(Candidate { puppet, name, runtime, own: true });
    }
    candidates
}

/// Other owners' active Agents on the project roster; `false` when the
/// roster could not be read (a room outside a project has none).
async fn add_roster(worker: &Worker<'_>, joined: &Room, mine: &[&str], out: &mut Vec<Candidate>) -> bool {
    let rows = match crate::profile::agent_facts::room_roster_with(worker.service, joined.room_id().as_str()).await {
        Ok(rows) => rows,
        Err(code) => {
            makepad_widgets::log!("WARN issue_track_roster_unavailable room={}: {code}", joined.room_id());
            return false;
        }
    };
    // Sorted: the same fallback every check (GPT review).
    let mut rows: Vec<_> = rows.into_iter().filter(|(p, r)| !mine.contains(&p.as_str()) && r.binding_state == "active").collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (puppet, row) in rows {
        let Some(name) = joined_name(joined, &puppet).await else { continue };
        out.push(Candidate { puppet, name, runtime: row.runtime, own: false });
    }
    true
}

/// Every other joined Agent not yet a candidate, and every roster row
/// whose runtime the roster left unsaid, with the runtime its profile
/// publishes; this server's Agents are added only when `roster_read` is false
/// (the roster then left out the paused ones on purpose). `false` when some
/// Agent could not be checked.
async fn add_members(joined: &Room, mine: &[&str], roster_read: bool, out: &mut Vec<Candidate>) -> bool {
    let Some(client) = crate::sliding_sync::get_client() else { return false };
    let Some(server) = client.user_id().map(|id| id.server_name().to_owned()) else { return false };
    // `members` fetches the list when the local store has not synced it (GPT review).
    let Ok(Ok(members)) = tokio::time::timeout(PROFILES_TIMEOUT, joined.members(RoomMemberships::JOIN)).await else { return false };
    let unknown: Vec<(OwnedUserId, String)> = out.iter().filter(|c| !c.own && c.runtime.is_none()).map(|c| (c.puppet.clone(), c.name.clone())).collect();
    let others: Vec<(OwnedUserId, String)> = members.iter()
        .filter(|m| crate::hagency::backend::is_agent_invitation(m.user_id()) && !mine.contains(&m.user_id().as_str()))
        .filter(|m| !(roster_read && m.user_id().server_name() == server) && !out.iter().any(|c| c.puppet == m.user_id()))
        .map(|m| (m.user_id().to_owned(), m.display_name().map_or_else(|| m.user_id().localpart().to_owned(), str::to_owned)))
        .collect();
    let all_read = unknown.len() + others.len() <= MAX_PROFILES;
    let read = read_runtimes(&client, unknown.into_iter().chain(others).take(MAX_PROFILES).collect()).await;
    reviewer_pick::add_profiles(out, read) && all_read
}

/// Each Agent's published runtime, every read bounded on its own so one
/// stalled server keeps the others' answers (GPT review).
async fn read_runtimes(client: &matrix_sdk::Client, agents: Vec<(OwnedUserId, String)>) -> Vec<reviewer_pick::ProfileRead> {
    futures_util::future::join_all(agents.into_iter().map(|(puppet, name)| async move {
        let read = crate::profile::agent_profile_owner::read_runtime_kind(client, &puppet);
        let runtime = tokio::time::timeout(PROFILES_TIMEOUT, read).await.unwrap_or_else(|_| Err("profile_read_timed_out".into()));
        (puppet, name, runtime)
    })).await
}

/// The name the room shows for `user`, when they are joined.
async fn joined_name(room: &Room, user: &OwnedUserId) -> Option<String> {
    use matrix_sdk::ruma::events::room::member::MembershipState;
    let member = room.get_member_no_sync(user).await.ok().flatten().filter(|m| *m.membership() == MembershipState::Join)?;
    Some(member.display_name().map_or_else(|| user.localpart().to_owned(), str::to_owned))
}
