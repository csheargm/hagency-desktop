//! The room's status for the Status tab and `/status` (ADR 0023): open
//! Agent tasks for every room, then, only when the room has repositories
//! (watched by any owner's Agent, or linked by hand), one table per
//! repository from GitHub and the tracker states the watchers' owners
//! published. GitHub is read with this owner's token only for a repository
//! this owner's own Agent actively watches for the room.
use super::tasks::{self, Task};
use ::hagency::repo_status::{self as status, ISSUE_STATUS_STATE_TYPE, Member, Seen, linked::{self, ROOM_REPOS_STATE_TYPE}, table::{self, Table}};
use matrix_sdk::{Client, room::{MessagesOptions, Room}, ruma::{RoomId, UserId, events::{StateEventType, room::member::MembershipState}}};
use serde_json::Value;
use std::collections::BTreeMap;

/// Repositories the tab reads per load (two GitHub requests each).
const MAX_TABLES: usize = 10;

/// One of this owner's watches reporting to the room (agent id, repo, active).
pub type OwnWatch = (String, String, bool);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoomStatus {
    pub tasks: Vec<Task>,
    /// Empty for a room without repositories: no issue or pull request UI then.
    pub tables: Vec<Table>,
    pub linked: Vec<String>,
    /// Whether this account's power level lets it link and unlink repositories.
    pub can_link: bool,
    pub notes: Vec<String>,
    /// Display names by Matrix ID, for tasks and activity lines.
    pub names: BTreeMap<String, String>,
    /// Accepted published issues, for the members list's activity lines.
    pub seen: Vec<Seen>,
}

/// "working on #37 (PR #38 in review)", "waiting for approval" or "idle".
/// Published state counts only from `owner`, the Agent's owner as the
/// server's roster names it: a member's claim about someone else's Agent is
/// never shown on that Agent's row (GPT review). Unknown owner: tasks only.
pub fn activity(status: &RoomStatus, agent: &str, owner: Option<&str>) -> String {
    let busy = status.seen.iter().filter(|s| s.issue.agent == agent && Some(s.publisher.as_str()) == owner && !s.issue.state.is_terminal()).max_by_key(|s| s.issue.updated_at);
    if let Some(s) = busy {
        let i = &s.issue;
        return match i.pr {
            Some(pr) if awaits_person(i) => crate::i18n::format("working on #{number} (PR #{pr} ready — a person merges)", &[("number", i.number.to_string()), ("pr", pr.to_string())]),
            Some(pr) => crate::i18n::format("working on #{number} (PR #{pr} {state})", &[("number", i.number.to_string()), ("pr", pr.to_string()), ("state", crate::i18n::tr(i.state.words()).to_owned())]),
            None => crate::i18n::format("working on #{number}", &[("number", i.number.to_string())]),
        };
    }
    match status.tasks.iter().find(|t| t.agent == agent && t.state.open()) {
        Some(t) => crate::i18n::tr(t.state.words()).to_owned(),
        None => crate::i18n::tr("idle").to_owned(),
    }
}

/// Reviewed and held only by the room's level (`merge_hold::only_assist`):
/// the room was told "is ready … a person merges it", so the line says so
/// too. Any other hold, also the Autonomy label of a stale authority or a
/// missing permission, stays "in review" (GPT review).
fn awaits_person(issue: &::hagency::repo_status::Published) -> bool {
    issue.state == ::hagency::repo_status::TrackState::Reviewed
        && issue.hold.as_deref() == Some(crate::hagency::issue_watch::merge_hold::LEVEL_ONLY_HOLD)
}

async fn member(room: &Room, id: &str) -> Member {
    let Ok(user) = UserId::parse(id) else { return Member::default() };
    // A member that cannot be read counts as absent: it is never trusted.
    match room.get_member_no_sync(&user).await {
        Ok(Some(m)) => Member { joined: *m.membership() == MembershipState::Join, name: m.display_name().map(str::to_owned) },
        _ => Member::default(),
    }
}

async fn recent_events(room: &Room, encrypted: bool) -> Result<Vec<Value>, String> {
    let (mut out, mut from) = (Vec::new(), None::<String>);
    for _ in 0..2 {
        let mut options = MessagesOptions::backward();
        options.from = from.clone();
        options.limit = (tasks::SCAN_EVENTS / 2).into();
        let page = room.messages(options).await.map_err(|e| crate::i18n::format("Room history is unavailable: {error}", &[("error", e.to_string())]))?;
        out.extend(page.chunk.iter().filter_map(|e| tasks::authentic_json(e, encrypted)));
        match page.end {
            Some(end) => from = Some(end),
            None => break,
        }
    }
    Ok(out)
}

pub async fn load(client: &Client, room_id: &RoomId, own: &[OwnWatch]) -> Result<RoomStatus, String> {
    let room = client.get_room(room_id).ok_or_else(|| crate::i18n::tr("This desktop has not joined the room").to_owned())?;
    let me = client.user_id().ok_or("Signed out")?.to_owned();
    let security = room.latest_encryption_state().await.map_err(|e| e.to_string())?;
    if security.is_unknown() {
        return Err(crate::i18n::tr("The room's encryption is not known yet").to_owned());
    }
    let encrypted = security.is_encrypted();
    let mut out = RoomStatus::default();
    let (events, repos_state) = if encrypted {
        out.notes.push(crate::i18n::tr("Issue status is not shared in end-to-end encrypted rooms.").to_owned());
        (Vec::new(), Value::Null)
    } else {
        let events = crate::home::room_memory::server_state::events_of_type(client, room_id, ISSUE_STATUS_STATE_TYPE).await?;
        let repos = crate::home::room_memory::server_state::events_of_type(client, room_id, ROOM_REPOS_STATE_TYPE).await?;
        (events, repos.into_iter().find(|e| e["state_key"] == "").unwrap_or_default())
    };
    out.linked = linked::linked(&repos_state["content"]);
    out.can_link = !encrypted && room.power_levels().await.is_ok_and(|l| l.user_can_send_state(&me, StateEventType::from(ROOM_REPOS_STATE_TYPE)));
    let recent = recent_events(&room, encrypted).await?;
    let mut members: BTreeMap<String, Member> = BTreeMap::new();
    let senders = recent.iter().filter_map(|e| e["sender"].as_str().map(str::to_owned));
    for id in status::people_in(&events).into_iter().chain(senders).collect::<std::collections::BTreeSet<_>>() {
        members.insert(id.clone(), member(&room, &id).await);
    }
    let now = chrono::Utc::now();
    let owners = owners_of(room_id, &me, &mut out.notes).await;
    out.tasks = tasks::tasks(&recent, |id| members.get(id).is_some_and(|m| m.joined) && owners.contains_key(id), now.timestamp_millis());
    for task in &out.tasks {
        if !members.contains_key(&task.requester) {
            members.insert(task.requester.clone(), member(&room, &task.requester).await);
        }
    }
    let accepted = status::accepted(&events, |id| members.get(id).cloned().unwrap_or_default(), |agent| owners.get(agent).cloned(), now.timestamp());
    if accepted.ignored_claims > 0 {
        out.notes.push(crate::i18n::tr("Some agent status updates could not be confirmed or were out of date, so they are hidden.").to_owned());
    }
    out.names = members.iter().filter_map(|(id, m)| m.name.clone().map(|n| (id.clone(), n))).collect();
    let watched = own.iter().map(|(_, repo, _)| repo.as_str()).chain(accepted.seen.iter().map(|s| s.issue.repo.as_str()));
    let repos = linked::room_repos(watched, &out.linked);
    tables(&mut out, &repos, own, &accepted.seen, now.timestamp()).await;
    out.seen = accepted.seen;
    Ok(out)
}

/// Whose Agent each puppet is, from the server's listings only (GPT
/// integration review); unavailable listings leave the map empty (fail closed).
async fn owners_of(room_id: &RoomId, me: &matrix_sdk::ruma::OwnedUserId, notes: &mut Vec<String>) -> BTreeMap<String, String> {
    match crate::home::mobile_chat_info::fetch_member_owners(room_id.as_str(), me).await {
        Ok((owners, _)) => owners.into_iter().map(|(agent, owner)| (agent.to_string(), owner.to_string())).collect(),
        Err(error) => {
            notes.push(crate::i18n::format("Agent owners are unavailable: {error}", &[("error", error)]));
            BTreeMap::new()
        }
    }
}

/// One table per repository; a GitHub failure becomes a note and a table of tracker states only.
async fn tables(out: &mut RoomStatus, repos: &[String], own: &[OwnWatch], seen: &[Seen], now: i64) {
    let github = ::hagency::issue_watch::new_client();
    for repo in repos {
        if out.tables.len() >= MAX_TABLES {
            out.notes.push(crate::i18n::format("{n} more repositories not shown", &[("n", (repos.len() - MAX_TABLES).to_string())]));
            break;
        }
        let token = match own.iter().find(|(_, r, active)| *active && r.eq_ignore_ascii_case(repo)) {
            Some((agent, _, _)) => crate::hagency::issue_watch::github_token(agent).await.ok(),
            None => None,
        };
        let since = now - status::RECENT_SECS;
        let items = match ::hagency::issue_watch::fetch_status_items(&github, ::hagency::issue_watch::GITHUB_API, repo, token.as_deref(), since).await {
            Ok(items) => Some(items),
            Err(error) => {
                out.notes.push(crate::i18n::format("GitHub could not be read for {repo}: {error}", &[("repo", repo.clone()), ("error", status::one_line(&error.to_string(), 120))]));
                None
            }
        };
        let name = |id: &str| out.names.get(id).cloned().unwrap_or_else(|| id.to_owned());
        let built = table::build(repo, items.as_deref(), seen, name, now);
        out.tables.push(built);
    }
}

/// Link (`true`) or unlink `repo` under this account's own power level; the
/// homeserver refuses a member below the room's level for the event. `Some`
/// note when linked but a room admin still has power levels to set.
pub async fn set_link(client: &Client, room_id: &RoomId, repo: &str, link: bool) -> Result<Option<String>, String> {
    let room = change_repos(client, room_id, repo, link).await?;
    if link { after_link(&room).await } else { Ok(None) }
}

/// The repository list change alone, its read-modify-write one at a time per
/// room (GPT review); the power-level follow-up ([`after_link`]) runs
/// outside that lock, and a slow room never holds up another room's link.
pub async fn change_repos(client: &Client, room_id: &RoomId, repo: &str, link: bool) -> Result<Room, String> {
    static LINKING: crate::home::call_power_levels::RoomLocks = crate::home::call_power_levels::RoomLocks::new();
    let lock = LINKING.of(room_id);
    let _linking = lock.lock().await;
    let room = client.get_room(room_id).ok_or_else(|| crate::i18n::tr("This desktop has not joined the room").to_owned())?;
    if !room.latest_encryption_state().await.is_ok_and(|s| !s.is_unknown() && !s.is_encrypted()) {
        // Plaintext state would show the repositories to the homeserver (ADR 0023).
        return Err(crate::i18n::tr("Repositories can be linked only in rooms without end-to-end encryption").to_owned());
    }
    let events = crate::home::room_memory::server_state::events_of_type(client, room_id, ROOM_REPOS_STATE_TYPE).await?;
    let current = events.into_iter().find(|e| e["state_key"] == "").map(|e| e["content"].clone()).unwrap_or_default();
    let next = if link {
        linked::link(&current, repo).map_err(|e| crate::i18n::tr(match e {
            linked::LinkError::NotARepo => "Enter a repository as owner/name",
            linked::LinkError::Full => "This room links as many repositories as it can",
        }).to_owned())?
    } else {
        linked::unlink(&current, repo)
    };
    if let Some(content) = next {
        room.send_state_event_raw(ROOM_REPOS_STATE_TYPE, "", content).await
            .map_err(|e| crate::i18n::format("Could not change the room's repositories: {error}", &[("error", e.to_string())]))?;
    }
    Ok(room)
}

/// After a link — also when the repository was already linked: linking again
/// is how an admin opens an old room — the missing power-level entries.
pub async fn after_link(room: &Room) -> Result<Option<String>, String> {
    allow_member_status(room).await.map(|left| link_note(&left))
}

/// What the link says when power-level entries are left for a room admin
/// (GPT integration review: it looked done while members still could not
/// share issue status, or moderators could still change the merge rules).
pub fn link_note(left: &[&str]) -> Option<String> {
    let status = left.contains(&::hagency::repo_status::ISSUE_STATUS_STATE_TYPE);
    let rules = left.contains(&::hagency::issue_watch::safeguards::MERGE_RULES_STATE_TYPE);
    let note = match (status, rules) {
        (true, true) => "Linked. A room admin still needs to link it too, to let members share issue status and to make the merge rules admin-only.",
        (true, false) => "Linked. A room admin still needs to link it too, to let members share issue status.",
        (false, true) => "Linked. A room admin still needs to link it too, to make the merge rules admin-only (until then moderators can change them).",
        (false, false) => return None,
    };
    Some(crate::i18n::tr(note).to_owned())
}

/// Linking is the admin's go-ahead to let members publish their own issue
/// status in a room that predates it (ADR 0023), and, for an admin, to make
/// the merge rules admin-only (ADR 0029), when this account may change power
/// levels; otherwise nothing is sent. Only missing entries are added.
async fn allow_member_status(room: &Room) -> Result<Vec<&'static str>, String> {
    crate::home::call_power_levels::allow_member_state_in(room).await
        .map_err(|e| crate::i18n::format("Members cannot share issue status here yet: {error}", &[("error", format!("{e:#}"))]))
}
