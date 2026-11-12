//! Bounded additive-budget reconciliation under the signed-in native lifetime.
//! Stop recovery precedes both new process and original-demand recovery. Each
//! cursor belongs to its own journal; closing the mini app changes no custody.
use super::NativeError;
use ::hagency::native_owner::{CurrentSharedExecutor,AgentBudgetProgress,SharedExecutionProgress};
trait BudgetIo {
    fn recover_stop(&self,after:Option<String>)->impl std::future::Future<Output=Result<Option<String>,NativeError>>+Send;
    fn apply_delta(&self,after:Option<String>)->impl std::future::Future<Output=Result<Option<String>,NativeError>>+Send;
    fn recover_demand(&self,after:Option<String>)->impl std::future::Future<Output=Result<Option<String>,NativeError>>+Send;
    fn poll_process(&self,after:Option<String>)->impl std::future::Future<Output=Result<Option<String>,NativeError>>+Send;
    fn poll_demand(&self,after:Option<String>)->impl std::future::Future<Output=Result<Option<String>,NativeError>>+Send;
}
impl BudgetIo for CurrentSharedExecutor {
    async fn recover_stop(&self,after:Option<String>)->Result<Option<String>,NativeError>{self.resume_next_demand_stop(after).await.map(|s|s.map(|v|v.command_id))}
    async fn apply_delta(&self,after:Option<String>)->Result<Option<String>,NativeError>{
        self.poll_budget(after).await.map(|s|match s{AgentBudgetProgress::Idle=>None,AgentBudgetProgress::HistoricalReceiptMissing{command_id}|AgentBudgetProgress::Recorded{command_id,..}=>Some(command_id)})
    }
    async fn recover_demand(&self,after:Option<String>)->Result<Option<String>,NativeError>{self.resume_next_budget_demand(after).await.map(|s|s.map(|v|v.demand_id))}
    async fn poll_process(&self,after:Option<String>)->Result<Option<String>,NativeError>{
        self.poll_budget_successor(after).await.map(|s|match s{SharedExecutionProgress::Idle=>None,SharedExecutionProgress::Running{command_id}|SharedExecutionProgress::Stopping{command_id}|SharedExecutionProgress::HistoricalEvidenceMissing{command_id}|SharedExecutionProgress::Recorded{command_id,..}=>Some(command_id)})
    }
    async fn poll_demand(&self,after:Option<String>)->Result<Option<String>,NativeError>{self.poll_budget_demand(after).await.map(|s|s.next_after)}
}
#[derive(Default)]
pub(super) struct Polling {stop:Option<String>,delta:Option<String>,demand:Option<String>,process:Option<String>,assignment:Option<String>}
impl Polling {
    pub(super) async fn poll(&mut self,executor:&CurrentSharedExecutor,live:impl Fn()->bool){self.existing(executor,live).await;}
    async fn existing(&mut self,io:&impl BudgetIo,live:impl Fn()->bool){
        if !live(){return}
        let result=io.recover_stop(self.stop.clone()).await;
        if !live(){return} if let Ok(next)=result {self.stop=next;}
        let result=io.apply_delta(self.delta.clone()).await;
        if !live(){return} if let Ok(next)=result {self.delta=next;}
        let result=io.recover_demand(self.demand.clone()).await;
        if !live(){return} if let Ok(next)=result {self.demand=next;}
        let result=io.poll_process(self.process.clone()).await;
        if !live(){return} if let Ok(next)=result {self.process=next;}
        let result=io.poll_demand(self.assignment.clone()).await;
        if !live(){return} if let Ok(next)=result {self.assignment=next;}
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex,atomic::{AtomicBool,Ordering}};
    struct Fixture {live:AtomicBool,retire_at:Option<&'static str>,fail_at:Option<&'static str>,requests:Mutex<Vec<(&'static str,Option<String>)>>}
    impl Fixture {
        fn call(&self,step:&'static str,after:Option<String>)->Result<Option<String>,NativeError>{
            self.requests.lock().unwrap().push((step,after));
            if self.retire_at==Some(step){self.live.store(false,Ordering::Release)}
            if self.fail_at==Some(step){return Err(super::super::source_error("journal_unavailable"))}
            Ok(Some(format!("original-{step}")))
        }
    }
    impl BudgetIo for Fixture {
        async fn recover_stop(&self,a:Option<String>)->Result<Option<String>,NativeError>{self.call("stop",a)}
        async fn apply_delta(&self,a:Option<String>)->Result<Option<String>,NativeError>{self.call("delta",a)}
        async fn recover_demand(&self,a:Option<String>)->Result<Option<String>,NativeError>{self.call("demand",a)}
        async fn poll_process(&self,a:Option<String>)->Result<Option<String>,NativeError>{self.call("process",a)}
        async fn poll_demand(&self,a:Option<String>)->Result<Option<String>,NativeError>{self.call("assignment",a)}
    }
    fn fixture(retire:Option<&'static str>,fail:Option<&'static str>)->Fixture{Fixture{live:AtomicBool::new(true),retire_at:retire,fail_at:fail,requests:Mutex::new(Vec::new())}}
    #[tokio::test]
    async fn budget_stop_precedes_delta_and_launch_with_independent_original_cursors(){
        let io=fixture(None,None);let mut p=Polling::default();
        for _ in 0..2 {p.existing(&io,||io.live.load(Ordering::Acquire)).await;}
        let steps=["stop","delta","demand","process","assignment"];
        let expected:Vec<_>=steps.into_iter().map(|s|(s,None)).chain(steps.into_iter().map(|s|(s,Some(format!("original-{s}"))))).collect();
        assert_eq!(*io.requests.lock().unwrap(),expected);
    }
    #[tokio::test]
    async fn budget_retirement_at_every_await_prevents_followup(){
        for (index,step) in ["stop","delta","demand","process","assignment"].into_iter().enumerate(){
            let io=fixture(Some(step),None);let mut p=Polling::default();
            for _ in 0..2 {p.existing(&io,||io.live.load(Ordering::Acquire)).await;}
            assert_eq!(io.requests.lock().unwrap().len(),index+1);
            let cursor=match step{"stop"=>p.stop,"delta"=>p.delta,"demand"=>p.demand,"process"=>p.process,_=>p.assignment};assert!(cursor.is_none());
        }
    }
    #[tokio::test]
    async fn budget_failed_delta_retains_retry_without_replacing_demand(){
        let io=fixture(None,Some("delta"));let mut p=Polling{delta:Some("prior-delta".into()),..Default::default()};
        p.existing(&io,||true).await;
        assert_eq!(p.delta.as_deref(),Some("prior-delta"));assert_eq!(p.demand.as_deref(),Some("original-demand"));assert_eq!(p.assignment.as_deref(),Some("original-assignment"));
    }
}
