//! Which project a joined room is registered in, for
//! [`super::backend::Service::agents_for_registered_room`].
//!
//! The server has no "project of this room" read, so every project's room
//! list is read. One read after another, each queued behind the owner's
//! other commands (in process the owner runs one command at a time), ran
//! past the 20 s limit on Nov 18 while the server was slow, and the timeout
//! was reported as the same code as an unreadable listing. Now the reads go
//! out [`PARALLEL_READS`] at once, in listing order (they overlap through the owner daemon),
//! a room found in a project is remembered for [`FOUND_TTL`] per server and
//! owner, so the tracker's every check and the hover cards walk the projects
//! once, and a timeout is [`TIMED_OUT`]. A room in no project is never
//! remembered (it may be registered next).
use std::{collections::HashMap, future::Future, sync::{LazyLock, Mutex}, time::{Duration, Instant}};
use futures_util::future::join_all;
use serde_json::Value;

/// Project room lists read at once.
pub(crate) const PARALLEL_READS: usize = 8;
/// How long a found room → project association is reused.
const FOUND_TTL: Duration = Duration::from_secs(300);
/// Upper bound of projects walked (the server listing caps alike).
const MAX_PROJECTS: usize = 128;
/// The lookup ran out of time: distinct from an unreadable reply.
pub(crate) const TIMED_OUT: &str = "room_discovery_timed_out";
/// The project listing could not be read as one.
pub(crate) const UNREADABLE: &str = "room_discovery_unavailable";

/// A remembered association is for one server, owner and room.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct Key { origin: String, owner: String, room: String }

impl Key {
    pub(crate) fn new(origin: &str, owner: &str, room: &str) -> Self {
        Self { origin: origin.into(), owner: owner.into(), room: room.into() }
    }
}

static FOUND: LazyLock<Mutex<HashMap<Key, (Instant, String)>>> = LazyLock::new(Default::default);

/// The project `key`'s room was last found in, while still fresh.
pub(crate) fn cached(key: &Key) -> Option<String> {
    let found = FOUND.lock().unwrap_or_else(|e| e.into_inner());
    found.get(key).filter(|(at, _)| at.elapsed() < FOUND_TTL).map(|(_, project)| project.clone())
}

pub(crate) fn remember(key: &Key, project: &str) {
    FOUND.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone(), (Instant::now(), project.to_owned()));
}

/// Drops `key`'s association, after a read for its project failed.
pub(crate) fn forget(key: &Key) {
    FOUND.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
}

/// The project `room` is registered in for `service`'s owner (remembered
/// once found); `Ok(None)` when in none.
pub(crate) async fn registered_project(service: &super::backend::Service, key: &Key, room: &str) -> Result<Option<String>, String> {
    use super::model::Command;
    if let Some(project) = cached(key) { return Ok(Some(project)); }
    let projects = service.execute(Command::Projects).await?;
    let found = find_project(room, &projects, |id| service.execute(Command::ProjectRooms { project_id: id })).await?;
    if let Some(project) = &found { remember(key, project); }
    Ok(found)
}

/// Whether a `ProjectRooms` reply lists `room` as active.
fn lists_room(rooms: &Value, room: &str) -> bool {
    rooms["rooms"].as_array().is_some_and(|rows| rows.iter().any(|row| row["roomId"] == room && row["active"] == true))
}

/// The project of a `Projects` listing whose rooms list `room`, reading
/// `read_rooms(project)` for up to [`PARALLEL_READS`] projects at once.
/// `Ok(None)`: in no project. A failed read fails the lookup only when no
/// other project lists the room.
pub(crate) async fn find_project<F, Fut>(room: &str, projects: &Value, read_rooms: F) -> Result<Option<String>, String>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    let projects = projects["projects"].as_array().ok_or(UNREADABLE)?;
    if projects.len() > MAX_PROJECTS { return Err("room_discovery_incomplete".into()); }
    let ids: Vec<String> = projects.iter().filter_map(|p| p["id"].as_str().map(str::to_owned)).collect();
    let mut failed = None;
    // Chunks, not a stream: every read future is built before it is awaited,
    // which keeps the caller's future `Send` (a borrowing stream was not).
    for chunk in ids.chunks(PARALLEL_READS) {
        let reads: Vec<Fut> = chunk.iter().map(|id| read_rooms(id.clone())).collect();
        for (id, rooms) in chunk.iter().zip(join_all(reads).await) {
            match rooms {
                Ok(rooms) if lists_room(&rooms, room) => return Ok(Some(id.clone())),
                Ok(_) => {}
                Err(error) => { failed.get_or_insert(error); }
            }
        }
    }
    failed.map_or(Ok(None), Err)
}

#[cfg(test)]
#[path = "room_project_tests.rs"]
mod tests;
