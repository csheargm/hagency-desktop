//! Personal providers use the same Matrix card, encrypted reply checks,
//! recovery journal and outbox as pooled execution.
use super::*;
use super::super::native::shared_execution::questions::{QuestionContext, Questions, Source};
use hagency_agent_local::questions::PendingQuestion;

#[derive(Default)]
pub(in crate::console) struct Host {
    questions: Option<Questions<PendingQuestion>>,
    receipts: Vec<hagency_agent_local::questions::Delivery>,
}
impl Host {
    fn settle(&mut self) {
        self.receipts.retain_mut(|receipt| {
            let Some(accepted) = receipt.status() else {
                return true;
            };
            if let Some(questions) = &mut self.questions {
                questions.delivered(&receipt.id, accepted);
            }
            false
        });
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        // A final result may arrive immediately after the native answer write,
        // before the next poll. Preserve that synchronous receipt on drop.
        self.settle();
    }
}

pub(super) fn device_ready(device: Arc<e2ee::AgentDevice>) {
    super::super::native::shared_execution::questions::device_ready(
        Arc::new(Source::Device(device.clone())),
        Some(device.question_records.clone()),
    );
}

pub(super) async fn serve<T: Transport>(
    transport: &T,
    lease: &LeaseRef,
    config: &StartConfig,
    owner: &str,
    prepared: &inbox::Prepared,
    mut cancel: watch::Receiver<bool>,
    mut receiver: tokio::sync::mpsc::Receiver<PendingQuestion>,
) {
    let mut responders = vec![prepared.dispatch.requester_mxid.clone()];
    if responders[0] != owner {
        responders.push(owner.to_owned());
    }
    let context = QuestionContext {
        id: prepared.dispatch.id.clone(),
        room_id: prepared.dispatch.room_id.clone(),
        event_id: prepared.dispatch.event_id.clone(),
        thread_root: prepared.dispatch.thread_root.clone(),
        responders,
    };
    let mut host = Host::default();
    let mut opening_time = Duration::ZERO;
    let mut poll = tokio::time::interval(Duration::from_millis(500));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! { biased;
            _ = canceled(&mut cancel) => break,
            pending = receiver.recv() => {
                let Some(pending) = pending else { break; };
                if pending.scope != prepared.dispatch.scope() || pending.dispatch != prepared.dispatch.id || pending.is_closed() {
                    continue;
                }
                if host.questions.is_none() {
                    // Device opening uses the same deadline as the native
                    // question. A failed open returns no answer, never a tool
                    // approval and never a second model request.
                    let started = tokio::time::Instant::now();
                    let result = tokio::time::timeout_at(pending.deadline, e2ee::device(transport, lease, config, &mut cancel)).await;
                    opening_time += started.elapsed();
                    if let Ok(Ok(device)) = result {
                        host.questions = Some(Questions::with_device(device.clone(), device.question_records.clone()));
                        host.questions.as_mut().unwrap().charge_opening(opening_time);
                    }
                }
                let delivery = match host.questions.as_mut() {
                    Some(questions) => questions.ask_on_device(&context, pending).await.ok().flatten(),
                    None => Some(pending.reply(Default::default())),
                };
                if let Some(delivery) = delivery {
                    host.receipts.push(delivery);
                }
            }
            _ = poll.tick(), if host.questions.as_ref().is_some_and(Questions::busy) => {
                host.settle();
                let questions = host.questions.as_mut().expect("active card controller");
                for delivery in questions.collect_ready().await.unwrap_or_default() {
                    host.receipts.push(delivery);
                }
                let _ = questions.publish().await;
            }
        }
    }
    // Drop closes every still-waiting or unacknowledged card through the
    // common durable outbox, including host restart/task cancellation.
}
