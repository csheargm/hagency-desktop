//! Original-owner SDK cache IO. One OS worker awaits every SDK future to its
//! actual completion, including SQLite interact after its observer is dropped.
//! The queue holds at most 16 jobs, in addition to one active job. Reads accept
//! at most 64MiB; serialized reads need at most 129MiB of SDK Rust scratch.
//! SQLite native row buffers/page cache are additional, not a Rust/RSS cap.
//! Writes reserve at most 32MiB across queued and active cloned payloads before
//! cloning; SDK serialization/encryption and caller-owned results are separate.
use crate::transfer_budget::ByteBudget;
use matrix_sdk::{Client, media::MediaRequestParameters};
use matrix_sdk_base::media::store::IgnoreMediaRetentionPolicy;
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, OnceLock},
    time::{Duration, Instant},
};

pub(crate) type Current = Arc<dyn Fn() -> bool + Send + Sync>;
const MAX_READ_BYTES: usize = 64 * 1024 * 1024;
const MAX_WRITE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_QUEUED: usize = 16;
type Work = Box<dyn FnOnce() + Send>;
#[derive(Default)]
struct State {
    queue: VecDeque<Work>,
    active: bool,
    stopped: bool,
}
struct Worker {
    state: Mutex<State>,
    wake: Condvar,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
    writes: Arc<ByteBudget>,
}
static WORKER: OnceLock<Arc<Worker>> = OnceLock::new();
fn worker() -> &'static Arc<Worker> {
    WORKER.get_or_init(Worker::start)
}

pub(crate) fn scope(client: &Client) -> Current {
    let epoch = crate::account_session::epoch();
    let original = client.downgrade();
    Arc::new(move || {
        epoch == crate::account_session::epoch()
            && !crate::logout::logout_state_machine::is_logout_in_progress()
            && original
                .get()
                .is_some_and(|client| crate::matrix_context::is_current(&client))
    })
}
impl Worker {
    fn start() -> Arc<Self> {
        let worker = Arc::new(Self {
            state: Mutex::default(),
            wake: Condvar::new(),
            join: Mutex::default(),
            writes: Arc::default(),
        });
        let owned = worker.clone();
        let join = std::thread::Builder::new()
            .name("media-cache-owner".into())
            .spawn(move || {
                loop {
                    let work = {
                        let mut state = owned.state.lock().unwrap();
                        while state.queue.is_empty() && !state.stopped {
                            state = owned.wake.wait(state).unwrap();
                        }
                        if state.stopped {
                            break;
                        }
                        state.active = true;
                        state.queue.pop_front().unwrap()
                    };
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).is_err() {
                        makepad_widgets::error!("Original media cache job panicked");
                    }
                    owned.state.lock().unwrap().active = false;
                    owned.wake.notify_all();
                }
                owned.wake.notify_all();
            })
            .expect("media cache owner worker");
        *worker.join.lock().unwrap() = Some(join);
        worker
    }
    async fn execute<T: Send + 'static>(
        &self,
        current: Current,
        operation: impl FnOnce(Current) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let (done, receiver) = tokio::sync::oneshot::channel();
        {
            let mut state = self.state.lock().unwrap();
            if state.stopped || state.queue.len() >= MAX_QUEUED || !current() {
                return Err("Media cache is busy, stopping or the session changed".into());
            }
            let owner = current.clone();
            state.queue.push_back(Box::new(move || {
                // A cancelled observer can remove work before SDK IO starts.
                if done.is_closed() || !owner() {
                    return;
                }
                let result = operation(owner.clone());
                // Once started, operation is never dropped early. Retired/late
                // results are discarded while original client/buffers still belong here.
                if owner() {
                    let _ = done.send(result);
                }
            }));
            self.wake.notify_one();
        }
        let result = receiver
            .await
            .map_err(|_| "Media cache request cancelled or stopped".to_owned())?;
        if !current() {
            return Err("Matrix session changed".into());
        }
        result
    }
    fn stop(&self, deadline: Instant) -> bool {
        let mut state = self.state.lock().unwrap();
        state.stopped = true;
        state.queue.clear();
        self.wake.notify_all();
        loop {
            let finished = self
                .join
                .lock()
                .unwrap()
                .as_ref()
                .is_none_or(|join| join.is_finished());
            if finished {
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                makepad_widgets::error!(
                    "Media cache shutdown unconfirmed: original IO active={} write_bytes={}",
                    state.active,
                    self.writes.used()
                );
                return false;
            }
            state = self
                .wake
                .wait_timeout(state, (deadline - now).min(Duration::from_millis(50)))
                .unwrap()
                .0;
        }
        drop(state);
        if let Some(join) = self.join.lock().unwrap().take() {
            if join.join().is_err() {
                makepad_widgets::error!("Original media cache worker failed");
            }
        }
        true
    }
}
async fn read_from(
    worker: &Worker,
    client: Client,
    request: MediaRequestParameters,
    max: usize,
    current: Current,
) -> Result<Option<Vec<u8>>, String> {
    if max > MAX_READ_BYTES {
        return Err("Media cache read limit exceeds 64MiB".into());
    }
    worker
        .execute(current, move |current| {
            crate::sliding_sync::backend_runtime().block_on(async move {
                let store = client
                    .media_store()
                    .lock()
                    .await
                    .map_err(|error| error.to_string())?;
                if !current() {
                    return Err("Matrix session changed".into());
                }
                // The pinned official SDK has no row-bounded read. Under its
                // cross-process media lock, the SQLite database size is a conservative
                // upper bound on a stored row (including encrypted serialization).
                // Never fetch an unbounded row and inspect its size afterwards.
                let size = store.get_size().await.map_err(|error| error.to_string())?;
                if size.is_none_or(|size| size > max) {
                    return Err("Pending upload preview exceeds the safe SDK cache-read budget; retry after upload completes".into());
                }
                let content = store.get_media_content(&request).await.map_err(|error| error.to_string())?;
                if content.as_ref().is_some_and(|data| data.len() > max) {
                    return Err("SDK cache violated its reported storage bound".into());
                }
                Ok(content)
            })
        })
        .await
}
pub(crate) async fn read(
    client: Client,
    request: MediaRequestParameters,
    max: usize,
    current: Current,
) -> Result<Option<Vec<u8>>, String> {
    read_from(worker(), client, request, max, current).await
}
async fn write_to(
    worker: &Worker,
    client: Client,
    request: MediaRequestParameters,
    content: &[u8],
    current: Current,
) -> Result<(), String> {
    // Reserve before cloning. This reservation lives in the original job, not
    // its async observer, until add_media_content actually returns.
    let quota = worker
        .writes
        .reserve(content.len() as u64, MAX_WRITE_BYTES)?;
    let content = content.to_vec();
    worker
        .execute(current, move |current| {
            let _quota = quota;
            crate::sliding_sync::backend_runtime().block_on(async move {
                let store = client
                    .media_store()
                    .lock()
                    .await
                    .map_err(|error| error.to_string())?;
                if !current() {
                    return Err("Matrix session changed".into());
                }
                store
                    .add_media_content(&request, content, IgnoreMediaRetentionPolicy::No)
                    .await
                    .map_err(|error| error.to_string())
            })
        })
        .await
}
pub(crate) async fn write(
    client: Client,
    request: MediaRequestParameters,
    content: &[u8],
    current: Current,
) -> Result<(), String> {
    write_to(worker(), client, request, content, current).await
}
pub(crate) fn shutdown(deadline: Instant) {
    if let Some(worker) = WORKER.get() {
        worker.stop(deadline);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::{media::MediaFormat, ruma::{api::MatrixVersion, events::room::MediaSource}};

    #[tokio::test]
    async fn official_encrypted_sqlite_local_cache_reads_are_admitted_before_payload_allocation() {
        let root = tempfile::tempdir().unwrap();
        let client = Client::builder().homeserver_url("http://127.0.0.1:1")
            .server_versions([MatrixVersion::V1_11])
            .sqlite_store(root.path(), Some("isolated-cache-test-secret"))
            .build().await.unwrap();
        let request = MediaRequestParameters {
            source: MediaSource::Plain("mxc://send-queue.localhost/local-echo".into()),
            format: MediaFormat::File,
        };
        let owner = Worker::start();
        {
            let store = client.media_store().lock().await.unwrap();
            store.add_media_content(&request, b"local plaintext".to_vec(), IgnoreMediaRetentionPolicy::No).await.unwrap();
        }
        let current: Current = Arc::new(|| true);
        assert_eq!(read_from(&owner, client.clone(), request.clone(), 8 * 1024 * 1024, current.clone()).await.unwrap().unwrap(), b"local plaintext");
        assert!(read_from(&owner, client.clone(), request.clone(), 1, current.clone()).await.is_err());
        // Rejecting a read does not delete queued upload content or its metadata.
        assert_eq!(read_from(&owner, client, request, 8 * 1024 * 1024, current).await.unwrap().unwrap(), b"local plaintext");
        assert!(owner.stop(Instant::now() + Duration::from_secs(5)));
    }

    #[tokio::test]
    async fn unknown_storage_bound_is_rejected_without_unbounded_fallback() {
        let client = Client::builder().homeserver_url("http://127.0.0.1:1")
            .server_versions([MatrixVersion::V1_11]).build().await.unwrap();
        let request = MediaRequestParameters {
            source: MediaSource::Plain("mxc://send-queue.localhost/unknown".into()), format: MediaFormat::File,
        };
        let owner = Worker::start();
        assert!(read_from(&owner, client, request, 8 * 1024 * 1024, Arc::new(|| true)).await.is_err());
        assert!(owner.stop(Instant::now() + Duration::from_secs(5)));
    }
}
