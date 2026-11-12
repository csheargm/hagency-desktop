//! One owned blocking credential worker, with finite admission. Cancelling the
//! observing future never cancels a native operation or releases its custody.
use super::Error;
use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

type Job = Box<dyn FnOnce() + Send>;
type IdleOperation = Arc<dyn Fn() + Send + Sync>;
const MAX_WORKERS: usize = 8;
static ORIGINAL_WORKERS: AtomicUsize = AtomicUsize::new(0);
static RETIRED: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());
struct Admission;
impl Drop for Admission {
    fn drop(&mut self) {
        ORIGINAL_WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}
fn reap_retired() {
    let complete = {
        let mut retired = RETIRED.lock().unwrap_or_else(|e| e.into_inner());
        let mut complete = Vec::new();
        let mut i = 0;
        while i < retired.len() {
            if retired[i].is_finished() {
                complete.push(retired.swap_remove(i));
            } else {
                i += 1;
            }
        }
        complete
    };
    for join in complete {
        let _ = join.join();
    }
}
/// Original native operations retained after their owner dropped or timed out.
/// Reading this never waits for OS IO and never claims that a prompt was closed.
pub fn retained_shutdowns() -> usize {
    reap_retired();
    RETIRED.lock().unwrap_or_else(|e| e.into_inner()).len()
}
pub struct Worker {
    sender: SyncSender<Job>,
    state: Arc<State>,
    join: Mutex<Option<JoinHandle<()>>>,
    idle: Arc<Mutex<Option<IdleOperation>>>,
}
struct State {
    stopped: AtomicBool,
    done: Mutex<bool>,
    wake: Condvar,
}
struct Original {
    receiver: Option<Receiver<Job>>,
    state: Arc<State>,
}
impl Drop for Original {
    fn drop(&mut self) {
        self.state.stopped.store(true, Ordering::Release);
        // Queued closure destructors can retain actual files. Observe their
        // completion before reporting the original worker done, even on panic.
        drop(self.receiver.take());
        let mut done = self.state.done.lock().unwrap_or_else(|e| e.into_inner());
        *done = true;
        self.state.wake.notify_all();
    }
}
impl Worker {
    pub fn new() -> Result<Self, Error> {
        reap_retired();
        ORIGINAL_WORKERS
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_WORKERS).then_some(n + 1)
            })
            .map_err(|_| Error::Unavailable)?;
        let admission = Admission;
        let (sender, receiver) = mpsc::sync_channel::<Job>(16);
        let state = Arc::new(State {
            stopped: AtomicBool::new(false),
            done: Mutex::new(false),
            wake: Condvar::new(),
        });
        let original = state.clone();
        let idle: Arc<Mutex<Option<IdleOperation>>> = Arc::new(Mutex::new(None));
        let background = idle.clone();
        let join = std::thread::Builder::new()
            .name("hagency-credentials".into())
            .spawn(move || {
                let _admission = admission;
                let original_io = Original {
                    receiver: Some(receiver),
                    state: original.clone(),
                };
                let mut last_idle = Instant::now();
                while !original.stopped.load(Ordering::Acquire) {
                    match original_io
                        .receiver
                        .as_ref()
                        .expect("original receiver")
                        .recv_timeout(Duration::from_millis(100))
                    {
                        Ok(job) => {
                            if original.stopped.load(Ordering::Acquire) {
                                break;
                            }
                            job();
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if last_idle.elapsed() >= Duration::from_secs(30) {
                                let operation =
                                    background.lock().unwrap_or_else(|e| e.into_inner()).clone();
                                if !original.stopped.load(Ordering::Acquire)
                                    && let Some(operation) = operation
                                {
                                    operation();
                                }
                                last_idle = Instant::now();
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                // Dropping queued work releases only jobs that never started. An
                // in-progress OS call reaches this point only after it really ends.
                drop(original_io);
            })
            .map_err(|_| Error::Unavailable)?;
        Ok(Self {
            sender,
            state,
            join: Mutex::new(Some(join)),
            idle,
        })
    }
    /// Optional retry work runs on this same original worker, with identical
    /// shutdown/join custody. Installing it neither performs IO nor confirms a
    /// durable transaction. The callback must not retain the owning Worker.
    pub fn set_idle_operation(&self, operation: IdleOperation) -> Result<(), Error> {
        if self.state.stopped.load(Ordering::Acquire) {
            return Err(Error::Unavailable);
        }
        *self.idle.lock().map_err(|_| Error::Unavailable)? = Some(operation);
        Ok(())
    }
    pub async fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce() -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        if self.state.stopped.load(Ordering::Acquire) {
            return Err(Error::Unavailable);
        }
        let (sender, receiver) = oneshot::channel();
        let state = self.state.clone();
        self.sender
            .try_send(Box::new(move || {
                if state.stopped.load(Ordering::Acquire) || sender.is_closed() {
                    return;
                }
                let _ = sender.send(operation());
            }))
            .map_err(|_| Error::Unavailable)?;
        receiver.await.map_err(|_| Error::Unavailable)?
    }
    /// Bounded observation for a native shutdown hook that cannot await. A
    /// timeout does not terminate the original operation or detach its worker.
    pub fn call_sync<T: Send + 'static>(
        &self,
        timeout: Duration,
        operation: impl FnOnce() -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        if self.state.stopped.load(Ordering::Acquire) {
            return Err(Error::Unavailable);
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let state = self.state.clone();
        self.sender
            .try_send(Box::new(move || {
                if state.stopped.load(Ordering::Acquire) {
                    return;
                }
                let _ = sender.send(operation());
            }))
            .map_err(|_| Error::Unavailable)?;
        receiver
            .recv_timeout(timeout)
            .map_err(|_| Error::DurabilityUnknown)?
    }
    /// A false result means original native IO is still unknown. Keep this
    /// worker and join handle owned; never claim that an OS prompt was closed.
    pub fn shutdown(&self, timeout: Duration) -> bool {
        self.state.stopped.store(true, Ordering::Release);
        let deadline = Instant::now() + timeout;
        let mut done = self.state.done.lock().unwrap_or_else(|e| e.into_inner());
        while !*done {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let (next, _) = self
                .state
                .wake
                .wait_timeout(done, remaining)
                .unwrap_or_else(|e| e.into_inner());
            done = next;
        }
        drop(done);
        if let Some(join) = self.join.lock().unwrap_or_else(|e| e.into_inner()).take() {
            return join.join().is_ok();
        }
        true
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.state.stopped.store(true, Ordering::Release);
        if let Some(join) = self
            .join
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            if join.is_finished() {
                let _ = join.join();
            } else {
                RETIRED.lock().unwrap_or_else(|e| e.into_inner()).push(join);
            }
        }
    }
}
