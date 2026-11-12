//! Cached space membership for immediate room-list filtering after restart.
//! Adapted from project-robius/robrix c0a8644a. This is display data only;
//! joined-room state and all authorization still come from the Matrix client.

use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use matrix_sdk::{Client, RoomState};
use ruma::{OwnedRoomId, OwnedUserId, UserId};
use serde::{Deserialize, Serialize};

const VERSION: u8 = 1;
const MAX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSpace {
    pub parent_chain: Vec<OwnedRoomId>,
    pub rooms: HashSet<OwnedRoomId>,
    pub subspaces: HashSet<OwnedRoomId>,
}

pub type SavedSpaceGraph = HashMap<OwnedRoomId, SavedSpace>;

#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u8,
    user_id: OwnedUserId,
    graph: SavedSpaceGraph,
}

fn cache_path(user_id: &UserId) -> PathBuf {
    crate::accounts::account_dir(crate::cache_dir(), user_id).join("space_graph.json")
}

fn decode(bytes: &[u8], user_id: &UserId) -> anyhow::Result<SavedSpaceGraph> {
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "space graph cache exceeds size limit"
    );
    let cached: CacheFile = serde_json::from_slice(bytes)?;
    anyhow::ensure!(
        cached.version == VERSION && cached.user_id == user_id,
        "space graph cache identity/version mismatch"
    );
    Ok(cached.graph)
}

/// Drop explicitly left spaces unless still reachable through a joined parent.
/// A space may have several parents; the saved parent chain is only one path.
fn prune_left_spaces(
    graph: &mut SavedSpaceGraph,
    state: impl Fn(&OwnedRoomId) -> Option<RoomState>,
) {
    let mut dropped: SavedSpaceGraph = graph
        .extract_if(|id, space| {
            let mut left = false;
            for id in std::iter::once(id).chain(&space.parent_chain) {
                match state(id) {
                    Some(RoomState::Joined) => return false,
                    Some(RoomState::Left | RoomState::Banned) => left = true,
                    _ => {}
                }
            }
            left
        })
        .collect();
    let mut to_visit: Vec<_> = graph.keys().cloned().collect();
    while let Some(id) = to_visit.pop() {
        let subspaces = graph
            .get(&id)
            .map(|s| s.subspaces.clone())
            .unwrap_or_default();
        for subspace in subspaces {
            if let Some(saved) = dropped.remove(&subspace) {
                graph.insert(subspace.clone(), saved);
                to_visit.push(subspace);
            }
        }
    }
}

pub async fn load_space_graph(client: &Client, user_id: &UserId) -> SavedSpaceGraph {
    let path = cache_path(user_id);
    let client = client.clone();
    let user_id = user_id.to_owned();
    tokio::task::spawn_blocking(move || -> anyhow::Result<SavedSpaceGraph> {
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(SavedSpaceGraph::new()),
            Err(e) => return Err(e.into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        let mut graph = decode(&bytes, &user_id)?;
        prune_left_spaces(&mut graph, |id| {
            client.get_room(id).map(|room| room.state())
        });
        Ok(graph)
    })
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or_default()
}

/// Write atomically so an interrupted exit never destroys the previous cache.
fn save_at(path: &Path, graph: &SavedSpaceGraph, user_id: &UserId) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(&CacheFile {
        version: VERSION,
        user_id: user_id.to_owned(),
        graph: graph.clone(),
    })?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "space graph cache exceeds size limit"
    );
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| anyhow::anyhow!("cache path has no parent"))?,
    )?;
    let temporary = path.with_extension(format!("{:016x}.tmp", rand::random::<u64>()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> anyhow::Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub fn save_space_graph(graph: &SavedSpaceGraph, user_id: &UserId) -> anyhow::Result<()> {
    save_at(&cache_path(user_id), graph, user_id)
}

/// A fresh login creates a new store, which cannot tell us what was left while logged out.
pub async fn delete_space_graph(user_id: &UserId) {
    if let Err(e) = tokio::fs::remove_file(cache_path(user_id)).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        makepad_widgets::error!("Failed to remove space graph cache: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruma::{room_id, user_id};

    #[test]
    fn cache_roundtrip_rejects_wrong_account_version_and_corruption() {
        let user = user_id!("@alice:example.org");
        let graph = HashMap::from([(
            room_id!("!space:example.org").to_owned(),
            SavedSpace {
                rooms: HashSet::from([room_id!("!room:example.org").to_owned()]),
                ..Default::default()
            },
        )]);
        let mut file = CacheFile {
            version: VERSION,
            user_id: user.to_owned(),
            graph: graph.clone(),
        };
        let bytes = serde_json::to_vec(&file).unwrap();
        assert_eq!(decode(&bytes, user).unwrap(), graph);
        assert!(decode(&bytes, user_id!("@alice:other.org")).is_err());
        assert!(decode(&bytes[..bytes.len() - 1], user).is_err());
        file.version += 1;
        assert!(decode(&serde_json::to_vec(&file).unwrap(), user).is_err());
        assert!(decode(&vec![b' '; MAX_BYTES as usize + 1], user).is_err());
    }

    #[test]
    fn prune_retains_alternate_joined_parent_and_terminates_on_cycles() {
        let a = room_id!("!a:example.org").to_owned();
        let b = room_id!("!b:example.org").to_owned();
        let c = room_id!("!c:example.org").to_owned();
        let abandoned = room_id!("!left:example.org").to_owned();
        let mut graph = HashMap::from([
            (
                a.clone(),
                SavedSpace {
                    subspaces: HashSet::from([b.clone()]),
                    ..Default::default()
                },
            ),
            (
                b.clone(),
                SavedSpace {
                    parent_chain: vec![abandoned.clone()],
                    subspaces: HashSet::from([c.clone()]),
                    ..Default::default()
                },
            ),
            (
                c.clone(),
                SavedSpace {
                    parent_chain: vec![abandoned.clone()],
                    subspaces: HashSet::from([b.clone()]),
                    ..Default::default()
                },
            ),
            (abandoned.clone(), SavedSpace::default()),
        ]);
        prune_left_spaces(&mut graph, |id| {
            Some(if id == &a {
                RoomState::Joined
            } else {
                RoomState::Left
            })
        });
        assert_eq!(graph.len(), 3);
        assert!(graph.contains_key(&b) && graph.contains_key(&c));
        assert!(!graph.contains_key(&abandoned));
        prune_left_spaces(&mut graph, |_| Some(RoomState::Banned));
        assert!(graph.is_empty());
    }

    #[test]
    fn cache_file_is_atomic_and_account_paths_do_not_collide() {
        let dir = std::env::temp_dir().join(format!(
            "hagency-space-cache-{:016x}",
            rand::random::<u64>()
        ));
        let path = dir.join("graph.json");
        let user = user_id!("@alice:example.org");
        let graph = SavedSpaceGraph::new();
        save_at(&path, &graph, user).unwrap();
        assert_eq!(decode(&std::fs::read(&path).unwrap(), user).unwrap(), graph);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        assert_ne!(
            crate::accounts::account_dir(&dir, user),
            crate::accounts::account_dir(&dir, user_id!("@alice:other.org"))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
