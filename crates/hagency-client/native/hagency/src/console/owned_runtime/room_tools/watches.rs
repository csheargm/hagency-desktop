//! Issue watches for one admitted dispatch (ADR 0019): the Agent's own watch
//! block for its run context and the `room_watches` tool. Both read this
//! device's ledger for the dispatch's room only; nothing here writes.
use super::Source;
use hagency_agent_local::{Ledger, attribution, watch_info};

/// The ledger of the dispatch's owner profile, opened read-side.
fn ledger(source: &Source) -> Result<Ledger, &'static str> {
    let console = &source.gate.console;
    let root = console.console.0.server_login.state_directory().ok_or("watches_unavailable")?;
    let p = &console.pin;
    let owners = super::super::super::owned_agents::ledger_path(&root, &p.origin, &p.issuer, &p.subject, &p.owner);
    let path = owners.map_err(|_| "watches_unavailable")?;
    let identity = super::super::super::owned_agents::profile_identity(&p.origin, &p.issuer, &p.subject, &p.owner)
        .map_err(|_| "watches_unavailable")?;
    Ledger::open_scoped(&path, &p.owner, &identity).map_err(|_| "watches_unavailable")
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T, &'static str> + Send + 'static) -> Result<T, &'static str> {
    tokio::task::spawn_blocking(work).await.map_err(|_| "watches_unavailable")?
}

/// The Agent's own watches for the next prompt. A ledger that cannot be read
/// gives no block, never a failed turn: the Agent then says it cannot see
/// its watch settings, which is true.
pub(super) async fn own_block(source: &std::sync::Arc<Source>) -> Option<String> {
    let s = source.clone();
    blocking(move || {
        let d = &s.gate.expected;
        let entries = ledger(&s)?.watch_entries(&d.agent_id).map_err(|_| "watches_unavailable")?;
        Ok(watch_info::render_own(&entries, &d.room_id, &s.gate.console.pin.owner))
    })
    .await
    .ok()
}

/// The `room_watches` result for this dispatch's room.
pub(super) async fn handle(source: &std::sync::Arc<Source>) -> String {
    let room = source.gate.expected.room_id.clone();
    if let Err(code) = source.authorize(Some(&room)).await {
        return serde_json::json!({"error": code}).to_string();
    }
    let s = source.clone();
    let listed = blocking(move || {
        let label = |id: &str| attribution::agent_label(id).unwrap_or_else(|| id.to_owned());
        ledger(&s)?.room_watches(&room, label).map_err(|_| "watches_unavailable")
    })
    .await;
    match listed {
        Ok(watches) => watch_info::render_room(&watches),
        Err(code) => serde_json::json!({"error": code}).to_string(),
    }
}
