//! Coalesce replaceable content without crossing ordered control events.
use crate::home::room_screen::TimelineUpdate;
use crossbeam_channel::{SendError, TryRecvError};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicUsize, Ordering},
    },
};

struct Route {
    target: Mutex<Weak<Mutex<State>>>,
    senders: AtomicUsize,
}

struct State {
    identity: usize,
    queue: VecDeque<TimelineUpdate>,
    senders: usize,
    receiving: bool,
    forwarded: bool,
    retired: bool,
    route: Arc<Route>,
}

pub struct Sender(Arc<Mutex<State>>);
pub struct Receiver(Arc<Mutex<State>>);

pub fn channel() -> (Sender, Receiver) {
    // Creation order also fences delayed cache-eviction actions: an old UI
    // endpoint may precede an unclaimed replacement, but a newer UI must survive.
    static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
    let state = Arc::new(Mutex::new(State {
        identity: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        queue: VecDeque::new(),
        senders: 1,
        receiving: true,
        forwarded: false,
        retired: false,
        route: Arc::new(Route {
            target: Mutex::new(Weak::new()),
            senders: AtomicUsize::new(1),
        }),
    }));
    (Sender(state.clone()), Receiver(state))
}

impl Clone for Sender {
    fn clone(&self) -> Self {
        let mut state = self.0.lock().unwrap();
        state.senders += 1;
        state.route.senders.fetch_add(1, Ordering::AcqRel);
        Self(self.0.clone())
    }
}
impl Drop for Sender {
    fn drop(&mut self) {
        let mut state = self.0.lock().unwrap();
        state.senders -= 1;
        state.route.senders.fetch_sub(1, Ordering::AcqRel);
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        let mut state = self.0.lock().unwrap();
        state.receiving = false;
        state.queue.clear();
    }
}

fn content(update: &TimelineUpdate) -> bool {
    matches!(
        update,
        TimelineUpdate::NewItems { .. } | TimelineUpdate::LocalEchoProgress { .. }
    )
}

fn operation(update: &TimelineUpdate) -> bool {
    matches!(
        update,
        TimelineUpdate::MessageEdited { .. }
            | TimelineUpdate::PinResult { .. }
            | TimelineUpdate::SendFailedBeforeBeingQueued { .. }
            | TimelineUpdate::FileUploadStarted { .. }
            | TimelineUpdate::FileUploadQueuing { .. }
            | TimelineUpdate::FileUploadProgress { .. }
            | TimelineUpdate::FileUploadError { .. }
            | TimelineUpdate::FileUploadComplete { .. }
            | TimelineUpdate::AttachmentDownloadFinished(..)
            | TimelineUpdate::AttachmentDownloadReset(..)
    )
}

impl Sender {
    pub(crate) fn identity(&self) -> usize {
        self.0.lock().unwrap().identity
    }
    /// Retain operation outcomes while dropping a hidden window's snapshots.
    pub(crate) fn discard_snapshots(&self) {
        let mut state = self.0.lock().unwrap();
        state.retired = true;
        state.queue.retain(operation);
    }
    /// Only mutation/transfer outcomes follow a same-account context switch.
    /// Positional snapshots, pagination results and search targets must stay fenced.
    pub(crate) fn forward_operations_to(&self, next: &Self) {
        if Arc::ptr_eq(&self.0, &next.0) {
            return;
        }
        // Lock endpoint states by address so simultaneous replacement attempts
        // cannot deadlock. Producers hold at most one state at a time.
        let (mut previous, mut destination) = if Arc::as_ptr(&self.0) < Arc::as_ptr(&next.0) {
            (self.0.lock().unwrap(), next.0.lock().unwrap())
        } else {
            let destination = next.0.lock().unwrap();
            (self.0.lock().unwrap(), destination)
        };
        if previous.forwarded {
            return;
        }
        // Every replacement is a freshly created channel. Share one weak
        // destination across the entire room's operation producers, rather than
        // retaining every historical endpoint until the oldest download ends.
        assert!(
            !destination.forwarded && destination.route.target.lock().unwrap().upgrade().is_none(),
            "operation destination must be a fresh channel"
        );
        previous
            .route
            .senders
            .fetch_add(destination.senders, Ordering::AcqRel);
        destination.route = previous.route.clone();
        *previous.route.target.lock().unwrap() = Arc::downgrade(&next.0);
        previous.forwarded = true;
        if destination.receiving {
            destination
                .queue
                .extend(previous.queue.drain(..).filter(operation));
        } else {
            previous.queue.clear();
        }
    }
    pub fn send(&self, update: TimelineUpdate) -> Result<(), SendError<()>> {
        Self::enqueue(&self.0, update)
    }
    fn enqueue(
        destination: &Arc<Mutex<State>>,
        update: TimelineUpdate,
    ) -> Result<(), SendError<()>> {
        let mut destination = destination.clone();
        loop {
            let mut state = destination.lock().unwrap();
            if state.forwarded {
                if !operation(&update) {
                    return Err(SendError(()));
                }
                let route = state.route.clone();
                drop(state);
                let next = route
                    .target
                    .lock()
                    .unwrap()
                    .upgrade()
                    .ok_or(SendError(()))?;
                destination = next;
                continue;
            }
            if !state.receiving || (state.retired && !operation(&update)) {
                return Err(SendError(()));
            }
            if content(&update) && state.queue.back().is_some_and(content) {
                let previous = state.queue.pop_back().unwrap();
                if matches!(previous, TimelineUpdate::LocalEchoProgress { .. })
                    && matches!(update, TimelineUpdate::LocalEchoProgress { .. })
                {
                    state.queue.push_back(update);
                    return Ok(());
                }
                // Once intermediate snapshots disappear, invalidate their combined content.
                // Preserve append notification even when the final update is upload progress.
                let was_append = matches!(
                    previous,
                    TimelineUpdate::NewItems {
                        is_append: true,
                        ..
                    }
                );
                let clear_cache = matches!(
                    previous,
                    TimelineUpdate::NewItems {
                        clear_cache: true,
                        ..
                    }
                ) || matches!(
                    update,
                    TimelineUpdate::NewItems {
                        clear_cache: true,
                        ..
                    }
                );
                let update = match update {
                    TimelineUpdate::NewItems {
                        new_items,
                        is_append,
                        ..
                    } => TimelineUpdate::NewItems {
                        changed_indices: 0..new_items.len(),
                        new_items,
                        clear_cache,
                        is_append: was_append || is_append,
                    },
                    TimelineUpdate::LocalEchoProgress { new_items } => TimelineUpdate::NewItems {
                        changed_indices: 0..new_items.len(),
                        new_items,
                        clear_cache,
                        is_append: was_append,
                    },
                    _ => unreachable!(),
                };
                state.queue.push_back(update);
            } else {
                state.queue.push_back(update);
            }
            return Ok(());
        }
    }
}
impl Receiver {
    pub(crate) fn identity(&self) -> usize {
        self.0.lock().unwrap().identity
    }
    pub(crate) fn shares_route(&self, sender: &Sender) -> bool {
        let route = self.0.lock().unwrap().route.clone();
        Arc::ptr_eq(&route, &sender.0.lock().unwrap().route)
    }
    pub fn try_recv(&self) -> Result<TimelineUpdate, TryRecvError> {
        let mut state = self.0.lock().unwrap();
        state
            .queue
            .pop_front()
            .ok_or(if state.route.senders.load(Ordering::Acquire) == 0 {
                TryRecvError::Disconnected
            } else {
                TryRecvError::Empty
            })
    }
    pub fn is_empty(&self) -> bool {
        self.0.lock().unwrap().queue.is_empty()
    }
    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.lock().unwrap().queue.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use imbl::Vector;
    #[test]
    fn unsent_message_payload_survives_context_replacement() {
        let (old, _old_receiver) = channel();
        old.send(TimelineUpdate::SendFailedBeforeBeingQueued {
            message: ruma::events::room::message::RoomMessageEventContent::text_plain(
                "unsent 中文 👩‍💻",
            ),
            replied_to: None,
        })
        .unwrap();
        let (next, receiver) = channel();
        old.forward_operations_to(&next);
        let TimelineUpdate::SendFailedBeforeBeingQueued {
            message,
            replied_to,
        } = receiver.try_recv().unwrap()
        else {
            panic!("unsent content must follow its operation route");
        };
        assert!(replied_to.is_none());
        assert!(
            matches!(message.msgtype, ruma::events::room::message::MessageType::Text(text) if text.body == "unsent 中文 👩‍💻")
        );
    }
    fn snapshot(append: bool) -> TimelineUpdate {
        TimelineUpdate::NewItems {
            new_items: Vector::new(),
            changed_indices: 0..0,
            clear_cache: false,
            is_append: append,
        }
    }
    #[test]
    fn content_bursts_keep_one_snapshot_and_append_semantics() {
        let (tx, rx) = channel();
        tx.send(snapshot(true)).unwrap();
        for _ in 0..100_000 {
            tx.send(snapshot(false)).unwrap();
        }
        tx.send(TimelineUpdate::LocalEchoProgress {
            new_items: Vector::new(),
        })
        .unwrap();
        assert_eq!(rx.len(), 1);
        assert!(matches!(
            rx.try_recv(),
            Ok(TimelineUpdate::NewItems {
                clear_cache: false,
                is_append: true,
                ..
            })
        ));
    }
    #[test]
    fn control_events_remain_ordered_between_content() {
        let (tx, rx) = channel();
        tx.send(snapshot(false)).unwrap();
        tx.send(TimelineUpdate::PaginationRunning(
            crate::sliding_sync::PaginationDirection::Backwards,
        ))
        .unwrap();
        tx.send(snapshot(true)).unwrap();
        assert_eq!(rx.len(), 3);
        assert!(matches!(
            rx.try_recv(),
            Ok(TimelineUpdate::NewItems {
                is_append: false,
                ..
            })
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(TimelineUpdate::PaginationRunning(_))
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(TimelineUpdate::NewItems {
                is_append: true,
                ..
            })
        ));
    }
    #[test]
    fn disconnect_drops_retained_content_and_rejects_late_producers() {
        let (tx, rx) = channel();
        let late = tx.clone();
        tx.send(snapshot(false)).unwrap();
        drop(rx);
        assert!(late.send(snapshot(false)).is_err());
        assert!(tx.0.lock().unwrap().queue.is_empty());
    }
    #[test]
    fn context_replacement_forwards_transfer_outcomes_but_rejects_stale_positions() {
        let (old, old_rx) = channel();
        let (next, next_rx) = channel();
        let uri = "mxc://example.org/download".into();
        old.send(TimelineUpdate::AttachmentDownloadFinished(uri, Ok(())))
            .unwrap();
        old.send(snapshot(false)).unwrap();
        old.forward_operations_to(&next);
        drop(old_rx);
        assert!(old.send(snapshot(true)).is_err());
        assert!(matches!(
            next_rx.try_recv(),
            Ok(TimelineUpdate::AttachmentDownloadFinished(_, Ok(())))
        ));
        let (latest, latest_rx) = channel();
        next.forward_operations_to(&latest);
        drop(next_rx);
        old.send(TimelineUpdate::AttachmentDownloadReset(
            "mxc://example.org/download".into(),
        ))
        .unwrap();
        assert!(matches!(
            latest_rx.try_recv(),
            Ok(TimelineUpdate::AttachmentDownloadReset(_))
        ));
    }
    #[test]
    fn repeated_context_switches_release_intermediate_endpoints_with_old_producers_alive() {
        let (oldest, receiver) = channel();
        let mut current = oldest.clone();
        let mut receiver = receiver;
        for _ in 0..20_000 {
            let retired = Arc::downgrade(&current.0);
            let (next, next_receiver) = channel();
            current.forward_operations_to(&next);
            drop(current);
            drop(receiver);
            current = next;
            receiver = next_receiver;
            if !Weak::ptr_eq(&retired, &Arc::downgrade(&oldest.0)) {
                assert!(retired.upgrade().is_none(), "historical endpoint retained");
            }
        }
        oldest
            .send(TimelineUpdate::AttachmentDownloadReset(
                "mxc://example.org/file".into(),
            ))
            .unwrap();
        assert!(matches!(
            receiver.try_recv(),
            Ok(TimelineUpdate::AttachmentDownloadReset(_))
        ));
        assert!(oldest.send(snapshot(false)).is_err());
        let final_state = Arc::downgrade(&current.0);
        drop(current);
        drop(receiver);
        assert!(final_state.upgrade().is_none());
        assert!(oldest
            .send(TimelineUpdate::AttachmentDownloadReset(
                "mxc://example.org/file".into()
            ))
            .is_err());
    }

    #[test]
    fn replacement_with_closed_receiver_drops_pending_operations() {
        let (old, _receiver) = channel();
        let (next, receiver) = channel();
        old.send(TimelineUpdate::AttachmentDownloadReset(
            "mxc://example.org/file".into(),
        ))
        .unwrap();
        drop(receiver);
        old.forward_operations_to(&next);
        assert!(old.0.lock().unwrap().queue.is_empty());
        assert!(next.0.lock().unwrap().queue.is_empty());
        assert!(old
            .send(TimelineUpdate::AttachmentDownloadReset(
                "mxc://example.org/file".into()
            ))
            .is_err());
    }

    #[test]
    fn forwarded_producers_keep_the_replacement_receiver_connected() {
        let (old, old_receiver) = channel();
        let (next, next_receiver) = channel();
        old.forward_operations_to(&next);
        drop(old_receiver);
        drop(next);
        assert!(matches!(next_receiver.try_recv(), Err(TryRecvError::Empty)));
        old.send(TimelineUpdate::AttachmentDownloadReset(
            "mxc://example.org/file".into(),
        ))
        .unwrap();
        assert!(next_receiver.try_recv().is_ok());
        drop(old);
        assert!(matches!(
            next_receiver.try_recv(),
            Err(TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn concurrent_old_producer_delivers_each_outcome_once_during_replacement() {
        let (original, receiver) = channel();
        let worker = original.clone();
        let producer = std::thread::spawn(move || {
            for _ in 0..2000 {
                worker
                    .send(TimelineUpdate::AttachmentDownloadReset(
                        "mxc://example.org/file".into(),
                    ))
                    .unwrap();
                std::thread::yield_now();
            }
        });
        let mut current = original;
        let mut receiver = receiver;
        let mut delivered = 0;
        for _ in 0..2000 {
            while receiver.try_recv().is_ok() {
                delivered += 1;
            }
            let (next, next_receiver) = channel();
            current.forward_operations_to(&next);
            drop(current);
            drop(receiver);
            current = next;
            receiver = next_receiver;
        }
        producer.join().unwrap();
        while receiver.try_recv().is_ok() {
            delivered += 1;
        }
        assert_eq!(delivered, 2000);
    }

    #[test]
    fn queued_message_survives_history_window_replacement_in_order() {
        let (old, old_receiver) = channel();
        old.send(TimelineUpdate::AttachmentDownloadReset(
            "mxc://example.org/test".into(),
        ))
        .unwrap();
        let (next, next_receiver) = channel();
        old.forward_operations_to(&next);
        drop(old_receiver);
        next.send(snapshot(false)).unwrap();
        old.send(TimelineUpdate::AttachmentDownloadReset(
            "mxc://example.org/test".into(),
        ))
        .unwrap();
        assert!(matches!(
            next_receiver.try_recv(),
            Ok(TimelineUpdate::AttachmentDownloadReset(_))
        ));
        assert!(matches!(
            next_receiver.try_recv(),
            Ok(TimelineUpdate::NewItems { .. })
        ));
        assert!(matches!(
            next_receiver.try_recv(),
            Ok(TimelineUpdate::AttachmentDownloadReset(_))
        ));
        assert!(next_receiver.is_empty());
    }

    #[test]
    fn progress_coalescing_keeps_the_progress_only_fast_path() {
        let (tx, rx) = channel();
        for _ in 0..1000 {
            tx.send(TimelineUpdate::LocalEchoProgress {
                new_items: Vector::new(),
            })
            .unwrap();
        }
        assert_eq!(rx.len(), 1);
        assert!(matches!(
            rx.try_recv(),
            Ok(TimelineUpdate::LocalEchoProgress { .. })
        ));
    }
}
