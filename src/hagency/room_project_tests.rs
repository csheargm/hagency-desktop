use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

const ROOM: &str = "!a3cQXHR8GZ4RP5n71s:localhost:8450";

fn projects(count: usize) -> Value {
    json!({"projects": (0..count).map(|i| json!({"id": format!("prj_{i}")})).collect::<Vec<_>>()})
}

fn rooms_of(project: &str, wanted: &str) -> Value {
    let listed = if project == wanted { json!([{"roomId": ROOM, "active": true}]) } else { json!([{"roomId": "!other:x", "active": true}]) };
    json!({"rooms": listed})
}

#[tokio::test]
async fn many_projects_are_read_a_few_at_a_time_not_one_after_another() {
    let (now, most) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let found = find_project(ROOM, &projects(40), |id| {
        let (now, most) = (&now, &most);
        async move {
            most.fetch_max(now.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
            now.fetch_sub(1, Ordering::SeqCst);
            Ok(rooms_of(&id, "prj_39"))
        }
    }).await;
    assert_eq!(found, Ok(Some("prj_39".into())), "the room's project, the last one listed");
    let most = most.load(Ordering::SeqCst);
    assert!(most > 1 && most <= PARALLEL_READS, "bounded parallel reads, got {most}");
}

#[tokio::test]
async fn a_failed_read_fails_only_when_no_project_lists_the_room() {
    let read = |id: String| async move { if id == "prj_0" { Err("server_unavailable".to_owned()) } else { Ok(rooms_of(&id, "prj_2")) } };
    assert_eq!(find_project(ROOM, &projects(3), read).await, Ok(Some("prj_2".into())));
    assert_eq!(find_project(ROOM, &projects(2), read).await, Err("server_unavailable".into()), "never 'in no project' after a failed read");
    let nowhere = |id: String| async move { Ok(rooms_of(&id, "none")) };
    assert_eq!(find_project(ROOM, &projects(3), nowhere).await, Ok(None));
}

#[tokio::test]
async fn an_unreadable_or_oversized_listing_is_its_own_code() {
    let never = |_: String| async { Ok(json!({})) };
    assert_eq!(find_project(ROOM, &json!({}), never).await, Err(UNREADABLE.into()));
    assert_eq!(find_project(ROOM, &projects(MAX_PROJECTS + 1), never).await, Err("room_discovery_incomplete".into()));
    assert_ne!(TIMED_OUT, UNREADABLE, "a timeout says so");
}

#[test]
fn a_found_project_is_remembered_per_server_owner_and_room() {
    let key = Key::new("https://alpha.test", "@anna:alpha.test", "!cache-test:alpha.test");
    assert_eq!(cached(&key), None);
    remember(&key, "prj_dev");
    assert_eq!(cached(&key).as_deref(), Some("prj_dev"));
    assert_eq!(cached(&Key::new("https://alpha.test", "@bo:alpha.test", "!cache-test:alpha.test")), None, "another owner reads its own");
    forget(&key);
    assert_eq!(cached(&key), None);
}
