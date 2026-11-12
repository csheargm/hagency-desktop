//! Native AskUserQuestion input/answer conversion. Only a callback privately
//! retained by SessionDriver can produce its control response.
use super::Error;
use crate::questions::{Answers, Question, QuestionOption, valid_answers, valid_questions};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(super) const TOOL: &str = "AskUserQuestion";

pub(super) fn parse(input: &Value) -> Result<Vec<Question>, Error> {
    let raw = input
        .get("questions")
        .and_then(Value::as_array)
        .ok_or(Error::Malformed)?;
    let mut texts = BTreeSet::new();
    let questions = raw
        .iter()
        .enumerate()
        .map(|(index, q)| {
            let text = q
                .get("question")
                .and_then(Value::as_str)
                .ok_or(Error::Malformed)?;
            // Native answers are keyed by question TEXT. Duplicate text cannot be
            // answered unambiguously even though the UI uses stable ordinal IDs.
            if !texts.insert(text) {
                return Err(Error::Malformed);
            }
            let options = q
                .get("options")
                .and_then(Value::as_array)
                .ok_or(Error::Malformed)?
                .iter()
                .map(|option| {
                    Ok(QuestionOption {
                        label: option
                            .get("label")
                            .and_then(Value::as_str)
                            .ok_or(Error::Malformed)?
                            .into(),
                        description: option
                            .get("description")
                            .and_then(Value::as_str)
                            .ok_or(Error::Malformed)?
                            .into(),
                    })
                })
                .collect::<Result<Vec<_>, Error>>()?;
            Ok(Question {
                id: format!("q{index}"),
                header: q
                    .get("header")
                    .and_then(Value::as_str)
                    .ok_or(Error::Malformed)?
                    .into(),
                question: text.into(),
                options,
                other: true,
                secret: false,
                multi_select: q
                    .get("multiSelect")
                    .and_then(Value::as_bool)
                    .ok_or(Error::Malformed)?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    if !valid_questions(&questions) {
        return Err(Error::Malformed);
    }
    Ok(questions)
}

pub(super) fn response(input: Value, answers: &Answers) -> Result<Value, Error> {
    let questions = parse(&input)?;
    if !valid_answers(&questions, answers) {
        return Err(Error::Identity);
    }
    if answers.is_empty() {
        return Ok(
            json!({"behavior":"deny","message":"No answer was received. Continue with the available information.","interrupt":false}),
        );
    }
    let mut updated = input;
    let mut native = serde_json::Map::new();
    for q in questions {
        if let Some(answer) = answers.get(&q.id) {
            // Claude Code's original AskUserQuestion schema accepts strings;
            // its own terminal uses comma-separated labels for multi-select.
            native.insert(q.question, Value::String(answer.display()));
        }
    }
    updated["answers"] = Value::Object(native);
    Ok(json!({"behavior":"allow","updatedInput":updated}))
}
