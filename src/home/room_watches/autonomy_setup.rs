//! "Let agents work on a repo" (ADR 0030): one action in Room info → Status
//! that links the repository to the room, turns "May work in this room" on
//! for this account's own Agents that are in the room (and run on this
//! device), sets the room to Assist when it is Off (owner or maintainer),
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
    pub rules: Rules,
    /// Agents of this account in the room whose "may work" could not be saved here.
    pub not_here: usize,
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

/// The one confirmation line.
pub fn done_line(done: &Done) -> String {
    use crate::i18n::{format, tr};
    let mut parts = vec![format("Linked {repo}", &[("repo", done.repo.clone())])];
    parts.push(match done.agents {
        0 => tr("none of your agents is in this room yet").to_owned(),
        1 => tr("1 of your agents may work here").to_owned(),
        n => format("{count} of your agents may work here", &[("count", n.to_string())]),
    });
    if done.not_here > 0 {
        parts.push(format("{count} run on another device: tick \"May work\" there", &[("count", done.not_here.to_string())]));
    }
    parts.push(match done.level {
        Some(level) => format("agent work is {level}", &[("level", tr(crate::hagency::room_autonomy::lines::level_name(level)).to_owned())]),
        None => tr("the room's owner or a maintainer still needs to choose Assist").to_owned(),
    });
    match done.rules {
        Rules::Suggested => parts.push(tr("suggested merge rules set").to_owned()),
        Rules::LeftForOwner => parts.push(tr("the room's owner can set merge rules below").to_owned()),
        Rules::Kept => {}
    }
    format!("{}.", parts.join(" · "))
}

/// "May work" on for this account's active Agents that are joined in `room`;
/// `(saved, saved elsewhere)`: an Agent another device runs cannot be saved here.
async fn let_agents_work(room: &Room) -> Result<(usize, usize), String> {
    let service = Service::for_current()?;
    let listed = service.execute(Command::Agents).await?;
    let (mut saved, mut elsewhere) = (0, 0);
    for agent in crate::hagency::issue_watch::active_agents(&listed)? {
        let Some(puppet) = agent["puppetMxid"].as_str().and_then(|m| matrix_sdk::ruma::OwnedUserId::try_from(m).ok()) else { continue };
        let joined = room.get_member_no_sync(&puppet).await.ok().flatten().is_some_and(|m| *m.membership() == matrix_sdk::ruma::events::room::member::MembershipState::Join);
        if !joined {
            continue;
        }
        let id = agent["id"].as_str().unwrap_or_default();
        let change = serde_json::json!({"mayWork": {"roomId": room.room_id().as_str(), "on": true}});
        match crate::hagency::issue_watch::live::saved(&service, id, Some(change)).await {
            Ok(_) => saved += 1,
            // The owner API saves only on the device that runs the Agent: counted and
            // named in the confirmation line ("run on another device"), not lost.
            Err(_) => elsewhere += 1,
        }
    }
    Ok((saved, elsewhere))
}

/// The room's level after the action: Assist when it was Off and this account may set it.
async fn level_after(room: &Room) -> Result<Option<Level>, String> {
    let state = crate::hagency::room_autonomy::server::read(room).await?;
    match state.level {
        Ok(Level::Off) if state.can_change => {
            crate::hagency::room_autonomy::server::set_level(room, Level::Assist).await?;
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

/// The whole action for `repo` in `room`; stops at the first part that fails.
pub async fn run(room: &Room, repo: &str) -> Result<Done, String> {
    let client = room.client();
    // Linking also adds the room's missing Hagency power-level entries this account may add.
    crate::home::room_watches::status::set_link(&client, room.room_id(), repo, true).await?;
    let repo = repo.trim().trim_start_matches("https://github.com/").trim_end_matches('/').trim_end_matches(".git").to_owned();
    let (agents, not_here) = let_agents_work(room).await?;
    let level = level_after(room).await?;
    let rules = rules_after(room).await?;
    Ok(Done { repo, agents, level, rules, not_here })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_confirmation_line_says_every_part() {
        let done = Done { repo: "acme/app".into(), agents: 2, level: Some(Level::Assist), rules: Rules::Suggested, not_here: 0 };
        assert_eq!(done_line(&done), "Linked acme/app · 2 of your agents may work here · agent work is Assist · suggested merge rules set.");
        let partial = Done { repo: "acme/app".into(), agents: 0, level: None, rules: Rules::LeftForOwner, not_here: 1 };
        let line = done_line(&partial);
        assert!(line.contains("none of your agents is in this room yet") && line.contains("1 run on another device") && line.contains("still needs to choose Assist") && line.contains("owner can set merge rules"), "{line}");
    }
}
