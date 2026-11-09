//! Installed CLI contract test. All model responses and credentials are local
//! fixtures; this never reads the owner's login or contacts a provider.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires HAGENCY_TEST_INSTALLED_CLAUDE; offline installed CLI and Seatbelt"]
fn installed_shared_claude_routes_permissions_inside_the_isolated_scope() {
    use hagency_runtime::owned::SharedClaudePolicy;
    let binary = std::env::var_os("HAGENCY_TEST_INSTALLED_CLAUDE")
        .expect("explicit installed Claude binary");
    let artifacts = tempfile::tempdir().unwrap();
    let args = SharedClaudePolicy::Yolo { web: false }
        .arguments("sonnet", "medium")
        .unwrap();
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/claude_isolation_fixture.py"
        ))
        .arg(binary)
        .arg(serde_json::to_string(&args).unwrap())
        .arg(artifacts.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["approval_callbacks"], 1);
    assert_eq!(result["completed"], true);
}
