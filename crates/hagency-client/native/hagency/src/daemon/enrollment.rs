//! Owner enrollment journal (ADR-192).
//!
//! The journal names exactly one owner (server origin and Matrix ID) and the
//! owner's registered root, which the daemon hosts. It carries no credentials.
//! There is no handover: enrollment records the root the owner-root registry
//! resolved, and whether the daemon proves an existing executor in place or
//! registers a new one depends only on that root's contents.
//!
//! 1. `prepare` records the owner as `preparing` before browser sign-in, so a
//!    logout during sign-in has a scope. The daemon never hosts it.
//! 2. `enroll` records `enrolled` after the daemon's own SDK sign-in. The
//!    daemon hosts the root from then on, across restarts.
//! 3. `withdraw` removes the journal at sign-out. The root and its
//!    registration remain.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const LIMIT: usize = 16 * 1024;
const VERSION: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentState {
    /// Browser sign-in has not completed: scope for logout, never hosted.
    Preparing,
    /// Signed in with the daemon's own device; the daemon hosts the root.
    Enrolled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Enrollment {
    pub version: u8,
    pub origin: String,
    pub owner: String,
    pub display_name: Option<String>,
    pub owner_root: PathBuf,
    pub state: EnrollmentState,
}

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum EnrollmentError {
    #[error("the enrollment journal is unavailable")]
    Storage,
    #[error("a different owner is already enrolled")]
    OtherOwner,
    #[error("the owner root must be an absolute path outside the daemon home")]
    Root,
    #[error("no owner is enrolled")]
    Missing,
}

impl EnrollmentError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Storage => "daemon_enrollment_storage_unavailable",
            Self::OtherOwner => "daemon_other_owner_enrolled",
            Self::Root => "daemon_owner_root_invalid",
            Self::Missing => "daemon_enrollment_required",
        }
    }
}

fn path(home: &Path) -> PathBuf {
    home.join(super::ENROLLMENT)
}

impl Enrollment {
    pub fn load(home: &Path) -> Result<Option<Self>, EnrollmentError> {
        let Some(raw) =
            super::read_private(&path(home), LIMIT).map_err(|_| EnrollmentError::Storage)?
        else {
            return Ok(None);
        };
        let stored: Self = serde_json::from_slice(&raw).map_err(|_| EnrollmentError::Storage)?;
        if stored.version != VERSION
            || Self::check_root(home, &stored.owner_root).is_err()
            || !super::protocol::valid_owner_id(&stored.owner)
        {
            return Err(EnrollmentError::Storage);
        }
        Ok(Some(stored))
    }

    fn save(&self, home: &Path) -> Result<(), EnrollmentError> {
        let raw = serde_json::to_vec(self).map_err(|_| EnrollmentError::Storage)?;
        hagency_store::private::replace(&path(home), &raw).map_err(|_| EnrollmentError::Storage)
    }

    /// Whether the daemon may open the owner root.
    pub fn hostable(&self) -> bool {
        self.state == EnrollmentState::Enrolled
    }

    fn same_owner(&self, origin: &str, owner: &str) -> bool {
        self.owner == owner && super::same_origin(&self.origin, origin)
    }

    pub(super) fn check_root(home: &Path, root: &Path) -> Result<(), EnrollmentError> {
        if !root.is_absolute()
            || root
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(EnrollmentError::Root);
        }
        let root = super::assignment::identity(root).map_err(|_| EnrollmentError::Root)?;
        let home = super::assignment::identity(home).map_err(|_| EnrollmentError::Root)?;
        // The owner root is a sibling custody domain. Only `owners/` under the
        // daemon home, where builds before ADR-192 created roots, is allowed;
        // never the home itself or its files.
        if root.starts_with(&home) && !root.starts_with(home.join("owners")) {
            return Err(EnrollmentError::Root);
        }
        Ok(())
    }

    fn validate(home: &Path, owner: &str, root: &Path) -> Result<(), EnrollmentError> {
        if !super::protocol::valid_owner_id(owner) {
            return Err(EnrollmentError::OtherOwner);
        }
        Self::check_root(home, root)
    }

    /// Record the owner before browser sign-in. A completed enrollment of the
    /// same owner is kept as it is, so a cancelled sign-in never stops hosting
    /// or names another root; an unfinished one is replaced.
    pub fn prepare(
        home: &Path,
        origin: &str,
        owner: &str,
        display_name: Option<String>,
        root: PathBuf,
    ) -> Result<Self, EnrollmentError> {
        Self::validate(home, owner, &root)?;
        match Self::load(home)? {
            Some(existing) if !existing.same_owner(origin, owner) => {
                Err(EnrollmentError::OtherOwner)
            }
            Some(existing) if existing.hostable() || existing.owner_root == root => Ok(existing),
            _ => {
                let value = Self {
                    version: VERSION,
                    origin: origin.into(),
                    owner: owner.into(),
                    display_name,
                    owner_root: root,
                    state: EnrollmentState::Preparing,
                };
                value.save(home)?;
                Ok(value)
            }
        }
    }

    /// Record a completed enrollment of the owner's registered root, after
    /// the daemon's own SDK sign-in. Replaces a current enrollment of the same
    /// owner when its registered root changes; refuses another owner.
    pub fn enroll(
        home: &Path,
        origin: &str,
        owner: &str,
        display_name: Option<String>,
        root: &Path,
    ) -> Result<Self, EnrollmentError> {
        Self::validate(home, owner, root)?;
        let existing = Self::load(home)?;
        if existing
            .as_ref()
            .is_some_and(|e| !e.same_owner(origin, owner))
        {
            return Err(EnrollmentError::OtherOwner);
        }
        let value = Self {
            version: VERSION,
            origin: origin.into(),
            owner: owner.into(),
            display_name: display_name
                .or_else(|| existing.as_ref().and_then(|e| e.display_name.clone())),
            owner_root: root.into(),
            state: EnrollmentState::Enrolled,
        };
        if existing.as_ref() != Some(&value) {
            value.save(home)?;
        }
        Ok(value)
    }

    /// Remove the enrollment after explicit owner sign-out. The owner root,
    /// its registration, receipts and registry remain on disk.
    pub fn withdraw(home: &Path) -> Result<(), EnrollmentError> {
        match std::fs::remove_file(path(home)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(EnrollmentError::Storage),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        temp
    }

    fn root(temp: &tempfile::TempDir, name: &str) -> PathBuf {
        temp.path()
            .parent()
            .unwrap()
            .join(format!("{name}-{}", super::super::new_request_id()))
    }

    #[test]
    fn preparation_never_hosts_and_enrollment_hosts_the_given_root() {
        let temp = home();
        let registered = root(&temp, "registered-root");
        let prepared =
            Enrollment::prepare(temp.path(), "https://s/", "@o:s", None, registered.clone())
                .unwrap();
        assert_eq!(prepared.state, EnrollmentState::Preparing);
        assert!(!prepared.hostable());
        assert_eq!(Enrollment::load(temp.path()).unwrap().unwrap(), prepared);
        let enrolled = Enrollment::enroll(
            temp.path(),
            "https://s/",
            "@o:s",
            Some("O".into()),
            &registered,
        )
        .unwrap();
        assert!(enrolled.hostable());
        assert_eq!(enrolled.owner_root, registered);
        assert_eq!(Enrollment::load(temp.path()).unwrap().unwrap(), enrolled);
        // Repeating either step for the same owner keeps the hosted root.
        assert_eq!(
            Enrollment::prepare(temp.path(), "https://s", "@o:s", None, registered.clone())
                .unwrap(),
            enrolled
        );
        assert_eq!(
            Enrollment::enroll(temp.path(), "https://s", "@o:s", None, &registered)
                .unwrap()
                .display_name
                .as_deref(),
            Some("O")
        );
        Enrollment::withdraw(temp.path()).unwrap();
        assert!(Enrollment::load(temp.path()).unwrap().is_none());
    }

    #[test]
    fn a_cancelled_sign_in_keeps_hosting_and_another_owner_is_refused() {
        let temp = home();
        let legacy = root(&temp, "legacy-root");
        let registered = root(&temp, "registered-root");
        let hosted = Enrollment::enroll(temp.path(), "https://s/", "@o:s", None, &legacy).unwrap();
        // Preparing a new sign-in of the same owner leaves hosting untouched.
        assert_eq!(
            Enrollment::prepare(temp.path(), "https://s/", "@o:s", None, registered.clone())
                .unwrap(),
            hosted
        );
        // Completing it records the registered root instead of the old one.
        assert_eq!(
            Enrollment::enroll(temp.path(), "https://s/", "@o:s", None, &registered)
                .unwrap()
                .owner_root,
            registered
        );
        for (origin, owner) in [("https://s/", "@p:s"), ("https://t/", "@o:s")] {
            assert_eq!(
                Enrollment::prepare(temp.path(), origin, owner, None, registered.clone())
                    .unwrap_err(),
                EnrollmentError::OtherOwner
            );
            assert_eq!(
                Enrollment::enroll(temp.path(), origin, owner, None, &registered).unwrap_err(),
                EnrollmentError::OtherOwner
            );
        }
        assert_eq!(
            Enrollment::load(temp.path()).unwrap().unwrap().owner_root,
            registered
        );
    }

    #[test]
    fn old_and_unknown_journals_are_refused_without_rewriting_identity() {
        let temp = home();
        let registered = root(&temp, "registered-root");
        let path = temp.path().join(super::super::ENROLLMENT);
        let mut invalid = Vec::new();
        for handover in [
            serde_json::json!({"state":"preparing"}),
            serde_json::json!({"state":"fresh"}),
            serde_json::json!({"state":"staged","source":"desktop","stagedAtMs":1}),
            serde_json::json!({"state":"active","source":null,"sinceMs":1}),
            serde_json::json!({"state":"released","atMs":1}),
        ] {
            invalid.push(serde_json::json!({"version":1,"origin":"https://s/","owner":"@o:s","displayName":null,"ownerRoot":registered,"handover":handover}));
        }
        invalid.extend([
            serde_json::json!({"version":1,"origin":"https://s/","owner":"@o:s","ownerRoot":registered,"state":"enrolled"}),
            serde_json::json!({"version":2,"origin":"https://s/","owner":"@o:s","ownerRoot":registered,"state":"enrolled","handover":{"state":"released"}}),
            serde_json::json!({"version":9,"origin":"https://s/","owner":"@o:s","ownerRoot":registered,"state":"enrolled"}),
            serde_json::json!({"version":2,"origin":"https://s/","owner":"@o:s","ownerRoot":registered,"state":"staged"}),
            serde_json::json!({"version":2,"origin":"https://s/","owner":"@o:s","ownerRoot":registered}),
            serde_json::json!({"version":2,"origin":"https://s/","owner":"@o:s","ownerRoot":registered,"state":"enrolled","futurePolicy":"paused"}),
        ]);
        for journal in invalid {
            let raw = serde_json::to_vec(&journal).unwrap();
            hagency_store::private::replace(&path, &raw).unwrap();
            assert_eq!(Enrollment::load(temp.path()).unwrap_err(), EnrollmentError::Storage);
            assert_eq!(
                Enrollment::prepare(temp.path(), "https://s/", "@o:s", None, registered.clone()).unwrap_err(),
                EnrollmentError::Storage
            );
            assert_eq!(
                Enrollment::enroll(temp.path(), "https://s/", "@o:s", None, &registered).unwrap_err(),
                EnrollmentError::Storage
            );
            assert_eq!(std::fs::read(&path).unwrap(), raw, "rejected enrollment was rewritten");
            assert!(!registered.exists(), "rejected enrollment opened the owner root");
        }
    }

    #[cfg(unix)]
    #[test]
    fn root_validation_resolves_aliases_before_custody_boundary_check() {
        let temp = home();
        let alias = temp
            .path()
            .parent()
            .unwrap()
            .join(format!("alias-{}", super::super::new_request_id()));
        std::os::unix::fs::symlink(temp.path(), &alias).unwrap();
        assert_eq!(
            Enrollment::check_root(temp.path(), &alias.join("matrix")),
            Err(EnrollmentError::Root)
        );
        assert_eq!(
            Enrollment::check_root(temp.path(), &temp.path().join("owners/../matrix")),
            Err(EnrollmentError::Root)
        );
        std::fs::remove_file(alias).unwrap();
    }

    #[test]
    fn roots_inside_the_daemon_home_are_refused() {
        let temp = home();
        for bad in [
            temp.path().to_path_buf(),
            temp.path().join("matrix"),
            PathBuf::from("relative"),
        ] {
            assert_eq!(
                Enrollment::enroll(
                    temp.path(),
                    "https://s.example/",
                    "@o:s.example",
                    None,
                    &bad
                )
                .unwrap_err(),
                EnrollmentError::Root,
                "{bad:?}"
            );
        }
        assert!(Enrollment::load(temp.path()).unwrap().is_none());
    }
}
