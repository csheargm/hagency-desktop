//! Authenticated Matrix controls for the currently admitted personal turn.
//! Native acknowledgements are not completion evidence. The journal claims
//! each Matrix event before a native write, and recovery only closes cards.
use super::*;
use crate::agent_run_controls::{Action, Card, Receipt, State as CardState};
use hagency_agent_local::run_controls::{self as native, Outcome as NativeOutcome};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::Path,
    sync::{LazyLock, Mutex as StdMutex},
};

static ACTIVE: LazyLock<StdMutex<BTreeSet<PathBuf>>> = LazyLock::new(Default::default);
static FLUSHING: LazyLock<StdMutex<BTreeSet<PathBuf>>> = LazyLock::new(Default::default);
const IO_WAIT: Duration = Duration::from_secs(10);
const RECORD_BYTES: u64 = 64 * 1024;
// The UI's send is bounded at 30 seconds. Continue receiving after closing so
// an operation sent during the last native poll gets an exact outcome too.
// This background grace never retains the native process or its work slot.
const CLOSED_RECEIPT_GRACE_MS: i64 = 60_000;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    original: Card,
    card: Card,
    event: Option<String>,
    published: Option<u64>,
    closed_at_ms: Option<i64>,
}
fn directory(device: &e2ee::AgentDevice) -> PathBuf {
    device.run_records.with_extension("d")
}
fn path(device: &e2ee::AgentDevice, card: &Card) -> PathBuf {
    directory(device).join(format!("{:x}.json", Sha256::digest(card.id.as_bytes())))
}
fn save(path: &Path, record: &Record) -> Result<(), ()> {
    let bytes = serde_json::to_vec(record).map_err(|_| ())?;
    hagency_store::private::directory(path.parent().ok_or(())?).map_err(|_| ())?;
    hagency_store::private::replace(path, &bytes).map_err(|_| ())
}
fn read(path: &Path) -> Result<Record, ()> {
    let mut bytes = Vec::new();
    hagency_store::private::open(path, false)
        .map_err(|_| ())?
        .take(RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() as u64 > RECORD_BYTES {
        return Err(());
    }
    let record: Record = serde_json::from_slice(&bytes).map_err(|_| ())?;
    record.original.validate().map_err(|_| ())?;
    record.card.validate().map_err(|_| ())?;
    if !record.original.same_identity(&record.card)
        || record.original.revision != 0
        || record.original.state != CardState::Running
        || record.published.is_some_and(|r| r > record.card.revision)
        || record
            .event
            .as_ref()
            .is_some_and(|event| !event.starts_with('$') || event.len() > 1024)
    {
        return Err(());
    }
    Ok(record)
}
fn claim(path: &Path, event: &str) -> bool {
    let directory = path.with_extension("actions");
    if hagency_store::private::directory(&directory).is_err() {
        return false;
    }
    let name = format!("{:x}", Sha256::digest(event.as_bytes()));
    // write_new also syncs before returning. A crash may lose an action, but
    // can never cause an uncertain native write to be replayed.
    hagency_store::private::write_new(&directory.join(name), b"claimed\n").is_ok()
}
fn forget(path: &Path) {
    if std::fs::remove_file(path).is_ok() {
        let directory = path.with_extension("actions");
        if let Ok(entries) = std::fs::read_dir(&directory) {
            for entry in entries.flatten() {
                if entry.file_name().to_str().is_some_and(|name| {
                    name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit())
                }) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        let _ = std::fs::remove_dir(directory);
    }
}
async fn publish(device: &e2ee::AgentDevice, record: &mut Record) -> Result<(), ()> {
    if device.is_revoked() {
        return Err(());
    }
    if record.event.is_none() {
        let event = tokio::time::timeout(
            IO_WAIT,
            device.send_room_message(
                &record.card.room_id,
                &format!("{}_open", record.card.id),
                record.original.content(),
            ),
        )
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
        record.event = Some(event);
        record.published = Some(0);
    }
    if record.published != Some(record.card.revision) {
        let content = record
            .card
            .edit_content(record.event.as_deref().ok_or(())?)
            .map_err(|_| ())?;
        tokio::time::timeout(
            IO_WAIT,
            device.send_room_message(
                &record.card.room_id,
                &format!("{}_{}", record.card.id, record.card.revision),
                content,
            ),
        )
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
        record.published = Some(record.card.revision);
    }
    Ok(())
}

/// Shared by personal and pooled hosts. Holds presentation state only;
/// the caller proves its own current dispatch/lease before native submission.
pub(in crate::console) struct CardHost {
    initial: Card,
    text: hagency_runtime::live_text::LiveText,
    messages: Option<Arc<super::messages::Outbox>>,
    text_seen: StdMutex<(u64, Instant)>,
    state: StdMutex<Option<(Arc<e2ee::AgentDevice>, PathBuf, Record)>>,
    receipts: StdMutex<Vec<PendingReceipt>>,
}
struct PendingReceipt {
    event: String,
    sender: String,
    stop: bool,
    poll: Box<dyn FnMut() -> Option<&'static str> + Send>,
}
impl CardHost {
    pub(in crate::console) fn new(card: Card) -> Self {
        Self {
            initial: card,
            text: Default::default(),
            messages: None,
            text_seen: StdMutex::new((0, Instant::now())),
            state: StdMutex::new(None),
            receipts: StdMutex::new(Vec::new()),
        }
    }
    pub(in crate::console) fn with_messages(card: Card, root: &Path) -> Result<Self, ()> {
        let messages = Arc::new(super::messages::Outbox::open(root, card.clone())?);
        let mut host = Self::new(card);
        host.text = hagency_runtime::live_text::LiveText::with_messages(messages.clone());
        host.messages = Some(messages);
        Ok(host)
    }
    pub(in crate::console) fn messages(&self) -> Option<Arc<super::messages::Outbox>> {
        self.messages.clone()
    }
    pub(in crate::console) fn live_text(&self) -> hagency_runtime::live_text::LiveText {
        self.text.clone()
    }
    fn refresh_text(&self) {
        let mut seen = self.text_seen.lock().unwrap_or_else(|e| e.into_inner());
        if seen.0 != 0 && seen.1.elapsed() < Duration::from_millis(1500) {
            return;
        }
        let Some((revision, text)) = self.text.since(seen.0) else {
            return;
        };
        let mut held = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, _, record)) = held.as_mut()
            && record.card.state.active()
            && record.card.preview != text
        {
            record.card.preview = text;
            record.card.fit_display();
            record.card.revision += 1;
        }
        *seen = (revision, Instant::now());
    }
    pub(in crate::console) fn open(&self, device: Arc<e2ee::AgentDevice>) -> Result<(), ()> {
        let path = path(&device, &self.initial);
        let record = Record {
            original: self.initial.clone(),
            card: self.initial.clone(),
            event: None,
            published: None,
            closed_at_ms: None,
        };
        let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        if path.exists() || !active.insert(path.clone()) {
            return Err(());
        }
        if save(&path, &record).is_err() {
            active.remove(&path);
            return Err(());
        }
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = Some((device, path, record));
        Ok(())
    }
    pub(in crate::console) fn finish(&self, state: CardState) {
        // The runtime future/stream must be dropped before this call. Its
        // unsent actions yield Unavailable; uncertain writes yield Unknown.
        // Keep their receivers outside serve(), which select may have dropped
        // in the very same poll that the provider completed.
        self.drain_receipts(true);
        let mut held = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((device, path, record)) = held.as_mut() {
            record.closed_at_ms.get_or_insert_with(wall_ms);
            if record.card.state.active() {
                record.card.state = state;
                record.card.revision += 1;
                if state == CardState::Completed {
                    record.card.preview.clear();
                }
                if save(path, record).is_err() {
                    tracing::warn!("Run card outcome awaits recovery");
                }
            }
            ACTIVE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(path);
            device_ready(device.clone());
        }
    }
    pub(in crate::console) async fn publish(&self) -> Result<(), ()> {
        self.refresh_text();
        let (device, path, mut record) = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or(())?;
        if record.event.is_some() && record.published == Some(record.card.revision) {
            return Ok(());
        }
        // Persist revision before attempting its immutable transaction.
        save(&path, &record)?;
        let sent = publish(&device, &mut record).await;
        let mut held = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, _, current)) = held.as_mut() {
            current.event = record.event;
            current.published = record.published;
            save(&path, current)?;
        }
        sent
    }
    pub(in crate::console) async fn actions(
        &self,
    ) -> Result<Vec<(crate::agent_questions::Received, Action)>, ()> {
        let (device, _, record) = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or(())?;
        let Some(event) = record.event else {
            return Ok(Vec::new());
        };
        if device.is_revoked() || record.card.state != CardState::Running {
            return Ok(Vec::new());
        }
        let actions =
            tokio::time::timeout(IO_WAIT, device.run_actions(&record.card.room_id, &event))
                .await
                .map_err(|_| ())?
                .map_err(|_| ())?;
        Ok(actions
            .into_iter()
            .filter_map(|received| {
                let action = record
                    .card
                    .receive(&event, &received.sender, &received.content)?;
                if !self.claim(&received.event_id) {
                    return None;
                }
                // Retain each authenticated, unseen event before another
                // authority await. Cancellation in that await made no native
                // write, and finish must be able to say so for the whole batch.
                self.stage(
                    &received.event_id,
                    &received.sender,
                    matches!(&action, Action::Interrupt {}),
                );
                Some((received, action))
            })
            .collect())
    }
    pub(in crate::console) fn claim(&self, event: &str) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|(_, path, record)| {
                record.card.state == CardState::Running && claim(path, event)
            })
    }
    pub(in crate::console) fn receipt(
        &self,
        event: &str,
        sender: &str,
        outcome: &str,
        stop: bool,
    ) -> Result<(), ()> {
        self.receipts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|r| r.event != event);
        let mut held = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (_, path, record) = held.as_mut().ok_or(())?;
        if stop && outcome == "accepted" && record.card.state.active() {
            record.card.state = CardState::Stopping;
        }
        record.card.record_receipt(Receipt {
            event_id: event.into(),
            sender: sender.into(),
            outcome: outcome.into(),
        });
        record.card.revision += 1;
        save(path, record)
    }
    pub(in crate::console) fn track<T: Send + 'static>(
        &self,
        event: String,
        sender: String,
        stop: bool,
        mut receiver: tokio::sync::oneshot::Receiver<T>,
        name: fn(T) -> &'static str,
    ) {
        let mut pending = self.receipts.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|r| r.event != event);
        pending.push(PendingReceipt {
            event,
            sender,
            stop,
            poll: Box::new(move || match receiver.try_recv() {
                Ok(value) => Some(name(value)),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
                Err(_) => Some("unknown"),
            }),
        });
    }
    fn stage(&self, event: &str, sender: &str, stop: bool) {
        self.receipts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(PendingReceipt {
                event: event.into(),
                sender: sender.into(),
                stop,
                poll: Box::new(|| Some("unavailable")),
            });
    }
    pub(in crate::console) fn pending_receipts(&self) -> usize {
        self.receipts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }
    pub(in crate::console) fn drain_receipts(&self, closing: bool) {
        for (receipt, stop) in self.ready_receipts(closing) {
            if self
                .receipt(&receipt.event_id, &receipt.sender, &receipt.outcome, stop)
                .is_err()
            {
                tracing::warn!("Run control receipt could not be persisted");
            }
        }
    }
    fn ready_receipts(&self, closing: bool) -> Vec<(Receipt, bool)> {
        let mut ready = Vec::new();
        let mut receipts = self.receipts.lock().unwrap_or_else(|e| e.into_inner());
        for index in (0..receipts.len()).rev() {
            let outcome = (receipts[index].poll)().or(closing.then_some("unknown"));
            let Some(outcome) = outcome else {
                continue;
            };
            let pending = receipts.remove(index);
            ready.push((
                Receipt {
                    event_id: pending.event,
                    sender: pending.sender,
                    outcome: outcome.into(),
                },
                pending.stop,
            ));
        }
        ready.reverse();
        ready
    }
}
impl Drop for CardHost {
    fn drop(&mut self) {
        self.finish(CardState::Unknown);
    }
}

pub(super) struct Controller {
    sender: Option<native::Sender>,
    card: Option<CardHost>,
    messages: Arc<super::messages::Outbox>,
    text: hagency_runtime::live_text::LiveText,
}
/// Pauses between attempts to open the Agent device for a run card: 20 s,
/// 60 s, 180 s, then every 6 minutes, which stays under the homeserver's
/// refill of about one login per 5.5 minutes.
pub(in crate::console) const DEVICE_RETRY_FIRST: Duration = Duration::from_secs(20);
pub(in crate::console) const DEVICE_RETRY_MAX: Duration = Duration::from_secs(360);
pub(super) fn optional_card(
    dispatch: &inbox::Dispatch,
    execution: &str,
    owner: &str,
    runtime: &str,
) -> Option<Card> {
    match Card::new(dispatch, execution, owner, runtime) {
        Ok(card) => Some(card),
        Err(code) => {
            tracing::warn!(code, "Run continues without its optional control card");
            None
        }
    }
}
impl Controller {
    pub(super) fn new(sender: native::Sender, card: Card, root: &Path) -> Result<Self, ()> {
        let card = CardHost::with_messages(card, root)?;
        Ok(Self {
            sender: Some(sender),
            messages: card.messages().ok_or(())?,
            text: card.live_text(),
            card: Some(card),
        })
    }
    pub(super) fn messages_only(route: super::messages::Route, root: &Path) -> Result<Self, ()> {
        let messages = Arc::new(super::messages::Outbox::open_route(root, route)?);
        let text = hagency_runtime::live_text::LiveText::with_messages(messages.clone());
        Ok(Self { sender: None, card: None, messages, text })
    }
    pub(super) fn finish(&self, state: CardState) {
        if let Some(card) = &self.card { card.finish(state); }
    }
    pub(super) fn live_text(&self) -> hagency_runtime::live_text::LiveText {
        self.text.clone()
    }
    pub(super) async fn serve<T: Transport>(
        &self,
        transport: &T,
        lease: &LeaseRef,
        config: &StartConfig,
        prepared: &inbox::Prepared,
        mut cancel: watch::Receiver<bool>,
    ) {
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        while self.sender.as_ref().is_some_and(|sender| !sender.is_active()) {
            tokio::select! { _ = canceled(&mut cancel) => return, _ = tick.tick() => {} }
        }
        // The card needs the Agent's device. One that is not ready yet is
        // asked for again while the turn runs, so a long turn still gets its
        // Stop. A new grant is a Matrix login and the homeserver limits
        // those, so the pauses grow.
        let mut pause = DEVICE_RETRY_FIRST;
        let device = loop {
            match tokio::time::timeout(
                Duration::from_secs(20),
                e2ee::device(transport, lease, config, &mut cancel),
            )
            .await
            {
                Ok(Ok(device)) => break device,
                Ok(Err(RuntimeError::Stopped)) => return,
                _ => {}
            }
            tokio::select! { _ = canceled(&mut cancel) => return, _ = tokio::time::sleep(pause) => {} }
            pause = (pause * 3).min(DEVICE_RETRY_MAX);
        };
        // A damaged optional control card must not discard the independent
        // message outbox; its own scope and authorization are checked below.
        if let Some(card) = &self.card { let _ = card.open(device.clone()); }
        loop {
            tokio::select! { _ = canceled(&mut cancel) => return, _ = tick.tick() => {} }
            if let Some(card) = &self.card {
                card.drain_receipts(false);
                let _ = card.publish().await;
            }
            if self.messages.next().ok().flatten().is_some()
                && fresh_running(transport, lease, &prepared.dispatch, &prepared.execution_id, &mut cancel).await
            {
                let _ = self.messages.send_next(&device, prepared.dispatch.body.is_empty()).await;
            }
            let (Some(card), Some(sender)) = (&self.card, &self.sender) else { continue; };
            if card.pending_receipts() >= 16 {
                continue;
            }
            for (received, action) in card.actions().await.unwrap_or_default() {
                if !fresh_running(
                    transport,
                    lease,
                    &prepared.dispatch,
                    &prepared.execution_id,
                    &mut cancel,
                )
                .await
                {
                    return;
                }
                let (action, stop) = match action {
                    Action::Steer { text } => (native::Action::Steer { text }, false),
                    Action::Interrupt {} => (native::Action::Interrupt, true),
                    Action::Queue { .. } => {
                        let _ = card.receipt(
                            &received.event_id,
                            &received.sender,
                            "unsupported",
                            false,
                        );
                        continue;
                    }
                };
                match sender.submit(action) {
                    Ok(submission) => card.track(
                        received.event_id,
                        received.sender,
                        stop,
                        submission.receipt,
                        outcome_name,
                    ),
                    Err(error) => {
                        let _ = card.receipt(
                            &received.event_id,
                            &received.sender,
                            if error == native::SubmitError::Busy {
                                "busy"
                            } else {
                                "unavailable"
                            },
                            stop,
                        );
                    }
                }
            }
        }
    }
    pub(super) async fn flush_messages<T: Transport>(
        &self, transport: &T, lease: &LeaseRef, config: &StartConfig,
        prepared: &inbox::Prepared, cancel: &mut watch::Receiver<bool>,
    ) -> Result<(), ()> {
        let messages = &self.messages;
        if messages.next()?.is_none() { return Ok(()); }
        let device = e2ee::device(transport, lease, config, cancel).await.map_err(|_| ())?;
        while messages.next()?.is_some() {
            if !fresh_running(transport, lease, &prepared.dispatch, &prepared.execution_id, cancel).await {
                return Err(());
            }
            messages.send_next(&device, prepared.dispatch.body.is_empty()).await?;
        }
        Ok(())
    }
}
fn outcome_name(outcome: NativeOutcome) -> &'static str {
    match outcome {
        NativeOutcome::Accepted => "accepted",
        NativeOutcome::Rejected => "rejected",
        NativeOutcome::Unsupported => "unsupported",
        NativeOutcome::InProgress => "in_progress",
        NativeOutcome::Busy => "busy",
        NativeOutcome::Unavailable => "unavailable",
        NativeOutcome::Unknown => "unknown",
    }
}

/// The caller supplies only device-authenticated, current-member events. Use
/// the original running card only to validate the old action's exact binding;
/// no closed card can acquire native authority again.
fn settle_closed_action(
    path: &Path,
    record: &mut Record,
    event: &str,
    sender: &str,
    content: &serde_json::Value,
) -> bool {
    let Some(card_event) = record.event.as_deref() else {
        return false;
    };
    if record.card.state.active()
        || record
            .original
            .receive(card_event, sender, content)
            .is_none()
        || record
            .card
            .action_receipts
            .iter()
            .chain(record.card.last_action.iter())
            .any(|receipt| receipt.event_id == event && receipt.sender == sender)
    {
        return false;
    }
    // A successful first durable claim proves that no host claimed this event
    // for a native write. An existing claim (or IO uncertainty) proves no such
    // thing, even after a restart, so it must remain unknown.
    let outcome = if claim(path, event) {
        "unavailable"
    } else {
        "unknown"
    };
    record.card.record_receipt(Receipt {
        event_id: event.into(),
        sender: sender.into(),
        outcome: outcome.into(),
    });
    record.card.revision += 1;
    true
}

async fn close_actions(
    device: &e2ee::AgentDevice,
    path: &Path,
    record: &mut Record,
) -> Result<bool, ()> {
    let Some(event) = record.event.clone() else {
        return Ok(true);
    };
    // Drain the bounded inbox, including an action requeued by a cancelled
    // membership lookup. New arrivals wait for the next grace-period pass.
    for _ in 0..16 {
        let actions =
            tokio::time::timeout(IO_WAIT, device.run_actions(&record.card.room_id, &event))
                .await
                .map_err(|_| ())?
                .map_err(|_| ())?;
        let full = actions.len() == 4;
        let mut changed = false;
        for received in actions {
            changed |= settle_closed_action(
                path,
                record,
                &received.event_id,
                &received.sender,
                &received.content,
            );
        }
        if changed {
            save(path, record)?;
        }
        if !full {
            break;
        }
    }
    Ok(!device.run_actions_pending(&record.card.room_id, &event))
}

fn may_forget_closed_record(record: &Record, drained: bool, at: i64) -> bool {
    drained
        && !record.card.state.active()
        && at.saturating_sub(record.closed_at_ms.unwrap_or(i64::MAX)) >= CLOSED_RECEIPT_GRACE_MS
}

/// Recover interrupted hosts without replaying any action. Retry original
/// card/edit transactions, including when the previous HTTP reply was lost.
pub(in crate::console) fn device_ready(device: Arc<e2ee::AgentDevice>) {
    let directory = directory(&device);
    if !FLUSHING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(directory.clone())
    {
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        FLUSHING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&directory);
        return;
    };
    runtime.spawn(async move {
        struct Guard(PathBuf);
        impl Drop for Guard {
            fn drop(&mut self) {
                FLUSHING
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&self.0);
            }
        }
        let _guard = Guard(directory.clone());
        // Keep already received but unchecked events recoverable beyond the
        // arrival grace. Network/membership failures are retried with capped
        // backoff; no native process or admission slot remains attached here.
        for attempt in 0_u64.. {
            if device.is_revoked() {
                return;
            }
            let paths: Vec<_> = std::fs::read_dir(&directory)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|e| e == "json"))
                .collect();
            let mut pending = false;
            for path in paths {
                if ACTIVE
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains(&path)
                {
                    continue;
                }
                let Ok(mut record) = read(&path) else {
                    continue;
                };
                if record.card.state.active() {
                    record.card.state = CardState::Unknown;
                    record.card.revision += 1;
                }
                record.closed_at_ms.get_or_insert_with(wall_ms);
                if save(&path, &record).is_err() {
                    pending = true;
                    continue;
                }
                let actions = close_actions(&device, &path, &mut record).await;
                if publish(&device, &mut record).await.is_ok() {
                    if may_forget_closed_record(&record, actions == Ok(true), wall_ms()) {
                        forget(&path);
                    } else {
                        let _ = save(&path, &record);
                        pending = true;
                    }
                } else {
                    let _ = save(&path, &record);
                    pending = true;
                }
            }
            if !pending {
                return;
            }
            tokio::time::sleep(Duration::from_secs(1 << attempt.min(4))).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_card_receipts_distinguish_unseen_actions_from_uncertain_native_claims() {
        let directory =
            hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = directory.path().join("cards/late.json");
        let original = crate::agent_run_controls::tests::card();
        let sender = original.requester.clone();
        let content = original
            .action_content(
                "$card",
                &sender,
                Action::Steer {
                    text: "late draft".into(),
                },
            )
            .unwrap();
        let mut record = Record {
            card: original.clone(),
            original,
            event: Some("$card".into()),
            published: Some(1),
            closed_at_ms: Some(wall_ms()),
        };
        record.card.state = CardState::Completed;
        record.card.revision = 1;
        save(&path, &record).unwrap();
        assert!(claim(&path, "$uncertain"));
        assert!(settle_closed_action(
            &path,
            &mut record,
            "$late",
            &sender,
            &content
        ));
        assert_eq!(
            record.card.last_action.as_ref().unwrap().outcome,
            "unavailable"
        );
        assert!(settle_closed_action(
            &path,
            &mut record,
            "$uncertain",
            &sender,
            &content
        ));
        assert_eq!(record.card.last_action.as_ref().unwrap().outcome, "unknown");
        let revision = record.card.revision;
        assert!(!settle_closed_action(
            &path,
            &mut record,
            "$late",
            &sender,
            &content
        ));
        let mut forged = content.clone();
        forged["executionId"] = serde_json::json!("other-execution");
        assert!(!settle_closed_action(
            &path,
            &mut record,
            "$other",
            &sender,
            &forged
        ));
        assert!(!settle_closed_action(
            &path,
            &mut record,
            "$other",
            "@stranger:test",
            &content
        ));
        assert_eq!(record.card.revision, revision);
        assert_eq!(record.card.state, CardState::Completed);
        assert!(record.original.same_identity(&record.card));
        save(&path, &record).unwrap();
        let recovered = read(&path).unwrap();
        assert_eq!(recovered.card.action_receipts.len(), 2);
        assert_eq!(recovered.closed_at_ms, record.closed_at_ms);
    }

    #[test]
    fn expired_arrival_grace_keeps_unchecked_actions_until_membership_recovers() {
        let directory =
            hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = directory.path().join("cards/pending-membership.json");
        let original = crate::agent_run_controls::tests::card();
        let sender = original.requester.clone();
        let content = original
            .action_content(
                "$card",
                &sender,
                Action::Steer {
                    text: "still waiting for membership".into(),
                },
            )
            .unwrap();
        let mut record = Record {
            card: original.clone(),
            original,
            event: Some("$card".into()),
            published: Some(1),
            closed_at_ms: Some(1),
        };
        record.card.state = CardState::Completed;
        record.card.revision = 1;
        save(&path, &record).unwrap();
        let later = CLOSED_RECEIPT_GRACE_MS + 100_000;
        // run_actions can return no checked actions while its cancelled or
        // failed membership checks are requeued. That is not a drained inbox.
        assert!(!may_forget_closed_record(&record, false, later));
        record = read(&path).unwrap();
        assert!(settle_closed_action(
            &path,
            &mut record,
            "$delayed",
            &sender,
            &content
        ));
        assert_eq!(
            record.card.last_action.as_ref().unwrap().outcome,
            "unavailable"
        );
        assert!(may_forget_closed_record(&record, true, later));
    }
    #[test]
    fn ending_during_authority_recheck_settles_every_staged_unwritten_action() {
        let card = crate::agent_run_controls::tests::card();
        let host = CardHost::new(card.clone());
        host.stage("$first", &card.requester, false);
        host.stage("$second", &card.requester, true);
        let (send, receive) = tokio::sync::oneshot::channel();
        host.track(
            "$first".into(),
            card.requester,
            false,
            receive,
            outcome_name,
        );
        send.send(NativeOutcome::Accepted).unwrap();
        let results = host.ready_receipts(true);
        assert_eq!(results.len(), 2);
        assert!(
            results
                .iter()
                .any(|(r, _)| r.event_id == "$first" && r.outcome == "accepted")
        );
        assert!(
            results
                .iter()
                .any(|(r, stop)| r.event_id == "$second" && r.outcome == "unavailable" && *stop)
        );
    }
    #[tokio::test]
    async fn completion_drains_every_unsent_control_without_claiming_uncertain_writes_failed() {
        let mut card = crate::agent_run_controls::tests::card();
        let scope = hagency_agent_local::Scope {
            agent: "agent".into(),
            binding: "binding".into(),
            room: card.room_id.clone(),
            requester: card.requester.clone(),
            thread: card.thread_root.clone(),
        };
        let (sender, inbox) = native::channel(&scope, "dispatch", "execution").unwrap();
        let mut stream = inbox.claim(&scope, "dispatch", "execution").unwrap();
        stream.activate();
        let host = CardHost::new(card.clone());
        for n in 0..3 {
            let receipt = sender
                .submit(native::Action::Steer {
                    text: format!("append {n}"),
                })
                .unwrap();
            host.track(
                format!("$append-{n}"),
                card.requester.clone(),
                false,
                receipt.receipt,
                outcome_name,
            );
        }
        let (uncertain, receiver) = tokio::sync::oneshot::channel::<NativeOutcome>();
        host.track(
            "$already-writing".into(),
            card.requester.clone(),
            false,
            receiver,
            outcome_name,
        );
        assert!(host.ready_receipts(false).is_empty());
        // The model completed without claiming these three pending writes.
        drop(stream);
        let outcomes = host.ready_receipts(true);
        assert_eq!(outcomes.len(), 4);
        for (receipt, _) in outcomes {
            card.record_receipt(receipt);
        }
        assert_eq!(
            card.action_receipts
                .iter()
                .filter(|r| r.outcome == "unavailable")
                .count(),
            3
        );
        assert_eq!(card.action_receipts.last().unwrap().outcome, "unknown");
        assert!(uncertain.is_closed());
        card.state = CardState::Completed;
        card.validate().unwrap();
        let restored = Card::from_content(&card.content()).unwrap();
        assert_eq!(
            restored.action_receipts.len(),
            4,
            "one card edit retains every raced acknowledgement"
        );
        assert_eq!(host.pending_receipts(), 0);
    }
    #[test]
    fn invalid_optional_card_does_not_require_a_runtime_control_channel() {
        let mut dispatch: inbox::Dispatch = serde_json::from_value(serde_json::json!({
            "id":"dispatch","bindingId":"binding","agentId":"agent", "eventId":"$trigger", "roomId":"!room:test",
            "requesterMxid":"@owner:test","threadRoot":"!room:test","body":"original answer wanted", "state":"running",
            "bindingGeneration":1,"dispatchEpoch":1,"dispatchDeviceId":"device","executionId":"execution","outcome":null
        })).unwrap();
        assert!(optional_card(&dispatch, "execution", "@owner:test", "claude").is_some());
        dispatch.thread_root = "unsupported presentation root".into();
        assert!(optional_card(&dispatch, "execution", "@owner:test", "claude").is_none());
        assert_eq!(dispatch.body, "original answer wanted");
    }
    #[test]
    fn agent_requester_has_message_delivery_without_control_authority() {
        let dispatch: inbox::Dispatch = serde_json::from_value(serde_json::json!({
            "id":"dispatch","bindingId":"binding","agentId":"agent", "eventId":"$trigger", "roomId":"!room:test",
            "requesterMxid":"@_hagency_agt_test:test","threadRoot":"$thread:test","body":"question", "state":"running",
            "bindingGeneration":1,"dispatchEpoch":1,"dispatchDeviceId":"device","executionId":"execution","outcome":null
        })).unwrap();
        assert!(optional_card(&dispatch, "execution", "@owner:test", "claude").is_none());
        let directory = tempfile::tempdir().unwrap();
        let route = super::super::messages::Route::personal(&dispatch, "execution").unwrap();
        let controller = Controller::messages_only(route, directory.path()).unwrap();
        assert!(controller.card.is_none());
        assert!(controller.sender.is_none());
        assert!(controller.live_text().message("claude:one", "progress").unwrap());
        assert_eq!(controller.messages.next().unwrap().unwrap().text, "progress");
        controller.finish(CardState::Completed);
    }
    #[test]
    fn durable_action_claim_is_unique_across_reopening_and_concurrent_delivery() {
        let directory =
            hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = directory.path().join("run.json");
        let accepted = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| claim(&path, "$same-event")))
                .collect();
            workers
                .into_iter()
                .filter_map(|worker| worker.join().ok())
                .filter(|accepted| *accepted)
                .count()
        });
        assert_eq!(accepted, 1);
        assert!(!claim(&path, "$same-event"));
        assert!(claim(&path, "$new-explicit-action"));
    }
    #[test]
    fn recovery_keeps_original_identity_revision_and_partial_output() {
        let directory =
            hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = directory.path().join("cards/run.json");
        let original = crate::agent_run_controls::tests::card();
        let mut record = Record {
            card: original.clone(),
            original,
            event: Some("$card".into()),
            published: Some(0),
            closed_at_ms: None,
        };
        record.card.state = CardState::Unknown;
        record.card.revision = 1;
        record.card.preview = "partial response".into();
        save(&path, &record).unwrap();
        let recovered = read(&path).unwrap();
        assert_eq!(recovered.card.state, CardState::Unknown);
        assert_eq!(recovered.card.preview, "partial response");
        assert_eq!(recovered.original.revision, 0);
        record.card.owner = "@substituted:test".into();
        save(&path, &record).unwrap();
        assert!(read(&path).is_err());
        record.card = record.original.clone();
        record.published = Some(100);
        save(&path, &record).unwrap();
        assert!(read(&path).is_err());
    }
}
