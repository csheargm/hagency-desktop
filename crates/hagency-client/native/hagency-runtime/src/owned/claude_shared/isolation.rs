//! Whole-process filesystem confinement; CLI permission prompts are additional
//! owner policy, never the boundary protecting another room's plaintext.
use super::StartError;
use hagency_platform::Launch;
use std::path::Path;

pub(super) fn confine(launch: &mut Launch, scope: &Path) -> Result<(), StartError> {
    if !scope.is_absolute()
        || scope.canonicalize().ok().as_deref() != Some(scope)
        || !launch.directory.starts_with(scope)
    {
        return Err(StartError::Settings);
    }
    for name in ["HOME", "CLAUDE_CONFIG_DIR", "TMPDIR", "CLAUDE_CODE_TMPDIR"] {
        let value = launch
            .environment
            .get(std::ffi::OsStr::new(name))
            .ok_or(StartError::Settings)?;
        let path = Path::new(value);
        if !path.starts_with(scope) || path.canonicalize().ok().as_deref() != Some(path) {
            return Err(StartError::Settings);
        }
    }
    #[cfg(target_os = "macos")]
    {
        let executable = launch
            .executable
            .canonicalize()
            .map_err(|_| StartError::Settings)?;
        let policy = seatbelt(scope, &executable)?;
        let mut arguments = vec!["-p".into(), policy.into(), executable.into_os_string()];
        arguments.append(&mut launch.arguments);
        launch.executable = "/usr/bin/sandbox-exec".into();
        launch.arguments = arguments;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(StartError::Unsupported)
    }
}

#[cfg(target_os = "macos")]
fn seatbelt(scope: &Path, executable: &Path) -> Result<String, StartError> {
    let quote = |path: &Path| {
        serde_json::to_string(path.to_str().ok_or(StartError::Settings)?)
            .map_err(|_| StartError::Settings)
    };
    let scope = quote(scope)?;
    let executable = quote(executable)?;
    // Native CLI startup reads the OS directory database and timezone. None
    // of these rules admits user homes, other room scopes, owner policy,
    // Keychain services, process environments or shared temporary files.
    Ok(format!(
        r#"(version 1)
(allow default)
(deny file-read*)
(allow file-read-metadata)
(allow file-read* (literal "/") (subpath "/System/Library")
 (subpath "/System/iOSSupport") (subpath "/System/Volumes/Preboot")
 (subpath "/usr") (subpath "/bin") (subpath "/sbin")
 (subpath "/Library/Apple") (subpath "/dev") (subpath "/private/etc")
 (subpath "/private/var/db/timezone") (subpath "/private/var/db/DarwinDirectory")
 (subpath "/private/var/db/eligibilityd") (subpath "/private/var/run")
 (subpath {scope}) (literal {executable}))
(deny file-read* (subpath "/usr/local"))
(allow file-read* (literal {executable}))
(deny file-write*)
(allow file-write* (subpath {scope}) (literal "/dev/null") (subpath "/dev/fd"))
(deny process-info* (target others))
(deny mach-lookup (global-name-prefix "com.apple.security"))
(deny mach-priv-task-port)
(deny ipc-posix-shm*)
(deny ipc-posix-sem*)
"#
    ))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    #[test]
    fn shared_claude_sandbox_denies_other_scopes_and_owner_files() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let scope = root.join("room");
        std::fs::create_dir(&scope).unwrap();
        let own = scope.join("own");
        let other = root.join("other-room");
        std::fs::write(&own, "SCOPED").unwrap();
        std::fs::write(&other, "PRIVATE").unwrap();
        let command = format!(
            "cat '{}' && ! cat '{}' && ! printf overwrite > '{}'",
            own.display(),
            other.display(),
            other.display()
        );
        let result = std::process::Command::new("/usr/bin/sandbox-exec")
            .args([
                "-p",
                &seatbelt(&scope, Path::new("/bin/sh")).unwrap(),
                "/bin/sh",
                "-c",
                &command,
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .current_dir(&scope)
            .output()
            .unwrap();
        assert!(result.status.success(), "{:?}", result.status);
        assert_eq!(result.stdout, b"SCOPED");
        assert_eq!(std::fs::read_to_string(other).unwrap(), "PRIVATE");
    }
}
