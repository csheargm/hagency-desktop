//! Room-specific working directories without changing execution permissions.
use super::{Profile, RuntimeError, Scope};
use hagency_agent_local::room_files::{self, Workspace};

pub(super) fn for_turn(
    configured: &Profile,
    owner: &str,
    scope: &Scope,
    host_files: bool,
    shared_workspace: bool,
    shared_workspace_pin: Option<&str>,
) -> Result<(Profile, Option<Workspace>), RuntimeError> {
    let mut profile = configured.clone();
    let workspace = if shared_workspace {
        let expected = shared_workspace_pin.ok_or(RuntimeError::Profile)?;
        let directory = profile.home.parent().ok_or(RuntimeError::Profile)?;
        let workspace = Workspace::open_shared(directory, &profile.cwd, owner, scope).map_err(|_| RuntimeError::Profile)?;
        if workspace.shared_pin().map_err(|_| RuntimeError::Profile)? != expected { return Err(RuntimeError::Profile); }
        profile.cwd = workspace.canonical_directory().to_path_buf();
        Some(workspace)
    } else if host_files {
        // Preserve the existing explicitly granted file-capability namespace.
        let directory = profile.home.parent().ok_or(RuntimeError::Profile)?;
        let workspace =
            Workspace::open(directory, owner, scope).map_err(|_| RuntimeError::Profile)?;
        profile.cwd = workspace.canonical_directory().to_path_buf();
        Some(workspace)
    } else {
        // A private child of the owner's selected base; no file tools granted.
        profile.cwd = room_files::chat_directory(&profile.cwd, owner, scope)
            .map_err(|_| RuntimeError::Profile)?;
        None
    };
    Ok((profile, workspace))
}

/// The folder a Room's own file tools receive into and send from.
///
/// It is the turn's working directory, except with the shared-workspace
/// opt-in: there the working directory is the owner's project, which only the
/// host file tools change, so Room files stay in the folder this Room has
/// without the opt-in.
pub(super) fn for_room_files(
    configured: &Profile,
    turn: &Profile,
    owner: &str,
    scope: &Scope,
    shared_workspace: bool,
) -> Result<std::path::PathBuf, RuntimeError> {
    if !shared_workspace {
        return Ok(turn.cwd.clone());
    }
    room_files::chat_directory(&configured.cwd, owner, scope).map_err(|_| RuntimeError::Profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chat_only_rooms_keep_selected_base_and_never_enable_file_tools() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        std::fs::write(base.join("existing-project.txt"), "preserved").unwrap();
        let profile = Profile {
            executable: base.join("provider"),
            home: base.join("provider-home"),
            codex_home: base.join("codex-home"),
            cwd: base.clone(),
            model: "fixture".into(),
            effort: "low".into(),
            shared_auth: false,
            web_search: false,
        };
        let scope = Scope {
            agent: "agent".into(),
            binding: "dm".into(),
            room: "!dm:test".into(),
            requester: "@owner:test".into(),
            thread: "!dm:test".into(),
        };
        let (dm, files) = for_turn(&profile, "@owner:test", &scope, false, false, None).unwrap();
        assert!(files.is_none());
        assert!(dm.cwd.starts_with(&base));
        assert_ne!(dm.cwd, base);
        let mut group = scope.clone();
        group.binding = "group".into();
        group.room = "!group:test".into();
        let (group, files) = for_turn(&profile, "@owner:test", &group, false, false, None).unwrap();
        assert!(files.is_none());
        assert_ne!(dm.cwd, group.cwd);
        assert_eq!(
            dm.cwd,
            for_turn(&profile, "@owner:test", &scope, false, false, None)
                .unwrap()
                .0
                .cwd
        );
        assert_eq!(profile.cwd, base);
        assert_eq!(
            std::fs::read_to_string(base.join("existing-project.txt")).unwrap(),
            "preserved"
        );
        assert!(!dm.web_search);
    }
    #[test]
    fn shared_room_requires_exact_owner_authorized_directory() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let owner = temp.path().join(format!("owner_{}", "a".repeat(64)));
        std::fs::create_dir(&owner).unwrap();
        std::fs::set_permissions(&owner, std::fs::Permissions::from_mode(0o700)).unwrap();
        let owner = owner.canonicalize().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let project = project.canonicalize().unwrap();
        let profile = Profile {
            executable: owner.join("provider"), home: owner.join("provider-home"),
            codex_home: owner.join("codex-home"), cwd: project.clone(), model: "fixture".into(),
            effort: "low".into(), shared_auth: false, web_search: false,
        };
        let scope = Scope { agent:"agent".into(), binding:"room".into(), room:"!room:test".into(), requester:"@owner:test".into(), thread:"$thread".into() };
        assert!(for_turn(&profile, "@owner:test", &scope, false, true, None).is_err());
        let pin = super::super::shared_workspace_pin(&project).unwrap();
        let (selected, files) = for_turn(&profile, "@owner:test", &scope, false, true, Some(&pin)).unwrap();
        assert_eq!(selected.cwd, project);
        assert!(files.unwrap().is_shared());
        // Room files do not land in the project itself: they keep the folder
        // this Room has without the opt-in.
        let (plain, _) = for_turn(&profile, "@owner:test", &scope, false, false, None).unwrap();
        let room_files = for_room_files(&profile, &selected, "@owner:test", &scope, true).unwrap();
        assert_eq!(room_files, plain.cwd);
        assert_ne!(room_files, project);
        assert!(room_files.starts_with(project.join(".hagency-room-workspaces")));
        assert_eq!(for_room_files(&profile, &plain, "@owner:test", &scope, false).unwrap(), plain.cwd);
    }

}
