//! Inference-free protocol smoke test. Creates only disposable fresh local state.
use hagency_agent_local::{Ledger, Scope, codex::Profile};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let executable = std::env::args_os()
        .nth(1)
        .ok_or("Pass the absolute Codex executable path")?;
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    for dir in ["home", "codex", "workspace"] {
        std::fs::create_dir(root.join(dir))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root.join(dir), std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let profile = Profile {
        executable: std::path::PathBuf::from(executable).canonicalize()?,
        home: root.join("home"),
        codex_home: root.join("codex"),
        cwd: root.join("workspace"),
        model: "gpt-6.1-sol".into(),
        effort: "low".into(),
        shared_auth: false, local_model: None,
        web_search: true,
    };
    let scope = Scope {
        agent: "handshake".into(),
        binding: "handshake-binding".into(),
        room: "!handshake:example.test".into(),
        requester: "@owner:example.test".into(),
        thread: "main".into(),
    };
    let mut ledger = Ledger::open(root.join("ledger.db"), "@owner:example.test")?;
    ledger.register_binding("@owner:example.test", &scope)?;
    let mut process = profile.spawn().await?;
    let result = async {
        let initialized = process.session.initialize().await?;
        process.session.verify_host_environment().await?;
        // Disposable login has no credentials: account/read must fail closed.
        if process.session.require_local_account().await.is_ok() {
            return Err(hagency_agent_local::codex::Error::Protocol(
                "fresh profile unexpectedly authenticated",
            ));
        }
        process
            .session
            .open_context(&mut ledger, &scope, &profile)
            .await?;
        println!(
            "initialized: {}; scoped read-only thread opened; no turn/model/tool started",
            initialized
                .get("userAgent")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Codex")
        );
        Ok::<_, hagency_agent_local::codex::Error>(())
    }
    .await;
    process.stop().await?;
    result?;
    Ok(())
}
