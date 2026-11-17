//! Actual installed app-server registration/resume probe. No inference or tool call.
#[cfg(unix)]
use hagency_agent_local::{
    Ledger, Scope,
    codex::{HostToolGate, Profile},
    room_files::Workspace,
};
#[cfg(unix)]
use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};
#[cfg(unix)]
struct Closed;
#[cfg(unix)]
impl HostToolGate for Closed {
    fn authorize<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { false })
    }
}
#[cfg(unix)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let executable = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("absolute Codex executable required")?,
    )
    .canonicalize()?;
    let temp = tempfile::tempdir()?;
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
    let owner = temp
        .path()
        .canonicalize()?
        .join(format!("owner_{}", "f".repeat(64)));
    std::fs::create_dir(&owner)?;
    std::fs::set_permissions(&owner, std::fs::Permissions::from_mode(0o700))?;
    for dir in ["provider-home", "codex-home"] {
        std::fs::create_dir(owner.join(dir))?;
        std::fs::set_permissions(owner.join(dir), std::fs::Permissions::from_mode(0o700))?;
    }
    // Ancestor docs exist, but effective project_doc_max_bytes is forced to zero.
    std::fs::write(
        owner.join("AGENTS.md"),
        "HAGENCY_PROBE_ANCESTOR_INSTRUCTIONS_MUST_NOT_LOAD",
    )?;
    let scope = Scope {
        agent: "probe-agent".into(),
        binding: "probe-binding".into(),
        room: "!probe:example.test".into(),
        requester: "@owner:example.test".into(),
        thread: "matrix-thread".into(),
    };
    let mut ledger = Ledger::open(owner.join("ledger.db"), "@owner:example.test")?;
    ledger.register_binding("@owner:example.test", &scope)?;
    for _ in 0..2 {
        let workspace = Workspace::open(&owner, "@owner:example.test", &scope)?;
        let profile = Profile {
            executable: executable.clone(),
            home: owner.join("provider-home"),
            codex_home: owner.join("codex-home"),
            cwd: workspace.canonical_directory().into(),
            model: "gpt-6.1-sol".into(),
            effort: "low".into(),
            shared_auth: false, local_model: None,
            web_search: true,
        };
        let mut process = profile.spawn().await?;
        process
            .session
            .enable_host_files(workspace, Arc::new(Closed))?;
        let result = async {
            process.session.initialize().await?;
            process
                .session
                .open_context(&mut ledger, &scope, &profile)
                .await
        }
        .await;
        process.stop().await?;
        result?;
    }
    if ledger.context_session(&scope)?.is_some() {
        return Err("tool context overwrote chat history".into());
    }
    println!(
        "PASS: actual dynamic tool registration and separate capability context resume; effective ambient instructions disabled; no model/tool/fees"
    );
    Ok(())
}

#[cfg(not(unix))]
fn main() {
    eprintln!("host file capabilities require Unix");
}
