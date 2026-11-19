//! "Let agents work on a repo" (ADR 0030): one action in Room info → Status
//! that links the repository to the room, turns "May work in this room" on
//! for this account's own Agents that are in the room (and run on this
//! device; one another device runs is ticked there), sets the room to Assist when it is Off (owner or maintainer),
//! and fills the suggested merge rules when the room has none (room owner).
//! Each part is done only when this account may do it; the one line it
//! ends with says what was done and what is left.
use crate::hagency::backend::Service;
use crate::hagency::issue_watch::room_rules::{self, RoomRead};
use crate::hagency::model::Command;
use ::hagency::issue_watch::safeguards::{room_autonomy::Level, suggested_policy};
use matrix_sdk::Room;

/// What the one action did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Done {
    pub repo: String,
    pub agents: usize,
    /// The level the room has now (`None`: Off and this account may not change it).
    pub level: Option<Level>,
    /// Some of this account's Agents' "may work" could not be saved or read (named in `failed`).
    pub agents_failed: bool,
    /// The level could not be read or set (named in `failed`).
    pub level_failed: bool,
    pub rules: Rules,
    /// Agents of this account in the room that run on another device, so are ticked there.
    pub not_here: usize,
    /// What could not be done, each with its error (a local Agent's save, a room line).
    pub failed: Vec<String>,
}

/// What happened to the merge rules.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Rules {
    #[default]
    Kept,
    Suggested,
    /// None, and only the room owner can set them.
    LeftForOwner,
}

/// The one confirmation line; every count is about the same Agents, so a
/// failure is named as one and never read as "none in this room".
pub fn done_line(done: &Done) -> String {
    use crate::i18n::{format, tr};
    let mut parts = vec![format("Linked {repo}", &[("repo", done.repo.clone())])];
    match done.agents {
        0 if done.not_here > 0 || done.agents_failed => {}
        0 => parts.push(tr("none of your agents is in this room yet").to_owned()),
        1 => parts.push(tr("1 of your agents may work here").to_owned()),
        n => parts.push(format("{count} of your agents may work here", &[("count", n.to_string())])),
    }
    if done.not_here > 0 {
        parts.push(format("{count} run on another device: tick \"May work\" there", &[("count", done.not_here.to_string())]));
    }
    parts.extend(done.failed.iter().cloned());
    match done.level {
        _ if done.level_failed => {}
        Some(level) => parts.push(format("agent work is {level}", &[("level", tr(crate::hagency::room_autonomy::lines::level_name(level)).to_owned())])),
        None => parts.push(tr("the room's owner or a maintainer still needs to choose Assist").to_owned()),
    }
    match done.rules {
        Rules::Suggested => parts.push(tr("suggested merge rules set").to_owned()),
        Rules::LeftForOwner => parts.push(tr("the room's owner can set merge rules below").to_owned()),
        Rules::Kept => {}
    }
    format!("{}.", parts.join(" · "))
}

/// What "may work" came to for this account's Agents in the room.
#[derive(Debug, Default)]
struct Agents {
    saved: Vec<String>,
    elsewhere: usize,
    failed: Vec<String>,
}

/// "May work" on for this account's active Agents that are joined in `room`.
/// Only an Agent this device runs is saved here (the owner API keeps it on the
/// Agent's device); one another device runs is counted, and a failed save of a
/// local one is a failure with its error, never "another device" (GPT review).
async fn let_agents_work(room: &Room) -> Result<Agents, String> {
    let service = Service::for_current()?;
    let listed = service.execute(Command::Agents).await?;
    let local = crate::hagency::kill_switch::here(&listed, &service.execute(Command::Devices).await?);
    let mut out = Agents::default();
    for agent in crate::hagency::issue_watch::active_agents(&listed)? {
        let Some(puppet) = agent["puppetMxid"].as_str().and_then(|m| matrix_sdk::ruma::OwnedUserId::try_from(m).ok()) else { continue };
        let joined = room.get_member_no_sync(&puppet).await.ok().flatten().is_some_and(|m| *m.membership() == matrix_sdk::ruma::events::room::member::MembershipState::Join);
        let id = agent["id"].as_str().unwrap_or_default();
        if !joined {
            continue;
        }
        if !local.iter().any(|l| l == id) {
            out.elsewhere += 1;
            continue;
        }
        let change = serde_json::json!({"mayWork": {"roomId": room.room_id().as_str(), "on": true}});
        match crate::hagency::issue_watch::live::saved(&service, id, Some(change)).await {
            Ok(_) => out.saved.push(id.to_owned()),
            Err(error) => {
                let name = agent["displayName"].as_str().filter(|n| !n.is_empty()).unwrap_or(id).to_owned();
                out.failed.push(crate::i18n::format("\"May work\" could not be saved for {name}: {error}", &[("name", name), ("error", error)]));
            }
        }
    }
    Ok(out)
}

/// The room's level after the action: Assist when it was Off and this account may set it.
async fn level_after(room: &Room, failed: &mut Vec<String>) -> Result<Option<Level>, String> {
    let state = crate::hagency::room_autonomy::server::read(room).await?;
    match state.level {
        Ok(Level::Off) if state.can_change => {
            // Saved even when its room line was refused: that is reported on its own.
            let announced = crate::hagency::room_autonomy::server::set_level(room, Level::Assist).await?;
            failed.extend(announced);
            Ok(Some(Level::Assist))
        }
        Ok(Level::Off) | Err(_) => Ok(None),
        Ok(level) => Ok(Some(level)),
    }
}

/// The suggested rules when the room has none and this account may set them.
async fn rules_after(room: &Room) -> Result<Rules, String> {
    match room_rules::read_with_editor(room).await? {
        (RoomRead::None, true) => room_rules::save(room, &suggested_policy()).await.map(|()| Rules::Suggested),
        (RoomRead::None, false) => Ok(Rules::LeftForOwner),
        _ => Ok(Rules::Kept),
    }
}

/// The whole action for `repo` in `room`. Only a failed link is an error
/// (nothing was done); after it, each part that fails is named in the line
/// and the rest still runs, so no failure hides another (GPT review).
pub async fn run(room: &Room, repo: &str) -> Result<Done, String> {
    let client = room.client();
    // Linking also adds the room's missing Hagency power-level entries this account may add.
    crate::home::room_watches::status::set_link(&client, room.room_id(), repo, true).await?;
    let repo = repo.trim().trim_start_matches("https://github.com/").trim_end_matches('/').trim_end_matches(".git").to_owned();
    let Agents { saved, elsewhere, mut failed } = let_agents_work(room).await.unwrap_or_else(|error| Agents { failed: vec![error], ..Agents::default() });
    if !saved.is_empty() {
        // The Agent settings' "May work" ticks read again (GPT review).
        makepad_widgets::Cx::post_action(crate::hagency::issue_watch::may_work_section::MayWorkChanged { agent_ids: saved.clone() });
    }
    let agents_failed = !failed.is_empty();
    let (level, level_failed) = match level_after(room, &mut failed).await {
        Ok(level) => (level, false),
        Err(error) => {
            failed.push(error);
            (None, true)
        }
    };
    let rules = rules_after(room).await.unwrap_or_else(|error| {
        failed.push(error);
        Rules::Kept
    });
    Ok(Done { repo, agents: saved.len(), agents_failed, level, level_failed, rules, not_here: elsewhere, failed })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_confirmation_line_says_every_part() {
        let done = Done { repo: "acme/app".into(), agents: 2, level: Some(Level::Assist), rules: Rules::Suggested, not_here: 0, ..Done::default() };
        assert_eq!(done_line(&done), "Linked acme/app · 2 of your agents may work here · agent work is Assist · suggested merge rules set.");
        let partial = Done { repo: "acme/app".into(), agents: 0, level: None, rules: Rules::LeftForOwner, not_here: 1, ..Done::default() };
        let line = done_line(&partial);
        assert!(line.contains("1 run on another device") && line.contains("still needs to choose Assist") && line.contains("owner can set merge rules"), "{line}");
        assert!(!line.contains("none of your agents is in this room"), "an Agent on another device is in the room: {line}");
    }

    /// GPT review (P2): a local Agent's failed save is a failure with its error,
    /// not "runs on another device", and the line does not also say "none in this room".
    #[test]
    fn a_failed_local_save_is_named_as_a_failure() {
        let failed = vec!["\"May work\" could not be saved for bird: storage_unavailable".to_owned()];
        let done = Done { repo: "acme/app".into(), agents: 0, level: Some(Level::Assist), rules: Rules::Kept, failed, agents_failed: true, ..Done::default() };
        assert_eq!(done_line(&done), "Linked acme/app · \"May work\" could not be saved for bird: storage_unavailable · agent work is Assist.");
        let none = Done { repo: "acme/app".into(), level: Some(Level::Assist), ..Done::default() };
        assert_eq!(done_line(&none), "Linked acme/app · none of your agents is in this room yet · agent work is Assist.");
        let both = vec!["\"May work\" could not be saved for bird: storage_unavailable".to_owned(), "Could not save the merge rules: M_FORBIDDEN".to_owned()];
        let later = Done { repo: "acme/app".into(), failed: both, agents_failed: true, level_failed: true, ..Done::default() };
        let rules_only = Done { repo: "acme/app".into(), failed: vec!["Could not save the merge rules: M_FORBIDDEN".to_owned()], level: Some(Level::Assist), ..Done::default() };
        assert!(done_line(&rules_only).contains("none of your agents is in this room yet"), "a rules failure says nothing about the Agents");
        assert_eq!(done_line(&later), "Linked acme/app · \"May work\" could not be saved for bird: storage_unavailable · Could not save the merge rules: M_FORBIDDEN.", "a later failure keeps the earlier one and says no level it does not know");
    }
}
