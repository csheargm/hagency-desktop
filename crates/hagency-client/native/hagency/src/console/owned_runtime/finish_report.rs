//! How a run that ended without a reply tells the server. A failure the run
//! card already shows goes as `FinishOnCard`, so the room gets one failure
//! line (the card's, which carries the policy guidance) instead of the card
//! plus the server's "⚠️ This request failed…" notice. A server older than
//! that operation refuses it as unsupported; the plain finish then follows
//! and that server posts its notice as before.
use super::{LeaseRef, Op, Outcome, Response, RuntimeError, Transport, inbox, operation};
use tokio::sync::watch;

pub(super) async fn report_finish<T: Transport>(
    console: &T,
    cancel: &mut watch::Receiver<bool>,
    lease: &LeaseRef,
    prepared: &inbox::Prepared,
    outcome: Outcome,
    shown_on_card: bool,
) -> Result<Response, RuntimeError> {
    let (dispatch_id, execution_id) = (prepared.dispatch.id.clone(), prepared.execution_id.clone());
    if shown_on_card {
        let op = Op::FinishOnCard { lease: lease.clone(), dispatch_id: dispatch_id.clone(), execution_id: execution_id.clone() };
        match operation(console, cancel, op).await {
            Err(RuntimeError::Request) => {} // an older server: the plain finish below
            reported => return reported,
        }
    }
    operation(console, cancel, Op::Finish { lease: lease.clone(), dispatch_id, execution_id, outcome }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Answers each operation from a script and records which one came.
    struct Script(Mutex<Vec<(&'static str, Result<Response, RuntimeError>)>>, Mutex<Vec<&'static str>>);
    impl Transport for Script {
        async fn execute(&self, op: Op) -> Result<Response, RuntimeError> {
            let name = match &op {
                Op::FinishOnCard { dispatch_id, execution_id, .. } => {
                    assert_eq!((dispatch_id.as_str(), execution_id.as_str()), ("dispatch-a", "exec-a"));
                    "on-card"
                }
                Op::Finish { outcome, .. } => {
                    assert!(matches!(outcome, Outcome::Failed));
                    "finish"
                }
                _ => panic!("only finish operations"),
            };
            self.1.lock().unwrap().push(name);
            let (expected, result) = self.0.lock().unwrap().remove(0);
            assert_eq!(name, expected);
            result
        }
    }
    fn prepared() -> inbox::Prepared {
        let dispatch = serde_json::from_value(serde_json::json!({"id":"dispatch-a","bindingId":"b","agentId":"a","eventId":"$e:t",
            "roomId":"!r:t","requesterMxid":"@bob:t","threadRoot":"$e:t","body":"x","state":"running","bindingGeneration":1,
            "dispatchEpoch":1,"dispatchDeviceId":"d","executionId":"exec-a","outcome":null})).unwrap();
        inbox::Prepared { dispatch, execution_id: "exec-a".into() }
    }
    async fn run(shown: bool, script: Vec<(&'static str, Result<Response, RuntimeError>)>) -> (bool, Vec<&'static str>) {
        let console = Script(Mutex::new(script), Mutex::default());
        let (_stop, mut cancel) = watch::channel(false);
        let lease = LeaseRef { agent_id: "a".into(), epoch: 1 };
        let done = report_finish(&console, &mut cancel, &lease, &prepared(), Outcome::Failed, shown).await;
        (matches!(done, Ok(Response::Finished)), console.1.into_inner().unwrap())
    }

    #[tokio::test]
    async fn a_failure_on_the_card_asks_the_server_for_no_notice_and_falls_back_on_an_older_one() {
        assert_eq!(run(true, vec![("on-card", Ok(Response::Finished))]).await, (true, vec!["on-card"]));
        let older = vec![("on-card", Err(RuntimeError::Request)), ("finish", Ok(Response::Finished))];
        assert_eq!(run(true, older).await, (true, vec!["on-card", "finish"]), "an older server still records the failure");
        assert_eq!(run(false, vec![("finish", Ok(Response::Finished))]).await, (true, vec!["finish"]), "no card: the server's notice is owed");
        let lost = vec![("on-card", Err(RuntimeError::Transport))];
        assert_eq!(run(true, lost).await.1, vec!["on-card"], "a transport failure is not an older server");
    }
}
