//! Codex 0.166 `item/tool/requestUserInput`: the model asks the user one or
//! more questions and waits for the answers. Parsed text is untrusted data,
//! never authority. The host decides who may answer and for how long; this
//! module only checks shapes and builds the one response frame Codex expects.
use super::{Error, valid_id};
use crate::codex::RequestId;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const USER_INPUT_METHOD: &str = "item/tool/requestUserInput";
/// Parse bounds. Codex's own `request_user_input` tool asks one to three
/// questions with two or three options each; a host may show fewer.
pub const MAX_USER_INPUT_QUESTIONS: usize = 8;
pub const MAX_USER_INPUT_OPTIONS: usize = 16;
/// One question's text, and one free-text answer.
pub const MAX_USER_INPUT_TEXT_BYTES: usize = 4096;
const MAX_LABEL_BYTES: usize = 256;
const MAX_DESCRIPTION_BYTES: usize = 1024;
const MAX_REQUEST_BYTES: usize = 64 * 1024;
/// Codex's own client answers an "Other" choice with this label followed by
/// the typed text on a `user_note: ` line, and a free-form question with the
/// note alone. Hosts send the same words so the model reads one convention.
pub const OTHER_ANSWER_LABEL: &str = "None of the above";
const NOTE_PREFIX: &str = "user_note: ";
/// Question ids of Codex's own approval prompts that reach a client as
/// questions (0.166): an MCP tool call when MCP elicitation is off
/// (`mcp_tool_call_approval_<call>`), and a skill's MCP server install.
const CODEX_APPROVAL_PREFIX: &str = "mcp_tool_call_approval";
const CODEX_INSTALL_PROMPT: &str = "skill_mcp_dependency_install";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserInputOption {
    pub label: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    /// `None` (or empty) is a free-form question.
    pub options: Option<Vec<UserInputOption>>,
    pub is_other: bool,
    /// Codex marks a question secret when its answer must stay private.
    pub is_secret: bool,
}
impl UserInputQuestion {
    /// Codex's own tool/install permission prompt, never a question a room
    /// member may answer. The ids are protocol markers, not displayed text.
    pub fn is_codex_approval(&self) -> bool {
        self.id.starts_with(CODEX_APPROVAL_PREFIX) || self.id == CODEX_INSTALL_PROMPT
    }

    /// A typed answer is accepted for an "Other" choice or a free-form question.
    pub fn accepts_text(&self) -> bool {
        self.is_other || self.options.as_ref().is_none_or(Vec::is_empty)
    }
    fn has_option(&self, label: &str) -> bool {
        self.options
            .as_ref()
            .is_some_and(|options| options.iter().any(|option| option.label == label))
    }
}

/// One answer a host collected for one question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserInputAnswer {
    /// Exactly one of the question's own option labels.
    Option(String),
    /// Free text for an "Other" choice or a free-form question.
    Text(String),
}

/// One validated request, bound to the exact turn and callback that sent it.
#[derive(Clone, Debug)]
pub struct UserInputRequest {
    pub(crate) id: RequestId,
    thread_id: String,
    turn_id: String,
    item_id: String,
    questions: Vec<UserInputQuestion>,
    blocking: bool,
}

/// The single response frame for one request. Only [`UserInputRequest`]
/// builds one: there is no constructor from arbitrary JSON.
#[derive(Debug)]
pub struct UserInputResponse {
    pub(in crate::codex) id: RequestId,
    pub(in crate::codex) thread_id: String,
    pub(in crate::codex) turn_id: String,
    pub(in crate::codex) result: Value,
}
impl UserInputResponse {
    pub fn request_id(&self) -> &RequestId {
        &self.id
    }
    /// The exact `ToolRequestUserInputResponse` payload, for inspection only.
    pub fn result(&self) -> &Value {
        &self.result
    }
    /// The answer Codex itself submits when its client cannot ask: no answers,
    /// so the model decides on its own.
    pub(in crate::codex) fn unanswered(id: RequestId, thread_id: &str, turn_id: &str) -> Self {
        Self {
            id,
            thread_id: thread_id.into(),
            turn_id: turn_id.into(),
            result: json!({"answers":{}}),
        }
    }
    /// The wire shape check the connection repeats before writing: an object
    /// of question ids to `{"answers":[string…]}`, nothing else.
    pub(in crate::codex) fn well_formed(result: &Value) -> bool {
        let Some(object) = result.as_object() else {
            return false;
        };
        object.len() == 1
            && object
                .get("answers")
                .and_then(Value::as_object)
                .is_some_and(|answers| {
                    answers.len() <= MAX_USER_INPUT_QUESTIONS
                        && answers.iter().all(|(id, answer)| {
                            valid_id(id).is_ok()
                                && answer.as_object().is_some_and(|answer| {
                                    answer.len() == 1
                                        && answer
                                            .get("answers")
                                            .and_then(Value::as_array)
                                            .is_some_and(|values| {
                                                values.len() <= 2
                                                    && values.iter().all(|v| {
                                                        v.as_str().is_some_and(|s| {
                                                            s.len()
                                                                <= MAX_USER_INPUT_TEXT_BYTES
                                                                    + NOTE_PREFIX.len()
                                                        })
                                                    })
                                            })
                                })
                        })
                })
    }
}

impl UserInputRequest {
    pub fn id(&self) -> &RequestId {
        &self.id
    }
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }
    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }
    /// Codex's tool call item; not a timeline item this session tracks.
    pub fn item_id(&self) -> &str {
        &self.item_id
    }
    pub fn questions(&self) -> &[UserInputQuestion] {
        &self.questions
    }
    pub fn is_blocking(&self) -> bool {
        self.blocking
    }
    /// Codex asking for one of its own approvals, not the model asking a
    /// question. Answering "Allow" would grant it, so it is never shown to
    /// people who may answer questions; no answers declines it.
    pub fn is_codex_approval(&self) -> bool {
        self.questions
            .iter()
            .any(UserInputQuestion::is_codex_approval)
    }

    /// Decode untrusted callback data. The caller must still bind this to an
    /// original live connection and its exact native turn before responding.
    pub fn parse(id: RequestId, params: &Value) -> Result<Self, Error> {
        if serde_json::to_vec(params)
            .map_err(|_| Error::Malformed)?
            .len()
            > MAX_REQUEST_BYTES
        {
            return Err(Error::Capacity);
        }
        let identity = |key: &str| -> Result<String, Error> {
            let value = params
                .get(key)
                .and_then(Value::as_str)
                .ok_or(Error::Malformed)?;
            valid_id(value)?;
            Ok(value.to_owned())
        };
        let thread_id = identity("threadId")?;
        let turn_id = identity("turnId")?;
        let item_id = identity("itemId")?;
        let blocking = params
            .get("isBlocking")
            .and_then(Value::as_bool)
            .ok_or(Error::Malformed)?;
        // Deprecated by Codex in favour of `isBlocking`; accepted, never acted on.
        if params
            .get("autoResolutionMs")
            .is_some_and(|value| !value.is_null() && !value.is_u64())
        {
            return Err(Error::Malformed);
        }
        let raw = params
            .get("questions")
            .and_then(Value::as_array)
            .ok_or(Error::Malformed)?;
        if raw.is_empty() || raw.len() > MAX_USER_INPUT_QUESTIONS {
            return Err(Error::Malformed);
        }
        let mut ids = BTreeSet::new();
        let mut questions = Vec::with_capacity(raw.len());
        for value in raw {
            let question = question(value)?;
            if !ids.insert(question.id.clone()) {
                return Err(Error::Malformed);
            }
            questions.push(question);
        }
        Ok(Self {
            id,
            thread_id,
            turn_id,
            item_id,
            questions,
            blocking,
        })
    }

    /// No answers at all: the model decides on its own. Used when nobody
    /// answered in time, nothing could be shown, or the question was withdrawn.
    pub fn unanswered(&self) -> UserInputResponse {
        UserInputResponse::unanswered(self.id.clone(), &self.thread_id, &self.turn_id)
    }

    /// Data for a shared host UI; no native response or approval authority.
    pub fn host_questions(&self) -> Vec<crate::questions::Question> {
        self.questions.iter().map(crate::questions::Question::from).collect()
    }

    pub fn answer_host(&self, answers: &crate::questions::Answers) -> Result<UserInputResponse, Error> {
        use crate::questions::Answer;
        let answers = answers.iter().map(|(id, answer)| {
            let answer = match answer {
                Answer::Option(value) => UserInputAnswer::Option(value.clone()),
                Answer::Text(value) => UserInputAnswer::Text(value.clone()),
                Answer::Options(_) => return Err(Error::Malformed),
            };
            Ok((id.clone(), answer))
        }).collect::<Result<_, _>>()?;
        self.answer(&answers)
    }

    /// The collected answers, in the words Codex's own client uses. Every
    /// question gets an entry; one without an answer gets an empty list. A
    /// secret question can never be answered through a host, and an option
    /// must be one the question offered.
    pub fn answer(
        &self,
        answers: &BTreeMap<String, UserInputAnswer>,
    ) -> Result<UserInputResponse, Error> {
        // Keep the boundary here as well as at session dispatch: no caller
        // can turn a parsed approval (even mixed with ordinary questions)
        // into an approval response through this public question API.
        if self.is_codex_approval() {
            return Err(Error::Policy);
        }
        if answers.is_empty() {
            return Err(Error::Malformed);
        }
        for id in answers.keys() {
            let question = self
                .questions
                .iter()
                .find(|question| &question.id == id)
                .ok_or(Error::Scope)?;
            if question.is_secret {
                return Err(Error::Policy);
            }
        }
        let mut result = Map::new();
        for question in &self.questions {
            let values = match answers.get(&question.id) {
                None => Vec::new(),
                Some(UserInputAnswer::Option(label)) => {
                    if !question.has_option(label) {
                        return Err(Error::Scope);
                    }
                    vec![label.clone()]
                }
                Some(UserInputAnswer::Text(text)) => {
                    let text = text.trim();
                    if !question.accepts_text()
                        || text.is_empty()
                        || !prose(text, MAX_USER_INPUT_TEXT_BYTES)
                    {
                        return Err(Error::Malformed);
                    }
                    let note = format!("{NOTE_PREFIX}{text}");
                    if question.options.as_ref().is_some_and(|o| !o.is_empty()) {
                        vec![OTHER_ANSWER_LABEL.to_owned(), note]
                    } else {
                        vec![note]
                    }
                }
            };
            result.insert(question.id.clone(), json!({ "answers": values }));
        }
        let result = json!({ "answers": result });
        debug_assert!(UserInputResponse::well_formed(&result));
        Ok(UserInputResponse {
            id: self.id.clone(),
            thread_id: self.thread_id.clone(),
            turn_id: self.turn_id.clone(),
            result,
        })
    }
}

impl From<&UserInputQuestion> for crate::questions::Question {
    fn from(q: &UserInputQuestion) -> Self {
        Self {
            id: q.id.clone(), header: q.header.clone(), question: q.question.clone(),
            options: q.options.iter().flatten().map(|o| crate::questions::QuestionOption {
                label: o.label.clone(), description: o.description.clone(),
            }).collect(),
            other: q.accepts_text(), secret: q.is_secret, multi_select: false,
        }
    }
}

fn question(value: &Value) -> Result<UserInputQuestion, Error> {
    let string = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .ok_or(Error::Malformed)
    };
    let flag = |key: &str| match value.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(_) => Err(Error::Malformed),
    };
    if !value.is_object() {
        return Err(Error::Malformed);
    }
    let id = string("id")?;
    valid_id(id)?;
    let header = string("header")?;
    let text = string("question")?;
    if (!header.is_empty() && !prose(header, MAX_LABEL_BYTES))
        || !prose(text, MAX_USER_INPUT_TEXT_BYTES)
    {
        return Err(Error::Malformed);
    }
    let options = match value.get("options") {
        None | Some(Value::Null) => None,
        Some(Value::Array(raw)) => {
            if raw.len() > MAX_USER_INPUT_OPTIONS {
                return Err(Error::Malformed);
            }
            let mut labels = BTreeSet::new();
            let mut options = Vec::with_capacity(raw.len());
            for option in raw {
                let label = option
                    .get("label")
                    .and_then(Value::as_str)
                    .ok_or(Error::Malformed)?;
                let description = option
                    .get("description")
                    .and_then(Value::as_str)
                    .ok_or(Error::Malformed)?;
                if !prose(label, MAX_LABEL_BYTES)
                    || label.contains('\n')
                    || (!description.is_empty() && !prose(description, MAX_DESCRIPTION_BYTES))
                    || !labels.insert(label)
                {
                    return Err(Error::Malformed);
                }
                options.push(UserInputOption {
                    label: label.into(),
                    description: description.into(),
                });
            }
            Some(options)
        }
        Some(_) => return Err(Error::Malformed),
    };
    Ok(UserInputQuestion {
        id: id.into(),
        header: header.into(),
        question: text.into(),
        options,
        is_other: flag("isOther")?,
        is_secret: flag("isSecret")?,
    })
}

/// Printable text that may span lines: non-empty after trimming, bounded, and
/// free of control characters other than newline and tab.
fn prose(value: &str, max: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= max
        && !value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> Value {
        json!({"threadId":"thread","turnId":"turn","itemId":"call","isBlocking":true,"autoResolutionMs":null,
        "questions":[
            {"id":"db","header":"Database","question":"Which database?","isOther":true,"isSecret":false,
             "options":[{"label":"Postgres (Recommended)","description":"Durable."},{"label":"SQLite","description":"Simple."}]},
            {"id":"name","header":"Name","question":"What should it be called?","options":null},
            {"id":"token","header":"Token","question":"Paste the token","isSecret":true,"options":null}
        ]})
    }

    #[test]
    fn parses_codex_shapes_and_tolerates_new_fields() {
        let mut p = params();
        p["futureField"] = json!(true);
        p["questions"][0]["futureField"] = json!(1);
        let request = UserInputRequest::parse(RequestId::Number(4), &p).unwrap();
        assert_eq!(request.questions().len(), 3);
        assert!(request.questions()[0].accepts_text());
        assert!(
            request.questions()[1].accepts_text(),
            "a free-form question takes text"
        );
        assert!(request.questions()[2].is_secret);
        assert!(request.is_blocking());
        assert_eq!(request.item_id(), "call");
    }

    #[test]
    fn refuses_malformed_or_oversized_requests() {
        for (path, value) in [
            ("/threadId", json!(null)),
            ("/isBlocking", json!("yes")),
            ("/questions", json!([])),
            ("/questions/0/id", json!("")),
            ("/questions/0/question", json!("\u{0007}bell")),
            (
                "/questions/0/options/1/label",
                json!("Postgres (Recommended)"),
            ),
            ("/questions/0/options", json!("Postgres")),
            ("/questions/0/isOther", json!("true")),
            ("/autoResolutionMs", json!(-1)),
        ] {
            let mut p = params();
            *p.pointer_mut(path).unwrap() = value;
            assert!(
                UserInputRequest::parse(RequestId::Number(4), &p).is_err(),
                "{path}"
            );
        }
        let mut p = params();
        p["questions"][1]["id"] = json!("db");
        assert!(
            UserInputRequest::parse(RequestId::Number(4), &p).is_err(),
            "duplicate ids"
        );
        let mut p = params();
        p["questions"] = json!(vec![
            p["questions"][0].clone();
            MAX_USER_INPUT_QUESTIONS + 1
        ]);
        assert!(UserInputRequest::parse(RequestId::Number(4), &p).is_err());
        let mut p = params();
        p["questions"][0]["question"] = json!("x".repeat(MAX_REQUEST_BYTES));
        assert_eq!(
            UserInputRequest::parse(RequestId::Number(4), &p).err(),
            Some(Error::Capacity)
        );
    }

    #[test]
    fn answers_use_codex_client_words_and_never_answer_secrets() {
        let request = UserInputRequest::parse(RequestId::Number(4), &params()).unwrap();
        let answers = BTreeMap::from([
            ("db".to_owned(), UserInputAnswer::Option("SQLite".into())),
            ("name".to_owned(), UserInputAnswer::Text("  Atlas  ".into())),
        ]);
        let response = request.answer(&answers).unwrap();
        assert_eq!(
            response.result(),
            &json!({"answers":{"db":{"answers":["SQLite"]},"name":{"answers":["user_note: Atlas"]},"token":{"answers":[]}}})
        );
        assert!(UserInputResponse::well_formed(response.result()));
        let other = request
            .answer(&BTreeMap::from([(
                "db".to_owned(),
                UserInputAnswer::Text("MySQL".into()),
            )]))
            .unwrap();
        assert_eq!(
            other.result()["answers"]["db"]["answers"],
            json!([OTHER_ANSWER_LABEL, "user_note: MySQL"])
        );
        for (id, answer, error) in [
            (
                "token",
                UserInputAnswer::Text("secret".into()),
                Error::Policy,
            ),
            ("db", UserInputAnswer::Option("Mongo".into()), Error::Scope),
            (
                "missing",
                UserInputAnswer::Option("SQLite".into()),
                Error::Scope,
            ),
            ("db", UserInputAnswer::Text("   ".into()), Error::Malformed),
            (
                "db",
                UserInputAnswer::Text("x".repeat(MAX_USER_INPUT_TEXT_BYTES + 1)),
                Error::Malformed,
            ),
        ] {
            assert_eq!(
                request
                    .answer(&BTreeMap::from([(id.to_owned(), answer)]))
                    .err(),
                Some(error),
                "{id}"
            );
        }
        assert!(request.answer(&BTreeMap::new()).is_err());
        assert_eq!(request.unanswered().result(), &json!({"answers":{}}));
        let mut closed = params();
        closed["questions"][0]["isOther"] = json!(false);
        let closed = UserInputRequest::parse(RequestId::Number(4), &closed).unwrap();
        assert_eq!(
            closed
                .answer(&BTreeMap::from([(
                    "db".to_owned(),
                    UserInputAnswer::Text("MySQL".into())
                )]))
                .err(),
            Some(Error::Malformed),
            "text needs an Other choice when the question offers options"
        );
    }

    #[test]
    fn approval_requests_never_build_answers_even_when_mixed_or_secret() {
        for id in [
            "mcp_tool_call_approval_call",
            "skill_mcp_dependency_install",
        ] {
            for secret in [false, true] {
                let mut p = params();
                p["questions"][1]["id"] = json!(id);
                p["questions"][1]["isSecret"] = json!(secret);
                let request = UserInputRequest::parse(RequestId::Number(4), &p).unwrap();
                assert!(request.is_codex_approval());
                // Even an answer only to the ordinary question is refused:
                // the entire approval request belongs to the owner flow.
                let ordinary =
                    BTreeMap::from([("db".into(), UserInputAnswer::Option("SQLite".into()))]);
                assert_eq!(
                    request.answer(&ordinary).err(),
                    Some(Error::Policy),
                    "{id}, secret: {secret}"
                );
                let approval = BTreeMap::from([(id.into(), UserInputAnswer::Text("Allow".into()))]);
                assert_eq!(request.answer(&approval).err(), Some(Error::Policy));
                assert_eq!(request.unanswered().result(), &json!({"answers":{}}));
            }
        }
        // A normal question mentioning an MCP tool is still just a question;
        // prompts are classified by Codex's reserved ids, not prose/labels.
        let mut p = params();
        p["questions"][0]["question"] = json!("Which MCP tool should the service describe?");
        let request = UserInputRequest::parse(RequestId::Number(4), &p).unwrap();
        assert!(!request.is_codex_approval());
        assert!(
            request
                .answer(&BTreeMap::from([(
                    "db".into(),
                    UserInputAnswer::Option("SQLite".into())
                )]))
                .is_ok()
        );
    }

    #[test]
    fn response_shape_check_refuses_anything_but_answer_lists() {
        for bad in [
            json!({"answers":{"q":{"answers":["a"]}},"extra":1}),
            json!({"answers":{"q":{"answers":"a"}}}),
            json!({"answers":{"q":{"answers":[1]}}}),
            json!({"answers":{"q":{"answers":["a"],"note":"b"}}}),
            json!({"answers":{"":{"answers":[]}}}),
            json!([]),
        ] {
            assert!(!UserInputResponse::well_formed(&bad), "{bad}");
        }
        assert!(UserInputResponse::well_formed(&json!({"answers":{}})));
    }
}
