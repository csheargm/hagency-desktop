//! Which owner each Agent puppet in the dispatch's room belongs to, from the
//! server's own listings (ADR 0023): this owner's Agents, and the room's
//! project roster when the room belongs to a project. Published issue states
//! are accepted only from an Agent's owner, so this is the authority; room
//! content never is. A listing that cannot be read leaves those Agents out
//! (their published states are then not shown), never guessed.
use super::Source;
use crate::console::server_login::OwnerOperation;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// How long one room's mapping is reused before the server is asked again.
const OWNERS_TTL: Duration = Duration::from_secs(5 * 60);
/// Projects scanned to find the room's project.
const MAX_PROJECTS: usize = 64;
type Cached = HashMap<(String, String), (Instant, BTreeMap<String, String>)>;
static CACHE: LazyLock<Mutex<Cached>> = LazyLock::new(Default::default);

/// Puppet → owner from listing rows; a puppet two rows give different owners is dropped.
pub(super) fn add_rows(out: &mut BTreeMap<String, String>, contested: &mut BTreeSet<String>, rows: &Value, fallback_owner: Option<&str>) {
    for row in rows.as_array().into_iter().flatten() {
        let Some(puppet) = row["puppetMxid"].as_str().filter(|p| hagency_runtime::repo_status::is_puppet(p)) else { continue };
        let Some(owner) = row["ownerMxid"].as_str().or(fallback_owner).filter(|o| o.starts_with('@')) else { continue };
        if contested.contains(puppet) {
            continue;
        }
        match out.get(puppet) {
            Some(known) if known != owner => {
                out.remove(puppet);
                contested.insert(puppet.to_owned());
            }
            _ => {
                out.insert(puppet.to_owned(), owner.to_owned());
            }
        }
    }
}

async fn call(source: &Source, operation: OwnerOperation) -> Option<Value> {
    let console = &source.gate.console;
    console.console.owner_api_pinned(&console.pin, operation).await.ok().map(|reply| reply.value)
}

async fn project_of(source: &Source, room: &str) -> Option<String> {
    let projects = call(source, OwnerOperation::Projects).await?;
    for project in projects["projects"].as_array()?.iter().take(MAX_PROJECTS) {
        let Some(id) = project["id"].as_str() else { continue };
        let rooms = call(source, OwnerOperation::ProjectRooms { project: id.to_owned() }).await?;
        if rooms["rooms"].as_array().is_some_and(|rooms| rooms.iter().any(|r| r["roomId"] == room && r["active"] == true)) {
            return Some(id.to_owned());
        }
    }
    None
}

/// The room's puppet → owner mapping, cached for [`OWNERS_TTL`].
pub(super) async fn of_room(source: &Arc<Source>) -> BTreeMap<String, String> {
    let room = source.gate.expected.room_id.clone();
    let key = (source.gate.console.pin.owner.clone(), room.clone());
    // A poisoned lock still holds a whole map: inserts are single operations.
    if let Some((at, map)) = CACHE.lock().unwrap_or_else(|p| p.into_inner()).get(&key)
        && at.elapsed() < OWNERS_TTL
    {
        return map.clone();
    }
    let (mut out, mut contested) = (BTreeMap::new(), BTreeSet::new());
    if let Some(own) = call(source, OwnerOperation::Agents).await {
        add_rows(&mut out, &mut contested, &own["agents"], own["ownerMxid"].as_str());
    }
    if let Some(project) = project_of(source, &room).await
        && let Some(roster) = call(source, OwnerOperation::RoomRoster { project, room: room.clone() }).await
    {
        add_rows(&mut out, &mut contested, &roster["agents"], None);
    }
    CACHE.lock().unwrap_or_else(|p| p.into_inner()).insert(key, (Instant::now(), out.clone()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rows_map_puppets_to_owners_and_contested_puppets_are_dropped() {
        let (mut out, mut contested) = (BTreeMap::new(), BTreeSet::new());
        add_rows(&mut out, &mut contested, &json!([{"puppetMxid": "@_hagency_agt_a:s"}, {"puppetMxid": "@_hagency_csheargm_bird-claude:s", "ownerMxid": "@betty:s"}, {"puppetMxid": "@alice:s", "ownerMxid": "@x:s"}]), Some("@anna:s"));
        assert_eq!(out.get("@_hagency_agt_a:s").map(String::as_str), Some("@anna:s"));
        assert_eq!(out.get("@_hagency_csheargm_bird-claude:s").map(String::as_str), Some("@betty:s"), "readable puppet IDs count");
        assert!(!out.contains_key("@alice:s"), "a person is not an Agent");
        add_rows(&mut out, &mut contested, &json!([{"puppetMxid": "@_hagency_agt_a:s", "ownerMxid": "@mallory:s"}]), None);
        assert!(!out.contains_key("@_hagency_agt_a:s"));
    }
}
