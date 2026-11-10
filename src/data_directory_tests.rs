use std::{path::PathBuf, process::Command};

#[test]
fn data_directory_overrides_keep_their_order() {
    const CHILD: &str = "HAGENCY_DATA_DIRECTORY_TEST_CHILD";
    const ROOT: &str = "HAGENCY_DATA_DIRECTORY_TEST_ROOT";
    if let Some(mode) = std::env::var_os(CHILD) {
        let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
        assert!(!root.exists());
        // The current name wins; the two older names are aliases, in this order.
        let selected = match mode.to_str().unwrap() {
            "current" => Some("current"),
            "rinx" | "both" => Some("alias-rinx"),
            "robrix" => Some("alias-robrix"),
            _ => None,
        };
        match selected {
            Some(name) => {
                assert_eq!(crate::app_data_dir(), root.join(name));
                assert_eq!(crate::cache_dir(), root.join(name).join("cache"));
            }
            None => {
                assert_eq!(crate::app_data_dir(), crate::project_dir().data_dir());
                assert_eq!(crate::cache_dir(), crate::project_dir().cache_dir());
            }
        }
        assert!(
            !root.exists(),
            "directory selection must not create or migrate files"
        );
        return;
    }

    // Each child starts before the directory OnceLocks or environment are read.
    // Other tests never share these process-local overrides.
    let root = std::env::temp_dir().join(format!(
        "hagency-data-directory-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    for mode in ["none", "rinx", "robrix", "both", "current"] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "data_directory_tests::data_directory_overrides_keep_their_order",
                "--nocapture",
            ])
            .env_remove("HAGENCY_DESKTOP_DATA_DIR")
            .env_remove("RINX_DATA_DIR")
            .env_remove("ROBRIX_DATA_DIR")
            .env(CHILD, mode)
            .env(ROOT, &root);
        if matches!(mode, "rinx" | "both" | "current") {
            child.env("RINX_DATA_DIR", root.join("alias-rinx"));
        }
        if matches!(mode, "robrix" | "both" | "current") {
            child.env("ROBRIX_DATA_DIR", root.join("alias-robrix"));
        }
        if mode == "current" {
            child.env("HAGENCY_DESKTOP_DATA_DIR", root.join("current"));
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
