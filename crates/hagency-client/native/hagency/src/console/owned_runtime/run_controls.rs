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

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    original: Card,
    card: Card,
    event: Option<String>,
    published: Option<u64>,
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
        .take(48 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() > 48 * 1024 {
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
/// the caller proves its own current dispatch/lease before claiming an action.
pub(in crate::console) struct CardHost {
    initial: Card,
    text: hagency_runtime::live_text::LiveText,
    text_seen: StdMutex<(u64, Instant)>,
    state: StdMutex<Option<(Arc<e2ee::AgentDevice>, PathBuf, Record)>>,
}
impl CardHost {
    pub(in crate::console) fn new(card: Card) -> Self {
        Self {
            initial: card,
            text: Default::default(),
            text_seen: StdMutex::new((0, Instant::now())),
            state: StdMutex::new(None),
        }
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
        let mut held = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((device, path, record)) = held.as_mut() {
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
                record
                    .card
                    .receive(&event, &received.sender, &received.content)
                    .map(|action| (received, action))
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
        let mut held = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (_, path, record) = held.as_mut().ok_or(())?;
        if !record.card.state.active() {
            return Ok(());
        }
        if stop && outcome == "accepted" {
            record.card.state = CardState::Stopping;
        }
        record.card.last_action = Some(Receipt {
            event_id: event.into(),
            sender: sender.into(),
            outcome: outcome.into(),
        });
        record.card.revision += 1;
        save(path, record)
    }
}
impl Drop for CardHost {
    fn drop(&mut self) {
        self.finish(CardState::Unknown);
    }
}

pub(super) struct Controller {
    sender: native::Sender,
    card: CardHost,
}
impl Controller {
    pub(super) fn new(sender: native::Sender, card: Card) -> Self {
        Self {
            sender,
            card: CardHost::new(card),
        }
    }
    pub(super) fn finish(&self, state: CardState) {
        self.card.finish(state);
    }
    pub(super) fn live_text(&self) -> hagency_runtime::live_text::LiveText {
        self.card.live_text()
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
        while !self.sender.is_active() {
            tokio::select! { _ = canceled(&mut cancel) => return, _ = tick.tick() => {} }
        }
        let device = match tokio::time::timeout(
            Duration::from_secs(20),
            e2ee::device(transport, lease, config, &mut cancel),
        )
        .await
        {
            Ok(Ok(device)) => device,
            _ => return,
        };
        if self.card.open(device).is_err() {
            return;
        }
        let mut receipts = Vec::<(
            String,
            String,
            bool,
            tokio::sync::oneshot::Receiver<NativeOutcome>,
        )>::new();
        loop {
            tokio::select! { _ = canceled(&mut cancel) => return, _ = tick.tick() => {} }
            for index in (0..receipts.len()).rev() {
                let outcome = match receipts[index].3.try_recv() {
                    Ok(value) => value,
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => continue,
                    Err(_) => NativeOutcome::Unknown,
                };
                let (event, sender, stop, _) = receipts.remove(index);
                if self
                    .card
                    .receipt(&event, &sender, outcome_name(outcome), stop)
                    .is_err()
                {
                    return;
                }
            }
            let _ = self.card.publish().await;
            if receipts.len() >= 16 {
                continue;
            }
            for (received, action) in self.card.actions().await.unwrap_or_default() {
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
                if !self.card.claim(&received.event_id) {
                    continue;
                }
                let (action, stop) = match action {
                    Action::Steer { text } => (native::Action::Steer { text }, false),
                    Action::Interrupt {} => (native::Action::Interrupt, true),
                    Action::Queue { .. } => {
                        let _ = self.card.receipt(
                            &received.event_id,
                            &received.sender,
                            "unsupported",
                            false,
                        );
                        continue;
                    }
                };
                match self.sender.submit(action) {
                    Ok(submission) => receipts.push((
                        received.event_id,
                        received.sender,
                        stop,
                        submission.receipt,
                    )),
                    Err(error) => {
                        let _ = self.card.receipt(
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
        for attempt in 0..6 {
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
                    if save(&path, &record).is_err() {
                        continue;
                    }
                }
                if publish(&device, &mut record).await.is_ok() {
                    forget(&path);
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
    fn durable_action_claim_is_unique_across_reopening_and_concurrent_delivery() {
        let directory = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
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
        let directory = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = directory.path().join("cards/run.json");
        let original = crate::agent_run_controls::tests::card();
        let mut record = Record {
            card: original.clone(),
            original,
            event: Some("$card".into()),
            published: Some(0),
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
