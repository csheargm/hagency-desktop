//! Common Matrix question lifecycle for native provider callbacks. Each
//! request becomes one card in the run's thread, drawn under the message that
//! started the run. The host names its permitted participants. All questions of one
//! run share one wait of `ANSWER_WAIT_MS`, counted from each ask, so the
//! messages queued behind the run wait at most that long for answers, opening
//! the Agent's device and posting included; the activity lease is renewed as
//! usual meanwhile (the wait itself never extends it). Without an answer the
//! model decides on its own. Answers are room events, never chat messages, so
//! an answer never queues behind the run.
//!
//! A card's outcome is shown by an edit that outlives the run: outcomes not
//! yet shown wait in a per-Agent outbox, retried whenever that Agent's device
//! is in use again, and every posted card is recorded (identifiers only) so a
//! restarted executor still closes it. Question and answer text are never
//! logged or written to disk.
use super::*;
use crate::agent_questions::{self as protocol, Card, CardState};
use crate::console::owned_runtime::e2ee::AgentDevice;
use hagency_runtime::{
    codex::{
        RequestId,
        session::{UserInputRequest, UserInputResponse},
    },
    owned::OwnedSession,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    io::Read,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

/// Bound simultaneous cards, not how many a long native turn may ask.
/// Waiting time is accounted separately by WaitBudget.
const MAX_PENDING_CARDS: usize = 8;
/// Posting a card or its outcome. A first message in a large encrypted room
/// shares its room key with every member's devices first.
const POST_TIMEOUT: Duration = Duration::from_secs(30);
/// Reading the answers that reached the device, with membership checks. The
/// device bounds each of its own steps well inside this.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Outcomes owed to rooms are retried at most this often per Agent device,
/// and given up after so many attempts or so long.
const FLUSH_INTERVAL: Duration = Duration::from_secs(30);
const MAX_CLOSE_ATTEMPTS: u32 = 8;
const MAX_CLOSE_AGE: Duration = Duration::from_secs(6 * 3600);
const MAX_OUTBOX: usize = 64;
/// Posted cards are recorded this long, and at most this many per Agent.
const RECORD_AGE_MS: u64 = 24 * 3600 * 1000;
const MAX_RECORDS: usize = 64;

/// A member's answer event as the Matrix boundary hands it over: from a
/// current member, authenticated where the room is encrypted. Content is
/// still unchecked.
pub(in crate::console) struct Candidate {
    pub(super) event_id: String,
    pub(super) sender: String,
    pub(super) content: Value,
}

pub(super) trait QuestionIo {
    fn post(
        &self,
        room: &str,
        txn: &str,
        content: Value,
    ) -> impl std::future::Future<Output = Result<String, NativeError>> + Send;
    fn answers(
        &self,
        room: &str,
        card_event: &str,
    ) -> impl std::future::Future<Output = Result<Vec<Candidate>, NativeError>> + Send;
    /// One event this device can read (`sender`, `content`), decrypted.
    fn event(
        &self,
        room: &str,
        event: &str,
    ) -> impl std::future::Future<Output = Result<Value, NativeError>> + Send;
}
impl QuestionIo for AgentDevice {
    async fn post(&self, room: &str, txn: &str, content: Value) -> Result<String, NativeError> {
        self.send_room_message(room, txn, content)
            .await
            .map_err(|_| failure(503, "shared_question_send_pending"))
    }
    async fn answers(&self, room: &str, card_event: &str) -> Result<Vec<Candidate>, NativeError> {
        Ok(self
            .question_answers(room, card_event)
            .await
            .map_err(|_| failure(503, "shared_question_answers_unavailable"))?
            .into_iter()
            .map(|r| Candidate {
                event_id: r.event_id,
                sender: r.sender,
                content: r.content,
            })
            .collect())
    }
    async fn event(&self, room: &str, event: &str) -> Result<Value, NativeError> {
        self.room_event(room, event)
            .await
            .map_err(|_| failure(503, "shared_question_event_unavailable"))
    }
}

pub(in crate::console) enum Source {
    Device(Arc<AgentDevice>),
    #[cfg(test)]
    Fixture(Arc<Fixture>),
}
impl Source {
    fn owns(&self, event: &Value) -> bool {
        let Some(sender) = event["sender"].as_str() else {
            return false;
        };
        match self {
            Self::Device(device) => device
                .approval_client()
                .user_id()
                .is_some_and(|own| own.as_str() == sender),
            #[cfg(test)]
            Self::Fixture(_) => sender == "@_hagency_agt_fixture:test",
        }
    }
    fn revoked(&self) -> bool {
        match self {
            Self::Device(device) => device.is_revoked(),
            #[cfg(test)]
            Self::Fixture(fixture) => fixture.revoked.load(std::sync::atomic::Ordering::SeqCst),
        }
    }
    fn post_timeout(&self) -> Duration {
        match self {
            Self::Device(_) => POST_TIMEOUT,
            #[cfg(test)]
            Self::Fixture(fixture) => fixture.post_timeout,
        }
    }
}
impl QuestionIo for Source {
    async fn post(&self, room: &str, txn: &str, content: Value) -> Result<String, NativeError> {
        match self {
            Self::Device(device) => QuestionIo::post(device.as_ref(), room, txn, content).await,
            #[cfg(test)]
            Self::Fixture(fixture) => fixture.post(room, txn, content).await,
        }
    }
    async fn answers(&self, room: &str, card_event: &str) -> Result<Vec<Candidate>, NativeError> {
        match self {
            Self::Device(device) => QuestionIo::answers(device.as_ref(), room, card_event).await,
            #[cfg(test)]
            Self::Fixture(fixture) => fixture.answers(room, card_event).await,
        }
    }
    async fn event(&self, room: &str, event: &str) -> Result<Value, NativeError> {
        match self {
            Self::Device(device) => QuestionIo::event(device.as_ref(), room, event).await,
            #[cfg(test)]
            Self::Fixture(fixture) => fixture.event(room, event).await,
        }
    }
}

/// One card a run posted: room, card event, and whether it was answered.
/// Identifiers only; the card itself is read back from the room.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    room: String,
    #[serde(default)]
    event: String,
    /// Written before sending. A lost response (or process) can recover the
    /// card with the same Matrix transaction, without saving its text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    txn: Option<String>,
    posted_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    answered_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    answer_event: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordFile {
    version: u64,
    cards: Vec<Record>,
}
static RECORDS: Mutex<()> = Mutex::new(());

fn load_records(path: &Path) -> Result<Vec<Record>, ()> {
    let file = match hagency_store::private::open(path, false) {
        Ok(file) => file,
        Err(hagency_store::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        }
        Err(_) => return Err(()),
    };
    let mut bytes = Vec::new();
    file.take(256 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() > 256 * 1024 {
        return Err(());
    }
    let file: RecordFile = serde_json::from_slice(&bytes).map_err(|_| ())?;
    if file.version != 1 || file.cards.len() > MAX_RECORDS {
        return Err(());
    }
    Ok(file.cards)
}
#[cfg(test)]
fn read_records(path: &Path) -> Vec<Record> {
    load_records(path).expect("read the recovery record")
}
fn update_records(path: &Path, change: impl FnOnce(&mut Vec<Record>)) -> bool {
    let _guard = RECORDS.lock().unwrap_or_else(|e| e.into_inner());
    // Provision the dedicated private recovery directory before its first
    // record; an existing directory must satisfy the same privacy checks.
    if !path
        .parent()
        .is_some_and(|directory| hagency_store::private::directory(directory).is_ok())
    {
        tracing::warn!("Agent question recovery directory unavailable");
        return false;
    }
    let Ok(mut cards) = load_records(path) else {
        tracing::warn!(
            "Agent question recovery records cannot be read; retained without replacement"
        );
        return false;
    };
    change(&mut cards);
    let now = now_ms();
    cards.retain(|r| now.saturating_sub(r.posted_ms) < RECORD_AGE_MS);
    let excess = cards.len().saturating_sub(MAX_RECORDS);
    cards.drain(..excess);
    if cards.is_empty() {
        return match std::fs::remove_file(path) {
            Ok(()) => true,
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        };
    }
    if let Some(directory) = path.parent()
        && hagency_store::private::directory(directory).is_ok()
        && let Ok(bytes) = serde_json::to_vec(&RecordFile { version: 1, cards })
    {
        if hagency_store::private::replace(path, &bytes).is_ok() {
            return true;
        }
    }
    tracing::warn!("Agent question recovery records could not be saved");
    false
}

/// Where an outcome goes: the card's event, or a card post that may or may not
/// have reached the room (its response never came).
enum Target {
    Event(String),
    Unconfirmed { txn: String },
}
enum Outcome {
    /// The settled card to show.
    Show(Card),
    /// After a restart: the card is read back from the room.
    Recover {
        answered_by: Option<String>,
        answer_event: Option<String>,
    },
}
struct Closing {
    room: String,
    target: Target,
    transaction: Option<String>,
    outcome: Outcome,
    attempts: u32,
    since: Instant,
    tried: Option<Instant>,
}
impl Closing {
    fn new(room: String, target: Target, outcome: Outcome) -> Self {
        let transaction = match &target {
            Target::Unconfirmed { txn } => Some(txn.clone()),
            Target::Event(_) => None,
        };
        Self {
            room,
            target,
            transaction,
            outcome,
            attempts: 0,
            since: Instant::now(),
            tried: None,
        }
    }
}
#[derive(Default)]
struct Outbox {
    closings: Vec<Closing>,
    flushing: bool,
    last: Option<Instant>,
    /// The records of an earlier process were read.
    recovered: bool,
}
/// Card outcomes not shown yet, per Agent device (its records file).
static OUTBOX: LazyLock<Mutex<HashMap<String, Outbox>>> = LazyLock::new(Default::default);
/// Card events still waiting in a run of this process, per outbox: never
/// closed early.
static LIVE: LazyLock<Mutex<HashSet<(String, String)>>> = LazyLock::new(Default::default);

fn live(key: Option<&str>, card_event: &str, waiting: bool) {
    let Some(key) = key else { return };
    let entry = (key.to_owned(), card_event.to_owned());
    let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
    if waiting {
        live.insert(entry);
    } else {
        live.remove(&entry);
    }
}
fn outbox_key(source: &Source, records: Option<&Path>) -> String {
    match (records, source) {
        (Some(path), _) => path.display().to_string(),
        (None, Source::Device(device)) => format!("device:{:p}", Arc::as_ptr(device)),
        #[cfg(test)]
        (None, Source::Fixture(fixture)) => format!("fixture:{}", fixture.id),
    }
}
fn enqueue(key: &str, closings: Vec<Closing>) {
    if closings.is_empty() {
        return;
    }
    let mut outbox = OUTBOX.lock().unwrap_or_else(|e| e.into_inner());
    let entry = outbox.entry(key.to_owned()).or_default();
    entry.closings.extend(closings);
    let excess = entry.closings.len().saturating_sub(MAX_OUTBOX);
    entry.closings.drain(..excess);
}

/// The Agent's device is in use: show the outcomes it still owes its rooms,
/// at most every `FLUSH_INTERVAL`, and once per process close the cards an
/// earlier process left open.
pub(in crate::console) fn device_ready(source: Arc<Source>, records: Option<PathBuf>) {
    kick(source, records, false);
}

fn kick(source: Arc<Source>, records: Option<PathBuf>, now: bool) {
    if source.revoked() {
        return;
    }
    let key = outbox_key(&source, records.as_deref());
    let start = {
        let mut outbox = OUTBOX.lock().unwrap_or_else(|e| e.into_inner());
        let entry = outbox.entry(key.clone()).or_default();
        if !entry.recovered {
            entry.recovered = true;
            if let Some(path) = &records {
                let live = LIVE.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let records = match load_records(path) {
                    Ok(records) => records,
                    Err(()) => {
                        tracing::warn!(
                            "Agent question recovery records unavailable; retry on next device use"
                        );
                        entry.recovered = false;
                        Vec::new()
                    }
                };
                for record in records {
                    let known = entry.closings.iter().any(|c| match &c.target {
                        Target::Event(event) => *event == record.event,
                        Target::Unconfirmed { txn } => record.txn.as_ref() == Some(txn),
                    });
                    if live.contains(&(key.clone(), record.event.clone()))
                        || record
                            .txn
                            .as_ref()
                            .is_some_and(|txn| live.contains(&(key.clone(), txn.clone())))
                        || known
                    {
                        continue;
                    }
                    let target = if record.event.is_empty() {
                        let Some(txn) = record.txn else { continue };
                        Target::Unconfirmed { txn }
                    } else {
                        Target::Event(record.event)
                    };
                    entry.closings.push(Closing::new(
                        record.room,
                        target,
                        Outcome::Recover {
                            answered_by: record.answered_by,
                            answer_event: record.answer_event,
                        },
                    ));
                }
            }
        }
        let due = !entry.closings.is_empty()
            && !entry.flushing
            && (now || entry.last.is_none_or(|t| t.elapsed() >= FLUSH_INTERVAL));
        if due {
            entry.flushing = true;
            entry.last = Some(Instant::now());
        }
        due
    };
    if start {
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(flush(key, source, records));
        } else if let Some(entry) = OUTBOX
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&key)
        {
            // Drop cleanup can happen after the runtime stopped. Durable
            // records and the outbox still belong to the next device use.
            entry.flushing = false;
        }
    }
}

/// One pass over the outbox: every outcome is tried once; what fails waits for
/// the device's next use.
async fn flush(key: String, source: Arc<Source>, records: Option<PathBuf>) {
    let mut tried = 0;
    loop {
        let batch: Vec<Closing> = {
            let mut outbox = OUTBOX.lock().unwrap_or_else(|e| e.into_inner());
            let Some(entry) = outbox.get_mut(&key) else {
                break;
            };
            let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut entry.closings)
                .into_iter()
                .partition(|c| c.tried.is_none_or(|t| t.elapsed() >= FLUSH_INTERVAL));
            entry.closings = rest;
            due
        };
        if batch.is_empty() || tried > 4 * MAX_OUTBOX {
            break;
        }
        let mut kept = Vec::new();
        let mut batch = batch.into_iter();
        while let Some(mut closing) = batch.next() {
            tried += 1;
            if source.revoked() {
                // The next device takes over: nothing more to try with this one.
                kept.push(closing);
                kept.extend(batch.by_ref());
                break;
            }
            let unconfirmed = closing.transaction.clone();
            match close(&source, &mut closing).await {
                Ok(event) => {
                    if let Some(path) = &records {
                        update_records(path, |cards| {
                            cards.retain(|r| {
                                r.event != event && !(unconfirmed.is_some() && r.txn == unconfirmed)
                            })
                        });
                    }
                }
                Err(()) => {
                    closing.attempts += 1;
                    closing.tried = Some(Instant::now());
                    if closing.attempts < MAX_CLOSE_ATTEMPTS
                        && closing.since.elapsed() < MAX_CLOSE_AGE
                    {
                        kept.push(closing);
                    } else {
                        tracing::info!(
                            "Agent question outcome given up; the card reads as expired once its time is up"
                        );
                        if let (Some(path), Target::Event(event)) = (&records, &closing.target) {
                            update_records(path, |cards| {
                                cards.retain(|r| {
                                    &r.event != event
                                        && !(unconfirmed.is_some() && r.txn == unconfirmed)
                                })
                            });
                        }
                    }
                }
            }
        }
        {
            let mut outbox = OUTBOX.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = outbox.get_mut(&key) {
                kept.append(&mut entry.closings);
                entry.closings = kept;
            }
        }
        if source.revoked() {
            break;
        }
    }
    let mut outbox = OUTBOX.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = outbox.get_mut(&key) {
        entry.flushing = false;
    }
}

/// Shows one outcome. Returns the card's event once its edit was accepted.
async fn close(source: &Source, closing: &mut Closing) -> Result<String, ()> {
    let timeout = source.post_timeout();
    let event = match &closing.target {
        Target::Event(event) => event.clone(),
        Target::Unconfirmed { txn } => {
            // The same transaction returns the card if its post reached the
            // room after all; otherwise the room gets the card already closed.
            // A restarted process only retained identifiers. Its harmless
            // notice replays that same transaction, then reads the original
            // card back if there was one.
            let content = match &closing.outcome {
                Outcome::Show(card) => card.message_content(),
                Outcome::Recover { .. } => json!({
                    "msgtype": "m.notice",
                    "body": "Agent question expired. / Agent 提问已过期。",
                    "im.hagency.agent_question.closed_transaction": txn,
                }),
            };
            let event = tokio::time::timeout(timeout, source.post(&closing.room, txn, content))
                .await
                .ok()
                .and_then(Result::ok)
                .ok_or(())?;
            closing.target = Target::Event(event.clone());
            event
        }
    };
    let card = match &closing.outcome {
        Outcome::Show(card) => card.clone(),
        Outcome::Recover {
            answered_by,
            answer_event,
        } => {
            let read = |id: String| {
                let room = closing.room.clone();
                async move {
                    tokio::time::timeout(READ_TIMEOUT, source.event(&room, &id))
                        .await
                        .ok()
                        .and_then(Result::ok)
                }
            };
            let original = read(event.clone()).await.ok_or(())?;
            if !source.owns(&original) {
                return Err(());
            }
            if closing.transaction.as_ref().is_some_and(|txn| {
                original["content"]["im.hagency.agent_question.closed_transaction"].as_str()
                    == Some(txn)
            }) {
                // The card never reached the room before the process died.
                return Ok(event);
            }
            let card = Card::from_content(&original["content"]).ok_or(())?;
            match (answered_by, answer_event) {
                // Answered before the restart: shown as answered or not at all,
                // never as expired.
                (Some(by), Some(answer)) if card.may_answer(by) => read(answer.clone())
                    .await
                    .filter(|v| v["sender"] == by.as_str())
                    .and_then(|v| card.parse_answer(&event, &v["content"]))
                    .map(|answers| card.answered(by, answers))
                    .ok_or(())?,
                _ => card.expired(),
            }
        }
    };
    if card.state == CardState::Pending {
        return Err(());
    }
    let state = if card.state == CardState::Answered {
        "answered"
    } else {
        "expired"
    };
    let txn = format!("hagency_question_{state}_{}", &card.id[3..]);
    tokio::time::timeout(
        timeout,
        source.post(&closing.room, &txn, card.edit_content(&event)),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .ok_or(())?;
    Ok(event)
}

struct Pending<R> {
    request: R,
    card: Card,
    room: String,
    card_event: String,
    /// The card's own expiry: one instant with `card.expires_at_ms`.
    deadline: tokio::time::Instant,
}

/// A room answer is only shown as accepted after its runtime callback was
/// written. A withdrawn callback, failed write, or dropped task expires it.
struct Delivery<K> {
    request_id: K,
    room: String,
    card_event: String,
    unanswered: Card,
    answered: Card,
    answer_event: String,
}

struct Posting {
    room: String,
    txn: String,
    card: Card,
}

/// The whole time one run may hold its Agent's queue waiting for answers.
/// Every question in the turn shares it, so the messages behind the run wait
/// at most this long for questions in total, however many the model asks.
struct WaitBudget {
    total: Duration,
    used: Duration,
    /// Since when at least one card has been waiting.
    since: Option<tokio::time::Instant>,
}
impl WaitBudget {
    fn new(total: Duration) -> Self {
        Self {
            total,
            used: Duration::ZERO,
            since: None,
        }
    }
    /// The least wait worth asking anyone in.
    fn floor(&self) -> Duration {
        self.total / 20
    }
    /// How long a card asked at `now` may wait, or `None` when too little of
    /// the budget is left for anyone to answer in.
    fn next(&self, now: tokio::time::Instant) -> Option<Duration> {
        let waiting = self
            .since
            .map_or(Duration::ZERO, |since| now.saturating_duration_since(since));
        let left = self.total.saturating_sub(self.used).saturating_sub(waiting);
        (!left.is_zero() && left >= self.floor()).then_some(left)
    }
    /// A card started waiting at `now`.
    fn start(&mut self, now: tokio::time::Instant) {
        self.since.get_or_insert(now);
    }
    /// No card is waiting any more.
    fn stop(&mut self, now: tokio::time::Instant) {
        if let Some(since) = self.since.take() {
            self.used = self
                .used
                .saturating_add(now.saturating_duration_since(since));
        }
    }
}

/// The Agent's device for this run, with where its cards are recorded.
type Opened = (Arc<Source>, Option<PathBuf>);

/// Immutable host-selected addressing. Never supplied by the model or an
/// answer event. A shared task admits its requester; personal hosting also
/// admits the Agent owner, using the already authenticated dispatch/lease.
pub(in crate::console) struct QuestionContext {
    pub id: String,
    pub room_id: String,
    pub event_id: String,
    pub thread_root: String,
    pub responders: Vec<String>,
}
impl QuestionContext {
    /// The thread a card is posted in. A request outside any thread (an
    /// owner's direct conversation) names its room as the root; its card goes
    /// into the thread its own message starts, where its run card already is.
    fn card_thread(&self) -> String {
        if self.thread_root == self.room_id {
            self.event_id.clone()
        } else {
            self.thread_root.clone()
        }
    }
}

pub(in crate::console) trait CardRequest: Sized {
    type Key: Clone + PartialEq + Serialize;
    type Response;
    fn id(&self) -> &Self::Key;
    fn questions(&self) -> Vec<hagency_runtime::questions::Question>;
    fn forbidden(&self) -> bool { false }
    fn closed(&self) -> bool { false }
    fn deadline(&self) -> Option<tokio::time::Instant> { None }
    fn answer(self, answers: &hagency_runtime::questions::Answers) -> Result<Self::Response, Self>;
    fn unanswered(self) -> Self::Response;
}
impl CardRequest for UserInputRequest {
    type Key = RequestId;
    type Response = UserInputResponse;
    fn id(&self) -> &RequestId { self.id() }
    fn questions(&self) -> Vec<hagency_runtime::questions::Question> { self.host_questions() }
    fn forbidden(&self) -> bool { self.is_codex_approval() }
    fn answer(self, answers: &hagency_runtime::questions::Answers) -> Result<Self::Response, Self> {
        self.answer_host(answers).map_err(|_| self)
    }
    fn unanswered(self) -> Self::Response { UserInputRequest::unanswered(&self) }
}
impl CardRequest for hagency_agent_local::questions::PendingQuestion {
    type Key = String;
    type Response = hagency_agent_local::questions::Delivery;
    fn id(&self) -> &String { &self.id }
    fn questions(&self) -> Vec<hagency_runtime::questions::Question> { self.questions.clone() }
    fn closed(&self) -> bool { self.is_closed() }
    fn deadline(&self) -> Option<tokio::time::Instant> { Some(self.deadline) }
    fn answer(self, answers: &hagency_runtime::questions::Answers) -> Result<Self::Response, Self> {
        if self.is_closed() || !hagency_runtime::questions::valid_answers(&self.questions, answers) { return Err(self); }
        Ok(self.reply(answers.clone()))
    }
    fn unanswered(self) -> Self::Response { self.reply(Default::default()) }
}

pub(in crate::console) struct Questions<R: CardRequest = UserInputRequest> {
    source: Option<Arc<Source>>,
    /// The device being opened for this run. Never cut short (an abandoned
    /// grant would cost another device login); the run only stops waiting.
    opening: Option<tokio::task::JoinHandle<Result<Opened, NativeError>>>,
    /// Where the Agent's posted cards are recorded.
    records: Option<PathBuf>,
    /// The outbox of the device this run used; it outlives that device.
    outbox: Option<String>,
    pending: Vec<Pending<R>>,
    delivering: Vec<Delivery<R::Key>>,
    /// Retained before the first await of a send. Cancellation still owns
    /// closing a card whose response was never observed.
    posting: Option<Posting>,
    /// Settled cards whose outcome the room has not been shown yet.
    settled: Vec<(String, String, Card)>,
    asked: usize,
    /// The assignment whose Agent device posted the cards.
    assignment: Option<String>,
    encrypted: bool,
    budget: Option<WaitBudget>,
}

impl<R: CardRequest> Default for Questions<R> {
    fn default() -> Self {
        Self { source: None, opening: None, records: None, outbox: None,
            pending: Vec::new(), delivering: Vec::new(), posting: None, settled: Vec::new(),
            asked: 0, assignment: None, encrypted: false, budget: None }
    }
}

/// Question updates seen while a decision was being sent, in arrival order.
pub(super) enum QuestionUpdate<R = UserInputRequest> {
    Asked(R),
    Withdrawn(RequestId),
}
pub(super) trait Asked {
    fn request_id(&self) -> &RequestId;
}
impl Asked for UserInputRequest {
    fn request_id(&self) -> &RequestId {
        self.id()
    }
}
/// The updates to apply, in order. A question Codex withdrew within the same
/// batch is never shown: there is nobody left to answer.
pub(super) fn in_order<R: Asked>(updates: Vec<QuestionUpdate<R>>) -> Vec<QuestionUpdate<R>> {
    let withdrawn: BTreeSet<RequestId> = updates
        .iter()
        .filter_map(|u| match u {
            QuestionUpdate::Withdrawn(id) => Some(id.clone()),
            QuestionUpdate::Asked(_) => None,
        })
        .collect();
    updates
        .into_iter()
        .filter(|u| match u {
            QuestionUpdate::Asked(request) => !withdrawn.contains(request.request_id()),
            QuestionUpdate::Withdrawn(_) => true,
        })
        .collect()
}

impl<R: CardRequest> Questions<R> {
    pub(in crate::console) fn with_device(device: Arc<AgentDevice>, records: PathBuf) -> Self {
        let mut questions = Self::default();
        questions.adopt((Arc::new(Source::Device(device)), Some(records)));
        questions
    }
    pub(in crate::console) fn charge_opening(&mut self, elapsed: Duration) {
        let budget = self.budget.get_or_insert_with(|| WaitBudget::new(Duration::from_millis(protocol::ANSWER_WAIT_MS)));
        budget.used = budget.used.saturating_add(elapsed);
    }
    /// Work for the next maintenance pass: waiting cards or outcomes to show.
    pub(in crate::console) fn busy(&self) -> bool {
        !self.pending.is_empty() || !self.delivering.is_empty() || !self.settled.is_empty()
    }

    /// Show one question request to the room. `Some`: nothing could be asked
    /// (only secret questions, more than a card holds, too many questions,
    /// Codex's own approval prompt, or the room cannot be reached within the
    /// wait), so the model gets no answers at once and decides on its own.
    /// Never fails the run.
    pub(super) async fn ask(
        &mut self,
        io: &Io,
        work: &SharedWorkItem,
        request: R,
    ) -> Result<Option<R::Response>, NativeError> {
        self.assignment = Some(work.assignment_id.clone());
        self.encrypted = work.encrypted;
        let context = QuestionContext {
            id: work.id.clone(), room_id: work.room_id.clone(), event_id: work.event_id.clone(),
            thread_root: work.thread_root.clone(), responders: vec![work.requester_mxid.clone()],
        };
        self.ask_inner(Some(io), &context, request).await
    }

    pub(in crate::console) async fn ask_on_device(&mut self, context: &QuestionContext, request: R) -> Result<Option<R::Response>, NativeError> {
        self.ask_inner(None, context, request).await
    }

    async fn ask_inner(&mut self, io: Option<&Io>, work: &QuestionContext, request: R) -> Result<Option<R::Response>, NativeError> {
        self.asked += 1;
        if request.forbidden() {
            // Answering "Allow" would grant it: approvals go to the owner.
            tracing::warn!(
                "Codex tool approval asked as a question; declined, never shown to the room"
            );
            return Ok(Some(request.unanswered()));
        }
        let now = tokio::time::Instant::now();
        let budget = self.budget.get_or_insert_with(|| WaitBudget::new(io.map(wait).unwrap_or(Duration::from_millis(protocol::ANSWER_WAIT_MS))));
        let floor = budget.floor();
        let wait = budget.next(now).map(|wait| request.deadline().map_or(wait, |until| wait.min(until.saturating_duration_since(now))))
            .filter(|wait| *wait >= floor);
        let id = format!(
            "aq_{}",
            super::digest(
                &json!({"work":work.id,"request":request.id(),"questions":request.questions().iter().map(|q| &q.id).collect::<Vec<_>>()})
            )?
        );
        // The card's expiry and this run's deadline are one instant.
        let expires_at_ms = now_ms().saturating_add(wait.unwrap_or_default().as_millis() as u64);
        let card = (self.pending.len() < MAX_PENDING_CARDS && wait.is_some())
            .then(|| {
                Card::ask_questions(
                    id,
                    work.event_id.clone(),
                    work.card_thread(),
                    expires_at_ms,
                    &request.questions(),
                    work.responders.clone(),
                )
            })
            .flatten();
        let (Some(card), Some(wait)) = (card, wait) else {
            tracing::info!(
                questions = request.questions().len(),
                asked = self.asked,
                "Agent question cannot be shown in the room; the model decides on its own"
            );
            return Ok(Some(request.unanswered()));
        };
        let deadline = now + wait;
        // Opening the device and posting come out of the same wait.
        if let Some(budget) = &mut self.budget {
            budget.start(now);
        }
        let source = match self
            .source_until(io, deadline.checked_sub(floor).unwrap_or(now))
            .await
        {
            Ok(source) => source,
            Err(error) => {
                tracing::warn!(code = %error.code, "Agent question has no room device in time; the model decides on its own");
                self.idle_check();
                return Ok(Some(request.unanswered()));
            }
        };
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left < floor {
            tracing::info!(
                "Agent question has too little of its wait left; the model decides on its own"
            );
            self.idle_check();
            return Ok(Some(request.unanswered()));
        }
        let txn = format!("hagency_question_{}", &card.id[3..]);
        live(self.outbox.as_deref(), &txn, true);
        if let Some(path) = &self.records {
            let record = Record {
                room: work.room_id.clone(),
                event: String::new(),
                txn: Some(txn.clone()),
                posted_ms: now_ms(),
                answered_by: None,
                answer_event: None,
            };
            if !update_records(path, |cards| cards.push(record)) {
                live(self.outbox.as_deref(), &txn, false);
                self.idle_check();
                tracing::warn!("Agent question recovery record unavailable; no card was posted");
                return Ok(Some(request.unanswered()));
            }
        }
        self.posting = Some(Posting {
            room: work.room_id.clone(),
            txn: txn.clone(),
            card: card.clone(),
        });
        let posted = tokio::time::timeout(
            source.post_timeout().min(left),
            source.post(&work.room_id, &txn, card.message_content()),
        )
        .await;
        let Ok(Ok(card_event)) = posted else {
            // The post may have reached the room all the same; its own
            // transaction later finds that card and closes it.
            tracing::warn!("Agent question card could not be posted; the model decides on its own");
            self.close_posting();
            self.publish_now();
            self.idle_check();
            return Ok(Some(request.unanswered()));
        };
        tracing::info!(
            questions = card.questions.len(),
            withheld = card.withheld,
            wait_secs = left.as_secs(),
            "Agent question card posted; waiting for an answer from the room"
        );
        self.posting = None;
        live(self.outbox.as_deref(), &txn, false);
        live(self.outbox.as_deref(), &card_event, true);
        if let Some(path) = &self.records {
            update_records(path, |cards| {
                for record in cards
                    .iter_mut()
                    .filter(|record| record.txn.as_ref() == Some(&txn))
                {
                    record.event = card_event.clone();
                }
            });
        }
        self.pending.push(Pending {
            request,
            card,
            room: work.room_id.clone(),
            card_event,
            deadline,
        });
        Ok(None)
    }

    /// Answers that arrived and waits that ran out, as the responses to send
    /// now. The first answer that is valid for its card wins; later answers
    /// to a settled card are never read. Never waits for a device to open.
    pub(super) async fn collect(&mut self, io: &Io) -> Result<Vec<R::Response>, NativeError> {
        if !self.pending.is_empty()
            && let Err(error) = self.source_until(Some(io), tokio::time::Instant::now()).await
        {
            tracing::debug!(code = %error.code, "Agent question room device not ready; still waiting");
        }
        self.collect_ready().await
    }

    pub(in crate::console) async fn collect_ready(&mut self) -> Result<Vec<R::Response>, NativeError> {
        let mut ready = Vec::new();
        let mut index = 0;
        while index < self.pending.len() {
            let pending = &self.pending[index];
            let mut answered = None;
            if tokio::time::Instant::now() < pending.deadline
                && let Some(source) = self.source.as_ref().filter(|s| !s.revoked())
            {
                // The whole read, including device/membership retries, must
                // finish before this question's deadline.
                let left = pending
                    .deadline
                    .saturating_duration_since(tokio::time::Instant::now());
                let candidates = tokio::time::timeout(
                    READ_TIMEOUT.min(left),
                    source.answers(&pending.room, &pending.card_event),
                )
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default();
                for candidate in candidates {
                    if tokio::time::Instant::now() >= pending.deadline {
                        break;
                    }
                    // No Agent answers a card, whichever device saw it.
                    if !pending.card.may_answer(&candidate.sender) {
                        continue;
                    }
                    let Some(answers) = pending
                        .card
                        .parse_answer(&pending.card_event, &candidate.content)
                    else {
                        continue;
                    };
                    answered = Some((candidate, answers));
                    break;
                }
            }
            if let Some((candidate, answers)) = answered {
                let pending = self.pending.remove(index);
                let request_id = pending.request.id().clone();
                let response = match pending.request.answer(&answers) {
                    Ok(response) => response,
                    Err(request) => {
                        live(self.outbox.as_deref(), &pending.card_event, false);
                        self.settled.push((pending.room, pending.card_event, pending.card.expired()));
                        ready.push(request.unanswered());
                        continue;
                    }
                };
                let answered = pending.card.answered(&candidate.sender, answers);
                self.delivering.push(Delivery {
                    request_id,
                    room: pending.room,
                    card_event: pending.card_event,
                    unanswered: pending.card,
                    answered,
                    answer_event: candidate.event_id,
                });
                ready.push(response);
                continue;
            }
            if tokio::time::Instant::now() >= pending.deadline || pending.request.closed() {
                let pending = self.pending.remove(index);
                tracing::info!(
                    "Agent question expired without an answer; the model decides on its own"
                );
                live(self.outbox.as_deref(), &pending.card_event, false);
                self.settled
                    .push((pending.room, pending.card_event, pending.card.expired()));
                ready.push(pending.request.unanswered());
                continue;
            }
            index += 1;
        }
        self.idle_check();
        Ok(ready)
    }

    /// The runtime callback finished. Never advertise a room answer the
    /// model could not receive (including a callback it already withdrew).
    pub(in crate::console) fn delivered(&mut self, id: &R::Key, accepted: bool) {
        let Some(index) = self
            .delivering
            .iter()
            .position(|delivery| &delivery.request_id == id)
        else {
            return;
        };
        let delivery = self.delivering.remove(index);
        let card = if accepted {
            if let Some(path) = &self.records {
                update_records(path, |cards| {
                    for record in cards
                        .iter_mut()
                        .filter(|record| record.event == delivery.card_event)
                    {
                        record.answered_by = delivery.answered.answered_by.clone();
                        record.answer_event = Some(delivery.answer_event.clone());
                    }
                });
            }
            delivery.answered
        } else {
            delivery.unanswered.expired()
        };
        live(self.outbox.as_deref(), &delivery.card_event, false);
        self.settled
            .push((delivery.room, delivery.card_event, card));
    }

    /// Stop the budget's clock once no card waits.
    fn idle_check(&mut self) {
        if self.pending.is_empty()
            && let Some(budget) = &mut self.budget
        {
            budget.stop(tokio::time::Instant::now());
        }
    }

    /// Codex withdrew a question: stop waiting and close its card.
    pub(super) fn withdrawn(&mut self, id: &R::Key) {
        self.delivered(id, false);
        if let Some(index) = self.pending.iter().position(|p| p.request.id() == id) {
            let pending = self.pending.remove(index);
            tracing::info!("Agent question withdrawn by the model before an answer");
            live(self.outbox.as_deref(), &pending.card_event, false);
            self.settled
                .push((pending.room, pending.card_event, pending.card.expired()));
        }
        self.idle_check();
    }

    /// The turn is over: every card still waiting closes unanswered.
    pub(in crate::console) fn close(&mut self) {
        self.close_posting();
        for delivery in std::mem::take(&mut self.delivering) {
            live(self.outbox.as_deref(), &delivery.card_event, false);
            self.settled.push((
                delivery.room,
                delivery.card_event,
                delivery.unanswered.expired(),
            ));
        }
        for pending in std::mem::take(&mut self.pending) {
            live(self.outbox.as_deref(), &pending.card_event, false);
            self.settled
                .push((pending.room, pending.card_event, pending.card.expired()));
        }
        self.idle_check();
    }

    /// Hand each settled card's outcome to the Agent's outbox, which shows it
    /// in the room now if the device can, or whenever the device is next in
    /// use. Never waits: the run does not hold for an edit.
    pub(in crate::console) async fn publish(&mut self) -> Result<(), NativeError> {
        self.publish_now();
        Ok(())
    }

    fn close_posting(&mut self) {
        if let Some(posting) = self.posting.take() {
            live(self.outbox.as_deref(), &posting.txn, false);
            if let Some(key) = self.outbox() {
                enqueue(
                    &key,
                    vec![Closing::new(
                        posting.room,
                        Target::Unconfirmed { txn: posting.txn },
                        Outcome::Show(posting.card.expired()),
                    )],
                );
            }
        }
    }

    fn publish_now(&mut self) {
        let settled = std::mem::take(&mut self.settled);
        let Some(key) = self.outbox() else {
            return;
        };
        enqueue(
            &key,
            settled
                .into_iter()
                .map(|(room, card_event, card)| {
                    Closing::new(room, Target::Event(card_event), Outcome::Show(card))
                })
                .collect(),
        );
        if let Some(source) = self.source.clone().filter(|s| !s.revoked()) {
            kick(source, self.records.clone(), true);
        }
    }

    /// The outbox of the device this run used, if it used one.
    fn outbox(&self) -> Option<String> {
        self.outbox.clone()
    }

    fn adopt(&mut self, (source, records): Opened) -> Arc<Source> {
        self.outbox = Some(outbox_key(&source, records.as_deref()));
        self.source = Some(source.clone());
        self.records = records;
        source
    }

    /// The Agent's device for this run's room, waiting at most until `until`.
    /// A device already open costs nothing. In an encrypted room a closed
    /// device is opened and the run waits for it until `until`; the opening
    /// itself carries on. A plaintext room never had this device open for
    /// its own sake, so no device login starts in the middle of a turn there.
    async fn source_until(
        &mut self,
        io: Option<&Io>,
        until: tokio::time::Instant,
    ) -> Result<Arc<Source>, NativeError> {
        if let Some(source) = self.source.as_ref().filter(|s| !s.revoked()) {
            return Ok(source.clone());
        }
        self.source = None;
        let io = io.ok_or_else(|| failure(409, "question_device_unavailable"))?;
        let assignment = self.assignment.clone().ok_or_else(invalid)?;
        if self.opening.is_none() {
            if let Some(opened) = tokio::time::timeout_at(until, opened(io, &assignment))
                .await
                .map_err(|_| failure(504, "shared_question_device_pending"))??
            {
                return Ok(self.adopt(opened));
            }
            if !self.encrypted {
                return Err(failure(409, "shared_question_device_unavailable"));
            }
            self.opening = Some(tokio::spawn(open(io.clone(), assignment)));
        }
        let Some(opening) = self.opening.as_mut() else {
            return Err(invalid());
        };
        match tokio::time::timeout_at(until, opening).await {
            Ok(joined) => {
                self.opening = None;
                let opened = joined.map_err(|_| invalid())??;
                Ok(self.adopt(opened))
            }
            Err(_) => Err(failure(504, "shared_question_device_pending")),
        }
    }
}

impl<R: CardRequest> Drop for Questions<R> {
    fn drop(&mut self) {
        // A revoked lease or task abort can drop the driver before its
        // normal cleanup. Do not leave LIVE entries or unsettled cards behind.
        self.close();
        self.publish_now();
    }
}

/// The Agent's device if it is open already.
async fn opened(io: &Io, assignment: &str) -> Result<Option<Opened>, NativeError> {
    #[cfg(test)]
    if let Some(fixture) = io.crypto_fixture.as_ref().and_then(|f| f.questions.clone()) {
        let open = fixture
            .device_open
            .load(std::sync::atomic::Ordering::SeqCst)
            && !fixture.revoked.load(std::sync::atomic::Ordering::SeqCst);
        let records = fixture.records.clone();
        return Ok(open.then(|| (Arc::new(Source::Fixture(fixture)), records)));
    }
    let b = io.bound().await?;
    let records = super::e2ee::question_records(io, &b, assignment)?;
    Ok(super::e2ee::opened(io, &b, assignment)?
        .map(|device| (Arc::new(Source::Device(device)), Some(records))))
}

/// Opens the Agent's device: reuses a saved grant or takes a new one, and
/// syncs once.
async fn open(io: Io, assignment: String) -> Result<Opened, NativeError> {
    #[cfg(test)]
    if let Some(fixture) = io.crypto_fixture.as_ref().and_then(|f| f.questions.clone()) {
        let delay = *fixture.open_delay.lock().unwrap();
        tokio::time::sleep(delay).await;
        fixture
            .revoked
            .store(false, std::sync::atomic::Ordering::SeqCst);
        fixture
            .device_open
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let records = fixture.records.clone();
        let source = Arc::new(Source::Fixture(fixture));
        kick(source.clone(), records.clone(), false);
        return Ok((source, records));
    }
    let b = io.bound().await?;
    let device = super::e2ee::device(&io, &b, &assignment).await?;
    let records = super::e2ee::question_records(&io, &b, &assignment)?;
    Ok((Arc::new(Source::Device(device)), Some(records)))
}

/// Before the thread starts. Every shared turn may ask its room, so Codex
/// offers the model its question tool. Never fails the run: a thread without
/// it tells the model the tool is for Plan mode only.
pub(super) fn offer(process: &mut OwnedSession) {
    if let Err(error) = process.offer_shared_user_input_tool() {
        tracing::warn!(
            ?error,
            "Agent question tool not offered; the model decides on its own"
        );
    }
}

/// Every shared turn may ask its room. Without this (a session that is not
/// running), the session itself answers each question with nothing.
pub(super) fn enable(process: &mut OwnedSession) -> Result<(), NativeError> {
    if let Err(error) = process.enable_shared_user_input() {
        tracing::warn!(
            ?error,
            "Agent questions unavailable for this turn; the model decides on its own"
        );
    }
    Ok(())
}

/// One answer to Codex. An answer to a question Codex withdrew meanwhile, or
/// whose turn is over, is moot: nothing is written and the run goes on.
pub(super) async fn answer(
    process: &mut OwnedSession,
    response: UserInputResponse,
) -> Result<bool, NativeError> {
    match process.respond_shared_user_input(response).await {
        Ok(true) => Ok(true),
        Ok(false) => {
            tracing::debug!("Agent question answer is moot; Codex no longer waits for it");
            Ok(false)
        }
        Err(_) => Err(failure(409, "shared_question_answer_failed")),
    }
}

fn wait(io: &Io) -> Duration {
    #[cfg(test)]
    if let Some(fixture) = io
        .crypto_fixture
        .as_ref()
        .and_then(|f| f.questions.as_ref())
    {
        return fixture.wait;
    }
    let _ = io;
    Duration::from_millis(protocol::ANSWER_WAIT_MS)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The supervised driver tests substitute only the Matrix boundary: posting a
/// card or its edit, reading an event back, and the answers that reached the
/// Agent's device from current members. Card building, answer checks, the
/// wait, the Codex response, the outbox and the lease handling stay real.
#[cfg(test)]
pub(in crate::console) struct Fixture {
    /// Names this room's outbox. An address would be reused by the next
    /// fixture, which then inherits this one's flush time and waits out
    /// `FLUSH_INTERVAL` before its first outcome is shown.
    id: u64,
    posts: std::sync::Mutex<Vec<(String, String, Value)>>,
    answers: std::sync::Mutex<Vec<(String, String, Value)>>,
    /// Every answer event, readable back like the room's own.
    answer_log: std::sync::Mutex<Vec<(String, String, Value)>>,
    members: Vec<String>,
    pub(super) wait: Duration,
    pub(super) post_timeout: Duration,
    /// Where the Agent's posted cards are recorded, as a device's would be.
    pub(super) records: Option<PathBuf>,
    pub(in crate::console::native) unreachable: std::sync::atomic::AtomicBool,
    /// Posts that reach the room but whose responses never arrive in time.
    pub(in crate::console::native) lost_responses: std::sync::atomic::AtomicUsize,
    /// The device's token was replaced; a new open clears it.
    pub(in crate::console::native) revoked: std::sync::atomic::AtomicBool,
    /// Whether the Agent's device is open already (a plaintext room uses
    /// only an open one).
    pub(in crate::console::native) device_open: std::sync::atomic::AtomicBool,
    /// How long opening the device takes.
    pub(in crate::console::native) open_delay: std::sync::Mutex<Duration>,
    pub(in crate::console::native) answer_delay: std::sync::Mutex<Duration>,
    answer_events: std::sync::atomic::AtomicUsize,
}
#[cfg(test)]
impl Fixture {
    pub(super) fn new(members: &[&str], wait: Duration, records: Option<PathBuf>) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        Self {
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            posts: Default::default(),
            answers: Default::default(),
            answer_log: Default::default(),
            members: members.iter().map(|m| (*m).to_owned()).collect(),
            wait,
            post_timeout: POST_TIMEOUT.min(wait / 2),
            records,
            unreachable: Default::default(),
            lost_responses: Default::default(),
            revoked: Default::default(),
            device_open: std::sync::atomic::AtomicBool::new(true),
            open_delay: Default::default(),
            answer_delay: Default::default(),
            answer_events: Default::default(),
        }
    }
    /// The posted card messages (not their edits), as the room received them.
    pub(in crate::console::native) fn cards(&self) -> Vec<(String, Value)> {
        self.posts
            .lock()
            .unwrap()
            .iter()
            .enumerate()
            .filter(|(_, (_, _, content))| content["m.relates_to"]["rel_type"] != "m.replace")
            .map(|(index, (_, _, content))| (format!("$question_{index}:test"), content.clone()))
            .collect()
    }
    pub(in crate::console::native) fn edits(&self) -> Vec<Value> {
        self.posts
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, _, content)| content["m.relates_to"]["rel_type"] == "m.replace")
            .map(|(_, _, content)| content.clone())
            .collect()
    }
    /// A card message an earlier process posted, as the room holds it.
    pub(in crate::console::native) fn posted_earlier(&self, room: &str, content: Value) -> String {
        let mut posts = self.posts.lock().unwrap();
        let txn = format!("earlier_{}", posts.len());
        posts.push((room.to_owned(), txn, content));
        format!("$question_{}:test", posts.len() - 1)
    }
    pub(in crate::console::native) fn answer(&self, sender: &str, content: Value) -> String {
        let id = format!(
            "$answer_{}:test",
            self.answer_events
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        );
        self.answer_log
            .lock()
            .unwrap()
            .push((sender.to_owned(), id.clone(), content.clone()));
        self.answers
            .lock()
            .unwrap()
            .push((sender.to_owned(), id.clone(), content));
        id
    }
}
#[cfg(test)]
impl QuestionIo for Fixture {
    async fn post(&self, room: &str, txn: &str, content: Value) -> Result<String, NativeError> {
        use std::sync::atomic::Ordering;
        if self.unreachable.load(Ordering::SeqCst) || self.revoked.load(Ordering::SeqCst) {
            return Err(failure(503, "shared_question_send_pending"));
        }
        let event = {
            let mut posts = self.posts.lock().unwrap();
            // A repeated transaction returns the original event, as a homeserver does.
            if let Some(index) = posts.iter().position(|(_, t, _)| t == txn) {
                format!("$question_{index}:test")
            } else {
                posts.push((room.to_owned(), txn.to_owned(), content));
                format!("$question_{}:test", posts.len() - 1)
            }
        };
        let lost = self
            .lost_responses
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if lost {
            // The room has it; its response is lost on the way back.
            tokio::time::sleep(self.post_timeout * 4).await;
            return Err(failure(503, "shared_question_send_pending"));
        }
        Ok(event)
    }
    async fn answers(&self, _room: &str, card_event: &str) -> Result<Vec<Candidate>, NativeError> {
        let delay = *self.answer_delay.lock().unwrap();
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let mut answers = self.answers.lock().unwrap();
        let (mine, others): (Vec<_>, Vec<_>) = std::mem::take(&mut *answers)
            .into_iter()
            .partition(|(_, _, content)| content["m.relates_to"]["event_id"] == card_event);
        *answers = others;
        Ok(mine
            .into_iter()
            .filter(|(sender, _, _)| self.members.contains(sender))
            .map(|(sender, event_id, content)| Candidate {
                event_id,
                sender,
                content,
            })
            .collect())
    }
    async fn event(&self, _room: &str, event: &str) -> Result<Value, NativeError> {
        let unavailable = || failure(503, "shared_question_event_unavailable");
        if let Some(index) = event
            .strip_prefix("$question_")
            .and_then(|rest| rest.strip_suffix(":test"))
            .and_then(|n| n.parse::<usize>().ok())
        {
            let posts = self.posts.lock().unwrap();
            let (_, _, content) = posts.get(index).ok_or_else(unavailable)?;
            return Ok(json!({"sender":"@_hagency_agt_fixture:test","content":content}));
        }
        let log = self.answer_log.lock().unwrap();
        let (sender, _, content) = log
            .iter()
            .find(|(_, id, _)| id == event)
            .ok_or_else(unavailable)?;
        Ok(json!({"sender":sender,"content":content}))
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn questions_in_one_turn_share_one_wait() {
        let start = tokio::time::Instant::now();
        let minutes = |n: u64| Duration::from_secs(60 * n);
        let mut budget = WaitBudget::new(minutes(10));
        assert_eq!(budget.next(start), Some(minutes(10)));
        budget.start(start);
        // A second question while the first waits gets only what is left.
        assert_eq!(budget.next(start + minutes(3)), Some(minutes(7)));
        budget.stop(start + minutes(4));
        assert_eq!(
            budget.next(start + minutes(20)),
            Some(minutes(6)),
            "idle time is free"
        );
        budget.start(start + minutes(20));
        budget.stop(start + minutes(25));
        assert_eq!(budget.next(start + minutes(30)), Some(minutes(1)));
        budget.start(start + minutes(30));
        budget.stop(start + minutes(30) + Duration::from_secs(40));
        // Twenty seconds left of ten minutes is too little to ask anyone.
        assert_eq!(budget.next(start + minutes(31)), None);
    }
}

#[cfg(test)]
mod outbox_tests {
    use super::*;
    use hagency_runtime::codex::session::{UserInputOption, UserInputQuestion};
    use std::{collections::BTreeMap, sync::atomic::Ordering};

    const ROOM: &str = "!room:test";

    fn card(digit: char) -> Card {
        let question = UserInputQuestion {
            id: "db".into(),
            header: "Database".into(),
            question: "Which database should the service use?".into(),
            options: Some(vec![
                UserInputOption {
                    label: "Postgres".into(),
                    description: "Durable.".into(),
                },
                UserInputOption {
                    label: "SQLite".into(),
                    description: "Simple.".into(),
                },
            ]),
            is_other: true,
            is_secret: false,
        };
        Card::ask(
            format!("aq_{}", digit.to_string().repeat(64)),
            "$trigger".into(),
            "$root".into(),
            now_ms() + 600_000,
            &[question],
        )
        .unwrap()
    }
    async fn eventually(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..200 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("{what}");
    }
    fn edited(fixture: &Fixture) -> Vec<(String, Card)> {
        fixture
            .edits()
            .iter()
            .map(|e| {
                (
                    e["m.relates_to"]["event_id"].as_str().unwrap().to_owned(),
                    Card::from_content(&e["m.new_content"]).unwrap(),
                )
            })
            .collect()
    }

    struct Ask(RequestId);
    impl Asked for Ask {
        fn request_id(&self) -> &RequestId {
            &self.0
        }
    }

    #[test]
    fn a_question_withdrawn_in_the_same_batch_is_never_shown() {
        let ids = |updates: Vec<QuestionUpdate<Ask>>| -> Vec<String> {
            updates
                .into_iter()
                .map(|u| match u {
                    QuestionUpdate::Asked(Ask(id)) => format!("ask {id:?}"),
                    QuestionUpdate::Withdrawn(id) => format!("withdraw {id:?}"),
                })
                .collect()
        };
        let batch = vec![
            QuestionUpdate::Asked(Ask(RequestId::Number(1))),
            QuestionUpdate::Withdrawn(RequestId::Number(1)),
            QuestionUpdate::Withdrawn(RequestId::Number(7)),
            QuestionUpdate::Asked(Ask(RequestId::Number(2))),
        ];
        // Question 1 never becomes a card; 7 was asked in an earlier batch
        // and its card closes; 2 is asked after that, in order.
        assert_eq!(
            ids(in_order(batch)),
            ["withdraw Number(1)", "withdraw Number(7)", "ask Number(2)"]
        );
    }

    #[tokio::test]
    async fn common_cards_bind_joined_responders_and_allow_more_than_six_sequential_requests() {
        let fixture = Arc::new(Fixture::new(&["@requester:test", "@owner:test", "@other:test"], Duration::from_secs(60), None));
        let mut questions: Questions = Questions::default();
        questions.adopt((Arc::new(Source::Fixture(fixture.clone())), None));
        let context=QuestionContext {id:"dispatch".into(),room_id:ROOM.into(),event_id:"$trigger".into(),thread_root:"$root".into(),
            responders:vec!["@requester:test".into(),"@owner:test".into()]};
        for index in 0..7 {
            let request=UserInputRequest::parse(RequestId::Number(index),&json!({"threadId":"native-thread","turnId":"native-turn", "itemId":format!("item-{index}"),"isBlocking":true,
                "questions":[{"id":"q","header":"Pick","question":"Which one?","options":[{"label":"One","description":"First"},{"label":"Two","description":"Second"}]}]})).unwrap();
            assert!(questions.ask_on_device(&context,request).await.unwrap().is_none());
            let (event,content)=fixture.cards().last().unwrap().clone();
            let card=Card::from_content(&content).unwrap();
            let answers=[("q".into(),protocol::AnswerValue::Option("One".into()))].into();
            fixture.answer("@other:test",card.answer_content(&event,&answers).unwrap());
            assert!(questions.collect_ready().await.unwrap().is_empty(),"joined membership does not confer answer authority");
            fixture.answer(if index%2==0 {"@requester:test"}else{"@owner:test"},card.answer_content(&event,&answers).unwrap());
            let ready=questions.collect_ready().await.unwrap();
            assert_eq!(ready.len(),1);
            assert_eq!(ready[0].result(),&json!({"answers":{"q":{"answers":["One"]}}}));
            assert_eq!(fixture.edits().len(),index as usize,"no answered state before native acceptance");
            questions.delivered(ready[0].request_id(), true);
            questions.publish().await.unwrap();
            eventually("native receipt closes card",||fixture.edits().len()==index as usize+1).await;
        }
        assert_eq!(fixture.cards().len(),7);
        assert!(edited(&fixture).iter().all(|(_,card)|card.state==CardState::Answered));
    }

    #[tokio::test]
    async fn a_question_outside_any_thread_is_shown_under_its_own_message() {
        // An owner's direct conversation is not threaded: its requests name
        // the room as their root.
        let fixture = Arc::new(Fixture::new(&["@owner:test"], Duration::from_secs(60), None));
        let mut questions: Questions = Questions::default();
        questions.adopt((Arc::new(Source::Fixture(fixture.clone())), None));
        let context = QuestionContext {
            id: "dispatch".into(),
            room_id: ROOM.into(),
            event_id: "$trigger".into(),
            thread_root: ROOM.into(),
            responders: vec!["@owner:test".into()],
        };
        let request = UserInputRequest::parse(RequestId::Number(1), &json!({"threadId":"native-thread","turnId":"native-turn","itemId":"item","isBlocking":true,
            "questions":[{"id":"q","header":"Pick","question":"Which one?","options":[{"label":"One","description":"First"},{"label":"Two","description":"Second"}]}]})).unwrap();
        assert!(
            questions.ask_on_device(&context, request).await.unwrap().is_none(),
            "the question is asked, not answered with nothing"
        );
        let (event, content) = fixture.cards().last().unwrap().clone();
        // Where the run card of the same request already is.
        assert_eq!(content["m.relates_to"]["rel_type"], "m.thread");
        assert_eq!(content["m.relates_to"]["event_id"], "$trigger");
        let card = Card::from_content(&content).unwrap();
        assert_eq!(card.trigger_event_id, "$trigger");
        let answers = [("q".into(), protocol::AnswerValue::Option("Two".into()))].into();
        fixture.answer("@owner:test", card.answer_content(&event, &answers).unwrap());
        let ready = questions.collect_ready().await.unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].result(), &json!({"answers":{"q":{"answers":["Two"]}}}));
        // Finish the card like a real run does, so that no outcome is left in
        // the process-wide outbox when this test's runtime stops.
        questions.delivered(ready[0].request_id(), true);
        questions.publish().await.unwrap();
        eventually("the answered card is shown", || fixture.edits().len() == 1).await;
        assert_eq!(edited(&fixture)[0].1.state, CardState::Answered);
        // Any other root that is not an event stays refused.
        let request = UserInputRequest::parse(RequestId::Number(2), &json!({"threadId":"native-thread","turnId":"native-turn","itemId":"item-2","isBlocking":true,
            "questions":[{"id":"q","header":"Pick","question":"Which one?","options":[{"label":"One","description":"First"},{"label":"Two","description":"Second"}]}]})).unwrap();
        let elsewhere = QuestionContext { thread_root: "!other:test".into(), ..context };
        assert!(questions.ask_on_device(&elsewhere, request).await.unwrap().is_some());
        assert_eq!(fixture.cards().len(), 1);
    }

    #[tokio::test]
    async fn an_outcome_waits_for_a_device_that_can_show_it() {
        let fixture = Arc::new(Fixture::new(&[], Duration::from_secs(60), None));
        let source = Arc::new(Source::Fixture(fixture.clone()));
        let mut questions: Questions = Questions::default();
        questions.adopt((source.clone(), None));
        let card = card('a');
        let event = fixture.posted_earlier(ROOM, card.message_content());
        questions
            .settled
            .push((ROOM.into(), event.clone(), card.expired()));
        // The device's token was replaced: the outcome is kept, not dropped.
        fixture.revoked.store(true, Ordering::SeqCst);
        questions.publish().await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(fixture.edits().is_empty());
        // The Agent's device is in use again: the outcome goes out then.
        fixture.revoked.store(false, Ordering::SeqCst);
        device_ready(source, None);
        eventually("the outcome is shown once a device can", || {
            fixture.edits().len() == 1
        })
        .await;
        let shown = edited(&fixture);
        assert_eq!(shown[0].0, event);
        assert_eq!(shown[0].1.state, CardState::Expired);
    }

    #[tokio::test]
    async fn a_restart_closes_the_cards_an_earlier_process_left_open() {
        let directory = tempfile::tempdir().unwrap();
        let records = directory
            .path()
            .join("agent-question-cards")
            .join("agent.json");
        let fixture = Arc::new(Fixture::new(
            &["@alice:test"],
            Duration::from_secs(60),
            Some(records.clone()),
        ));
        let source = Arc::new(Source::Fixture(fixture.clone()));
        // An earlier process posted three cards and stopped. One was answered
        // (its edit never went out); one was still waiting; one belongs to a
        // run of this process and is still waiting now.
        let (waiting, answered, running) = (card('b'), card('c'), card('d'));
        let waiting_event = fixture.posted_earlier(ROOM, waiting.message_content());
        let answered_event = fixture.posted_earlier(ROOM, answered.message_content());
        let running_event = fixture.posted_earlier(ROOM, running.message_content());
        let pick = BTreeMap::from([(
            "db".to_owned(),
            protocol::AnswerValue::Option("SQLite".into()),
        )]);
        let answer_event = fixture.answer(
            "@alice:test",
            answered.answer_content(&answered_event, &pick).unwrap(),
        );
        let record = |event: &str| Record {
            room: ROOM.into(),
            event: event.into(),
            txn: None,
            posted_ms: now_ms(),
            answered_by: None,
            answer_event: None,
        };
        update_records(&records, |cards| {
            cards.push(record(&waiting_event));
            cards.push(Record {
                answered_by: Some("@alice:test".into()),
                answer_event: Some(answer_event.clone()),
                ..record(&answered_event)
            });
            cards.push(record(&running_event));
        });
        live(
            Some(&outbox_key(&source, Some(&records))),
            &running_event,
            true,
        );
        device_ready(source, Some(records.clone()));
        eventually("both cards an earlier process left open are closed", || {
            fixture.edits().len() == 2
        })
        .await;
        let shown: BTreeMap<String, Card> = edited(&fixture).into_iter().collect();
        assert_eq!(shown[&waiting_event].state, CardState::Expired);
        assert_eq!(
            shown[&answered_event].state,
            CardState::Answered,
            "an answered card is never shown as expired"
        );
        assert_eq!(
            shown[&answered_event].answered_by.as_deref(),
            Some("@alice:test")
        );
        assert_eq!(shown[&answered_event].answers, pick);
        assert!(
            !shown.contains_key(&running_event),
            "a card still waiting in this process stays open"
        );
        // Only the running card is still recorded, by its identifiers alone.
        let kept = read_records(&records);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].event, running_event);
        let bytes = std::fs::read(&records).unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains("database"),
            "no question text on disk"
        );
    }

    #[tokio::test]
    async fn only_a_delivered_runtime_answer_is_shown_as_answered() {
        for accepted in [false, true] {
            let fixture = Arc::new(Fixture::new(
                &["@alice:test"],
                Duration::from_secs(60),
                None,
            ));
            let source = Arc::new(Source::Fixture(fixture.clone()));
            let mut questions: Questions = Questions::default();
            questions.adopt((source, None));
            let unanswered = card(if accepted { 'e' } else { 'f' });
            let event = fixture.posted_earlier(ROOM, unanswered.message_content());
            let answers =
                BTreeMap::from([("db".into(), protocol::AnswerValue::Option("SQLite".into()))]);
            let answered = unanswered.answered("@alice:test", answers);
            live(questions.outbox.as_deref(), &event, true);
            questions.delivering.push(Delivery {
                request_id: RequestId::Number(42),
                room: ROOM.into(),
                card_event: event.clone(),
                unanswered,
                answered,
                answer_event: "$answer:test".into(),
            });
            questions.publish().await.unwrap();
            assert!(
                fixture.edits().is_empty(),
                "reading a room answer alone never settles the card"
            );
            questions.delivered(&RequestId::Number(42), accepted);
            questions.publish().await.unwrap();
            eventually("the callback outcome is shown", || {
                fixture.edits().len() == 1
            })
            .await;
            let closed = &edited(&fixture)[0].1;
            assert_eq!(
                closed.state,
                if accepted {
                    CardState::Answered
                } else {
                    CardState::Expired
                }
            );
            assert_eq!(closed.answered_by.is_some(), accepted);
            assert!(
                !LIVE
                    .lock()
                    .unwrap()
                    .contains(&(questions.outbox().unwrap(), event))
            );
        }
    }

    #[tokio::test]
    async fn dropping_a_post_in_progress_closes_the_room_card() {
        let directory = tempfile::tempdir().unwrap();
        let records = directory
            .path()
            .join("agent-question-cards")
            .join("question-records.json");
        let fixture = Arc::new(Fixture::new(
            &[],
            Duration::from_secs(60),
            Some(records.clone()),
        ));
        let source = Arc::new(Source::Fixture(fixture.clone()));
        let mut questions: Questions = Questions::default();
        questions.adopt((source, Some(records.clone())));
        let card = card('1');
        let txn = format!("hagency_question_{}", &card.id[3..]);
        let event = fixture
            .post(ROOM, &txn, card.message_content())
            .await
            .unwrap();
        assert!(update_records(&records, |cards| cards.push(Record {
            room: ROOM.into(),
            event: String::new(),
            txn: Some(txn.clone()),
            posted_ms: now_ms(),
            answered_by: None,
            answer_event: None,
        })));
        live(questions.outbox.as_deref(), &txn, true);
        let key = questions.outbox().unwrap();
        questions.posting = Some(Posting {
            room: ROOM.into(),
            txn: txn.clone(),
            card,
        });
        // The task died while sending; its returned event was never observed.
        drop(questions);
        eventually("drop closes the unconfirmed card", || {
            fixture.edits().len() == 1
        })
        .await;
        assert_eq!(edited(&fixture)[0].0, event);
        assert_eq!(edited(&fixture)[0].1.state, CardState::Expired);
        assert_eq!(
            fixture.cards().len(),
            1,
            "the transaction never duplicates a card"
        );
        assert!(!LIVE.lock().unwrap().contains(&(key, txn)));
        eventually("the recovery record is cleared", || {
            read_records(&records).is_empty()
        })
        .await;
    }

    #[tokio::test]
    async fn dropping_a_callback_in_progress_expires_instead_of_claiming_an_answer() {
        let fixture = Arc::new(Fixture::new(
            &["@alice:test"],
            Duration::from_secs(60),
            None,
        ));
        let source = Arc::new(Source::Fixture(fixture.clone()));
        let mut questions: Questions = Questions::default();
        questions.adopt((source, None));
        let unanswered = card('3');
        let event = fixture.posted_earlier(ROOM, unanswered.message_content());
        let answered = unanswered.answered(
            "@alice:test",
            BTreeMap::from([("db".into(), protocol::AnswerValue::Option("SQLite".into()))]),
        );
        live(questions.outbox.as_deref(), &event, true);
        let key = questions.outbox().unwrap();
        questions.delivering.push(Delivery {
            request_id: RequestId::Number(99),
            room: ROOM.into(),
            card_event: event.clone(),
            unanswered,
            answered,
            answer_event: "$answer:test".into(),
        });
        drop(questions);
        eventually("a canceled callback leaves a closed card", || {
            fixture.edits().len() == 1
        })
        .await;
        assert_eq!(edited(&fixture)[0].1.state, CardState::Expired);
        assert!(!LIVE.lock().unwrap().contains(&(key, event)));
    }

    #[tokio::test]
    async fn a_fresh_process_recovers_an_unconfirmed_card_transaction() {
        const CHILD_DIRECTORY: &str = "HAGENCY_QUESTION_RECOVERY_TEST_DIRECTORY";
        if let Some(directory) = std::env::var_os(CHILD_DIRECTORY) {
            let directory = PathBuf::from(directory);
            let records = directory.join("agent-question-cards").join("records.json");
            assert!(
                OUTBOX.lock().unwrap().is_empty(),
                "a fresh process has no outbox memory"
            );
            let fixture = Arc::new(Fixture::new(
                &[],
                Duration::from_secs(60),
                Some(records.clone()),
            ));
            let saved: (String, Value) =
                serde_json::from_slice(&std::fs::read(directory.join("room.json")).unwrap())
                    .unwrap();
            fixture.post(ROOM, &saved.0, saved.1).await.unwrap();
            device_ready(
                Arc::new(Source::Fixture(fixture.clone())),
                Some(records.clone()),
            );
            eventually("a fresh process closes the lost-response card", || {
                fixture.edits().len() == 1
            })
            .await;
            assert_eq!(fixture.cards().len(), 1);
            assert_eq!(edited(&fixture)[0].1.state, CardState::Expired);
            eventually("the recovered record is cleared", || {
                read_records(&records).is_empty()
            })
            .await;
            std::fs::write(
                directory.join("closed.json"),
                serde_json::to_vec(&fixture.edits()).unwrap(),
            )
            .unwrap();
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let records = directory
            .path()
            .join("agent-question-cards")
            .join("records.json");
        let card = card('2');
        let txn = format!("hagency_question_{}", &card.id[3..]);
        assert!(update_records(&records, |cards| cards.push(Record {
            room: ROOM.into(),
            event: String::new(),
            txn: Some(txn.clone()),
            posted_ms: now_ms(),
            answered_by: None,
            answer_event: None,
        })));
        // Only the fake room holds the question text. The executor's durable
        // record has identifiers, just as it did before the process died.
        let bytes = std::fs::read(&records).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("database"));
        std::fs::write(
            directory.path().join("room.json"),
            serde_json::to_vec(&(txn, card.message_content())).unwrap(),
        )
        .unwrap();
        let name = "console::native::shared_execution::questions::outbox_tests::a_fresh_process_recovers_an_unconfirmed_card_transaction";
        let output = tokio::time::timeout(
            Duration::from_secs(20),
            tokio::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", name, "--nocapture"])
                .env(CHILD_DIRECTORY, directory.path())
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "the child test really ran"
        );
        assert!(directory.path().join("closed.json").is_file());
        assert!(read_records(&records).is_empty());
    }

    #[test]
    fn unreadable_recovery_records_are_retained_without_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let records = directory
            .path()
            .join("agent-question-cards")
            .join("records.json");
        hagency_store::private::directory(records.parent().unwrap()).unwrap();
        hagency_store::private::replace(&records, b"partial-record").unwrap();
        assert!(!update_records(&records, |cards| cards.clear()));
        assert_eq!(std::fs::read(&records).unwrap(), b"partial-record");
    }
}
