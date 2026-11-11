//! Finite, owned consumers for verified files. No filesystem IO on the UI/Tokio.
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, OnceLock},
    time::{Duration, Instant},
};
type Work = Box<dyn FnOnce() + Send>;
#[derive(Default)]
struct State {
    queue: VecDeque<Work>,
    active: usize,
    stopped: bool,
}
struct Worker {
    state: Mutex<State>,
    wake: Condvar,
    joins: Mutex<Vec<std::thread::JoinHandle<()>>>,
}
static WORKER: OnceLock<Arc<Worker>> = OnceLock::new();
fn worker() -> &'static Arc<Worker> {
    WORKER.get_or_init(|| {
        let worker = Arc::new(Worker {
            state: Mutex::default(),
            wake: Condvar::new(),
            joins: Mutex::default(),
        });
        for index in 0..2 {
            let owned = worker.clone();
            let join = std::thread::Builder::new()
                .name(format!("attachment-consumer-{index}"))
                .spawn(move || loop {
                    let work = {
                        let mut state = owned.state.lock().unwrap();
                        while state.queue.is_empty() && !state.stopped {
                            state = owned.wake.wait(state).unwrap();
                        }
                        if state.stopped {
                            break;
                        }
                        state.active += 1;
                        state.queue.pop_front().unwrap()
                    };
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).is_err() {
                        makepad_widgets::error!(
                            "Attachment consumer failed; original file remains observed"
                        );
                    }
                    owned.state.lock().unwrap().active -= 1;
                    owned.wake.notify_all();
                })
                .expect("attachment consumer worker");
            worker.joins.lock().unwrap().push(join);
        }
        worker
    })
}
pub(crate) fn submit(work: impl FnOnce() + Send + 'static) -> Result<(), String> {
    let mut state = worker().state.lock().unwrap();
    if state.stopped || state.queue.len() >= 16 {
        return Err("Attachment file operations are busy or stopping".into());
    }
    state.queue.push_back(Box::new(work));
    worker().wake.notify_one();
    Ok(())
}
pub(crate) fn shutdown(deadline: Instant) {
    let Some(worker) = WORKER.get() else {
        return;
    };
    let mut state = worker.state.lock().unwrap();
    state.stopped = true;
    state.queue.clear();
    worker.wake.notify_all();
    while !worker
        .joins
        .lock()
        .unwrap()
        .iter()
        .all(|join| join.is_finished())
    {
        let now = Instant::now();
        if now >= deadline {
            makepad_widgets::error!(
                "Attachment consumer shutdown unconfirmed: {} original IO jobs remain",
                state.active
            );
            return;
        }
        state = worker
            .wake
            .wait_timeout(state, (deadline - now).min(Duration::from_millis(50)))
            .unwrap()
            .0;
    }
    drop(state);
    for join in worker.joins.lock().unwrap().drain(..) {
        if join.join().is_err() {
            makepad_widgets::error!("Original attachment consumer failed");
        }
    }
}
