//! GitHub issue watch, desktop side: every `POLL_INTERVAL` one account-scoped
//! task reads each Agent's watch from this device's ledger (owner API), lists
//! the repository's open issues with the Agent's GitHub token, and hands the
//! chosen ones to the Agent by posting the watcher's notice ("New GitHub
//! issue #N ... @Agent please fix it", see `notice`) into the chosen room. The Agent's normal
//! dispatch and approval flow applies. The task runs in the account session,
//! so sign-out aborts it; an Agent set to Off is skipped. The result of each
//! check is a status line under the setting (`section`).
pub(crate) mod choices;
mod handoff;
pub(crate) mod live;
pub mod notice;
pub mod section;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod notice_tests;

use crate::hagency::{backend::Service, model::Command};
use ::hagency::github_access::{Token, TokenStore};
use ::hagency::issue_watch::{Client, GITHUB_API, IssueWatch, WatchMode, fetch_open_issues, select};
use handoff::Outcome;
use makepad_widgets::Cx;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

/// How often each watched repository is checked.
pub const POLL_INTERVAL: Duration = Duration::from_secs(60);
/// Status key for a failure that is not about one Agent (the Agents list).
const ALL_AGENTS: &str = "*";
/// The environment fallback for an Agent without its own token, first match
/// wins (the `gh` CLI's order).
pub const TOKEN_VARIABLES: [&str; 2] = ["GH_TOKEN", "GITHUB_TOKEN"];

/// Posted after each check so an open settings view shows the new line.
#[derive(Clone, Debug)]
pub struct IssueWatchChecked;

/// Latest status line per Agent, for the account epoch it was written in.
static STATUS: LazyLock<Mutex<(u64, HashMap<String, String>)>> = LazyLock::new(Default::default);
/// `epoch + 1` of the account session the poller runs in (0: none yet).
static STARTED: AtomicU64 = AtomicU64::new(0);

/// The last check's line for this Agent, if it is watched in this session.
pub fn status_line(agent_id: &str) -> Option<String> {
    let status = STATUS.lock().unwrap_or_else(|p| p.into_inner());
    if status.0 != crate::account_session::epoch() {
        return None;
    }
    status.1.get(agent_id).or_else(|| status.1.get(ALL_AGENTS)).cloned()
}
/// Stores a line written by the check of account session `epoch`; a check
/// that outlived its session writes nothing.
fn record(epoch: u64, agent_id: &str, line: Option<String>) {
    let mut status = STATUS.lock().unwrap_or_else(|p| p.into_inner());
    if epoch != crate::account_session::epoch() {
        return;
    }
    if status.0 != epoch {
        *status = (epoch, HashMap::new());
    }
    match line {
        Some(line) => status.1.insert(agent_id.to_owned(), line),
        None => status.1.remove(agent_id),
    };
}
/// Drops the lines of Agents that are gone or no longer active.
fn prune(epoch: u64, active: &HashSet<String>) {
    let mut status = STATUS.lock().unwrap_or_else(|p| p.into_inner());
    if status.0 == epoch {
        status.1.retain(|id, _| id == ALL_AGENTS || active.contains(id));
    }
}
/// `Err` once the account session `epoch` has ended, so an old check stops
/// before it sends or saves anything as the next account.
fn still_current(epoch: u64) -> Result<(), String> {
    if crate::account_session::epoch() == epoch && !crate::account_session::is_closing() {
        Ok(())
    } else {
        Err(crate::i18n::tr("The account changed during the check").to_owned())
    }
}

/// The longest one Agent's check may take.
const CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

/// Starts the poller once per account session; cheap to call on every tick.
pub fn ensure_running() {
    let epoch = crate::account_session::epoch();
    if STARTED.swap(epoch + 1, Ordering::AcqRel) == epoch + 1 {
        return;
    }
    crate::account_session::spawn(run(epoch));
}
async fn run(epoch: u64) {
    let client = Client::new();
    while still_current(epoch).is_ok() {
        check_all(epoch, &client).await;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn check_all(epoch: u64, client: &Client) {
    // Signed out or switching accounts: there is no Agent to watch yet, and
    // the next tick tries again.
    let Ok(service) = Service::for_current() else { return };
    let agents = match service.execute(Command::Agents).await.and_then(|listed| active_agents(&listed)) {
        Ok(agents) => agents,
        Err(error) => return report(epoch, ALL_AGENTS, Some(failed_line(&now(), &error))),
    };
    record(epoch, ALL_AGENTS, None);
    prune(epoch, &agents.iter().filter_map(|a| a["id"].as_str().map(str::to_owned)).collect());
    for agent in &agents {
        let Some(id) = agent["id"].as_str() else { continue };
        // Bounded: one Agent whose host does not answer must not stop every later check.
        let checked = tokio::time::timeout(CHECK_TIMEOUT, check_agent(epoch, &service, client, agent)).await
            .unwrap_or_else(|_| Err(crate::i18n::tr("No answer from this device's Agent host").to_owned()));
        if let Err(error) = &checked {
            makepad_widgets::log!("WARN issue_watch_check_failed agent={id}: {error}");
        }
        let line = match checked {
            Ok(outcome) => outcome.map(|outcome| outcome_line(&now(), &outcome)),
            Err(error) => Some(failed_line(&now(), &error)),
        };
        report(epoch, id, line);
    }
}
/// The active Agents of an Agents reply; a reply without an `agents` list is
/// an error, not "no Agents", so a schema change cannot silently stop watching.
pub(crate) fn active_agents(listed: &Value) -> Result<Vec<Value>, String> {
    let agents = listed["agents"].as_array().ok_or_else(|| crate::i18n::tr("Unreadable Agents list").to_owned())?;
    Ok(agents.iter().filter(|a| a["state"] == "active").cloned().collect())
}
fn report(epoch: u64, agent_id: &str, line: Option<String>) {
    record(epoch, agent_id, line);
    Cx::post_action(IssueWatchChecked);
}

/// One Agent's check; `Ok(None)` while its watch is Off.
async fn check_agent(epoch: u64, service: &Service, client: &Client, agent: &Value) -> Result<Option<Outcome>, String> {
    let id = agent["id"].as_str().unwrap_or_default();
    let saved = live::saved(service, id, None).await?;
    let watch: IssueWatch = serde_json::from_value(saved["issueWatch"].clone()).map_err(|e| format!("Unreadable issue watch: {e}"))?;
    if watch.setting.mode == WatchMode::Off {
        return Ok(None);
    }
    let token = github_token(id).await?;
    let issues = fetch_open_issues(client, GITHUB_API, &watch.setting.repo, &token).await.map_err(|e| e.to_string())?;
    let seen: HashSet<i64> = saved["issueSeen"].as_array().into_iter().flatten().filter_map(Value::as_i64).collect();
    let picks = select(&watch, &issues, &seen);
    let auto_merge = !picks.is_empty() && live::auto_merge(id).await;
    let live = live::Live::new(service, agent, &watch, auto_merge, epoch)?;
    Ok(Some(handoff::hand_off(&live, &picks).await))
}

/// The GitHub token the watcher reads `agent_id`'s repository with: the
/// Agent's own (its GitHub access settings), else `TOKEN_VARIABLES`.
pub(crate) async fn github_token(agent_id: &str) -> Result<String, String> {
    let agent = agent_id.to_owned();
    let saved = tokio::task::spawn_blocking(move || TokenStore::current().load(&agent))
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    choose_token(saved, |name| std::env::var(name).ok())
}
/// First match wins: the Agent's saved token, `GH_TOKEN`, `GITHUB_TOKEN`. A
/// store that cannot be read is reported unless the environment has a token.
pub(crate) fn choose_token(saved: Result<Option<Token>, String>, env: impl Fn(&str) -> Option<String>) -> Result<String, String> {
    if let Ok(Some(token)) = &saved {
        return Ok(token.expose().to_owned());
    }
    if let Some(token) = TOKEN_VARIABLES.iter().find_map(|name| env(name).filter(|v| !v.trim().is_empty())) {
        return Ok(token);
    }
    saved?;
    Err(crate::i18n::tr(NO_TOKEN).to_owned())
}
/// Shown when neither the Agent nor the environment has a token.
pub const NO_TOKEN: &str = "No GitHub token — add one in the Agent's GitHub access settings";

fn now() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}
/// "Last check 14:05: 2 issues handed off", with the failure after it, if any.
pub(crate) fn outcome_line(time: &str, outcome: &Outcome) -> String {
    let template = match outcome.handed {
        0 => "Last check {time}: no new issues",
        1 => "Last check {time}: 1 issue handed off",
        _ => "Last check {time}: {count} issues handed off",
    };
    let line = crate::i18n::format(template, &[("time", time.to_owned()), ("count", outcome.handed.to_string())]);
    match &outcome.error {
        Some(error) => format!("{line} · {error}"),
        None => line,
    }
}
pub(crate) fn failed_line(time: &str, error: &str) -> String {
    crate::i18n::format("Last check {time} failed: {error}", &[("time", time.to_owned()), ("error", error.to_owned())])
}
