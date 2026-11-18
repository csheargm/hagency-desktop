//! `repo_status` flows (ADR 0023): Anna's Agent reads what Betty's watcher
//! published; GitHub uses this device's token only for its own active watch.
use super::*;
use hagency_runtime::repo_status::{ISSUE_STATUS_STATE_TYPE, table::GithubState};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Mutex;

const ROOM_REPO: &str = "csheargm/hagency-demo-sandbox";
const BIRD: &str = "@_hagency_agt_bird:hs.test";
const BETTY: &str = "@betty:hs.test";
const MALLORY: &str = "@mallory:hs.test";
const NOW: i64 = 1_800_000_000;

struct Fake {
    events: Vec<Value>,
    linked: Vec<String>,
    own: Vec<OwnWatch>,
    reads: Mutex<Vec<(String, Option<String>)>>,
}
impl StatusIo for Fake {
    async fn published(&self) -> Result<StatusRoom, &'static str> {
        let mut members = BTreeMap::new();
        for (id, name) in [(BIRD, "bird-claude"), (BETTY, "Betty"), (MALLORY, "Mallory")] {
            members.insert(id.to_owned(), Member { joined: true, name: Some(name.into()) });
        }
        Ok(StatusRoom { events: self.events.clone(), members, linked: self.linked.clone() })
    }
    async fn own_watches(&self) -> Result<Vec<OwnWatch>, &'static str> {
        Ok(self.own.clone())
    }
    async fn github(&self, repo: &str, agent: Option<&str>, _since: i64) -> Result<Vec<GithubItem>, String> {
        self.reads.lock().unwrap().push((repo.to_owned(), agent.map(str::to_owned)));
        Ok(vec![GithubItem { number: 37, title: "Crash on start".into(), is_pr: false, state: GithubState::Open, closed_at: None }])
    }
    fn now(&self) -> i64 {
        NOW
    }
}
fn published(sender: &str, key: &str, state: &str) -> Value {
    json!({"type": ISSUE_STATUS_STATE_TYPE, "sender": sender, "state_key": key, "content": {"v": 1, "issues": [
        {"agent": BIRD, "repo": ROOM_REPO, "number": 37, "title": "Crash on start", "state": state, "attempts": 1, "pr": 38, "updatedAt": NOW - 60}]}})
}
fn fake(events: Vec<Value>, linked: Vec<String>, own: Vec<OwnWatch>) -> Fake {
    Fake { events, linked, own, reads: Mutex::new(Vec::new()) }
}

#[tokio::test]
async fn another_owners_watcher_state_is_read_and_github_is_read_publicly() {
    let io = fake(vec![published(BETTY, BETTY, "reviewed")], Vec::new(), Vec::new());
    let out = answer(&io, None).await;
    assert!(out.contains("in review (attempt 1) · PR #38 · bird-claude (Betty, @betty:hs.test)"), "{out}");
    assert!(out.contains("1 in progress across 1 repo"), "{out}");
    assert_eq!(*io.reads.lock().unwrap(), vec![(ROOM_REPO.to_owned(), None)], "never this device's token for another owner's repository");
}

#[tokio::test]
async fn a_spoofed_publisher_is_ignored() {
    let io = fake(vec![published(MALLORY, BETTY, "merged")], Vec::new(), Vec::new());
    let out = answer(&io, Some(ROOM_REPO)).await;
    assert!(out.contains("repo_not_in_this_room"), "a forged event adds no repository: {out}");
    let io = fake(vec![published(BETTY, BETTY, "reviewed"), published(MALLORY, MALLORY, "merged")], Vec::new(), Vec::new());
    let out = answer(&io, None).await;
    assert!(!out.contains("bird-claude (Mallory") && !out.contains("bird-claude (Betty, @betty:hs.test)"), "a contested Agent shows nobody's claim: {out}");
    assert!(out.contains("claimed by more than one owner"));
}

#[tokio::test]
async fn own_active_watch_uses_its_agent_token_and_linked_repos_get_sections() {
    let own = vec![OwnWatch { agent: "agt_anna".into(), repo: "h/server".into(), active: true }, OwnWatch { agent: "agt_off".into(), repo: "h/paused".into(), active: false }];
    let io = fake(vec![published(BETTY, BETTY, "pr_opened")], vec!["h/desktop".into()], own);
    let out: Value = serde_json::from_str(&answer(&io, None).await).unwrap();
    let repos: Vec<&str> = out["repos"].as_array().unwrap().iter().map(|r| r["repo"].as_str().unwrap()).collect();
    assert_eq!(repos, vec![ROOM_REPO, "h/desktop", "h/paused", "h/server"]);
    let reads = io.reads.lock().unwrap().clone();
    assert!(reads.contains(&("h/server".into(), Some("agt_anna".into()))));
    assert!(reads.contains(&("h/paused".into(), None)), "a paused watch reads publicly");
    assert!(reads.contains(&("h/desktop".into(), None)));
    assert_eq!(out["summary"], "1 in progress across 4 repos");
}

#[tokio::test]
async fn a_room_without_repositories_says_so() {
    let out: Value = serde_json::from_str(&answer(&fake(Vec::new(), Vec::new(), Vec::new()), None).await).unwrap();
    assert!(out["repos"].as_array().unwrap().is_empty());
    assert!(out["notes"][0].as_str().unwrap().contains("No repository"));
}
