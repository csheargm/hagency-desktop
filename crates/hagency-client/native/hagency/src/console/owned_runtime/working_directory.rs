//! Room-specific working directories without changing execution permissions.
use super::{Profile, RuntimeError, Scope};
use hagency_agent_local::room_files::{self, Workspace};
use hagency_runtime::owned::SharedProject;

/// `space` is the shared project folder of the Room's space, when it has one:
/// without the owner's shared project or host file tools, the turn works
/// there instead of the Room's own folder, with the same native tools.
///
/// `owner_directory` is the owner's private folder, which holds the state of
/// the host file tools; only those need it.
#[allow(clippy::too_many_arguments)]
pub(super) fn for_turn(
    configured: &Profile,
    owner_directory: Option<&std::path::Path>,
    owner: &str,
    scope: &Scope,
    host_files: bool,
    shared_workspace: bool,
    shared_workspace_pin: Option<&str>,
    space: Option<&SharedProject>,
) -> Result<(Profile, Option<Workspace>), RuntimeError> {
    let mut profile = configured.clone();
    let workspace = if shared_workspace {
        let expected = shared_workspace_pin.ok_or(RuntimeError::Profile)?;
        let directory = owner_directory.ok_or(RuntimeError::Profile)?;
        let workspace = Workspace::open_shared(directory, &profile.cwd, owner, scope).map_err(|_| RuntimeError::Profile)?;
        if workspace.shared_pin().map_err(|_| RuntimeError::Profile)? != expected { return Err(RuntimeError::Profile); }
        profile.cwd = workspace.canonical_directory().to_path_buf();
        Some(workspace)
    } else if host_files {
        // Preserve the existing explicitly granted file-capability namespace.
        let directory = owner_directory.ok_or(RuntimeError::Profile)?;
        let workspace =
            Workspace::open(directory, owner, scope).map_err(|_| RuntimeError::Profile)?;
        profile.cwd = workspace.canonical_directory().to_path_buf();
        Some(workspace)
    } else if let Some(space) = space.filter(|space| space.check()) {
        // Checked again here: the folder may have been replaced since it was
        // chosen. Every Agent of the space on this computer works in it.
        profile.cwd = space.directory().to_path_buf();
        None
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
            environment: Default::default(),
        };
        let scope = Scope {
            agent: "agent".into(),
            binding: "dm".into(),
            room: "!dm:test".into(),
            requester: "@owner:test".into(),
            thread: "!dm:test".into(),
        };
        let (dm, files) = for_turn(&profile, None, "@owner:test", &scope, false, false, None, None).unwrap();
        assert!(files.is_none());
        assert!(dm.cwd.starts_with(&base));
        assert_ne!(dm.cwd, base);
        let mut group = scope.clone();
        group.binding = "group".into();
        group.room = "!group:test".into();
        let (group, files) = for_turn(&profile, None, "@owner:test", &group, false, false, None, None).unwrap();
        assert!(files.is_none());
        assert_ne!(dm.cwd, group.cwd);
        assert_eq!(
            dm.cwd,
            for_turn(&profile, None, "@owner:test", &scope, false, false, None, None)
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
        // A local Claude or Octos sign-in: the provider home is the person's
        // own home folder, outside the owner's folder.
        let profile = Profile {
            executable: owner.join("provider"), home: temp.path().join("person"),
            codex_home: temp.path().join("person/.octos"), cwd: project.clone(), model: "fixture".into(),
            effort: "low".into(), shared_auth: true, web_search: false,
            environment: Default::default(),
        };
        let scope = Scope { agent:"agent".into(), binding:"room".into(), room:"!room:test".into(), requester:"@owner:test".into(), thread:"$thread".into() };
        assert!(for_turn(&profile, Some(owner.as_path()), "@owner:test", &scope, false, true, None, None).is_err());
        let pin = super::super::shared_workspace_pin(&project).unwrap();
        // Without the owner's folder there is nowhere to keep the workspace state.
        assert!(for_turn(&profile, None, "@owner:test", &scope, false, true, Some(&pin), None).is_err());
        let (selected, files) = for_turn(&profile, Some(owner.as_path()), "@owner:test", &scope, false, true, Some(&pin), None).unwrap();
        assert_eq!(selected.cwd, project);
        assert!(files.unwrap().is_shared());
        // Room files do not land in the project itself: they keep the folder
        // this Room has without the opt-in.
        let (plain, _) = for_turn(&profile, None, "@owner:test", &scope, false, false, None, None).unwrap();
        let room_files = for_room_files(&profile, &selected, "@owner:test", &scope, true).unwrap();
        assert_eq!(room_files, plain.cwd);
        assert_ne!(room_files, project);
        assert!(room_files.starts_with(project.join(".hagency-room-workspaces")));
        assert_eq!(for_room_files(&profile, &plain, "@owner:test", &scope, false).unwrap(), plain.cwd);
    }
    #[test]
    fn a_space_folder_is_every_rooms_folder_unless_the_owner_chose_one() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let owner = base.join(format!("owner_{}", "a".repeat(64)));
        std::fs::create_dir(&owner).unwrap();
        std::fs::set_permissions(&owner, std::fs::Permissions::from_mode(0o700)).unwrap();
        let work = base.join("work");
        let project = base.join("project");
        let projects = base.join("Hagency Projects");
        let folder = projects.join("Team app");
        for directory in [&work, &project, &folder] {
            std::fs::create_dir_all(directory).unwrap();
        }
        let space = SharedProject::new(projects, folder.clone()).unwrap();
        let profile = Profile {
            executable: owner.join("provider"), home: owner.join("provider-home"),
            codex_home: owner.join("codex-home"), cwd: work.clone(), model: "fixture".into(),
            effort: "low".into(), shared_auth: false, web_search: false,
        };
        let scope = Scope { agent:"agent".into(), binding:"room".into(), room:"!room:test".into(), requester:"@owner:test".into(), thread:"$thread".into() };
        let (turn, files) = for_turn(&profile, None, "@owner:test", &scope, false, false, None, Some(&space)).unwrap();
        assert_eq!(turn.cwd, folder);
        assert!(files.is_none());
        // Room files go where the Agent works.
        assert_eq!(for_room_files(&profile, &turn, "@owner:test", &scope, false).unwrap(), folder);
        // Another Agent in another Room of the space works in the same folder.
        let other = Scope { agent:"agent-b".into(), binding:"room-b".into(), room:"!room-b:test".into(), requester:"@guest:test".into(), thread:"$other".into() };
        assert_eq!(for_turn(&profile, None, "@owner:test", &other, false, false, None, Some(&space)).unwrap().0.cwd, folder);
        // Without a space folder the Room keeps its own folder.
        let (plain, _) = for_turn(&profile, None, "@owner:test", &scope, false, false, None, None).unwrap();
        assert!(plain.cwd.starts_with(work.join(".hagency-room-workspaces")));
        // Host file tools keep their own workspace.
        let (hosted, files) = for_turn(&profile, Some(owner.as_path()), "@owner:test", &scope, true, false, None, Some(&space)).unwrap();
        assert!(hosted.cwd.starts_with(owner.join("room-workspaces")) && files.is_some());
        // That workspace holds the Room's lock until it is let go.
        drop(files);
        // The owner's shared project stays the owner's choice.
        let chosen = Profile { cwd: project.clone(), ..profile.clone() };
        let pin = super::super::shared_workspace_pin(&project).unwrap();
        let (selected, files) = for_turn(&chosen, Some(owner.as_path()), "@owner:test", &scope, false, true, Some(&pin), Some(&space)).unwrap();
        assert_eq!(selected.cwd, project);
        assert!(files.unwrap().is_shared());
        // A space folder replaced after it was chosen is not used.
        std::fs::remove_dir(&folder).unwrap();
        let (fallback, _) = for_turn(&profile, None, "@owner:test", &scope, false, false, None, Some(&space)).unwrap();
        assert_eq!(fallback.cwd, plain.cwd);
    }
}
