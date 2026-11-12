//! Provider-independent question data. These values carry no tool permission,
//! native callback identity, or authority to write to a runtime connection.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_QUESTIONS: usize = 8;
pub const MAX_OPTIONS: usize = 16;
pub const MAX_TEXT_BYTES: usize = 4096;
pub const MAX_LABEL_BYTES: usize = 256;
pub const MAX_DESCRIPTION_BYTES: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
    pub other: bool,
    pub secret: bool,
    pub multi_select: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum Answer {
    Option(String),
    Options(Vec<String>),
    Text(String),
}
pub type Answers = BTreeMap<String, Answer>;

impl Answer {
    pub fn display(&self) -> String {
        match self {
            Self::Option(value) | Self::Text(value) => value.clone(),
            Self::Options(values) => values.join(", "),
        }
    }
}

impl Question {
    pub fn accepts_text(&self) -> bool {
        self.other || self.options.is_empty()
    }

    pub fn accepts(&self, answer: &Answer) -> bool {
        if self.secret {
            return false;
        }
        let has = |label: &String| self.options.iter().any(|option| &option.label == label);
        match answer {
            Answer::Option(label) => has(label),
            Answer::Options(labels) => {
                self.multi_select
                    && !labels.is_empty()
                    && labels.len() <= self.options.len()
                    && labels.iter().all(has)
                    && labels.iter().collect::<BTreeSet<_>>().len() == labels.len()
            }
            Answer::Text(text) => self.accepts_text() && prose(text, MAX_TEXT_BYTES),
        }
    }

    pub fn valid(&self) -> bool {
        !self.id.is_empty()
            && self.id.len() <= 255
            && !self.id.chars().any(char::is_control)
            && (self.header.is_empty() || prose(&self.header, MAX_LABEL_BYTES))
            && prose(&self.question, MAX_TEXT_BYTES)
            && self.options.len() <= MAX_OPTIONS
            && self.options.iter().all(|option| {
                prose(&option.label, MAX_LABEL_BYTES)
                    && !option.label.contains('\n')
                    && (option.description.is_empty()
                        || prose(&option.description, MAX_DESCRIPTION_BYTES))
            })
            && self
                .options
                .iter()
                .map(|option| &option.label)
                .collect::<BTreeSet<_>>()
                .len()
                == self.options.len()
    }
}

pub fn valid_questions(questions: &[Question]) -> bool {
    !questions.is_empty()
        && questions.len() <= MAX_QUESTIONS
        && questions.iter().all(Question::valid)
        && questions
            .iter()
            .map(|q| &q.id)
            .collect::<BTreeSet<_>>()
            .len()
            == questions.len()
}

pub fn valid_answers(questions: &[Question], answers: &Answers) -> bool {
    answers
        .iter()
        .all(|(id, answer)| questions.iter().any(|q| &q.id == id && q.accepts(answer)))
}

fn prose(value: &str, max: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= max
        && !value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
}
