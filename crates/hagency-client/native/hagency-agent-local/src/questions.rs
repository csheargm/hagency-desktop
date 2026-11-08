//! One turn's user questions. Separate from owner tool approvals; answer data
//! never authorizes a tool. Dropping a native wait withdraws its host card.
use crate::Scope;
use hagency_runtime::questions::{Answers, Question, valid_answers, valid_questions};
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};

#[derive(Clone)]
pub struct QuestionQueue(mpsc::Sender<PendingQuestion>);

pub struct PendingQuestion {
    pub id: String,
    pub scope: Scope,
    pub dispatch: String,
    pub questions: Vec<Question>,
    pub deadline: Instant,
    response: oneshot::Sender<SubmittedAnswer>,
}

/// A host answer waiting for the original native callback to accept it.
pub struct SubmittedAnswer {
    pub(crate) answers: Answers,
    accepted: oneshot::Sender<bool>,
}
impl SubmittedAnswer {
    pub(crate) fn accepted(self) {
        let _ = self.accepted.send(true);
    }
}

/// The card may show "answered" only when the original native write succeeds.
pub struct Delivery {
    pub id: String,
    accepted: oneshot::Receiver<bool>,
}
impl Delivery {
    pub async fn accepted(self) -> bool {
        self.accepted.await.unwrap_or(false)
    }
    pub fn status(&mut self) -> Option<bool> {
        match self.accepted.try_recv() {
            Ok(accepted) => Some(accepted),
            Err(oneshot::error::TryRecvError::Closed) => Some(false),
            Err(oneshot::error::TryRecvError::Empty) => None,
        }
    }
}

impl PendingQuestion {
    pub fn is_closed(&self) -> bool {
        self.response.is_closed() || Instant::now() >= self.deadline
    }
    pub fn reply(self, answers: Answers) -> Delivery {
        let (accepted, receipt) = oneshot::channel();
        if !self.is_closed() && valid_answers(&self.questions, &answers) {
            let _ = self.response.send(SubmittedAnswer { answers, accepted });
        }
        Delivery {
            id: self.id,
            accepted: receipt,
        }
    }
}

pub fn question_queue() -> (QuestionQueue, mpsc::Receiver<PendingQuestion>) {
    let (send, receive) = mpsc::channel(8);
    (QuestionQueue(send), receive)
}
impl QuestionQueue {
    pub(crate) async fn ask(
        &self,
        scope: &Scope,
        dispatch: &str,
        id: String,
        questions: Vec<Question>,
        deadline: Instant,
    ) -> Option<SubmittedAnswer> {
        if !valid_questions(&questions) {
            return None;
        }
        let (response, receive) = oneshot::channel();
        self.0
            .try_send(PendingQuestion {
                id,
                scope: scope.clone(),
                dispatch: dispatch.into(),
                questions,
                deadline,
                response,
            })
            .ok()?;
        tokio::time::timeout_at(deadline, receive).await.ok()?.ok()
    }
}
