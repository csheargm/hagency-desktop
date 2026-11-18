//! Repositories linked to a room by hand (ADR 0023), beside those any
//! owner's Agent watches for it: one `io.hagency.room_repos` state event with
//! an empty state key, so the room's power levels decide who may change it
//! (the homeserver enforces that), and every member reads the same list.
use super::repo_shaped;
use serde_json::{Value, json};

pub const ROOM_REPOS_STATE_TYPE: &str = "io.hagency.room_repos";
/// Repositories one room links by hand.
pub const MAX_LINKED: usize = 20;

/// The linked repositories in `content`, cleaned, deduplicated, bounded.
pub fn linked(content: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for repo in content["repos"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::trim) {
        if repo_shaped(repo) && !out.iter().any(|r| r.eq_ignore_ascii_case(repo)) && out.len() < MAX_LINKED {
            out.push(repo.to_owned());
        }
    }
    out
}

/// Why a link or unlink was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkError {
    NotARepo,
    Full,
}

/// The content after linking `repo`; `Ok(None)` when it is already linked.
pub fn link(content: &Value, repo: &str) -> Result<Option<Value>, LinkError> {
    let repo = repo.trim().trim_start_matches("https://github.com/").trim_end_matches('/').trim_end_matches(".git");
    if !repo_shaped(repo) {
        return Err(LinkError::NotARepo);
    }
    let mut repos = linked(content);
    if repos.iter().any(|r| r.eq_ignore_ascii_case(repo)) {
        return Ok(None);
    }
    if repos.len() >= MAX_LINKED {
        return Err(LinkError::Full);
    }
    repos.push(repo.to_owned());
    Ok(Some(json!({"repos": repos})))
}

/// The content after unlinking `repo`; `None` when it was not linked.
pub fn unlink(content: &Value, repo: &str) -> Option<Value> {
    let repos = linked(content);
    let kept: Vec<&String> = repos.iter().filter(|r| !r.eq_ignore_ascii_case(repo.trim())).collect();
    (kept.len() != repos.len()).then(|| json!({"repos": kept}))
}

/// Every repository of a room: watched (from any owner) and linked, once each, sorted.
pub fn room_repos<'a>(watched: impl IntoIterator<Item = &'a str>, linked: &'a [String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for repo in watched.into_iter().chain(linked.iter().map(String::as_str)) {
        if repo_shaped(repo) && !out.iter().any(|r| r.eq_ignore_ascii_case(repo)) {
            out.push(repo.to_owned());
        }
    }
    out.sort_by_key(|r| r.to_ascii_lowercase());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_unlink_and_bounds() {
        let one = link(&Value::Null, "https://github.com/hagency-org/hagency-desktop.git").unwrap().unwrap();
        assert_eq!(linked(&one), vec!["hagency-org/hagency-desktop"]);
        assert_eq!(link(&one, "HAGENCY-ORG/hagency-desktop"), Ok(None));
        assert_eq!(link(&one, "not a repo"), Err(LinkError::NotARepo));
        let full = json!({"repos": (0..MAX_LINKED).map(|n| format!("o/r{n}")).collect::<Vec<_>>()});
        assert_eq!(link(&full, "o/new"), Err(LinkError::Full));
        assert_eq!(linked(&unlink(&one, "hagency-org/hagency-desktop").unwrap()), Vec::<String>::new());
        assert_eq!(unlink(&one, "o/absent"), None);
        assert_eq!(linked(&json!({"repos": ["a/b", "A/B", "../x", 5]})), vec!["a/b"]);
    }

    #[test]
    fn a_rooms_repos_join_watched_and_linked_once() {
        let repos = room_repos(["h/server", "h/desktop"], &["H/DESKTOP".into(), "h/client".into()]);
        assert_eq!(repos, vec!["h/client", "h/desktop", "h/server"]);
        assert!(room_repos([], &[]).is_empty());
    }
}
