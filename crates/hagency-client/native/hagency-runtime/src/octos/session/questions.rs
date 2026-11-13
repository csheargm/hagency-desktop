//! The OUP question's ordered answers and the host's provider-neutral card.
//! Question answers carry no tool or resource authority.
use super::Error;
use crate::questions::{Answer, Answers, Question, QuestionOption, valid_answers, valid_questions};
use serde_json::{Value, json};

pub(super) fn parse(params: &Value) -> Result<Vec<Question>, Error> {
    let raw = params["questions"].as_array().ok_or(Error::Identity)?;
    if !(1..=4).contains(&raw.len()) {
        return Err(Error::Identity);
    }
    let mut questions = Vec::with_capacity(raw.len());
    for (index, item) in raw.iter().enumerate() {
        let options = item["options"].as_array().ok_or(Error::Identity)?;
        if !(2..=4).contains(&options.len()) {
            return Err(Error::Identity);
        }
        let options = options
            .iter()
            .map(|option| {
                Ok(QuestionOption {
                    label: option["label"].as_str().ok_or(Error::Identity)?.into(),
                    description: option["description"].as_str().unwrap_or_default().into(),
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        questions.push(Question {
            id: format!("q{index}"),
            header: item["header"].as_str().unwrap_or_default().into(),
            question: item["question"].as_str().ok_or(Error::Identity)?.into(),
            options,
            other: item["allow_free_text"].as_bool().unwrap_or(false),
            secret: false,
            multi_select: item["multi_select"].as_bool().unwrap_or(false),
        });
    }
    if !valid_questions(&questions) {
        return Err(Error::Identity);
    }
    Ok(questions)
}

pub(super) fn response(questions: &[Question], answers: &Answers) -> Result<Value, Error> {
    if !valid_answers(questions, answers) {
        return Err(Error::Identity);
    }
    Ok(Value::Array(
        questions
            .iter()
            .map(|question| match answers.get(&question.id) {
                Some(Answer::Option(label)) => json!({"selected_labels":[label]}),
                Some(Answer::Options(labels)) => json!({"selected_labels":labels}),
                Some(Answer::Text(text)) => json!({"selected_labels":[], "free_text":text}),
                None => json!({"selected_labels":[]}),
            })
            .collect(),
    ))
}
