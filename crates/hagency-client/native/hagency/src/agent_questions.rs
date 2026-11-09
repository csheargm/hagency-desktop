//! Native questions an Agent asks its room during a run.
//!
//! The executor's Agent device posts a card as an ordinary room message with a
//! custom msgtype, encrypted whenever the room is. Hagency clients draw it as
//! buttons under the message that started the run and hide the message
//! itself; other clients show its plain-text body. A named participant answers
//! with a custom room event that refers to the card. That event is not
//! `m.room.message`, so it never becomes chat intake: plaintext ingress takes
//! only `m.text` messages, and the executor's encrypted addressing check
//! confirms only text messages. A text reply in the room stays a new message.
//! The executor takes the first valid answer from its admitted participants,
//! answers the native runtime and edits the card to show the outcome. Secret questions never reach
//! the room.
use hagency_runtime::{codex::session::UserInputQuestion, questions::{self, Question}};
pub use hagency_runtime::questions::Answer as AnswerValue;
use matrix_sdk::{
    Client, Room,
    deserialized_responses::{EncryptionInfo, VerificationLevel, VerificationState},
    event_handler::EventHandlerHandle,
    ruma::{events::AnySyncTimelineEvent, serde::Raw},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// The card: an `m.room.message` msgtype and the content key of its payload.
pub const MSGTYPE: &str = "im.hagency.agent_question";
/// A member's answer: a message-like event that is never a chat message.
pub const ANSWER_EVENT: &str = "im.hagency.agent_question.answer";
pub const VERSION: u64 = 1;
pub const SCOPED_VERSION: u64 = 2;
/// How long the room has to answer before the Agent continues on its own.
pub const ANSWER_WAIT_MS: u64 = 600_000;
/// Clock difference allowed between the Agent's host and the homeserver when
/// a client checks a card's expiry against the card's own timestamp.
pub const EXPIRY_SKEW_MS: u64 = 120_000;
/// What one card shows. A request beyond it is answered with nothing.
pub const MAX_CARD_QUESTIONS: usize = questions::MAX_QUESTIONS;
pub const MAX_CARD_OPTIONS: usize = questions::MAX_OPTIONS;
/// One typed answer, the same bound the runtime enforces.
pub const MAX_ANSWER_TEXT_BYTES: usize = questions::MAX_TEXT_BYTES;
/// Encrypted and plaintext events alike stay far below the 64 KiB limit.
const MAX_EVENT_BYTES: usize = 48 * 1024;
const MAX_TEXT_BYTES: usize = 4096;
const MAX_LABEL_BYTES: usize = 256;
const MAX_DESCRIPTION_BYTES: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CardOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CardQuestion {
    pub id: String,
    #[serde(default)]
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<CardOption>,
    /// A typed answer is offered ("Other", or a question without options).
    pub other: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multi_select: bool,
}
impl CardQuestion {
    pub fn accepts_text(&self) -> bool {
        self.other || self.options.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardState {
    Pending,
    Answered,
    /// Nobody answered in time, or the Agent no longer waits for an answer.
    Expired,
}

/// The structured card under the content key [`MSGTYPE`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Card {
    pub version: u64,
    /// `aq_` and 64 lowercase hex digits, stable for one question request.
    pub id: String,
    /// The room message that started the run; the card is drawn under it.
    pub trigger_event_id: String,
    /// The thread the run answers in.
    pub thread_root: String,
    pub expires_at_ms: u64,
    pub questions: Vec<CardQuestion>,
    /// Authenticated Matrix identities admitted by the host for this task.
    /// Empty only on legacy v1 cards; v2 always binds its answering audience.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub responders: Vec<String>,
    /// Private questions the Agent also asked, never shown or answered here.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub withheld: u64,
    pub state: CardState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub answers: BTreeMap<String, AnswerValue>,
}
fn is_zero(value: &u64) -> bool {
    *value == 0
}

fn hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn is_event_id(value: &str) -> bool {
    value.starts_with('$')
        && value.len() > 1
        && value.len() <= 255
        && !value.chars().any(char::is_control)
}
fn user_id(value: &str) -> bool {
    value.starts_with('@')
        && value.contains(':')
        && value.len() <= 255
        && !value.chars().any(char::is_control)
}
fn prose(value: &str, max: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= max
        && !value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
}
fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 255 && !value.chars().any(char::is_control)
}

/// Whether `user` is a Hagency Agent puppet on any server. No Agent ever
/// answers a card, whichever server it is on.
pub fn is_agent(user: &str) -> bool {
    user.strip_prefix('@')
        .is_some_and(|rest| rest.starts_with("_hagency_agt_"))
        && user_id(user)
}

/// The server part of a user or room ID (`@local:server`, `!opaque:server`).
pub fn server_name(id: &str) -> Option<&str> {
    id.split_once(':')
        .map(|(_, server)| server)
        .filter(|server| !server.is_empty())
}

/// Whether `user` is an Agent on `server`, the room's own homeserver. Only
/// such an Agent's card is drawn: another server could name any account
/// `_hagency_agt_…`.
pub fn is_agent_on(user: &str, server: &str) -> bool {
    is_agent(user) && server_name(user) == Some(server)
}

/// `YYYY-MM-DD HH:MM UTC`, the same instant for every reader.
fn utc_minute(ms: u64) -> String {
    let secs = ms / 1000;
    let (year, month, day) = crate::operator_graphs::civil_from_days((secs / 86_400) as i64);
    let day_secs = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        day_secs / 3600,
        (day_secs % 3600) / 60
    )
}

impl Card {
    /// The card for the questions a run may show, or `None` when it has
    /// nothing to show (every question is secret) or more than one card holds.
    pub fn ask(
        id: String,
        trigger_event_id: String,
        thread_root: String,
        expires_at_ms: u64,
        questions: &[UserInputQuestion],
    ) -> Option<Self> {
        // Never project an approval into a room card, including a mixed
        // request where its permission question would otherwise be withheld.
        if questions.iter().any(UserInputQuestion::is_codex_approval) {
            return None;
        }
        Self::ask_inner(id, trigger_event_id, thread_root, expires_at_ms,
            &questions.iter().map(Question::from).collect::<Vec<_>>(), Vec::new(), VERSION)
    }

    pub fn ask_questions(
        id: String, trigger_event_id: String, thread_root: String, expires_at_ms: u64,
        questions: &[Question], responders: Vec<String>,
    ) -> Option<Self> {
        Self::ask_inner(id, trigger_event_id, thread_root, expires_at_ms, questions, responders, SCOPED_VERSION)
    }

    fn ask_inner(
        id: String, trigger_event_id: String, thread_root: String, expires_at_ms: u64,
        questions: &[Question], responders: Vec<String>, version: u64,
    ) -> Option<Self> {
        if !questions::valid_questions(questions) { return None; }
        let withheld = questions.iter().filter(|q| q.secret).count() as u64;
        let shown: Vec<CardQuestion> = questions
            .iter()
            .filter(|q| !q.secret)
            .map(|q| CardQuestion {
                id: q.id.clone(),
                header: q.header.clone(),
                question: q.question.clone(),
                options: q
                    .options
                    .iter()
                    .map(|o| CardOption {
                        label: o.label.clone(),
                        description: o.description.clone(),
                    })
                    .collect(),
                other: q.accepts_text(),
                multi_select: q.multi_select,
            })
            .collect();
        let card = Self {
            version,
            id,
            trigger_event_id,
            thread_root,
            expires_at_ms,
            questions: shown,
            responders,
            withheld,
            state: CardState::Pending,
            answered_by: None,
            answers: BTreeMap::new(),
        };
        card.validate().ok().map(|()| card)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        let invalid = Err("invalid_agent_question");
        if !matches!(self.version, VERSION | SCOPED_VERSION)
            || (self.version == SCOPED_VERSION && self.responders.is_empty())
            || self.responders.len() > 2
            || self.responders.iter().any(|r| !user_id(r) || is_agent(r))
            || self.responders.iter().collect::<BTreeSet<_>>().len() != self.responders.len()
            || !self.id.strip_prefix("aq_").is_some_and(hex64)
            || !is_event_id(&self.trigger_event_id)
            || !is_event_id(&self.thread_root)
            || self.expires_at_ms == 0
            || self.questions.is_empty()
            || self.questions.len() > MAX_CARD_QUESTIONS
            || self.withheld > 64
        {
            return invalid;
        }
        let mut ids = BTreeSet::new();
        for q in &self.questions {
            if !identifier(&q.id)
                || !ids.insert(q.id.as_str())
                || (!q.header.is_empty() && !prose(&q.header, MAX_LABEL_BYTES))
                || !prose(&q.question, MAX_TEXT_BYTES)
                || q.options.len() > MAX_CARD_OPTIONS
                || (q.options.is_empty() && !q.other)
            {
                return invalid;
            }
            let mut labels = BTreeSet::new();
            for o in &q.options {
                if !prose(&o.label, MAX_LABEL_BYTES)
                    || o.label.contains('\n')
                    || !labels.insert(o.label.as_str())
                    || (!o.description.is_empty() && !prose(&o.description, MAX_DESCRIPTION_BYTES))
                {
                    return invalid;
                }
            }
        }
        match self.state {
            CardState::Answered => {
                if !self.answered_by.as_deref().is_some_and(user_id)
                    || !self.answered_by.as_deref().is_some_and(|who| self.may_answer(who))
                    || self.answers.is_empty()
                    || check_answers(self, &self.answers).is_err()
                {
                    return invalid;
                }
            }
            CardState::Pending | CardState::Expired => {
                if self.answered_by.is_some() || !self.answers.is_empty() {
                    return invalid;
                }
            }
        }
        Ok(())
    }

    /// The card carried by a card message or by an edit's replacement content.
    pub fn from_content(content: &Value) -> Option<Self> {
        if content["msgtype"] != MSGTYPE {
            return None;
        }
        let card: Self = serde_json::from_value(content.get(MSGTYPE)?.clone()).ok()?;
        card.validate().ok()?;
        Some(card)
    }

    pub fn answered(&self, by: &str, answers: BTreeMap<String, AnswerValue>) -> Self {
        Self {
            state: CardState::Answered,
            answered_by: Some(by.into()),
            answers,
            ..self.clone()
        }
    }
    pub fn may_answer(&self, who: &str) -> bool {
        user_id(who) && !is_agent(who)
            && (self.responders.is_empty() && self.version == VERSION || self.responders.iter().any(|r| r == who))
    }
    pub fn expired(&self) -> Self {
        Self {
            state: CardState::Expired,
            answered_by: None,
            answers: BTreeMap::new(),
            ..self.clone()
        }
    }
    pub fn is_open(&self, now_ms: u64) -> bool {
        self.state == CardState::Pending && now_ms < self.expires_at_ms
    }
    /// A card can wait at most one run's whole answer budget from when it was
    /// posted (`origin_server_ts`). A later expiry is not one an executor sets.
    pub fn expiry_plausible(&self, origin_server_ts: u64) -> bool {
        self.expires_at_ms <= origin_server_ts.saturating_add(ANSWER_WAIT_MS + EXPIRY_SKEW_MS)
    }

    /// What clients without Hagency's card show: the questions and choices,
    /// and how to answer. It never contains a secret question.
    pub fn fallback_text(&self) -> String {
        let mut text = String::new();
        match self.state {
            CardState::Pending => text.push_str("❓ Question from the Agent / Agent 提问\n"),
            CardState::Answered => {
                text.push_str(&format!(
                    "✅ Answered by {} / 已回答\n",
                    self.answered_by.as_deref().unwrap_or_default()
                ));
            }
            CardState::Expired => {
                text.push_str("⌛ This question closed without an answer; the Agent continued on its own. / 此问题已关闭，Agent 已在无人回答的情况下继续。\n");
            }
        }
        let numbered = self.questions.len() > 1;
        for (index, q) in self.questions.iter().enumerate() {
            text.push('\n');
            if numbered {
                text.push_str(&format!("{}. ", index + 1));
            }
            if !q.header.is_empty() {
                text.push_str(&format!("{} — ", q.header));
            }
            text.push_str(&q.question);
            text.push('\n');
            if self.state == CardState::Answered {
                let answer = self.answers.get(&q.id).map_or_else(|| "—".into(), AnswerValue::display);
                text.push_str(&format!("   → {answer}\n"));
                continue;
            }
            for o in &q.options {
                if o.description.is_empty() {
                    text.push_str(&format!("   • {}\n", o.label));
                } else {
                    text.push_str(&format!("   • {} — {}\n", o.label, o.description));
                }
            }
            if q.other && !q.options.is_empty() {
                text.push_str("   • Other (type your own answer) / 其他（自行填写）\n");
            }
        }
        if self.state == CardState::Pending {
            if !self.responders.is_empty() {
                text.push_str(&format!("\n回答人 / Answers accepted from: {}\n", self.responders.join(", ")));
            }
            // An absolute time: the message may be read long after it was sent.
            let by = utc_minute(self.expires_at_ms);
            text.push_str(&format!(
                "\nAnswer with the buttons in Hagency by {by}. A text reply is a new message, not an answer.\n请在 {by} 前用 Hagency 中的按钮回答；文字回复会作为新消息，不算回答。"
            ));
        }
        text.trim_end().to_owned()
    }

    /// The room message that posts this card in its run's thread.
    pub fn message_content(&self) -> Value {
        json!({
            "msgtype": MSGTYPE,
            "body": self.fallback_text(),
            MSGTYPE: self,
            "m.relates_to": {"rel_type":"m.thread","event_id":self.thread_root,"is_falling_back":true,
                "m.in_reply_to":{"event_id":self.thread_root}},
        })
    }

    /// The edit that replaces the posted card with this state.
    pub fn edit_content(&self, card_event_id: &str) -> Value {
        let new = json!({"msgtype": MSGTYPE, "body": self.fallback_text(), MSGTYPE: self});
        json!({
            "msgtype": MSGTYPE,
            "body": format!("* {}", self.fallback_text()),
            MSGTYPE: self,
            "m.new_content": new,
            "m.relates_to": {"rel_type":"m.replace","event_id":card_event_id},
        })
    }

    /// A member's answer event for this card. Only answers the card offers.
    pub fn answer_content(
        &self,
        card_event_id: &str,
        answers: &BTreeMap<String, AnswerValue>,
    ) -> Result<Value, &'static str> {
        if !is_event_id(card_event_id) || answers.is_empty() {
            return Err("invalid_agent_question_answer");
        }
        check_answers(self, answers)?;
        Ok(json!({
            "version": self.version,
            "cardId": self.id,
            "answers": answers,
            "m.relates_to": {"rel_type":"m.reference","event_id":card_event_id},
        }))
    }

    /// The answers in a member's answer event, when it answers this exact card
    /// posted as `card_event_id` with choices the card offers.
    pub fn parse_answer(
        &self,
        card_event_id: &str,
        content: &Value,
    ) -> Option<BTreeMap<String, AnswerValue>> {
        if content["version"] != self.version
            || content["cardId"] != self.id.as_str()
            || content["m.relates_to"]["rel_type"] != "m.reference"
            || content["m.relates_to"]["event_id"] != card_event_id
        {
            return None;
        }
        let answers: BTreeMap<String, AnswerValue> =
            serde_json::from_value(content.get("answers")?.clone()).ok()?;
        (!answers.is_empty() && check_answers(self, &answers).is_ok()).then_some(answers)
    }
}

fn check_answers(card: &Card, answers: &BTreeMap<String, AnswerValue>) -> Result<(), &'static str> {
    let invalid = Err("invalid_agent_question_answer");
    if answers.len() > card.questions.len() {
        return invalid;
    }
    for (id, value) in answers {
        let Some(q) = card.questions.iter().find(|q| &q.id == id) else {
            return invalid;
        };
        let valid = match value {
            AnswerValue::Option(label) => q.options.iter().any(|o| &o.label == label),
            AnswerValue::Options(labels) => {
                q.multi_select && !labels.is_empty()
                && labels.len() <= q.options.len()
                && labels.iter().all(|label| q.options.iter().any(|o| &o.label == label))
                && labels.iter().collect::<BTreeSet<_>>().len() == labels.len()
            }
            AnswerValue::Text(text) => q.accepts_text() && prose(text, MAX_ANSWER_TEXT_BYTES),
        };
        if !valid {
            return invalid;
        }
    }
    Ok(())
}

/// One answer event the executor's device received, before membership checks.
#[derive(Clone, Debug)]
pub struct Received {
    order: u64,
    pub room_id: String,
    pub event_id: String,
    /// The card message it refers to.
    pub card_event_id: String,
    pub sender: String,
    pub content: Value,
    /// `None` for a plaintext event; for an encrypted one, whether its Megolm
    /// session's Olm-authenticated sender is the event's sender.
    pub authentic: Option<bool>,
    received: Instant,
}
impl Received {
    /// Whether this event can be an answer at all, before membership: never
    /// from an Agent (this one or any other), and in an encrypted room only
    /// when Megolm authenticates its sender. A plaintext event in an encrypted
    /// room, as a homeserver could inject, is not.
    pub fn admissible(&self, encrypted_room: bool, own_user: Option<&str>) -> bool {
        own_user != Some(self.sender.as_str())
            && !is_agent(&self.sender)
            && (!encrypted_room || self.authentic == Some(true))
    }
}
struct Undecrypted {
    order: u64,
    room_id: String,
    event_id: String,
    card_event_id: String,
    received: Instant,
    tried: Option<Instant>,
}
struct Entries {
    event_type: &'static str,
    order: u64,
    answers: Vec<Received>,
    undecrypted: Vec<Undecrypted>,
    taken: BTreeSet<String>,
}
impl Default for Entries {
    fn default() -> Self {
        Self {
            event_type: ANSWER_EVENT,
            order: 0,
            answers: Vec::new(),
            undecrypted: Vec::new(),
            taken: BTreeSet::new(),
        }
    }
}
/// Entries older than any card that could still be waiting are dropped.
const RETAIN: Duration = Duration::from_millis(ANSWER_WAIT_MS + 120_000);
const MAX_ENTRIES: usize = 256;

/// Whether Megolm authenticates `sender` for an event: the device that set up
/// the session belongs to the sender, is known, and broke no verification.
/// (`EncryptionInfo::sender` is the event's own claim, never proof.)
pub fn authentic(info: &EncryptionInfo, sender: &str) -> bool {
    info.sender.as_str() == sender
        && !matches!(
            info.verification_state,
            VerificationState::Unverified(
                VerificationLevel::None(_)
                    | VerificationLevel::MismatchedSender
                    | VerificationLevel::VerificationViolation
            )
        )
}

impl Entries {
    fn prune(&mut self) {
        self.answers.retain(|e| e.received.elapsed() < RETAIN);
        self.undecrypted.retain(|e| e.received.elapsed() < RETAIN);
        while self.answers.len() + self.undecrypted.len() > MAX_ENTRIES {
            if self.undecrypted.is_empty()
                || self
                    .answers
                    .first()
                    .is_some_and(|a| a.order < self.undecrypted[0].order)
            {
                self.answers.remove(0);
            } else {
                self.undecrypted.remove(0);
            }
        }
        if self.taken.len() > 4 * MAX_ENTRIES {
            self.taken.clear();
        }
    }
    /// Record one timeline event if it is an answer, or an encrypted event
    /// that refers to some message and may be an answer once its keys arrive.
    fn observe(
        &mut self,
        room_id: &str,
        value: &Value,
        info: Option<&EncryptionInfo>,
        order: Option<u64>,
    ) {
        let (Some(event_id), Some(sender)) = (value["event_id"].as_str(), value["sender"].as_str())
        else {
            return;
        };
        if !is_event_id(event_id) || !user_id(sender) || self.taken.contains(event_id) {
            return;
        }
        let relation = &value["content"]["m.relates_to"];
        let Some(card_event_id) = relation["event_id"].as_str().filter(|e| is_event_id(e)) else {
            return;
        };
        if relation["rel_type"] != "m.reference" {
            return;
        }
        let order = order.unwrap_or_else(|| {
            self.order += 1;
            self.order
        });
        match value["type"].as_str() {
            Some(kind) if kind == self.event_type => {
                self.undecrypted.retain(|e| e.event_id != event_id);
                if self.answers.iter().any(|e| e.event_id == event_id) {
                    return;
                }
                self.answers.push(Received {
                    order,
                    room_id: room_id.into(),
                    event_id: event_id.into(),
                    card_event_id: card_event_id.into(),
                    sender: sender.into(),
                    content: value["content"].clone(),
                    authentic: info.map(|info| authentic(info, sender)),
                    received: Instant::now(),
                });
                self.answers.sort_by_key(|e| e.order);
            }
            Some("m.room.encrypted") => {
                if !self.undecrypted.iter().any(|e| e.event_id == event_id) {
                    self.undecrypted.push(Undecrypted {
                        order,
                        room_id: room_id.into(),
                        event_id: event_id.into(),
                        card_event_id: card_event_id.into(),
                        received: Instant::now(),
                        tried: None,
                    });
                }
            }
            _ => return,
        }
        self.prune();
    }
}

/// The executor device's answers, collected by its own sync. Bounded and
/// volatile; dropping the device drops its handler.
pub struct Inbox {
    client: Client,
    handle: EventHandlerHandle,
    entries: Arc<Mutex<Entries>>,
}
impl Drop for Inbox {
    fn drop(&mut self) {
        self.client.remove_event_handler(self.handle.clone());
    }
}
impl Inbox {
    pub fn attach(client: &Client) -> Self {
        Self::attach_event(client, ANSWER_EVENT)
    }

    /// Reuse the same authenticated, late-decrypting inbox for another
    /// closed-schema host interaction. Each type has a separate queue.
    pub(crate) fn attach_event(client: &Client, event_type: &'static str) -> Self {
        let entries = Arc::new(Mutex::new(Entries {
            event_type,
            ..Default::default()
        }));
        let shared = entries.clone();
        let handle = client.add_event_handler(
            move |event: Raw<AnySyncTimelineEvent>, room: Room, info: Option<EncryptionInfo>| {
                let shared = shared.clone();
                async move {
                    if event.json().get().len() > MAX_EVENT_BYTES {
                        return;
                    }
                    let Ok(value) = event.deserialize_as::<Value>() else {
                        return;
                    };
                    if value["type"] != event_type && value["type"] != "m.room.encrypted" {
                        return;
                    }
                    if let Ok(mut entries) = shared.lock() {
                        entries.observe(room.room_id().as_str(), &value, info.as_ref(), None);
                    }
                }
            },
        );
        Self {
            client: client.clone(),
            handle,
            entries,
        }
    }

    /// Up to `limit` encrypted references to `card_event_id` whose keys had
    /// not arrived, due for another decryption attempt (every few seconds).
    pub fn due_undecrypted(&self, room_id: &str, card_event_id: &str, limit: usize) -> Vec<String> {
        let Ok(mut entries) = self.entries.lock() else {
            return Vec::new();
        };
        let mut due = Vec::new();
        for e in &mut entries.undecrypted {
            if due.len() < limit
                && e.room_id == room_id
                && e.card_event_id == card_event_id
                && e.tried
                    .is_none_or(|t| t.elapsed() >= Duration::from_secs(5))
            {
                e.tried = Some(Instant::now());
                due.push(e.event_id.clone());
            }
        }
        due
    }

    /// A late decryption of an earlier undecryptable reference keeps its place.
    pub fn decrypted(&self, room_id: &str, value: &Value, info: Option<&EncryptionInfo>) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        let Some(event_id) = value["event_id"].as_str() else {
            return;
        };
        let Some(order) = entries
            .undecrypted
            .iter()
            .find(|e| e.event_id == event_id)
            .map(|e| e.order)
        else {
            return;
        };
        if value["type"] == entries.event_type {
            entries.observe(room_id, value, info, Some(order));
        } else {
            // Decrypted to something else: never an answer.
            entries.undecrypted.retain(|e| e.event_id != event_id);
        }
    }

    /// Hand back answers whose membership could not be checked yet; they keep
    /// their place ahead of later arrivals.
    pub fn requeue(&self, answers: Vec<Received>) {
        if answers.is_empty() {
            return;
        }
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        for answer in answers {
            entries.taken.remove(&answer.event_id);
            entries.answers.push(answer);
        }
        entries.answers.sort_by_key(|e| e.order);
    }

    /// The oldest `limit` answers to `card_event_id` not handed out before.
    /// Later ones wait for the next call, so a slow check loses none.
    pub fn take(&self, room_id: &str, card_event_id: &str, limit: usize) -> Vec<Received> {
        let Ok(mut entries) = self.entries.lock() else {
            return Vec::new();
        };
        let mut taken = Vec::new();
        let mut kept = Vec::new();
        for e in std::mem::take(&mut entries.answers) {
            if taken.len() < limit && e.room_id == room_id && e.card_event_id == card_event_id {
                taken.push(e);
            } else {
                kept.push(e);
            }
        }
        entries.answers = kept;
        for e in &taken {
            entries.taken.insert(e.event_id.clone());
        }
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hagency_runtime::codex::session::UserInputOption;

    fn questions() -> Vec<UserInputQuestion> {
        vec![
            UserInputQuestion {
                id: "db".into(),
                header: "Database".into(),
                question: "Which database should the service use?".into(),
                options: Some(vec![
                    UserInputOption {
                        label: "Postgres (Recommended)".into(),
                        description: "Durable.".into(),
                    },
                    UserInputOption {
                        label: "SQLite".into(),
                        description: "Simplest to run.".into(),
                    },
                ]),
                is_other: true,
                is_secret: false,
            },
            UserInputQuestion {
                id: "token".into(),
                header: "Token".into(),
                question: "Paste the deploy token".into(),
                options: None,
                is_other: false,
                is_secret: true,
            },
        ]
    }
    fn card() -> Card {
        Card::ask(
            format!("aq_{}", "a".repeat(64)),
            "$trigger".into(),
            "$root".into(),
            1_800_000_000_000,
            &questions(),
        )
        .unwrap()
    }

    #[test]
    fn scoped_cards_support_native_question_counts_multiselect_and_bound_answers() {
        let mut data: Vec<Question> = (0..8).map(|index| Question {
            id: format!("q{index}"), header: "Choice".into(), question: format!("Question {index}?"),
            options: (0..16).map(|i| questions::QuestionOption{label:format!("Option {i}"),description:String::new()}).collect(),
            other:true, secret:false, multi_select:index == 0,
        }).collect();
        let card = Card::ask_questions(format!("aq_{}","e".repeat(64)),"$trigger".into(),"$root".into(),1,
            &data,vec!["@requester:test".into(),"@owner:test".into()]).unwrap();
        assert_eq!(card.questions.len(), 8);
        assert_eq!(card.questions[7].options.len(), 16);
        assert!(card.may_answer("@requester:test") && card.may_answer("@owner:test"));
        assert!(!card.may_answer("@other_member:test") && !card.may_answer("@_hagency_agt_other:test"));
        let answers = BTreeMap::from([("q0".into(),AnswerValue::Options(vec!["Option 0".into(),"Option 15".into()]))]);
        let content = card.answer_content("$card",&answers).unwrap();
        assert_eq!(card.parse_answer("$card",&content),Some(answers.clone()));
        assert!(card.answered("@other_member:test",answers.clone()).validate().is_err());
        assert!(card.answered("@owner:test",answers).validate().is_ok());
        for answer in [AnswerValue::Options(vec!["Option 0".into(),"Option 0".into()]),AnswerValue::Options(vec![])] {
            assert!(card.answer_content("$card",&[("q0".into(),answer)].into()).is_err());
        }
        assert!(card.answer_content("$card",&[("q1".into(),AnswerValue::Options(vec!["Option 0".into()]))].into()).is_err());
        data[0].secret=true;
        let hidden=Card::ask_questions(card.id,"$trigger".into(),"$root".into(),1,&data,card.responders).unwrap();
        assert_eq!(hidden.questions.len(),7);
        assert!(hidden.answer_content("$card",&[("q0".into(),AnswerValue::Text("secret".into()))].into()).is_err());
    }

    #[test]
    fn secret_questions_never_reach_the_room() {
        let card = card();
        assert_eq!(card.questions.len(), 1);
        assert_eq!(card.withheld, 1);
        let content = card.message_content().to_string();
        assert!(!content.contains("deploy token") && !content.contains("\"token\""));
        let only_secret: Vec<_> = questions().into_iter().filter(|q| q.is_secret).collect();
        assert!(Card::ask(card.id.clone(), "$t".into(), "$r".into(), 1, &only_secret).is_none());
    }

    #[test]
    fn approval_questions_never_become_cards_even_when_mixed_or_withheld() {
        for id in [
            "mcp_tool_call_approval_call",
            "skill_mcp_dependency_install",
        ] {
            for secret in [false, true] {
                let mut questions = questions();
                questions[1].id = id.into();
                questions[1].is_secret = secret;
                assert!(
                    Card::ask(
                        format!("aq_{}", "a".repeat(64)),
                        "$t".into(),
                        "$r".into(),
                        1,
                        &questions
                    )
                    .is_none(),
                    "{id}, secret: {secret}"
                );
            }
        }
    }

    #[test]
    fn card_message_has_a_text_fallback_and_round_trips() {
        let card = card();
        let content = card.message_content();
        assert_eq!(content["msgtype"], MSGTYPE);
        assert_eq!(content["m.relates_to"]["rel_type"], "m.thread");
        assert_eq!(content["m.relates_to"]["event_id"], "$root");
        let body = content["body"].as_str().unwrap();
        assert!(
            body.contains("Which database")
                && body.contains("SQLite — Simplest to run.")
                && body.contains("Other")
        );
        assert!(body.contains("text reply is a new message"));
        // The deadline is an instant, not a duration the reader cannot anchor.
        assert!(
            body.contains("by 2027-01-15 08:00 UTC")
                && body.contains("请在 2027-01-15 08:00 UTC 前"),
            "{body}"
        );
        assert!(!body.contains(" min."));
        assert_eq!(Card::from_content(&content), Some(card.clone()));
        let answered = card.answered(
            "@alice:example.test",
            BTreeMap::from([("db".into(), AnswerValue::Option("SQLite".into()))]),
        );
        let edit = answered.edit_content("$card");
        assert_eq!(
            edit["m.relates_to"],
            json!({"rel_type":"m.replace","event_id":"$card"})
        );
        assert_eq!(
            Card::from_content(&edit["m.new_content"]),
            Some(answered.clone())
        );
        assert!(
            edit["m.new_content"]["body"]
                .as_str()
                .unwrap()
                .contains("Answered by @alice:example.test")
        );
        assert!(
            edit["m.new_content"]["body"]
                .as_str()
                .unwrap()
                .contains("→ SQLite")
        );
        let expired = card.expired().edit_content("$card");
        assert!(
            expired["m.new_content"]["body"]
                .as_str()
                .unwrap()
                .starts_with("⌛")
        );
        let mut forged = content.clone();
        forged[MSGTYPE]["state"] = json!("answered");
        assert!(
            Card::from_content(&forged).is_none(),
            "an answered card names who answered"
        );
        let mut other = content;
        other["msgtype"] = json!("m.text");
        assert!(Card::from_content(&other).is_none());
    }

    #[test]
    fn answers_must_match_the_exact_card_and_its_choices() {
        let card = card();
        let answers = BTreeMap::from([("db".into(), AnswerValue::Option("SQLite".into()))]);
        let content = card.answer_content("$card", &answers).unwrap();
        assert_eq!(
            content["m.relates_to"],
            json!({"rel_type":"m.reference","event_id":"$card"})
        );
        assert_eq!(content["answers"], json!({"db":{"option":"SQLite"}}));
        assert_eq!(card.parse_answer("$card", &content), Some(answers));
        assert!(card.parse_answer("$other", &content).is_none());
        let typed = BTreeMap::from([("db".into(), AnswerValue::Text("MySQL".into()))]);
        assert!(card.answer_content("$card", &typed).is_ok());
        for bad in [
            BTreeMap::from([("db".into(), AnswerValue::Option("Mongo".into()))]),
            BTreeMap::from([("token".into(), AnswerValue::Text("hunter2".into()))]),
            BTreeMap::from([("db".into(), AnswerValue::Text("  ".into()))]),
            BTreeMap::new(),
        ] {
            assert!(card.answer_content("$card", &bad).is_err());
        }
        let mut wrong = content.clone();
        wrong["cardId"] = json!(format!("aq_{}", "b".repeat(64)));
        assert!(card.parse_answer("$card", &wrong).is_none());
        let mut wrong = content;
        wrong["answers"]["db"] = json!({"option":"SQLite","text":"both"});
        assert!(card.parse_answer("$card", &wrong).is_none());
        assert!(is_agent("@_hagency_agt_01abc:example.test"));
        assert!(!is_agent("@alice:example.test") && !is_agent("@_hagency_service:example.test"));
    }

    #[test]
    fn only_authenticated_answers_from_others_are_admissible() {
        let mut entries = Entries::default();
        let event = json!({"type":ANSWER_EVENT,"event_id":"$a","sender":"@alice:example.test",
            "content":{"m.relates_to":{"rel_type":"m.reference","event_id":"$card"}}});
        entries.observe("!room:test", &event, None, None);
        let mut answer = entries.answers.remove(0);
        assert!(
            answer.admissible(false, Some("@agent:example.test")),
            "plaintext room, plaintext answer"
        );
        assert!(
            !answer.admissible(true, Some("@agent:example.test")),
            "plaintext answer in an encrypted room"
        );
        answer.authentic = Some(false);
        assert!(
            !answer.admissible(true, None),
            "Megolm sender is not the event sender"
        );
        answer.authentic = Some(true);
        assert!(answer.admissible(true, None));
        assert!(
            !answer.admissible(true, Some("@alice:example.test")),
            "the Agent never answers itself"
        );
        // No Agent answers any card, on any server, encrypted or not.
        for agent in [
            "@_hagency_agt_other:example.test",
            "@_hagency_agt_remote:elsewhere.test",
        ] {
            answer.sender = agent.into();
            assert!(
                !answer.admissible(true, Some("@_hagency_agt_asking:example.test")),
                "{agent}"
            );
            assert!(!answer.admissible(false, None), "{agent}");
        }
    }

    #[test]
    fn only_an_agent_of_the_rooms_server_posts_cards_and_expiry_is_bounded() {
        assert_eq!(server_name("!room:example.test"), Some("example.test"));
        assert_eq!(
            server_name("@_hagency_agt_a:example.test:8448"),
            Some("example.test:8448")
        );
        assert_eq!(server_name("!opaque"), None);
        assert!(is_agent_on("@_hagency_agt_a:example.test", "example.test"));
        assert!(
            !is_agent_on("@_hagency_agt_a:elsewhere.test", "example.test"),
            "another server's lookalike"
        );
        assert!(!is_agent_on("@alice:example.test", "example.test"));
        let mut card = card();
        let posted = card.expires_at_ms - ANSWER_WAIT_MS;
        assert!(card.expiry_plausible(posted));
        assert!(card.expiry_plausible(posted - EXPIRY_SKEW_MS), "clock skew");
        assert!(!card.expiry_plausible(posted - EXPIRY_SKEW_MS - 1));
        card.expires_at_ms = u64::MAX;
        assert!(
            !card.expiry_plausible(posted),
            "a card cannot wait for days"
        );
    }

    #[test]
    fn inbox_keeps_arrival_order_and_late_decryptions_keep_their_place() {
        let mut entries = Entries::default();
        let event = |id: &str, kind: &str| {
            json!({"type":kind,"event_id":id,"sender":"@alice:example.test",
            "content":{"m.relates_to":{"rel_type":"m.reference","event_id":"$card"}}})
        };
        entries.observe(
            "!room:test",
            &event("$late", "m.room.encrypted"),
            None,
            None,
        );
        entries.observe("!room:test", &event("$second", ANSWER_EVENT), None, None);
        entries.observe(
            "!room:test",
            &event("$thread", "m.room.message"),
            None,
            None,
        );
        let order = entries.undecrypted[0].order;
        entries.observe(
            "!room:test",
            &event("$late", ANSWER_EVENT),
            None,
            Some(order),
        );
        assert!(entries.undecrypted.is_empty());
        let ids: Vec<_> = entries
            .answers
            .iter()
            .map(|e| e.event_id.as_str())
            .collect();
        assert_eq!(ids, ["$late", "$second"]);
        let mut unrelated = event("$x", ANSWER_EVENT);
        unrelated["content"]["m.relates_to"]["rel_type"] = json!("m.thread");
        entries.observe("!room:test", &unrelated, None, None);
        assert_eq!(entries.answers.len(), 2);
    }
}
