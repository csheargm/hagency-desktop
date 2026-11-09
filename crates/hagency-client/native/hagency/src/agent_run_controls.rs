//! Matrix run controls carry no tool-approval authority. The executor combines
//! these closed-schema values with the authenticated Matrix sender and the
//! still-running dispatch/lease before accepting an action.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const MSGTYPE: &str = "im.hagency.agent_run";
pub const ACTION_EVENT: &str = "im.hagency.agent_run.action";
pub const VERSION: u64 = 1;
pub const MAX_INPUT_BYTES: usize = 32 * 1024;
pub const MAX_ACTION_RECEIPTS: usize = 64;
// Edits carry both the outer card and m.new_content. Keep enough room for
// the Matrix envelope even when receipt IDs contain long escaped strings.
const MAX_CARD_BYTES: usize = 20 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Running,
    Stopping,
    Completed,
    Interrupted,
    Failed,
    Unknown,
}
impl State {
    pub fn active(self) -> bool {
        matches!(self, Self::Running | Self::Stopping)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum Action {
    Steer { text: String },
    Queue { text: String },
    // Empty struct variant makes serde reject extra fields here as well.
    Interrupt {},
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Receipt {
    pub event_id: String,
    pub sender: String,
    pub outcome: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Card {
    pub version: u64,
    pub id: String,
    pub dispatch_id: String,
    pub execution_id: String,
    pub room_id: String,
    pub event_id: String,
    pub thread_root: String,
    pub requester: String,
    pub owner: String,
    pub runtime: String,
    pub state: State,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub preview: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_action: Option<Receipt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub action_receipts: Vec<Receipt>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Envelope {
    version: u64,
    card_id: String,
    execution_id: String,
    action: Action,
    #[serde(rename = "m.relates_to")]
    relation: Relation,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Relation {
    rel_type: String,
    event_id: String,
}
fn id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control)
}
fn event(value: &str) -> bool {
    value.starts_with('$') && id(value)
}
fn person(value: &str) -> bool {
    matrix_sdk::ruma::OwnedUserId::try_from(value).is_ok()
        && !crate::agent_questions::is_agent(value)
}
impl Card {
    pub fn same_identity(&self, other: &Self) -> bool {
        let mut original = self.clone();
        original.state = other.state;
        original.revision = other.revision;
        original.preview = other.preview.clone();
        original.last_action = other.last_action.clone();
        original.action_receipts = other.action_receipts.clone();
        original == *other
    }
    /// Shared-resource executors are not automatically the Agent's owner.
    /// Only the original authenticated task requester controls this run.
    pub fn for_shared(
        work: &hagency_store::owner_resources::shared_execution::SharedWorkItem,
        call: &str,
    ) -> Result<Self, &'static str> {
        let identity = serde_json::to_vec(&(
            &work.assignment_id,
            &work.runtime_command_digest,
            &work.id,
            call,
        ))
        .map_err(|_| "run_identity")?;
        let card = Self {
            version: VERSION,
            id: format!("run_{:x}", Sha256::digest(identity)),
            dispatch_id: work.id.clone(),
            execution_id: call.into(),
            room_id: work.room_id.clone(),
            event_id: work.event_id.clone(),
            thread_root: work.thread_root.clone(),
            requester: work.requester_mxid.clone(),
            owner: work.requester_mxid.clone(),
            runtime: "codex".into(),
            state: State::Running,
            revision: 0,
            preview: String::new(),
            last_action: None,
            action_receipts: Vec::new(),
        };
        card.validate()?;
        Ok(card)
    }
    pub fn new(
        dispatch: &hagency_agent_local::inbox::Dispatch,
        execution: &str,
        owner: &str,
        runtime: &str,
    ) -> Result<Self, &'static str> {
        let encoded = serde_json::to_vec(&(dispatch.scope(), &dispatch.id, execution))
            .map_err(|_| "run_identity")?;
        let card = Self {
            version: VERSION,
            id: format!("run_{:x}", Sha256::digest(encoded)),
            dispatch_id: dispatch.id.clone(),
            execution_id: execution.into(),
            room_id: dispatch.room_id.clone(),
            event_id: dispatch.event_id.clone(),
            thread_root: dispatch.thread_root.clone(),
            requester: dispatch.requester_mxid.clone(),
            owner: owner.into(),
            runtime: runtime.into(),
            state: State::Running,
            revision: 0,
            preview: String::new(),
            last_action: None,
            action_receipts: Vec::new(),
        };
        card.validate()?;
        Ok(card)
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != VERSION
            || !self.id.starts_with("run_")
            || !id(&self.id)
            || !id(&self.dispatch_id)
            || !id(&self.execution_id)
            || !event(&self.event_id)
            || matrix_sdk::ruma::OwnedRoomId::try_from(self.room_id.as_str()).is_err()
            || !(event(&self.thread_root) || self.thread_root == self.room_id)
            || !person(&self.requester)
            || !person(&self.owner)
            || !matches!(self.runtime.as_str(), "codex" | "claude")
            || self.preview.len() > hagency_runtime::live_text::MAX_PREVIEW_BYTES
            || self
                .preview
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\t' | '\r'))
            || self.action_receipts.len() > MAX_ACTION_RECEIPTS
            || serde_json::to_vec(self).map_or(true, |bytes| bytes.len() > MAX_CARD_BYTES)
            || self
                .last_action
                .iter()
                .chain(&self.action_receipts)
                .any(|r| {
                    !event(&r.event_id)
                        || !person(&r.sender)
                        || !matches!(
                            r.outcome.as_str(),
                            "accepted"
                                | "queued"
                                | "rejected"
                                | "unsupported"
                                | "in_progress"
                                | "busy"
                                | "unavailable"
                                | "unknown"
                        )
                })
        {
            return Err("invalid_run_card");
        }
        Ok(())
    }
    pub fn record_receipt(&mut self, receipt: Receipt) {
        self.action_receipts
            .retain(|r| r.event_id != receipt.event_id);
        self.action_receipts.push(receipt.clone());
        self.last_action = Some(receipt);
        self.fit_display();
    }
    pub(crate) fn fit_display(&mut self) {
        while self.action_receipts.len() > 1
            && (self.action_receipts.len() > MAX_ACTION_RECEIPTS
                || serde_json::to_vec(self).map_or(true, |bytes| bytes.len() > MAX_CARD_BYTES))
        {
            self.action_receipts.remove(0);
        }
        // A later live preview may fill the space previously used by receipts.
        // Presentation is optional: never make an admitted turn fail for it.
        if serde_json::to_vec(self).map_or(true, |bytes| bytes.len() > MAX_CARD_BYTES) {
            self.preview.clear();
        }
    }
    pub fn from_content(content: &Value) -> Option<Self> {
        if content["msgtype"] != MSGTYPE {
            return None;
        }
        let card: Self = serde_json::from_value(content[MSGTYPE].clone()).ok()?;
        card.validate().ok()?;
        Some(card)
    }
    pub fn content(&self) -> Value {
        let body = match self.state {
            State::Running => "Agent 正在处理，可追加指令或停止本次任务。",
            State::Stopping => "已请求停止，正在等待原生运行结束。",
            State::Completed => "本次任务已完成。",
            State::Interrupted => "本次任务已停止。",
            State::Failed => "本次任务执行失败。",
            State::Unknown => "本次任务已离线，运行结果需要核对。",
        };
        let root = if event(&self.thread_root) {
            &self.thread_root
        } else {
            &self.event_id
        };
        json!({"msgtype":MSGTYPE,"body":body,MSGTYPE:self,"m.relates_to":{"rel_type":"m.thread","event_id":root,"is_falling_back":true,"m.in_reply_to":{"event_id":self.event_id}}})
    }
    pub fn edit_content(&self, card_event: &str) -> Result<Value, &'static str> {
        if !event(card_event) {
            return Err("invalid_run_event");
        }
        let content = self.content();
        Ok(
            json!({"msgtype":MSGTYPE,"body":content["body"],MSGTYPE:self,"m.new_content":content,"m.relates_to":{"rel_type":"m.replace","event_id":card_event}}),
        )
    }
    pub fn permits(&self, sender: &str, action: &Action) -> bool {
        self.state == State::Running
            && person(sender)
            && match action {
                Action::Interrupt {} => sender == self.requester || sender == self.owner,
                Action::Steer { text } => {
                    sender == self.requester && self.runtime == "codex" && valid_text(text)
                }
                Action::Queue { text } => {
                    sender == self.requester && self.runtime == "claude" && valid_text(text)
                }
            }
    }
    pub fn action_content(
        &self,
        card_event: &str,
        sender: &str,
        action: Action,
    ) -> Result<Value, &'static str> {
        self.validate()?;
        if !event(card_event) || !self.permits(sender, &action) {
            return Err("run_action_not_allowed");
        }
        serde_json::to_value(Envelope {
            version: VERSION,
            card_id: self.id.clone(),
            execution_id: self.execution_id.clone(),
            action,
            relation: Relation {
                rel_type: "m.reference".into(),
                event_id: card_event.into(),
            },
        })
        .map_err(|_| "run_action")
    }
    /// Claude's busy input is a separate admitted turn. Send an ordinary
    /// addressed room message through the existing durable dispatch queue;
    /// never invent a dispatch or feed a second unaccounted native command.
    pub fn queued_message(
        &self,
        card_event: &str,
        sender: &str,
        agent: &str,
        text: &str,
    ) -> Result<Value, &'static str> {
        self.validate()?;
        if !event(card_event)
            || !crate::agent_questions::is_agent(agent)
            || !self.permits(sender, &Action::Queue { text: text.into() })
        {
            return Err("run_action_not_allowed");
        }
        let mut value = json!({"msgtype":"m.text","body":text,"m.mentions":{"user_ids":[agent]},
            "im.hagency.queued_after":{"cardEventId":card_event,"executionId":self.execution_id},
            "m.relates_to":{"m.in_reply_to":{"event_id":self.event_id}}});
        if event(&self.thread_root) {
            value["m.relates_to"] = json!({"rel_type":"m.thread","event_id":self.thread_root,"is_falling_back":true,"m.in_reply_to":{"event_id":self.event_id}});
        }
        Ok(value)
    }
    /// The caller must first prove current room membership, Megolm sender
    /// authenticity in encrypted rooms, and this original dispatch's lease.
    pub fn receive(&self, card_event: &str, sender: &str, content: &Value) -> Option<Action> {
        self.validate().ok()?;
        let value: Envelope = serde_json::from_value(content.clone()).ok()?;
        (value.version == VERSION
            && value.card_id == self.id
            && value.execution_id == self.execution_id
            && value.relation.rel_type == "m.reference"
            && value.relation.event_id == card_event
            && self.permits(sender, &value.action))
        .then_some(value.action)
    }
}
fn valid_text(text: &str) -> bool {
    !text.trim().is_empty()
        && text.len() <= MAX_INPUT_BYTES
        && serde_json::to_vec(text).is_ok_and(|v| v.len() <= MAX_INPUT_BYTES)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[test]
    fn receipt_history_keeps_card_edits_within_the_wire_budget() {
        let mut card = card();
        for i in 0..100 {
            card.record_receipt(Receipt {
                event_id: format!("${i}{}", "x".repeat(1000)),
                sender: "@requester:test".into(),
                outcome: "unavailable".into(),
            });
        }
        card.validate().unwrap();
        assert!(card.action_receipts.len() < MAX_ACTION_RECEIPTS);
        assert!(
            card.last_action
                .as_ref()
                .unwrap()
                .event_id
                .starts_with("$99")
        );
        card.preview = "\\".repeat(hagency_runtime::live_text::MAX_PREVIEW_BYTES);
        card.fit_display();
        card.validate().unwrap();
        let encoded = serde_json::to_vec(&card.edit_content("$card").unwrap()).unwrap();
        assert!(encoded.len() < 44 * 1024);
        assert!(
            encoded.len().div_ceil(3) * 4 < 60 * 1024,
            "leave room for the encrypted Matrix envelope"
        );
    }
    pub(crate) fn card() -> Card {
        Card {
            version: VERSION,
            id: "run_fixture".into(),
            dispatch_id: "dispatch".into(),
            execution_id: "execution".into(),
            room_id: "!room:test".into(),
            event_id: "$trigger".into(),
            thread_root: "!room:test".into(),
            requester: "@requester:test".into(),
            owner: "@owner:test".into(),
            runtime: "codex".into(),
            state: State::Running,
            revision: 0,
            preview: String::new(),
            last_action: None,
            action_receipts: Vec::new(),
        }
    }

    #[test]
    fn controls_are_bound_to_requester_owner_and_original_execution() {
        let c = card();
        let value = c
            .action_content(
                "$card",
                &c.requester,
                Action::Steer {
                    text: "继续原任务".into(),
                },
            )
            .unwrap();
        assert!(matches!(
            c.receive("$card", &c.requester, &value),
            Some(Action::Steer { .. })
        ));
        for sender in [&c.owner, "@other:test", "@_hagency_agt_test:test"] {
            assert!(c.receive("$card", sender, &value).is_none());
        }
        assert!(c.receive("$another", &c.requester, &value).is_none());
        for key in ["version", "cardId", "executionId"] {
            let mut forged = value.clone();
            forged[key] = json!("different");
            assert!(c.receive("$card", &c.requester, &forged).is_none());
        }
        let stop = c
            .action_content("$card", &c.owner, Action::Interrupt {})
            .unwrap();
        assert_eq!(
            c.receive("$card", &c.owner, &stop),
            Some(Action::Interrupt {})
        );
        let mut shared = c.clone();
        shared.owner = shared.requester.clone();
        assert!(
            shared.receive("$card", &c.owner, &stop).is_none(),
            "executor has no implicit control authority"
        );
        for state in [
            State::Stopping,
            State::Completed,
            State::Interrupted,
            State::Failed,
            State::Unknown,
        ] {
            let mut closed = c.clone();
            closed.state = state;
            assert!(closed.receive("$card", &c.requester, &value).is_none());
            assert!(closed.receive("$card", &c.owner, &stop).is_none());
        }
    }

    #[test]
    fn closed_schema_cannot_smuggle_tool_approval_or_oversized_input() {
        let c = card();
        let value = c
            .action_content("$card", &c.requester, Action::Interrupt {})
            .unwrap();
        for forged in [
            {
                let mut v = value.clone();
                v["approval"] = json!(true);
                v
            },
            {
                let mut v = value.clone();
                v["action"]["allow"] = json!(true);
                v
            },
            {
                let mut v = value.clone();
                v["action"]["kind"] = json!("approve");
                v
            },
            {
                let mut v = value;
                v["m.relates_to"]["rel_type"] = json!("m.thread");
                v
            },
        ] {
            assert!(c.receive("$card", &c.requester, &forged).is_none());
        }
        for text in [
            " ".into(),
            "中".repeat(MAX_INPUT_BYTES),
            "\n".repeat(MAX_INPUT_BYTES / 2),
        ] {
            assert!(
                c.action_content("$card", &c.requester, Action::Steer { text })
                    .is_err()
            );
        }
        let mut impersonated = c;
        impersonated.owner = "@_hagency_agt_test:test".into();
        assert!(Card::from_content(&impersonated.content()).is_none());
    }

    #[test]
    fn claude_queue_preserves_dispatch_scope_and_uses_ordinary_addressed_message() {
        let mut c = card();
        c.runtime = "claude".into();
        let agent = "@_hagency_agt_test:test";
        let content = c
            .queued_message("$card", &c.requester, agent, "next task")
            .unwrap();
        assert_eq!(content["msgtype"], "m.text");
        assert_eq!(content["m.mentions"]["user_ids"], json!([agent]));
        assert!(
            content["m.relates_to"]["rel_type"].is_null(),
            "room-scoped input must not accidentally create a new thread"
        );
        assert_eq!(
            content["m.relates_to"]["m.in_reply_to"]["event_id"],
            "$trigger"
        );
        c.thread_root = "$thread".into();
        let content = c
            .queued_message("$card", &c.requester, agent, "next task")
            .unwrap();
        assert_eq!(content["m.relates_to"]["event_id"], "$thread");
        assert_eq!(content["m.relates_to"]["rel_type"], "m.thread");
        assert!(
            c.queued_message("$card", &c.owner, agent, "next task")
                .is_err()
        );
        assert!(
            c.action_content(
                "$card",
                &c.requester,
                Action::Steer {
                    text: "extra".into()
                }
            )
            .is_err()
        );
    }
}
