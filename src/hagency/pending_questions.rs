//! Native questions Agents ask a room during a run,
//! shown as cards under the message that started the run.
//!
//! The cards come from the room itself. The Agent's device posts each card as
//! a message with a custom msgtype, which the timeline does not draw as a
//! message (other clients show its text body), and edits it once it is
//! answered or expired. The task identifies who may answer; the answer is a
//! custom room event, encrypted like any other in an encrypted room, which the
//! server never treats as a chat message, so it never queues behind the run.
//! The Agent takes the first valid answer from a named, authenticated current
//! member. The UI mirrors that audience; enforcement stays with the executor.
//!
//! A card is drawn only when it is what an executor posts: from an Agent
//! account of the room's own server, authenticated by Megolm in an encrypted
//! room (never a plaintext card there), first posted waiting, with an expiry
//! within one run's answer budget. Only that sender may settle it. A card or
//! edit whose keys or sender device are not known yet is tried again later.
//! Every open card under a message is shown, by its Agent, up to a bound.
use ::hagency::agent_questions::{self as protocol, AnswerValue, Card, CardState};
use makepad_widgets::*;
use matrix_sdk::{
    Client, Room,
    deserialized_responses::{
        DeviceLinkProblem, EncryptionInfo, TimelineEventKind, VerificationLevel, VerificationState,
    },
    ruma::{
        OwnedEventId, OwnedRoomId, OwnedUserId, RoomId, events::AnySyncTimelineEvent, serde::Raw,
    },
};
use futures_util::{stream, StreamExt};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::sync::{
    LazyLock, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

/// Cards kept for presentation, oldest dropped first.
const MAX_CARDS: usize = 256;
/// Cards drawn under one message: every open one first, in the order posted.
pub const MAX_CARDS_PER_MESSAGE: usize = 3;
/// Events tried again for keys or a sender device, and for how long.
const MAX_PARKED: usize = 64;
const PARKED_FOR: Duration =
    Duration::from_millis(protocol::ANSWER_WAIT_MS + protocol::EXPIRY_SKEW_MS);
const RETRY_EVERY: Duration = Duration::from_secs(5);
const RETRY_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY_CONCURRENCY: usize = 4;
/// Card events are small; anything larger is not one.
const MAX_EVENT_BYTES: usize = 48 * 1024;

/// What this client did with a card since it arrived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalAnswer {
    None,
    Sending,
    Sent,
    Failed(String),
}

/// One card as posted in a room, with this client's choices in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct QuestionCard {
    pub room_id: String,
    /// The card message itself, which answers refer to. It identifies the
    /// card here: two Agents can never post the same event.
    pub card_event: String,
    pub agent: String,
    pub agent_name: String,
    /// When the homeserver received the card.
    pub posted_ms: u64,
    /// Arrival order, for cards posted in the same millisecond.
    order: u64,
    /// Whether the timer already requested a redraw for this card's expiry.
    expiry_notified: bool,
    /// The latest state the Agent published (the original or its edit).
    pub card: Card,
    pub local: LocalAnswer,
    /// Question id to the chosen option label.
    pub choices: BTreeMap<String, String>,
    pub multiple: BTreeMap<String, Vec<String>>,
    /// Captured from the authenticated login when this card was admitted.
    pub may_answer: bool,
    /// Question id to typed text.
    pub texts: BTreeMap<String, String>,
}
impl QuestionCard {
    /// The card's identity in this store and in its widget: its event.
    pub fn key(&self) -> &str {
        &self.card_event
    }
    /// Buttons work only while the Agent waits and nothing was sent yet.
    pub fn open(&self, now_ms: u64) -> bool {
        self.may_answer && self.card.is_open(now_ms)
            && matches!(self.local, LocalAnswer::None | LocalAnswer::Failed(_))
    }
    /// The answers chosen so far, in the shape the Agent accepts.
    pub fn answers(&self) -> BTreeMap<String, AnswerValue> {
        let mut answers = BTreeMap::new();
        for q in &self.card.questions {
            if let Some(text) = self
                .texts
                .get(&q.id)
                .map(|t| t.trim())
                .filter(|t| !t.is_empty())
            {
                answers.insert(q.id.clone(), AnswerValue::Text(text.to_owned()));
            } else if let Some(labels) = self.multiple.get(&q.id).filter(|labels| !labels.is_empty()) {
                answers.insert(q.id.clone(), AnswerValue::Options(labels.clone()));
            } else if let Some(label) = self.choices.get(&q.id) {
                answers.insert(q.id.clone(), AnswerValue::Option(label.clone()));
            }
        }
        answers
    }
    /// Every question has a choice or typed text that the card accepts.
    pub fn ready(&self) -> bool {
        let answers = self.answers();
        answers.len() == self.card.questions.len()
            && self.card.answer_content(&self.card_event, &answers).is_ok()
    }
}

/// Answer content for the room, produced by a click and sent by the app.
#[derive(Clone, Debug)]
pub struct QuestionAnswerRequested {
    /// The login that created this answer; stale jobs cannot use a new login.
    pub generation: u64,
    pub room_id: String,
    /// The card answered (its event).
    pub key: String,
    pub agent: String,
    pub content: Value,
}
/// Posted after the answer reached the homeserver, or failed to.
#[derive(Debug)]
pub struct QuestionAnswerSent {
    pub generation: u64,
    pub key: String,
    pub result: Result<(), String>,
}
/// Posted whenever cards arrive or change, so open timelines repopulate.
#[derive(Debug, Clone, Copy)]
pub struct PendingQuestionsChanged;

/// An event to try again: undecryptable so far, or from a sender device this
/// client did not know yet.
#[derive(Clone)]
struct Parked {
    room_id: String,
    event_id: String,
    /// The decrypted event, when only its sender device was unknown.
    decrypted: Option<(Value, String)>,
    since: Instant,
}

#[derive(Default)]
struct State {
    generation: u64,
    owner: String,
    cards: Vec<QuestionCard>,
    arrivals: u64,
    /// Edits seen before their card, applied when the card arrives.
    early_edits: Vec<(String, String, String, Card)>,
    announced: HashSet<String>,
    parked: Vec<Parked>,
}
static STATE: LazyLock<Mutex<State>> = LazyLock::new(Default::default);
/// Bumped for every new client: an older client's retry loop ends.
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Collect cards from this client's own sync. A new client (another account,
/// or a re-login) starts with no cards.
pub fn attach(client: &Client) {
    let generation = reset(client.user_id().map(|u| u.to_string()).unwrap_or_default());
    client.add_event_handler(
        move |event: Raw<AnySyncTimelineEvent>,
              room: Room,
              info: Option<EncryptionInfo>,
              client: Client| async move {
            if event.json().get().len() > MAX_EVENT_BYTES || !current(generation) {
                return;
            }
            let Ok(value) = event.deserialize_as::<Value>() else {
                return;
            };
            if consider(generation, &client, &room, &value, info.as_ref()).await {
                Cx::post_action(PendingQuestionsChanged);
            }
        },
    );
    let client = client.clone();
    crate::sliding_sync::spawn_async_task(async move {
        while current(generation) {
            tokio::time::sleep(RETRY_EVERY).await;
            retry(&client, generation).await;
        }
    });
    // Redraw expiry even when no sync events arrive, or retries are waiting
    // on a broken network. Keep the Agent's published state intact: a late
    // authenticated edit can still tell us which answer it accepted.
    crate::sliding_sync::spawn_async_task(async move {
        while current(generation) {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if expiry_changed(generation, now_ms()) {
                Cx::post_action(PendingQuestionsChanged);
            }
        }
    });
}

fn current(generation: u64) -> bool {
    GENERATION.load(Ordering::SeqCst) == generation
}

fn with_state<T>(generation: u64, f: impl FnOnce(&mut State) -> T) -> Option<T> {
    let mut state = STATE.lock().ok()?;
    (state.generation == generation).then(|| f(&mut state))
}

fn expiry_changed(generation: u64, now: u64) -> bool {
    with_state(generation, |state| {
        let mut changed = false;
        for card in &mut state.cards {
            if card.card.state == CardState::Pending
                && now >= card.card.expires_at_ms
                && !card.expiry_notified
            {
                card.expiry_notified = true;
                changed = true;
            }
        }
        changed
    })
    .unwrap_or(false)
}

/// The homeserver the room's own Agents are on: the room's, else this user's.
fn room_server(client: &Client, room: &Room) -> Option<String> {
    protocol::server_name(room.room_id().as_str())
        .map(str::to_owned)
        .or_else(|| client.user_id().map(|u| u.server_name().to_string()))
}

/// Takes one timeline event if it is a card or a card's edit an executor
/// could have posted. Returns whether the shown cards changed.
async fn consider(
    generation: u64,
    client: &Client,
    room: &Room,
    value: &Value,
    info: Option<&EncryptionInfo>,
) -> bool {
    if !current(generation) {
        return false;
    }
    let Some(server) = room_server(client, room) else {
        return false;
    };
    let Some(sender) = value["sender"]
        .as_str()
        .filter(|s| protocol::is_agent_on(s, &server))
    else {
        return false;
    };
    let relevant = value["type"] == "m.room.encrypted"
        || (value["type"] == "m.room.message" && (value["content"]["msgtype"] == protocol::MSGTYPE
                || value["content"]["msgtype"] == ::hagency::agent_run_controls::MSGTYPE));
    if !relevant {
        return false;
    }
    let plaintext =
        info.is_none() && value["type"] != "m.room.encrypted" && plaintext_room(room).await;
    let name = match OwnedUserId::try_from(sender) {
        Ok(user) => room
            .get_member_no_sync(&user)
            .await
            .ok()
            .flatten()
            .and_then(|m| m.display_name().map(str::to_owned))
            .unwrap_or_else(|| user.localpart().to_owned()),
        Err(_) => return false,
    };
    accept_event(
        generation,
        room.room_id().as_str(),
        &server,
        &name,
        value,
        info,
        plaintext,
    )
}

/// The same admission boundary handles live sync and a successful retry.
/// `plaintext` means the SDK positively knows this room is unencrypted.
fn accept_event(
    generation: u64,
    room_id: &str,
    server: &str,
    name: &str,
    value: &Value,
    info: Option<&EncryptionInfo>,
    plaintext: bool,
) -> bool {
    let (Some(sender), Some(event_id)) = (value["sender"].as_str(), value["event_id"].as_str())
    else {
        return false;
    };
    if !protocol::is_agent_on(sender, server)
        || OwnedEventId::try_from(event_id).is_err()
        || value
            .get("room_id")
            .is_some_and(|r| r.as_str() != Some(room_id))
        || serde_json::to_vec(value).map_or(true, |v| v.len() > MAX_EVENT_BYTES)
    {
        return false;
    }
    if value["type"] == "m.room.encrypted" {
        // Relation metadata is optional in ciphertext. Until it decrypts we
        // cannot tell whether it is a card; retain only this bounded queue.
        park(generation, room_id, event_id, None);
        return false;
    }
    if value["type"] != "m.room.message" || (value["content"]["msgtype"] != protocol::MSGTYPE
            && value["content"]["msgtype"] != ::hagency::agent_run_controls::MSGTYPE)
    {
        unpark(generation, room_id, event_id);
        return false;
    }
    match info {
        None if !plaintext => return false,
        None => {}
        Some(info) if protocol::authentic(info, sender) => {}
        Some(info) => {
            if device_unknown(info)
                && let Some(session) = info.session_id()
            {
                park(
                    generation,
                    room_id,
                    event_id,
                    Some((value.clone(), session.to_owned())),
                );
            } else {
                unpark(generation, room_id, event_id);
            }
            return false;
        }
    }
    unpark(generation, room_id, event_id);
    let content = &value["content"];
    if content["msgtype"] == ::hagency::agent_run_controls::MSGTYPE {
        return super::run_controls::authenticated(
            generation, room_id, event_id, sender, name, content,
        );
    }
    if content["m.relates_to"]["rel_type"] == "m.replace" {
        let Some(target) = content["m.relates_to"]["event_id"].as_str() else {
            return false;
        };
        let Some(card) = Card::from_content(&content["m.new_content"]) else {
            return false;
        };
        return edited_in(generation, room_id, target, sender, card);
    }
    let Some(card) = Card::from_content(content) else {
        return false;
    };
    let Some(posted_ms) = value["origin_server_ts"].as_u64() else {
        return false;
    };
    posted_in(generation, room_id, event_id, sender, name, posted_ms, card)
}

/// Whether the room is known not to be encrypted.
async fn plaintext_room(room: &Room) -> bool {
    let state = room.encryption_state();
    let state = if state.is_unknown() {
        match tokio::time::timeout(Duration::from_secs(5), room.latest_encryption_state()).await {
            Ok(Ok(state)) => state,
            _ => return false,
        }
    } else {
        state
    };
    !state.is_unknown() && !state.is_encrypted()
}

/// Megolm could not tell which device sent it: that device's keys are not
/// known here yet.
fn device_unknown(info: &EncryptionInfo) -> bool {
    matches!(
        info.verification_state,
        VerificationState::Unverified(VerificationLevel::None(DeviceLinkProblem::MissingDevice))
    )
}

fn park(generation: u64, room_id: &str, event_id: &str, decrypted: Option<(Value, String)>) {
    with_state(generation, |state| {
        if let Some(parked) = state
            .parked
            .iter_mut()
            .find(|p| p.room_id == room_id && p.event_id == event_id)
        {
            // Keys arrived, but the device list has not. Keep the original
            // retention deadline rather than extending it on every retry.
            if decrypted.is_some() {
                parked.decrypted = decrypted;
            }
            return;
        }
        state.parked.push(Parked {
            room_id: room_id.into(),
            event_id: event_id.into(),
            decrypted,
            since: Instant::now(),
        });
        let excess = state.parked.len().saturating_sub(MAX_PARKED);
        state.parked.drain(..excess);
    });
}
fn unpark(generation: u64, room_id: &str, event_id: &str) {
    with_state(generation, |state| {
        state
            .parked
            .retain(|p| p.room_id != room_id || p.event_id != event_id)
    });
}

/// A bounded batch; cancellation and every mutation are tied to the login
/// that queued the event. A slow event cannot block all later events.
async fn retry_with<F, Fut, N>(generation: u64, timeout: Duration, mut notify: N, fetch: F) -> bool
where
    F: Fn(Parked) -> Fut,
    Fut: std::future::Future<Output = bool>,
    N: FnMut(),
{
    let due = with_state(generation, |state| {
        state.parked.retain(|p| p.since.elapsed() < PARKED_FOR);
        state.parked.clone()
    })
    .unwrap_or_default();
    let mut attempts = stream::iter(due)
        .map(|parked| {
            let future = fetch(parked);
            async move {
                if !current(generation) {
                    return false;
                }
                tokio::time::timeout(timeout, future).await.unwrap_or(false)
            }
        })
        .buffer_unordered(RETRY_CONCURRENCY);
    let mut changed = false;
    while let Some(result) = attempts.next().await {
        if !current(generation) {
            return false;
        }
        if result {
            // Make a recovered card visible immediately. Other attempts may
            // still be blocked for many batches of network timeouts, longer
            // than the recovered card has left for an answer. Reset holds
            // this same lock, so an old login cannot enqueue a notification
            // after a new login has replaced its state.
            if with_state(generation, |_| notify()).is_none() {
                return false;
            }
            changed = true;
        }
    }
    changed
}

async fn retry(client: &Client, generation: u64) -> bool {
    retry_with(
        generation,
        RETRY_TIMEOUT,
        || Cx::post_action(PendingQuestionsChanged),
        |parked| retry_event(client, generation, parked),
    )
    .await
}

async fn retry_event(client: &Client, generation: u64, parked: Parked) -> bool {
    let Parked {
        room_id,
        event_id,
        decrypted,
        ..
    } = parked;
    let Some(room) = OwnedRoomId::try_from(room_id.as_str())
        .ok()
        .and_then(|id| client.get_room(&id))
    else {
        unpark(generation, &room_id, &event_id);
        return false;
    };
    match decrypted {
        Some((value, session)) => {
            let Some(sender) = value["sender"]
                .as_str()
                .and_then(|s| OwnedUserId::try_from(s).ok())
            else {
                unpark(generation, &room_id, &event_id);
                return false;
            };
            let mut info = room.get_encryption_info(&session, &sender).await;
            if info.as_ref().is_none_or(|info| device_unknown(info)) {
                // Refresh the device list inside this bounded attempt, not
                // as an unbounded task for every repeated sync event.
                let _ = client.encryption().request_user_identity(&sender).await;
                info = room.get_encryption_info(&session, &sender).await;
            }
            if let Some(info) = info {
                return consider(generation, client, &room, &value, Some(&info)).await;
            }
        }
        None => {
            let Ok(id) = OwnedEventId::try_from(event_id.as_str()) else {
                unpark(generation, &room_id, &event_id);
                return false;
            };
            if let Ok(event) = room.event(&id, None).await
                && let TimelineEventKind::Decrypted(decrypted) = &event.kind
                && let Ok(value) = decrypted.event.deserialize_as::<Value>()
                && value["event_id"].as_str() == Some(event_id.as_str())
            {
                return consider(
                    generation,
                    client,
                    &room,
                    &value,
                    Some(&decrypted.encryption_info),
                )
                .await;
            }
        }
    }
    false
}

fn reset(owner: String) -> u64 {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
    super::run_controls::reset(generation, &owner);
    *state = State {
        owner,
        generation,
        ..Default::default()
    };
    generation
}
pub fn forget_identity() {
    reset(String::new());
}
#[cfg(feature = "onboarding-test-support")]
pub fn review_identity(owner: &str) { reset(owner.into()); }

/// A card message arrived. Returns whether anything changed. Only a card an
/// executor posts is taken: first posted waiting, and waiting at most one
/// run's answer budget from when the homeserver got it.
pub fn posted(
    room_id: &str,
    card_event: &str,
    agent: &str,
    agent_name: &str,
    posted_ms: u64,
    card: Card,
) -> bool {
    posted_in(
        GENERATION.load(Ordering::SeqCst),
        room_id,
        card_event,
        agent,
        agent_name,
        posted_ms,
        card,
    )
}
fn posted_in(
    generation: u64,
    room_id: &str,
    card_event: &str,
    agent: &str,
    agent_name: &str,
    posted_ms: u64,
    card: Card,
) -> bool {
    if card.state != CardState::Pending || !card.expiry_plausible(posted_ms) {
        return false;
    }
    let Ok(mut state) = STATE.lock() else {
        return false;
    };
    if state.generation != generation {
        return false;
    }
    if state.cards.iter().any(|c| c.card_event == card_event) {
        return false;
    }
    // The same Agent's same card from another event (a retried post) is
    // never a second card. Another Agent's card is its own, whatever its id.
    if state
        .cards
        .iter()
        .any(|c| c.agent == agent && c.card.id == card.id)
    {
        return false;
    }
    state.arrivals += 1;
    let mut entry = QuestionCard {
        room_id: room_id.into(),
        card_event: card_event.into(),
        agent: agent.into(),
        agent_name: agent_name.into(),
        posted_ms,
        order: state.arrivals,
        expiry_notified: false,
        may_answer: card.may_answer(&state.owner),
        card,
        local: LocalAnswer::None,
        choices: BTreeMap::new(),
        multiple: BTreeMap::new(),
        texts: BTreeMap::new(),
    };
    let early: Vec<_> = state
        .early_edits
        .iter()
        .filter(|(room, target, sender, _)| {
            room == room_id && target == card_event && sender == agent
        })
        .map(|(.., card)| card.clone())
        .collect();
    state
        .early_edits
        .retain(|(room, target, ..)| !(room == room_id && target == card_event));
    for card in early {
        apply_edit(&mut entry, card);
    }
    state.cards.push(entry);
    let excess = state.cards.len().saturating_sub(MAX_CARDS);
    let dropped: Vec<_> = state.cards.drain(..excess).map(|c| c.card_event).collect();
    for key in dropped {
        state.announced.remove(&key);
    }
    true
}

/// The Agent edited its card (answered or expired). Only the card's own
/// sender can change it, and only to a settled state of the same card.
pub fn edited(room_id: &str, card_event: &str, sender: &str, card: Card) -> bool {
    edited_in(
        GENERATION.load(Ordering::SeqCst),
        room_id,
        card_event,
        sender,
        card,
    )
}
fn edited_in(generation: u64, room_id: &str, card_event: &str, sender: &str, card: Card) -> bool {
    if card.state == CardState::Pending {
        return false;
    }
    let Ok(mut state) = STATE.lock() else {
        return false;
    };
    if state.generation != generation {
        return false;
    }
    let Some(entry) = state
        .cards
        .iter_mut()
        .find(|c| c.room_id == room_id && c.card_event == card_event)
    else {
        if state.early_edits.len() < 64 {
            state
                .early_edits
                .push((room_id.into(), card_event.into(), sender.into(), card));
        }
        return false;
    };
    if entry.agent != sender {
        return false;
    }
    apply_edit(entry, card)
}
fn apply_edit(entry: &mut QuestionCard, card: Card) -> bool {
    let same = card.id == entry.card.id
        && card.version == entry.card.version
        && card.responders == entry.card.responders
        && card.trigger_event_id == entry.card.trigger_event_id
        && card.thread_root == entry.card.thread_root
        && card.expires_at_ms == entry.card.expires_at_ms
        && card.questions == entry.card.questions
        && card.withheld == entry.card.withheld;
    if !same || card.state == CardState::Pending || entry.card.state != CardState::Pending {
        return false;
    }
    entry.card = card;
    true
}

/// The cards raised by the message `event_id` in `room`, each Agent's card
/// its own: every open one first, then settled ones, each in the order they
/// were posted, at most [`MAX_CARDS_PER_MESSAGE`]. In the room timeline a
/// thread's root also carries its thread's cards, since the triggering reply
/// is not shown there.
pub fn for_event(room: &RoomId, event_id: &str, include_thread_root: bool) -> Vec<QuestionCard> {
    let Ok(state) = STATE.lock() else {
        return Vec::new();
    };
    let now = now_ms();
    let mut matching: Vec<&QuestionCard> = state
        .cards
        .iter()
        .filter(|c| c.room_id == room.as_str())
        .filter(|c| {
            c.card.trigger_event_id == event_id
                || (include_thread_root
                    && c.card.thread_root == event_id
                    && c.card.thread_root != c.card.trigger_event_id)
        })
        .collect();
    matching.sort_by_key(|c| (!c.card.is_open(now), c.posted_ms, c.order));
    matching
        .into_iter()
        .take(MAX_CARDS_PER_MESSAGE)
        .cloned()
        .collect()
}
pub fn get(key: &str) -> Option<QuestionCard> {
    STATE
        .lock()
        .ok()?
        .cards
        .iter()
        .find(|c| c.card_event == key)
        .cloned()
}

fn with_card<T>(key: &str, f: impl FnOnce(&mut QuestionCard) -> T) -> Option<T> {
    let mut state = STATE.lock().ok()?;
    state.cards.iter_mut().find(|c| c.card_event == key).map(f)
}

/// A click on an option. On a card with one question the click is the
/// answer itself; otherwise it marks the choice until Send.
pub fn choose(key: &str, question: &str, label: &str) -> Option<QuestionAnswerRequested> {
    let single = with_card(key, |c| {
        if !c.open(now_ms())
            || !c
                .card
                .questions
                .iter()
                .any(|q| q.id == question && q.options.iter().any(|o| o.label == label))
        {
            return None;
        }
        c.texts.remove(question);
        let multi = c.card.questions.iter().any(|q| q.id == question && q.multi_select);
        if multi {
            let labels = c.multiple.entry(question.into()).or_default();
            if let Some(index) = labels.iter().position(|value| value == label) { labels.remove(index); }
            else { labels.push(label.into()); }
        } else { c.choices.insert(question.into(), label.into()); }
        Some(c.card.questions.len() == 1 && !multi)
    })??;
    if single { submit(key) } else { None }
}
/// Typed text replaces the question's chosen option.
pub fn type_text(key: &str, question: &str, text: &str) {
    with_card(key, |c| {
        if !c.open(now_ms())
            || !c
                .card
                .questions
                .iter()
                .any(|q| q.id == question && q.accepts_text())
        {
            return;
        }
        if text.trim().is_empty() {
            c.texts.remove(question);
        } else {
            c.choices.remove(question);
            c.multiple.remove(question);
            c.texts.insert(question.into(), text.into());
        }
    });
}
/// The answer to send now, once every question has one. Marks it sending so
/// a second click cannot send it twice.
pub fn submit(key: &str) -> Option<QuestionAnswerRequested> {
    with_card(key, |c| {
        if !c.open(now_ms()) || !c.ready() {
            return None;
        }
        let content = c.card.answer_content(&c.card_event, &c.answers()).ok()?;
        c.local = LocalAnswer::Sending;
        Some(QuestionAnswerRequested {
            generation: GENERATION.load(Ordering::SeqCst),
            room_id: c.room_id.clone(),
            key: c.card_event.clone(),
            agent: c.agent.clone(),
            content,
        })
    })?
}
/// Record the outcome of an answer this client sent.
pub fn sent(update: &QuestionAnswerSent) -> bool {
    with_state(update.generation, |state| {
        let Some(c) = state.cards.iter_mut().find(|c| c.card_event == update.key) else {
            return false;
        };
        c.local = match &update.result {
            Ok(()) => LocalAnswer::Sent,
            Err(code) => LocalAnswer::Failed(code.clone()),
        };
        true
    })
    .unwrap_or(false)
}

/// Send one answer as a room event, encrypted whenever the room is.
pub fn send(request: QuestionAnswerRequested) {
    // Capture the actual client now. An answer queued before logout must
    // never acquire whichever client happens to be signed in later.
    let client = crate::sliding_sync::get_client();
    crate::sliding_sync::spawn_async_task(async move {
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            let client = client.ok_or("matrix_sign_in_required")?;
            let owner = client
                .user_id()
                .map(|u| u.as_str())
                .ok_or("matrix_sign_in_required")?;
            if !with_state(request.generation, |s| s.owner == owner).unwrap_or(false) {
                return Err("matrix_identity_changed");
            }
            let room_id =
                OwnedRoomId::try_from(request.room_id.as_str()).map_err(|_| "room_unavailable")?;
            let room = client.get_room(&room_id).ok_or("room_unavailable")?;
            // The room's own encryption state must be known first; an unknown
            // state could otherwise send plaintext into an encrypted room.
            let encryption = room
                .latest_encryption_state()
                .await
                .map_err(|_| "room_state_unavailable")?;
            if encryption.is_unknown() {
                return Err("room_state_unavailable");
            }
            // Fresh device keys for the Agent, so its device can read the answer.
            if let Ok(agent) = OwnedUserId::try_from(request.agent.as_str()) {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    client.encryption().request_user_identity(&agent),
                )
                .await;
            }
            if !current(request.generation) {
                return Err("matrix_identity_changed");
            }
            // One transaction per card and answer: a retry of the same answer
            // returns the original event instead of sending a second one.
            let digest = blake3::hash(format!("{}\n{}", request.key, request.content).as_bytes());
            let txn: matrix_sdk::ruma::OwnedTransactionId =
                format!("hagency_answer_{}", &digest.to_hex()[..32]).into();
            room.send_raw(protocol::ANSWER_EVENT, request.content.clone())
                .with_transaction_id(&txn)
                .await
                .map(|_| ())
                .map_err(|_| "answer_send_failed")
        })
        .await
        .unwrap_or(Err("answer_send_failed"))
        .map_err(str::to_owned);
        Cx::post_action(QuestionAnswerSent {
            generation: request.generation,
            key: request.key,
            result,
        });
    });
}

/// New open cards to announce once each, with the Agent's name.
pub fn take_announcements() -> Vec<(String, String)> {
    let Ok(mut state) = STATE.lock() else {
        return Vec::new();
    };
    let now = now_ms();
    let fresh: Vec<(String, String)> = state
        .cards
        .iter()
        .filter(|c| {
            c.open(now) && !state.announced.contains(&c.card_event) && c.agent != state.owner
        })
        .map(|c| (c.card_event.clone(), c.agent_name.clone()))
        .collect();
    for (key, _) in &fresh {
        state.announced.insert(key.clone());
    }
    fresh
}

/// Everything the card widget draws for one card at `now_ms`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionView {
    /// The card's key (its event), which clicks refer to.
    pub key: String,
    pub title: String,
    pub status: String,
    pub open: bool,
    pub questions: Vec<QuestionRow>,
    pub send_visible: bool,
    pub can_send: bool,
    pub hint: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionRow {
    pub id: String,
    pub header: String,
    pub question: String,
    /// Option labels with whether each is the current choice.
    pub options: Vec<(String, bool)>,
    /// The options' descriptions, one per line.
    pub details: String,
    /// The typed text, when the question takes text and the card is open.
    pub text: Option<String>,
    /// The published answer, once answered.
    pub answer: Option<String>,
}

pub fn view(c: &QuestionCard, now_ms: u64) -> QuestionView {
    let open = c.open(now_ms);
    let expired = c.card.state == CardState::Expired
        || (c.card.state == CardState::Pending && now_ms >= c.card.expires_at_ms);
    use crate::i18n::{format as tf, tr};
    let status = match (&c.card.state, &c.local) {
        (CardState::Answered, _) => tf(
            "Answered by {0}",
            &[("0", c.card.answered_by.clone().unwrap_or_default())],
        ),
        _ if expired => tr("Expired · the Agent continued on its own").to_owned(),
        (_, LocalAnswer::Sending) => tr("Sending…").to_owned(),
        (_, LocalAnswer::Sent) => tr("Sent · waiting for the Agent").to_owned(),
        (_, LocalAnswer::Failed(_)) => tr("Not sent · try again").to_owned(),
        _ if !c.may_answer => tr("Waiting for an authorized participant").to_owned(),
        _ => tr("Waiting for an answer").to_owned(),
    };
    let questions = c
        .card
        .questions
        .iter()
        .map(|q| QuestionRow {
            id: q.id.clone(),
            header: if q.multi_select {
                if q.header.is_empty() { tr("Select multiple").into() }
                else { format!("{} · {}",q.header,tr("Select multiple")) }
            } else { q.header.clone() },
            question: q.question.clone(),
            options: q
                .options
                .iter()
                .map(|o| {
                    (o.label.clone(), c.choices.get(&q.id) == Some(&o.label)
                    || c.multiple.get(&q.id).is_some_and(|labels| labels.contains(&o.label)),
                    )
                })
                .collect(),
            details: q
                .options
                .iter()
                .filter(|o| !o.description.is_empty())
                .map(|o| format!("{} — {}", o.label, o.description))
                .collect::<Vec<_>>()
                .join("\n"),
            text: (open && q.accepts_text())
                .then(|| c.texts.get(&q.id).cloned().unwrap_or_default()),
            answer: (c.card.state == CardState::Answered).then(|| {
                match c.card.answers.get(&q.id) {
                    Some(value) => value.display(),
                    None => "—".to_owned(),
                }
            }),
        })
        .collect::<Vec<_>>();
    let typed = c.card.questions.iter().any(|q| q.accepts_text());
    let mut hint = if c.card.is_open(now_ms) {
        use chrono::TimeZone;
        // Questions in one run share one wait, so a card states its own end.
        let by = chrono::Local
            .timestamp_millis_opt(c.card.expires_at_ms as i64)
            .single()
            .map(|t| t.format("%H:%M").to_string())
            .unwrap_or_default();
        let message = if c.card.responders.is_empty() {
            "Any member of this room can answer. With no answer by {0}, the Agent continues on its own."
        } else {
            "Only the named participants can answer. With no answer by {0}, the Agent continues on its own."
        };
        let mut hint = tf(message,
            &[("0", by)],
        );
        if !c.card.responders.is_empty() { hint.push_str(&format!("\n{}", c.card.responders.join(", "))); }
        if open && c.card.questions.iter().any(|q| q.multi_select) {
            hint.push('\n'); hint.push_str(tr("Select one or more options, then send your answer."));
        }
        hint
    } else {
        String::new()
    };
    if c.card.withheld > 0 && c.card.state == CardState::Pending {
        if !hint.is_empty() {
            hint.push('\n');
        }
        hint.push_str(tr(
            "A private question was not shown here and gets no answer.",
        ));
    }
    QuestionView {
        key: c.card_event.clone(),
        title: tf("{0} asks", &[("0", c.agent_name.clone())]),
        status,
        open,
        send_visible: open && (questions.len() > 1 || typed || c.card.questions.iter().any(|q| q.multi_select)),
        can_send: open && c.ready(),
        questions,
        hint,
    }
}

/// A short notification naming the Agent, never the question text itself.
pub fn announcement(agent_name: &str) -> String {
    crate::i18n::format(
        "{0} asks a question. Open the room to answer it under your message.",
        &[("0", agent_name.to_owned())],
    )
}

#[cfg(test)]
pub(crate) fn test_reset() {
    reset("@me:example.test".into());
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::hagency::agent_questions::{CardOption, CardQuestion};

    fn card(questions: usize) -> Card {
        let question = |id: &str| CardQuestion {
            id: id.into(),
            header: "Database".into(),
            question: format!("Which database for {id}?"),
            options: vec![
                CardOption {
                    label: "Postgres".into(),
                    description: "Durable.".into(),
                },
                CardOption {
                    label: "SQLite".into(),
                    description: String::new(),
                },
            ],
            other: true,
            multi_select: false,
        };
        Card {
            version: 1,
            id: format!("aq_{}", "c".repeat(64)),
            trigger_event_id: "$trigger".into(),
            thread_root: "$root".into(),
            expires_at_ms: now_ms() + 600_000,
            questions: (0..questions).map(|i| question(&format!("q{i}"))).collect(),
            responders: Vec::new(),
            withheld: 0,
            state: CardState::Pending,
            answered_by: None,
            answers: BTreeMap::new(),
        }
    }
    const AGENT: &str = "@_hagency_agt_one:example.test";
    const OTHER: &str = "@_hagency_agt_two:example.test";

    #[test]
    fn scoped_multiselect_requires_send_and_never_admits_other_members() {
        let _guard = serial();
        test_reset();
        let mut data=card(4);
        data.version=protocol::SCOPED_VERSION;
        data.responders=vec!["@me:example.test".into()];
        data.questions[0].multi_select=true;
        assert!(posted("!room:example.test","$multi",AGENT,"Agent",now_ms(),data.clone()));
        assert!(choose("$multi","q0","Postgres").is_none());
        assert!(choose("$multi","q0","SQLite").is_none());
        assert!(choose("$multi","q0","Postgres").is_none());
        for index in 1..4 { assert!(choose("$multi",&format!("q{index}"),"Postgres").is_none()); }
        let shown = view(&get("$multi").unwrap(), now_ms());
        assert_eq!(shown.questions.len(), 4);
        assert!(shown.send_visible && shown.can_send);
        assert_eq!(shown.questions[0].options,[("Postgres".into(),false),("SQLite".into(),true)]);
        let sent=submit("$multi").unwrap();
        assert_eq!(sent.content["answers"]["q0"],serde_json::json!({"options":["SQLite"]}));
        assert!(submit("$multi").is_none());
        reset("@other:example.test".into());
        assert!(posted("!room:example.test","$multi",AGENT,"Agent",now_ms(),data));
        assert!(!view(&get("$multi").unwrap(),now_ms()).open);
        assert!(choose("$multi","q1","Postgres").is_none());
        assert!(submit("$multi").is_none());
    }

    #[test]
    fn card_edit_cannot_change_the_authorized_answering_identity() {
        let _guard = serial();
        test_reset();
        let mut data = card(1);
        data.version=protocol::SCOPED_VERSION;
        data.responders=vec!["@me:example.test".into()];
        posted("!room:example.test","$scoped",AGENT,"Agent",now_ms(),data.clone());
        let mut edit = data.expired();
        edit.responders=vec!["@other:example.test".into()];
        assert!(!edited("!room:example.test","$scoped",AGENT,edit));
        assert_eq!(get("$scoped").unwrap().card.responders,data.responders);
    }

    #[test]
    fn single_question_click_is_the_answer_and_sends_once() {
        let _guard = serial();
        test_reset();
        let room = RoomId::parse("!room:example.test").unwrap();
        assert!(posted(
            room.as_str(),
            "$card",
            AGENT,
            "小黄瓜",
            now_ms(),
            card(1)
        ));
        assert!(
            !posted(room.as_str(), "$card", AGENT, "小黄瓜", now_ms(), card(1)),
            "the same card arrives once"
        );
        let shown = for_event(&room, "$trigger", false).remove(0);
        let v = view(&shown, now_ms());
        assert!(
            v.open && v.send_visible,
            "typed Other text needs the Send button"
        );
        assert_eq!(
            v.questions[0].options,
            vec![("Postgres".into(), false), ("SQLite".into(), false)]
        );
        assert!(
            for_event(&room, "$root", false).is_empty(),
            "a thread timeline matches the trigger only"
        );
        assert_eq!(
            for_event(&room, "$root", true).len(),
            1,
            "the room timeline shows it on the thread root"
        );
        let request =
            choose(shown.key(), "q0", "SQLite").expect("one click answers a one-question card");
        assert_eq!(
            request.content["answers"],
            serde_json::json!({"q0":{"option":"SQLite"}})
        );
        assert_eq!(request.content["m.relates_to"]["event_id"], "$card");
        assert!(
            choose(shown.key(), "q0", "Postgres").is_none(),
            "nothing more while sending"
        );
        assert!(submit(shown.key()).is_none());
        sent(&QuestionAnswerSent {
            generation: GENERATION.load(Ordering::SeqCst),
            key: shown.key().into(),
            result: Err("answer_send_failed".into()),
        });
        assert!(
            view(&get(shown.key()).unwrap(), now_ms())
                .status
                .starts_with("Not sent")
        );
        assert!(
            choose(shown.key(), "q0", "Postgres").is_some(),
            "a failed send can be retried"
        );
        sent(&QuestionAnswerSent {
            generation: GENERATION.load(Ordering::SeqCst),
            key: shown.key().into(),
            result: Ok(()),
        });
        assert!(
            view(&get(shown.key()).unwrap(), now_ms())
                .status
                .starts_with("Sent")
        );
    }

    #[test]
    fn several_questions_wait_for_send_and_typed_text_replaces_a_choice() {
        let _guard = serial();
        test_reset();
        posted(
            "!room:example.test",
            "$card",
            AGENT,
            "Agent",
            now_ms(),
            card(2),
        );
        let key = "$card";
        assert!(
            choose(key, "q0", "Postgres").is_none(),
            "a choice is not yet the answer"
        );
        assert!(!get(key).unwrap().ready());
        assert!(submit(key).is_none());
        type_text(key, "q1", "  CockroachDB ");
        let c = get(key).unwrap();
        assert!(c.ready() && view(&c, now_ms()).can_send);
        type_text(key, "q0", "MySQL");
        assert_eq!(
            get(key).unwrap().choices.get("q0"),
            None,
            "typed text replaces the choice"
        );
        let request = submit(key).unwrap();
        assert_eq!(
            request.content["answers"],
            serde_json::json!({"q0":{"text":"MySQL"},"q1":{"text":"CockroachDB"}})
        );
    }

    #[test]
    fn only_the_agents_own_edit_settles_its_card_and_expiry_shows_locally() {
        let _guard = serial();
        test_reset();
        let mut early = card(1);
        early.id = format!("aq_{}", "d".repeat(64));
        let answered = early.answered(
            "@alice:example.test",
            BTreeMap::from([("q0".into(), AnswerValue::Option("SQLite".into()))]),
        );
        // An edit that arrives before its card is applied once the card does.
        assert!(!edited(
            "!room:example.test",
            "$early",
            AGENT,
            answered.clone()
        ));
        assert!(posted(
            "!room:example.test",
            "$early",
            AGENT,
            "Agent",
            now_ms(),
            early.clone()
        ));
        let c = get("$early").unwrap();
        assert_eq!(c.card.state, CardState::Answered);
        let v = view(&c, now_ms());
        assert!(!v.open && v.status.starts_with("Answered by @alice:example.test"));
        assert_eq!(v.questions[0].answer.as_deref(), Some("SQLite"));
        // Somebody else cannot settle a card, and a settled card stays settled.
        let other = card(1);
        posted(
            "!room:example.test",
            "$card",
            AGENT,
            "Agent",
            now_ms(),
            other.clone(),
        );
        assert!(!edited(
            "!room:example.test",
            "$card",
            OTHER,
            other.expired()
        ));
        // An edit cannot move the card's expiry or change what it asks.
        let mut moved = other.expired();
        moved.expires_at_ms += 60_000;
        assert!(!edited("!room:example.test", "$card", AGENT, moved));
        assert!(edited(
            "!room:example.test",
            "$card",
            AGENT,
            other.expired()
        ));
        assert!(!edited(
            "!room:example.test",
            "$card",
            AGENT,
            other.answered(
                "@bob:example.test",
                BTreeMap::from([("q0".into(), AnswerValue::Option("SQLite".into()))])
            )
        ));
        assert!(
            view(&get("$card").unwrap(), now_ms())
                .status
                .starts_with("Expired")
        );
        // A card past its time reads as expired before the Agent's edit arrives.
        let mut late = card(1);
        late.id = format!("aq_{}", "e".repeat(64));
        late.trigger_event_id = "$late".into();
        posted(
            "!room:example.test",
            "$latecard",
            AGENT,
            "Agent",
            now_ms(),
            late.clone(),
        );
        let c = get("$latecard").unwrap();
        let v = view(&c, late.expires_at_ms + 1);
        assert!(!v.open && v.status.starts_with("Expired") && !v.send_visible);
    }

    #[test]
    fn only_a_card_an_executor_posts_is_taken() {
        let _guard = serial();
        test_reset();
        let now = now_ms();
        // Never first posted already answered or expired.
        let answered = card(1).answered(
            "@alice:example.test",
            BTreeMap::from([("q0".into(), AnswerValue::Option("SQLite".into()))]),
        );
        assert!(!posted(
            "!room:example.test",
            "$a",
            AGENT,
            "Agent",
            now,
            answered
        ));
        assert!(!posted(
            "!room:example.test",
            "$b",
            AGENT,
            "Agent",
            now,
            card(1).expired()
        ));
        // Never waiting longer than one run's answer budget from its post.
        let mut forever = card(1);
        forever.expires_at_ms = now + protocol::ANSWER_WAIT_MS + protocol::EXPIRY_SKEW_MS + 1;
        assert!(!posted(
            "!room:example.test",
            "$c",
            AGENT,
            "Agent",
            now,
            forever
        ));
        let mut backdated = card(1);
        backdated.expires_at_ms = now + 60_000;
        assert!(!posted(
            "!room:example.test",
            "$d",
            AGENT,
            "Agent",
            now - protocol::ANSWER_WAIT_MS - protocol::EXPIRY_SKEW_MS,
            backdated
        ));
        assert!(posted(
            "!room:example.test",
            "$e",
            AGENT,
            "Agent",
            now,
            card(1)
        ));
        assert!(
            for_event(
                &RoomId::parse("!room:example.test").unwrap(),
                "$trigger",
                false
            )
            .len()
                == 1
        );
    }

    #[test]
    fn every_agents_open_card_under_a_message_is_shown_in_order() {
        let _guard = serial();
        test_reset();
        let room = RoomId::parse("!room:example.test").unwrap();
        let now = now_ms();
        let mut first = card(1);
        first.id = format!("aq_{}", "1".repeat(64));
        // The same card id from another Agent is that Agent's own card.
        let second = first.clone();
        let mut third = card(1);
        third.id = format!("aq_{}", "3".repeat(64));
        let mut settled = card(1);
        settled.id = format!("aq_{}", "4".repeat(64));
        assert!(posted(
            room.as_str(),
            "$settled",
            AGENT,
            "One",
            now - 3_000,
            settled.clone()
        ));
        assert!(edited(room.as_str(), "$settled", AGENT, settled.expired()));
        assert!(posted(
            room.as_str(),
            "$first",
            AGENT,
            "One",
            now - 2_000,
            first.clone()
        ));
        assert!(posted(
            room.as_str(),
            "$second",
            OTHER,
            "Two",
            now - 1_000,
            second
        ));
        assert!(
            !posted(room.as_str(), "$again", AGENT, "One", now, first),
            "the same Agent's card again"
        );
        let shown: Vec<_> = for_event(&room, "$trigger", false)
            .iter()
            .map(|c| c.card_event.clone())
            .collect();
        assert_eq!(
            shown,
            ["$first", "$second", "$settled"],
            "open cards first, each Agent's own, then settled"
        );
        // A later card cannot push an open one off the message.
        assert!(posted(room.as_str(), "$third", OTHER, "Two", now, third));
        let shown: Vec<_> = for_event(&room, "$trigger", false)
            .iter()
            .map(|c| c.card_event.clone())
            .collect();
        assert_eq!(shown, ["$first", "$second", "$third"]);
        assert_eq!(MAX_CARDS_PER_MESSAGE, 3);
        // Answering one card never touches another Agent's card.
        let request = choose("$second", "q0", "SQLite").unwrap();
        assert_eq!(request.key, "$second");
        assert_eq!(request.agent, OTHER);
        assert_eq!(get("$first").unwrap().local, LocalAnswer::None);
    }

    fn encryption(level: VerificationState) -> EncryptionInfo {
        use matrix_sdk::deserialized_responses::AlgorithmInfo;
        EncryptionInfo {
            sender: OwnedUserId::try_from(AGENT).unwrap(),
            sender_device: Some("AGENT_DEVICE".into()),
            forwarder: None,
            algorithm_info: AlgorithmInfo::MegolmV1AesSha2 {
                curve25519_key: "curve25519".into(),
                sender_claimed_keys: BTreeMap::new(),
                session_id: Some("session".into()),
            },
            verification_state: level,
        }
    }
    fn event(id: &str, content: Value) -> Value {
        serde_json::json!({
            "event_id": id, "room_id": "!room:example.test", "sender": AGENT,
            "origin_server_ts": now_ms(), "type": "m.room.message", "content": content,
        })
    }
    fn accept(
        generation: u64,
        value: &Value,
        info: Option<&EncryptionInfo>,
        plaintext: bool,
    ) -> bool {
        accept_event(
            generation,
            "!room:example.test",
            "example.test",
            "Agent",
            value,
            info,
            plaintext,
        )
    }

    #[tokio::test]
    async fn late_room_keys_then_late_device_keys_recover_the_same_card_once() {
        let _guard = SERIAL.lock().await;
        let generation = reset("@me:example.test".into());
        let value = event("$late", card(1).message_content());
        let encrypted = serde_json::json!({"event_id":"$late", "sender":AGENT,
            "type":"m.room.encrypted", "content":{"ciphertext":"not yet decryptable"}});
        // Relation metadata need not be present outside the encrypted body.
        assert!(!accept(generation, &encrypted, None, false));
        assert!(get("$late").is_none());
        let since = with_state(generation, |s| s.parked[0].since).unwrap();
        let unknown = encryption(VerificationState::Unverified(VerificationLevel::None(
            DeviceLinkProblem::MissingDevice,
        )));
        assert!(
            !retry_with(
                generation,
                RETRY_TIMEOUT,
                || {},
                |_| async { accept(generation, &value, Some(&unknown), false) }
            )
            .await
        );
        with_state(generation, |s| {
            assert_eq!(s.parked.len(), 1);
            assert!(
                s.parked[0].decrypted.is_some(),
                "now waiting only for the device list"
            );
            assert_eq!(s.parked[0].since, since, "retries do not extend retention");
        })
        .unwrap();
        let known = encryption(VerificationState::Unverified(
            VerificationLevel::UnsignedDevice,
        ));
        assert!(
            retry_with(
                generation,
                RETRY_TIMEOUT,
                || {},
                |_| async { accept(generation, &value, Some(&known), false) }
            )
            .await
        );
        assert!(get("$late").is_some());
        assert!(!accept(generation, &value, Some(&known), false));
        with_state(generation, |s| {
            assert!(s.parked.is_empty());
            assert_eq!(s.cards.len(), 1);
        })
        .unwrap();
    }

    #[tokio::test]
    async fn an_edit_decrypted_before_its_card_settles_the_recovered_card() {
        let _guard = SERIAL.lock().await;
        let generation = reset("@me:example.test".into());
        let c = card(1);
        let answered = c.answered(
            "@alice:example.test",
            BTreeMap::from([("q0".into(), AnswerValue::Option("SQLite".into()))]),
        );
        let known = encryption(VerificationState::Verified);
        let edit = event("$edit", answered.edit_content("$late"));
        assert!(!accept(generation, &edit, Some(&known), false));
        park(generation, "!room:example.test", "$late", None);
        let original = event("$late", c.message_content());
        assert!(
            retry_with(
                generation,
                RETRY_TIMEOUT,
                || {},
                |_| async { accept(generation, &original, Some(&known), false) }
            )
            .await
        );
        let shown = get("$late").unwrap();
        assert_eq!(shown.card, answered);
        assert!(!view(&shown, now_ms()).open);
        assert_eq!(
            view(&shown, now_ms()).questions[0].answer.as_deref(),
            Some("SQLite")
        );
    }

    #[tokio::test]
    async fn logout_and_same_account_relogin_fence_in_flight_retries_and_answers() {
        let _guard = SERIAL.lock().await;
        let old = reset("@me:example.test".into());
        park(old, "!room:example.test", "$late", None);
        let started = tokio::sync::Notify::new();
        let resume = tokio::sync::Notify::new();
        let value = event("$late", card(1).message_content());
        let known = encryption(VerificationState::Verified);
        let running = retry_with(
            old,
            RETRY_TIMEOUT,
            || {},
            |_| async {
                started.notify_one();
                resume.notified().await;
                accept(old, &value, Some(&known), false)
            },
        );
        let relogin = async {
            started.notified().await;
            let new = reset("@me:example.test".into());
            park(new, "!room:example.test", "$late", None);
            resume.notify_one();
            new
        };
        let (changed, new) = tokio::join!(running, relogin);
        assert!(!changed);
        assert!(get("$late").is_none(), "old client cannot resurrect a card");
        with_state(new, |s| {
            assert_eq!(
                s.parked.len(),
                1,
                "old client cannot unpark the new client's event"
            )
        })
        .unwrap();
        assert!(posted(
            "!room:example.test",
            "$late",
            AGENT,
            "Agent",
            now_ms(),
            card(1)
        ));
        assert!(!sent(&QuestionAnswerSent {
            generation: old,
            key: "$late".into(),
            result: Ok(()),
        }));
        assert_eq!(get("$late").unwrap().local, LocalAnswer::None);
        park(old, "!room:example.test", "$stale", None);
        assert!(!edited_in(
            old,
            "!room:example.test",
            "$late",
            AGENT,
            card(1).expired()
        ));
        assert_eq!(get("$late").unwrap().card.state, CardState::Pending);
        assert!(!expiry_changed(old, u64::MAX));
    }

    #[test]
    fn encrypted_card_admission_rejects_plaintext_spoofs_and_wrong_room_or_sender() {
        let _guard = serial();
        let generation = reset("@me:example.test".into());
        let value = event("$card", card(1).message_content());
        assert!(
            !accept(generation, &value, None, false),
            "encrypted or unknown room never accepts plaintext"
        );
        for level in [
            VerificationLevel::MismatchedSender,
            VerificationLevel::VerificationViolation,
            VerificationLevel::None(DeviceLinkProblem::InsecureSource),
        ] {
            assert!(!accept(
                generation,
                &value,
                Some(&encryption(VerificationState::Unverified(level))),
                false
            ));
        }
        let known = encryption(VerificationState::Verified);
        let mut wrong_room = value.clone();
        wrong_room["room_id"] = "!elsewhere:example.test".into();
        assert!(!accept(generation, &wrong_room, Some(&known), false));
        let mut wrong_sender = value.clone();
        wrong_sender["sender"] = OTHER.into();
        assert!(!accept(generation, &wrong_sender, Some(&known), false));
        wrong_sender["sender"] = "@_hagency_agt_one:attacker.test".into();
        assert!(
            !accept(generation, &wrong_sender, None, true),
            "remote lookalike is not a local Agent"
        );
        assert!(
            accept(generation, &value, None, true),
            "known plaintext rooms still work"
        );
    }

    #[test]
    fn run_cards_share_authenticated_late_key_recovery_and_account_generation() {
        let _guard = serial();
        let generation = reset("@me:example.test".into());
        let mut card: ::hagency::agent_run_controls::Card = serde_json::from_value(serde_json::json!({
            "version":1,"id":"run_fixture","dispatchId":"dispatch","executionId":"execution",
            "roomId":"!room:example.test","eventId":"$trigger","threadRoot":"!room:example.test",
            "requester":"@me:example.test","owner":"@owner:example.test","runtime":"codex","state":"running","revision":0
        })).unwrap();
        let value = event("$run", card.content());
        assert!(!accept(generation, &value, None, false));
        let missing = encryption(VerificationState::Unverified(VerificationLevel::None(DeviceLinkProblem::MissingDevice)));
        assert!(!accept(generation, &value, Some(&missing), false));
        assert!(with_state(generation, |state| state.parked.iter().any(|parked| parked.event_id == "$run")).unwrap());
        let trusted = encryption(VerificationState::Verified);
        assert!(accept(generation, &value, Some(&trusted), false));
        let room = matrix_sdk::ruma::room_id!("!room:example.test");
        let shown = super::super::run_controls::for_event(room, "$trigger", true);
        assert_eq!(shown.len(), 1);
        assert!(shown[0].may_append && shown[0].may_stop);
        assert!(super::super::run_controls::for_event(room, "$run", true).is_empty(), "the transport event must not render a duplicate interactive card");
        card.revision = 2; card.state = ::hagency::agent_run_controls::State::Interrupted;
        assert!(accept(generation, &event("$edit", card.edit_content("$run").unwrap()), Some(&trusted), false));
        card.revision = 1; card.state = ::hagency::agent_run_controls::State::Running;
        assert!(!accept(generation, &event("$stale", card.edit_content("$run").unwrap()), Some(&trusted), false));
        assert_eq!(super::super::run_controls::for_event(room, "$trigger", true)[0].card.state, ::hagency::agent_run_controls::State::Interrupted);
        reset("@different:example.test".into());
        assert!(!accept(generation, &value, Some(&trusted), false));
        assert!(super::super::run_controls::for_event(room, "$trigger", true).is_empty());
    }

    #[test]
    fn expiry_requests_a_redraw_once_and_late_published_answers_still_win() {
        let _guard = serial();
        let generation = reset("@me:example.test".into());
        let c = card(1);
        assert!(posted(
            "!room:example.test",
            "$card",
            AGENT,
            "Agent",
            now_ms(),
            c.clone()
        ));
        assert!(!expiry_changed(generation, c.expires_at_ms - 1));
        assert!(expiry_changed(generation, c.expires_at_ms));
        assert!(
            !expiry_changed(generation, c.expires_at_ms + 1),
            "one redraw per expiry"
        );
        assert_eq!(
            get("$card").unwrap().card.state,
            CardState::Pending,
            "local expiry is not a fabricated Agent edit"
        );
        let shown = view(&get("$card").unwrap(), c.expires_at_ms);
        assert!(shown.status.starts_with("Expired") && !shown.open && !shown.send_visible);
        let answered = c.answered(
            "@alice:example.test",
            BTreeMap::from([("q0".into(), AnswerValue::Option("SQLite".into()))]),
        );
        assert!(edited("!room:example.test", "$card", AGENT, answered));
        assert!(
            view(&get("$card").unwrap(), c.expires_at_ms + 1)
                .status
                .starts_with("Answered by")
        );
    }

    #[tokio::test]
    async fn parked_retries_are_bounded_and_one_stall_does_not_hide_a_later_card() {
        let _guard = SERIAL.lock().await;
        let generation = reset("@me:example.test".into());
        for i in 0..MAX_PARKED + 3 {
            park(
                generation,
                "!room:example.test",
                &format!("$event{i}"),
                None,
            );
        }
        with_state(generation, |s| {
            assert_eq!(s.parked.len(), MAX_PARKED);
            assert_eq!(s.parked[0].event_id, "$event3");
            for p in &mut s.parked {
                p.since = Instant::now() - PARKED_FOR;
            }
        })
        .unwrap();
        assert!(
            !retry_with(
                generation,
                RETRY_TIMEOUT,
                || {},
                |_| async { panic!("expired entries are not fetched") }
            )
            .await
        );
        let value = event("$fast", card(1).message_content());
        let known = encryption(VerificationState::Verified);
        park(generation, "!room:example.test", "$stalled", None);
        park(generation, "!room:example.test", "$fast", None);
        assert!(
            retry_with(
                generation,
                Duration::from_millis(20),
                || {},
                |p| {
                    let value = &value;
                    let known = &known;
                    async move {
                        if p.event_id == "$stalled" {
                            std::future::pending::<()>().await;
                        }
                        accept(generation, value, Some(known), false)
                    }
                }
            )
            .await
        );
        assert!(get("$fast").is_some());
        with_state(generation, |s| {
            assert_eq!(s.parked.len(), 1);
            assert_eq!(
                s.parked[0].event_id, "$stalled",
                "a timed out event remains retryable"
            );
        })
        .unwrap();
    }

    #[tokio::test]
    async fn recovered_card_requests_redraw_before_another_retry_finishes() {
        let _guard = SERIAL.lock().await;
        let generation = reset("@me:example.test".into());
        park(generation, "!room:example.test", "$stalled", None);
        park(generation, "!room:example.test", "$fast", None);
        let value = event("$fast", card(1).message_content());
        let known = encryption(VerificationState::Verified);
        let release_stalled = tokio::sync::Notify::new();
        let stalled_finished = std::sync::atomic::AtomicBool::new(false);
        let (notify, mut notifications) = tokio::sync::mpsc::unbounded_channel();
        let running = retry_with(
            generation,
            Duration::from_secs(30),
            || {
                notify.send(()).unwrap();
            },
            |p| {
                let value = &value;
                let known = &known;
                let release_stalled = &release_stalled;
                let stalled_finished = &stalled_finished;
                async move {
                    if p.event_id == "$stalled" {
                        release_stalled.notified().await;
                        stalled_finished.store(true, Ordering::SeqCst);
                        return false;
                    }
                    accept(generation, value, Some(known), false)
                }
            },
        );
        tokio::pin!(running);
        // Poll the actual batch while observing its notification. The old
        // implementation only notified after its return, so this times out
        // while the stalled request remains deliberately unresolved.
        tokio::time::timeout(Duration::from_secs(1), async {
            tokio::select! {
                result = &mut running => panic!("batch completed before the stalled request was released: {result}"),
                notification = notifications.recv() => assert_eq!(notification, Some(())),
            }
        }).await.expect("the recovered card must request redraw before the batch finishes");
        assert!(get("$fast").is_some());
        assert!(!stalled_finished.load(Ordering::SeqCst));
        release_stalled.notify_one();
        assert!(running.await);
        assert!(stalled_finished.load(Ordering::SeqCst));
        assert!(
            notifications.try_recv().is_err(),
            "no duplicate batch-completion notification"
        );
    }

    #[tokio::test]
    async fn a_success_completed_after_login_reset_cannot_request_redraw() {
        let _guard = SERIAL.lock().await;
        let old = reset("@me:example.test".into());
        park(old, "!room:example.test", "$old", None);
        let notifications = std::sync::atomic::AtomicUsize::new(0);
        assert!(
            !retry_with(
                old,
                RETRY_TIMEOUT,
                || {
                    notifications.fetch_add(1, Ordering::SeqCst);
                },
                |_| async {
                    // Simulate an event accepted just before a login switch,
                    // whose success is observed by the retry loop afterward.
                    reset("@other:example.test".into());
                    true
                },
            )
            .await
        );
        assert_eq!(notifications.load(Ordering::SeqCst), 0);
    }

    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    pub(super) fn serial() -> tokio::sync::MutexGuard<'static, ()> {
        SERIAL.blocking_lock()
    }
}
