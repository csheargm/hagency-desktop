//! `repo_status` for one admitted dispatch (ADR 0023): every repository of
//! the dispatch's room (watched by any owner's Agent, or linked by hand), or
//! one of them, as a table of GitHub's open and recently closed items merged
//! with the tracker states the watchers' owners published to the room.
//! GitHub is read with this device's Agent token only for a repository this
//! device's own Agent actively watches for this room (the ADR 0019 read-only
//! scope); any other repository is read without a token, so a published or
//! linked name can never make this device read a private repository.
use super::Source;
use super::e2ee::StatusRoom;
use hagency_runtime::repo_status::{self as status, Member, linked::room_repos, table::{self, GithubItem}};
use serde_json::json;

/// One of this device's Agents' watches that reports to the room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OwnWatch {
    pub agent: String,
    pub repo: String,
    pub active: bool,
}

pub(super) trait StatusIo {
    async fn published(&self) -> Result<StatusRoom, &'static str>;
    async fn own_watches(&self) -> Result<Vec<OwnWatch>, &'static str>;
    /// `agent`: read with that Agent's token; `None`: a public read.
    async fn github(&self, repo: &str, agent: Option<&str>, since: i64) -> Result<Vec<GithubItem>, String>;
    /// Agent puppet → owner, from the server's listings only.
    async fn owners(&self) -> std::collections::BTreeMap<String, String>;
    fn now(&self) -> i64;
}

impl StatusIo for std::sync::Arc<Source> {
    async fn published(&self) -> Result<StatusRoom, &'static str> {
        let room = &self.gate.expected.room_id;
        self.authorize(Some(room)).await?;
        self.device.issue_status_room(room).await.map_err(|_| "status_unavailable")
    }
    async fn own_watches(&self) -> Result<Vec<OwnWatch>, &'static str> {
        let (source, room) = (self.clone(), self.gate.expected.room_id.clone());
        super::watches::blocking(move || {
            let ledger = super::watches::ledger(&source)?;
            let mut out = Vec::new();
            for agent in ledger.watching_agents().map_err(|_| "watches_unavailable")? {
                let entries = ledger.watch_entries(&agent).map_err(|_| "watches_unavailable")?;
                out.extend(entries.into_iter().filter(|e| e.room_id == room).map(|e| OwnWatch { agent: agent.clone(), repo: e.repo, active: e.active }));
            }
            Ok(out)
        })
        .await
    }
    async fn github(&self, repo: &str, agent: Option<&str>, since: i64) -> Result<Vec<GithubItem>, String> {
        // An own Agent with no token reads publicly, as for anyone else's repository.
        let token = match agent.map(str::to_owned) {
            Some(agent) => tokio::task::spawn_blocking(move || crate::github_access::TokenStore::current().runtime_environment(&agent).unwrap_or_default().agent_github_token())
                .await
                .map_err(|_| "token store unavailable".to_owned())?,
            None => None,
        };
        let client = crate::issue_watch::new_client();
        crate::issue_watch::fetch_status_items(&client, crate::issue_watch::GITHUB_API, repo, token.as_deref(), since)
            .await
            .map_err(|e| short(&e.to_string()))
    }
    async fn owners(&self) -> std::collections::BTreeMap<String, String> {
        super::owners::of_room(self).await
    }
    fn now(&self) -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or_default()
    }
}

/// A GitHub failure as one short line (never a response body).
fn short(error: &str) -> String {
    status::one_line(error.split(['\n', '{']).next().unwrap_or_default(), 120)
}

/// The tool result for `requested` (one repository) or every repository of the room.
pub(super) async fn answer(io: &impl StatusIo, requested: Option<&str>) -> String {
    let (room, own) = match (io.published().await, io.own_watches().await) {
        (Ok(room), Ok(own)) => (room, own),
        (Err(code), _) | (_, Err(code)) => return json!({"error": code}).to_string(),
    };
    let member = |id: &str| room.members.get(id).cloned().unwrap_or(Member::default());
    let owners = io.owners().await;
    let accepted = status::accepted(&room.events, member, |agent| owners.get(agent).cloned(), io.now());
    let watched = own.iter().map(|w| w.repo.as_str()).chain(accepted.seen.iter().map(|s| s.issue.repo.as_str()));
    let all = room_repos(watched, &room.linked);
    let repos: Vec<String> = match requested {
        Some(wanted) => match all.iter().find(|r| r.eq_ignore_ascii_case(wanted)) {
            Some(repo) => vec![repo.clone()],
            None => return json!({"error": "repo_not_in_this_room", "roomRepos": all.iter().take(20).collect::<Vec<_>>()}).to_string(),
        },
        None => all,
    };
    let mut notes = Vec::new();
    if room.events.is_empty() && room.linked.is_empty() && own.is_empty() {
        notes.push("No repository is watched for or linked to this room.".to_owned());
    }
    if accepted.ignored_claims > 0 {
        notes.push(format!("{} published state(s) were not from the Agent's own owner, or were stale, and are not shown.", accepted.ignored_claims));
    }
    let name = |id: &str| room.members.get(id).and_then(|m| m.name.clone()).unwrap_or_else(|| id.to_owned());
    let (now, mut tables) = (io.now(), Vec::new());
    for repo in repos.iter().take(MAX_REPOS) {
        let agent = own.iter().find(|w| w.active && w.repo.eq_ignore_ascii_case(repo)).map(|w| w.agent.as_str());
        let github = match io.github(repo, agent, now - status::RECENT_SECS).await {
            Ok(items) => Some(items),
            Err(error) => {
                notes.push(format!("GitHub could not be read for {repo}: {error}"));
                None
            }
        };
        tables.push(table::build(repo, github.as_deref(), &accepted.seen, name, now));
    }
    if repos.len() > MAX_REPOS {
        notes.push(format!("{} more repositories; ask for one by name.", repos.len() - MAX_REPOS));
    }
    table::render(&tables, &notes)
}

/// Before each turn: the room's repository set beyond this device's own
/// watches, for the card-free read-only GitHub queries (ADR 0023). Only
/// PUBLIC repositories are recorded: the query runs with this Agent's token,
/// and a name in room state must never let it read a private repository
/// without its owner's card (GPT review). A room that cannot be read, or a
/// visibility that cannot be checked, records nothing: nothing is widened.
pub(super) async fn record_read_scope(source: &std::sync::Arc<Source>) {
    let d = &source.gate.expected;
    let owners = super::owners::of_room(source).await;
    let named = source.device.read_scope(&d.room_id, &d.requester_mxid, &owners, source.now()).await.unwrap_or_default();
    let client = crate::issue_watch::new_client();
    let mut public = Vec::new();
    for repo in named.into_iter().take(MAX_SCOPE_CHECKS) {
        if public_repo(&client, &repo).await {
            public.push(repo);
        }
    }
    hagency_agent_local::watch_trust::room_scope::record(&d.room_id, &d.requester_mxid, public);
}

/// Visibility checks per turn; a room naming more repositories widens only these.
const MAX_SCOPE_CHECKS: usize = 20;
/// A repository that turns private stops being card-free within this long (GPT integration review).
const VISIBILITY_TTL: std::time::Duration = std::time::Duration::from_secs(60);
type Visibility = std::collections::HashMap<String, (std::time::Instant, bool)>;
static VISIBILITY: std::sync::LazyLock<std::sync::Mutex<Visibility>> = std::sync::LazyLock::new(Default::default);

/// Whether GitHub shows `repo` without a token, remembered for [`VISIBILITY_TTL`];
/// a failed check is never remembered and counts as private.
async fn public_repo(client: &crate::issue_watch::Client, repo: &str) -> bool {
    let key = repo.to_ascii_lowercase();
    // A poisoned lock still holds a whole map: inserts are single operations.
    if let Some((at, public)) = VISIBILITY.lock().unwrap_or_else(|p| p.into_inner()).get(&key)
        && at.elapsed() < VISIBILITY_TTL
    {
        return *public;
    }
    // An unchecked repository is treated as private: it then still asks its owner.
    let Ok(public) = crate::issue_watch::fetch_is_public(client, crate::issue_watch::GITHUB_API, repo).await else { return false };
    VISIBILITY.lock().unwrap_or_else(|p| p.into_inner()).insert(key, (std::time::Instant::now(), public));
    public
}

/// Repositories read in one call (two GitHub requests each).
const MAX_REPOS: usize = 6;

#[cfg(test)]
#[path = "repo_status_tests.rs"]
mod tests;
