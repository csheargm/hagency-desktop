//! Loading the Status tab within a bound: the owner serves one command at a
//! time, so a busy loop could otherwise leave the tab on "Loading…" forever.
//! Each owner call and the whole load are bounded; a timeout names the call
//! in a WARN line and the tab says it took too long.
use super::{Loaded, Watches, status, watches_of};
use futures_util::{StreamExt, TryStreamExt, stream};
use matrix_sdk::ruma::RoomId;
use serde_json::Value;
use std::future::Future;
use std::time::Duration;

/// The whole load: roster, every saved watch and the room's status.
pub const TOTAL_BOUND: Duration = Duration::from_secs(20);
/// One owner call (the roster or one Agent's saved watch).
const CALL_BOUND: Duration = Duration::from_secs(8);
/// Saved watches read at once.
pub const CONCURRENT_READS: usize = 4;
/// Wait before the one automatic retry after a timeout.
pub const RETRY_BACKOFF: Duration = Duration::from_secs(3);

/// The reason shown after "Room status is unavailable: ".
pub fn too_long() -> String {
    crate::i18n::tr("it took too long. Open Status again to retry.").to_owned()
}
pub fn is_too_long(loaded: &Loaded) -> bool {
    matches!(loaded, Err(e) if *e == too_long())
}

/// `work` within `bound`; on timeout a WARN line names `call`.
pub async fn bounded<T>(call: &str, bound: Duration, work: impl Future<Output = Result<T, String>>) -> Result<T, String> {
    match tokio::time::timeout(bound, work).await {
        Ok(result) => result,
        Err(_) => {
            makepad_widgets::warning!("Room status: {call} took longer than {}s; showing a timeout", bound.as_secs());
            Err(too_long())
        }
    }
}

/// Every Agent's saved watch, `limit` at a time, each within `per_call`, in
/// roster order. The first failure or timeout fails the whole read.
pub async fn saved_all<F, Fut>(ids: Vec<String>, limit: usize, per_call: Duration, fetch: F) -> Result<Vec<(String, Value)>, String>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    stream::iter(ids)
        .map(|id| {
            let read = fetch(id.clone());
            async move {
                let call = format!("the saved watch of Agent {id}");
                bounded(&call, per_call, read).await.map(|reply| (id, reply))
            }
        })
        .buffered(limit.max(1))
        .try_collect()
        .await
}

/// The tab's rows and status for `room`, unbounded as a whole (see [`load`]).
async fn load_unbounded(room: &str) -> Loaded {
    use crate::hagency::{backend::Service, model::Command};
    let service = Service::for_current()?;
    let roster = bounded("the Agents list", CALL_BOUND, service.execute(Command::Agents)).await?;
    let ids = roster["agents"].as_array().into_iter().flatten().filter_map(|a| a["id"].as_str().map(str::to_owned)).collect();
    let service = &service;
    let saved = saved_all(ids, CONCURRENT_READS, CALL_BOUND, |id| async move { crate::hagency::issue_watch::live::saved(service, &id, None).await }).await?;
    let watches = watches_of(room, &roster, &saved)?;
    let own: Vec<status::OwnWatch> = watches.iter().map(|(id, w)| (id.clone(), w.repo.clone(), w.active)).collect();
    let client = crate::sliding_sync::get_client().ok_or("Signed out")?;
    let room_id = RoomId::parse(room).map_err(|e| e.to_string())?;
    let status = bounded("the room's status", TOTAL_BOUND, status::load(&client, &room_id, &own)).await;
    Ok(Watches { rows: watches.into_iter().map(|(_, w)| w).collect(), status })
}

/// [`load_unbounded`] within [`TOTAL_BOUND`].
pub async fn load(room: &str) -> Loaded {
    bounded("the room status load", TOTAL_BOUND, load_unbounded(room)).await
}
